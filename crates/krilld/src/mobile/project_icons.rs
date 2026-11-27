//! 모바일 프로젝트 아이콘 비동기 SWR 탐지·디스크 캐시·배치 서빙.
//!
//! - 스냅샷에는 이미지 바이트·로컬 경로를 넣지 않고 16자리 콘텐츠 해시(`{"kind":"image","hash":"..."}`)만 싣는다.
//! - 콜드/만료 저장소는 백그라운드 스레드(`mobile-icon-swr`)에서 데스크톱(`krill_proto::project_icon`)과
//!   동일한 우선순위로 탐지해 `revision`을 올리므로 스냅샷 응답 지연을 0ms로 유지한다.
//! - 24KB를 넘는 래스터(PNG·JPEG·WebP)는 64×64 PNG 썸네일로 한 번만 축소·캐시(`m64-<hash>.png`)해 전송한다.

use super::e2ee::invalid;
use super::workspaces::Catalog;
use image::ImageReader;
use image::imageops::FilterType;
use krill_proto::ipc;
use krill_proto::project_icon::{
    MAX_ICON_FILE_BYTES, MAX_ICON_IMAGE_BYTES, REPO_ICON_DIR, ResolvedIconSource,
    cached_icon_path_for_url, detect_icon_mime, has_valid_icon_bytes, resolve_project_icon_source,
    short_sha256_hex, write_cached_icon_bytes,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Cursor, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const ICON_TTL: Duration = Duration::from_secs(300);
const MAX_BATCH_KEYS: usize = 64;
const MAX_BATCH_JSON_BYTES: usize = 256 * 1024;
const THUMBNAIL_THRESHOLD_BYTES: usize = 24 * 1024;
const THUMBNAIL_MAX_PX: u32 = 64;
const MAX_IMAGE_DECODE_DIM: u32 = 4096;
const MAX_IMAGE_DECODE_ALLOC: u64 = 32 * 1024 * 1024;
#[cfg(not(test))]
const ICON_RETRY_AFTER_NETWORK_ERROR: Duration = Duration::from_secs(60);
#[cfg(not(test))]
const ICON_RETRY_AFTER_INVALID_IMAGE: Duration = Duration::from_secs(600);

/// 저장소 하나의 아이콘 탐지 입력.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RepoIconTarget {
    pub repo: PathBuf,
    pub is_git: bool,
    pub explicit: Option<(String, String)>,
}

/// 스냅샷 JSON에 싣는 경량 아이콘 메타데이터(로컬 경로·원본 바이트 제외).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MobileIconMeta {
    Emoji(String),
    Lucide(String),
    Image { hash: String },
}

impl MobileIconMeta {
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Emoji(value) => json!({"kind": "emoji", "value": value}),
            Self::Lucide(value) => json!({"kind": "lucide", "value": value}),
            Self::Image { hash } => json!({"kind": "image", "hash": hash}),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedImage {
    path: PathBuf,
    hash: String,
    mime: &'static str,
}

#[derive(Debug, Clone)]
struct CachedRepoIcon {
    explicit: Option<(String, String)>,
    is_git: bool,
    checked_at: Instant,
    meta: Option<MobileIconMeta>,
    image: Option<ResolvedImage>,
}

/// 모바일 백엔드가 보유하는 프로젝트 아이콘 SWR 캐시.
#[derive(Default)]
pub(super) struct RepoIconCache {
    entries: Arc<Mutex<HashMap<PathBuf, CachedRepoIcon>>>,
    inflight: Arc<Mutex<HashSet<PathBuf>>>,
    #[cfg(not(test))]
    failed_urls: Arc<Mutex<HashMap<String, Instant>>>,
    revision: Arc<AtomicU64>,
}

struct InflightGuard {
    inflight: Arc<Mutex<HashSet<PathBuf>>>,
    paths: Vec<PathBuf>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut set = crate::session::lock(&self.inflight);
        for path in &self.paths {
            set.remove(path);
        }
    }
}

impl RepoIconCache {
    pub(super) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        crate::session::lock(&self.inflight).is_empty()
    }

    /// 스냅샷 경로에서 0ms로 아이콘 메타데이터를 돌려주고, 미캐시·만료 저장소만 백그라운드 스레드에 예약한다.
    pub(super) fn resolve_swr(
        &self,
        targets: &[RepoIconTarget],
        state_dir: Option<&Path>,
    ) -> BTreeMap<PathBuf, MobileIconMeta> {
        let mut found = BTreeMap::new();
        let mut queued = Vec::new();
        {
            let cache = crate::session::lock(&self.entries);
            for target in targets {
                if let Some((kind, value)) = target.explicit.as_ref() {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        match kind.trim() {
                            "emoji" => {
                                found.insert(
                                    target.repo.clone(),
                                    MobileIconMeta::Emoji(trimmed.to_owned()),
                                );
                                continue;
                            }
                            "lucide" => {
                                found.insert(
                                    target.repo.clone(),
                                    MobileIconMeta::Lucide(trimmed.to_owned()),
                                );
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(entry) = cache.get(&target.repo)
                    && entry.explicit == target.explicit
                    && entry.is_git == target.is_git
                {
                    if let Some(meta) = &entry.meta {
                        found.insert(target.repo.clone(), meta.clone());
                    }
                    if entry.checked_at.elapsed() >= ICON_TTL {
                        queued.push(target.clone());
                    }
                } else {
                    queued.push(target.clone());
                }
            }
        }
        if queued.is_empty() {
            return found;
        }
        {
            let mut inflight = crate::session::lock(&self.inflight);
            queued.retain(|target| inflight.insert(target.repo.clone()));
        }
        if queued.is_empty() {
            return found;
        }
        let cache_dir = state_dir.map(|d| d.join(REPO_ICON_DIR));
        let entries = Arc::clone(&self.entries);
        let inflight = Arc::clone(&self.inflight);
        #[cfg(not(test))]
        let failed_urls = Arc::clone(&self.failed_urls);
        let revision = Arc::clone(&self.revision);
        let scheduled_paths: Vec<PathBuf> = queued.iter().map(|t| t.repo.clone()).collect();
        let rollback_paths = scheduled_paths.clone();
        let spawned = thread::Builder::new()
            .name("mobile-icon-swr".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                let _guard = InflightGuard {
                    inflight,
                    paths: scheduled_paths,
                };
                let mut changed = false;
                let mut pending_remote: Vec<(RepoIconTarget, String)> = Vec::new();
                // 1단계: 로컬 파비콘과 이미 디스크에 캐시된 원격 아이콘을 먼저 전부 확정한다.
                for target in queued {
                    match resolve_target_local(&target, cache_dir.as_deref()) {
                        LocalProbe::Ready(meta, image) => {
                            if store_resolved(&entries, &target, meta, image) {
                                changed = true;
                            }
                        }
                        LocalProbe::NeedsRemoteFetch(url) => {
                            if store_resolved(&entries, &target, None, None) {
                                changed = true;
                            }
                            pending_remote.push((target, url));
                        }
                    }
                }
                if changed {
                    revision.fetch_add(1, Ordering::Relaxed);
                }
                // 2단계: 디스크에 없는 HTTPS 원격 아이콘만 URL 중복 제거 후 백그라운드로 내려받는다.
                #[cfg(not(test))]
                if !pending_remote.is_empty()
                    && let Some(dir) = cache_dir.as_deref()
                {
                    let mut by_url: HashMap<String, Vec<RepoIconTarget>> = HashMap::new();
                    for (target, url) in pending_remote {
                        by_url.entry(url).or_default().push(target);
                    }
                    let mut remote_changed = false;
                    for (url, url_targets) in by_url {
                        if let Some(img) = fetch_remote_icon_to_cache(&url, dir, &failed_urls) {
                            let meta = Some(MobileIconMeta::Image {
                                hash: img.hash.clone(),
                            });
                            for target in url_targets {
                                if store_resolved(
                                    &entries,
                                    &target,
                                    meta.clone(),
                                    Some(img.clone()),
                                ) {
                                    remote_changed = true;
                                }
                            }
                        }
                    }
                    if remote_changed {
                        revision.fetch_add(1, Ordering::Relaxed);
                    }
                }
                #[cfg(test)]
                let _ = pending_remote;
                crate::alloc::purge();
            });
        if spawned.is_err() {
            self.rollback_inflight(&rollback_paths);
        }
        found
    }

    fn rollback_inflight(&self, paths: &[PathBuf]) {
        let mut inflight = crate::session::lock(&self.inflight);
        for path in paths {
            inflight.remove(path);
        }
    }

    /// 등록된 저장소 키 배치를 읽어 최대 256 KiB 한도 안에서 아이콘 payload를 돌려준다.
    pub(super) fn read_batch(
        &self,
        catalog: &Catalog,
        keys: &[String],
        state_dir: Option<&Path>,
    ) -> io::Result<Value> {
        if keys.len() > MAX_BATCH_KEYS {
            return Err(invalid());
        }
        let cache_dir = state_dir.map(|d| d.join(REPO_ICON_DIR));
        let mut seen = HashSet::new();
        let mut icons = Map::new();
        let mut truncated = false;
        let mut approx_bytes = 32usize;
        let mut decoded_raster = false;

        for key in keys {
            if !seen.insert(key.as_str()) {
                continue;
            }
            let Some(target) = catalog.icon_target_for_key(key) else {
                continue;
            };
            let image = self.resolve_image_for_read(&target, cache_dir.as_deref());
            let Some(img) = image else {
                continue;
            };
            let Some((hash, mime, payload, used_decoder)) =
                load_mobile_icon_payload(&img.path, cache_dir.as_deref())
            else {
                continue;
            };
            if used_decoder {
                decoded_raster = true;
            }
            let encoded = super::ws::base64(&payload, false);
            let entry_bytes = key
                .len()
                .saturating_add(hash.len())
                .saturating_add(mime.len())
                .saturating_add(encoded.len())
                .saturating_add(64);
            if entry_bytes >= ipc::MAX_FRAME.saturating_sub(4096) {
                continue;
            }
            if !icons.is_empty() && approx_bytes.saturating_add(entry_bytes) > MAX_BATCH_JSON_BYTES
            {
                truncated = true;
                break;
            }
            approx_bytes = approx_bytes.saturating_add(entry_bytes);
            icons.insert(
                key.clone(),
                json!({
                    "hash": hash,
                    "mime": mime,
                    "data_base64": encoded,
                }),
            );
        }
        if decoded_raster {
            crate::alloc::purge();
        }
        Ok(json!({
            "icons": icons,
            "truncated": truncated,
        }))
    }

    fn resolve_image_for_read(
        &self,
        target: &RepoIconTarget,
        cache_dir: Option<&Path>,
    ) -> Option<ResolvedImage> {
        {
            let cache = crate::session::lock(&self.entries);
            if let Some(entry) = cache.get(&target.repo)
                && entry.explicit == target.explicit
                && entry.is_git == target.is_git
            {
                return entry.image.clone();
            }
        }
        match resolve_target_local(target, cache_dir) {
            LocalProbe::Ready(meta, image) => {
                store_resolved(&self.entries, target, meta, image.clone());
                image
            }
            LocalProbe::NeedsRemoteFetch(_) => None,
        }
    }
}

enum LocalProbe {
    Ready(Option<MobileIconMeta>, Option<ResolvedImage>),
    NeedsRemoteFetch(String),
}

fn store_resolved(
    entries: &Mutex<HashMap<PathBuf, CachedRepoIcon>>,
    target: &RepoIconTarget,
    meta: Option<MobileIconMeta>,
    image: Option<ResolvedImage>,
) -> bool {
    let mut cache = crate::session::lock(entries);
    let changed = cache.get(&target.repo).map_or(meta.is_some(), |prev| {
        prev.meta != meta || prev.image != image
    });
    cache.insert(
        target.repo.clone(),
        CachedRepoIcon {
            explicit: target.explicit.clone(),
            is_git: target.is_git,
            checked_at: Instant::now(),
            meta,
            image,
        },
    );
    changed
}

fn resolve_target_local(target: &RepoIconTarget, cache_dir: Option<&Path>) -> LocalProbe {
    let explicit_ref = target
        .explicit
        .as_ref()
        .map(|(k, v)| (k.as_str(), v.as_str()));
    match resolve_project_icon_source(&target.repo, target.is_git, cache_dir, explicit_ref) {
        Some(ResolvedIconSource::Emoji(emoji)) => {
            LocalProbe::Ready(Some(MobileIconMeta::Emoji(emoji)), None)
        }
        Some(ResolvedIconSource::Lucide(name)) => {
            LocalProbe::Ready(Some(MobileIconMeta::Lucide(name)), None)
        }
        Some(ResolvedIconSource::LocalFile(path)) => match inspect_icon_file(&path) {
            Some(img) => LocalProbe::Ready(
                Some(MobileIconMeta::Image {
                    hash: img.hash.clone(),
                }),
                Some(img),
            ),
            None => LocalProbe::Ready(None, None),
        },
        Some(ResolvedIconSource::RemoteUrl(url)) => {
            let Some(dir) = cache_dir else {
                return LocalProbe::Ready(None, None);
            };
            let cached = cached_icon_path_for_url(dir, &url);
            if let Some(img) = inspect_icon_file(&cached) {
                LocalProbe::Ready(
                    Some(MobileIconMeta::Image {
                        hash: img.hash.clone(),
                    }),
                    Some(img),
                )
            } else {
                LocalProbe::NeedsRemoteFetch(url)
            }
        }
        None => LocalProbe::Ready(None, None),
    }
}

fn is_downscalable_raster(mime: &str) -> bool {
    matches!(mime, "image/png" | "image/jpeg" | "image/webp")
}

fn read_icon_file_bounded(path: &Path) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() < 6 || meta.len() > MAX_ICON_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(MAX_ICON_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= MAX_ICON_FILE_BYTES).then_some(bytes)
}

fn inspect_icon_file(path: &Path) -> Option<ResolvedImage> {
    let bytes = read_icon_file_bounded(path)?;
    let mime = detect_icon_mime(&bytes)?;
    if !is_downscalable_raster(mime) && bytes.len() > MAX_ICON_IMAGE_BYTES {
        return None;
    }
    let hash = short_sha256_hex(&bytes);
    Some(ResolvedImage {
        path: path.to_path_buf(),
        hash,
        mime,
    })
}

/// 아이콘 원본을 읽어 모바일 전송용 `(hash, mime, bytes, used_decoder)`로 준비한다.
/// 24KB를 넘는 PNG·JPEG·WebP는 64×64 PNG 썸네일로 축소해 디스크 캐시(`m64-<hash>.png`)에 보관한다.
fn load_mobile_icon_payload(
    path: &Path,
    cache_dir: Option<&Path>,
) -> Option<(String, &'static str, Vec<u8>, bool)> {
    let bytes = read_icon_file_bounded(path)?;
    let mime = detect_icon_mime(&bytes)?;
    let hash = short_sha256_hex(&bytes);

    if is_downscalable_raster(mime) && bytes.len() > THUMBNAIL_THRESHOLD_BYTES {
        if let Some(dir) = cache_dir {
            let thumb_path = dir.join(format!("m64-{hash}.png"));
            if let Some(cached) = read_icon_file_bounded(&thumb_path)
                && cached.len() <= MAX_ICON_IMAGE_BYTES
                && detect_icon_mime(&cached) == Some("image/png")
            {
                return Some((hash, "image/png", cached, false));
            }
            if let Some(thumb) = downscale_to_png_64(&bytes)
                && thumb.len() <= MAX_ICON_IMAGE_BYTES
                && (thumb.len() < bytes.len() || bytes.len() > MAX_ICON_IMAGE_BYTES)
            {
                write_cached_icon_bytes(&thumb_path, &thumb).ok();
                return Some((hash, "image/png", thumb, true));
            }
        } else if let Some(thumb) = downscale_to_png_64(&bytes)
            && thumb.len() <= MAX_ICON_IMAGE_BYTES
            && (thumb.len() < bytes.len() || bytes.len() > MAX_ICON_IMAGE_BYTES)
        {
            return Some((hash, "image/png", thumb, true));
        }
    }

    if bytes.len() <= MAX_ICON_IMAGE_BYTES {
        return Some((hash, mime, bytes, false));
    }
    None
}

fn downscale_to_png_64(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DECODE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DECODE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let resized = if img.width() > THUMBNAIL_MAX_PX || img.height() > THUMBNAIL_MAX_PX {
        img.resize(THUMBNAIL_MAX_PX, THUMBNAIL_MAX_PX, FilterType::Lanczos3)
    } else {
        img
    };
    let mut out = Cursor::new(Vec::new());
    resized.write_to(&mut out, image::ImageFormat::Png).ok()?;
    let png = out.into_inner();
    has_valid_icon_bytes(&png).then_some(png)
}

#[cfg(not(test))]
fn system_curl() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let path = PathBuf::from(root).join("System32").join("curl.exe");
        if path.is_file() {
            return path;
        }
        PathBuf::from("curl.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let path = PathBuf::from("/usr/bin/curl");
        if path.is_file() {
            return path;
        }
        PathBuf::from("curl")
    }
}

#[cfg(not(test))]
fn fetch_remote_icon_to_cache(
    url: &str,
    cache_dir: &Path,
    failed_urls: &Mutex<HashMap<String, Instant>>,
) -> Option<ResolvedImage> {
    let clean_url = url.trim();
    if !clean_url.starts_with("https://") {
        return None;
    }
    let target = cached_icon_path_for_url(cache_dir, clean_url);
    if let Some(img) = inspect_icon_file(&target) {
        return Some(img);
    }
    {
        let mut failed = crate::session::lock(failed_urls);
        let now = Instant::now();
        failed.retain(|_, retry_at| *retry_at > now);
        if failed.contains_key(clean_url) {
            return None;
        }
    }
    let mut cmd = krill_proto::os::background_command(system_curl());
    cmd.args([
        "-fsSL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-time",
        "5",
        "--max-filesize",
        "262144",
        clean_url,
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null());
    let retry_after = match cmd.output() {
        Ok(out) if out.status.success() => match write_cached_icon_bytes(&target, &out.stdout) {
            Ok(()) => return inspect_icon_file(&target),
            Err(_) => ICON_RETRY_AFTER_INVALID_IMAGE,
        },
        _ => ICON_RETRY_AFTER_NETWORK_ERROR,
    };
    crate::session::lock(failed_urls).insert(clean_url.to_owned(), Instant::now() + retry_after);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::control::RepositoryTrees;
    use krill_proto::os::ffi::OsStrExt as _;

    fn make_large_png(width: u32, height: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(width, height);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([
                u8::try_from((x * 7 + y * 3) % 256).unwrap(),
                u8::try_from((x * 11 + y * 5) % 256).unwrap(),
                u8::try_from((x * 13 + y * 17) % 256).unwrap(),
                255,
            ]);
        }
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn swr_resolves_local_and_cached_icons_off_thread_and_batches_resized_payloads() {
        let base = std::env::temp_dir().join(format!("krill-mobile-icons-{}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        let state_dir = base.join("state");
        let repo_svg = base.join("repo-svg");
        let repo_big = base.join("repo-big");
        let repo_gh = base.join("repo-gh");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(repo_svg.join("public")).unwrap();
        fs::create_dir_all(repo_big.join("public")).unwrap();
        fs::create_dir_all(repo_gh.join(".git")).unwrap();

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6"/></svg>"#;
        fs::write(repo_svg.join("public/favicon.svg"), svg).unwrap();

        let big_png = make_large_png(256, 256);
        assert!(
            big_png.len() > THUMBNAIL_THRESHOLD_BYTES,
            "fixture PNG must exceed threshold ({} bytes)",
            big_png.len()
        );
        fs::write(repo_big.join("public/logo.png"), &big_png).unwrap();

        fs::write(
            repo_gh.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/craftowen/demo.git\n",
        )
        .unwrap();
        let gh_url = "https://github.com/craftowen.png?size=64";
        let jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xDB, 0, 0x43, 0, 8, 6, 6, 7, 6];
        write_cached_icon_bytes(
            &cached_icon_path_for_url(&state_dir.join(REPO_ICON_DIR), gh_url),
            jpeg,
        )
        .unwrap();

        let repos = vec![
            RepositoryTrees {
                repo: repo_svg.as_os_str().as_bytes().to_vec(),
                worktrees: vec![repo_svg.as_os_str().as_bytes().to_vec()],
            },
            RepositoryTrees {
                repo: repo_big.as_os_str().as_bytes().to_vec(),
                worktrees: vec![repo_big.as_os_str().as_bytes().to_vec()],
            },
            RepositoryTrees {
                repo: repo_gh.as_os_str().as_bytes().to_vec(),
                worktrees: vec![repo_gh.as_os_str().as_bytes().to_vec()],
            },
        ];
        let catalog = Catalog::load(Some(&state_dir), &repos);
        let cache = RepoIconCache::default();
        let targets = catalog.icon_targets();

        // Cold call returns immediately without blocking on filesystem probes.
        let initial = cache.resolve_swr(&targets, Some(&state_dir));
        assert!(initial.is_empty());

        let deadline = Instant::now() + Duration::from_secs(5);
        while (!cache.is_idle() || cache.revision() == 0) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(cache.is_idle());
        assert!(cache.revision() >= 1);

        let warm = cache.resolve_swr(&targets, Some(&state_dir));
        assert_eq!(warm.len(), 3);
        let repos_json = catalog.repositories_json_with_icons(&warm);
        let key_of = |name: &str| -> String {
            repos_json.iter().find(|r| r["label"] == name).unwrap()["key"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let key_svg = key_of("repo-svg");
        let key_big = key_of("repo-big");
        let key_gh = key_of("repo-gh");
        for row in &repos_json {
            assert_eq!(row["icon"]["kind"], "image");
            let h = row["icon"]["hash"].as_str().unwrap();
            assert_eq!(h.len(), 16);
            assert!(h.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(row["icon"].get("value").is_none());
        }

        // Batch read serves SVG, downscaled 64x64 PNG thumbnail, and JPEG-in-.png with accurate MIME.
        let req_keys = vec![
            key_svg.clone(),
            key_big.clone(),
            key_gh.clone(),
            "deadbeef".into(), // unregistered key must be ignored safely
        ];
        let batch = cache
            .read_batch(&catalog, &req_keys, Some(&state_dir))
            .unwrap();
        assert_eq!(batch["truncated"], false);
        let map = batch["icons"].as_object().unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map[&key_svg]["mime"], "image/svg+xml");
        assert_eq!(map[&key_big]["mime"], "image/png");
        assert_eq!(map[&key_gh]["mime"], "image/jpeg");

        // Oversized PNG must have been shrunk and cached as m64-<hash>.png.
        let big_b64 = map[&key_big]["data_base64"].as_str().unwrap();
        assert!(
            big_b64.len() < big_png.len(),
            "downscaled base64 ({}) should be much smaller than original PNG ({})",
            big_b64.len(),
            big_png.len()
        );
        let big_hash = map[&key_big]["hash"].as_str().unwrap();
        assert!(
            state_dir
                .join(REPO_ICON_DIR)
                .join(format!("m64-{big_hash}.png"))
                .is_file()
        );

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn explicit_emoji_and_lucide_resolve_synchronously_without_background_probe() {
        let cache = RepoIconCache::default();
        let targets = vec![
            RepoIconTarget {
                repo: PathBuf::from("/nonexistent/emoji"),
                is_git: true,
                explicit: Some(("emoji".into(), "🚀".into())),
            },
            RepoIconTarget {
                repo: PathBuf::from("/nonexistent/lucide"),
                is_git: true,
                explicit: Some(("lucide".into(), "Rocket".into())),
            },
        ];
        let resolved = cache.resolve_swr(&targets, None);
        assert_eq!(
            resolved.get(Path::new("/nonexistent/emoji")),
            Some(&MobileIconMeta::Emoji("🚀".into()))
        );
        assert_eq!(
            resolved.get(Path::new("/nonexistent/lucide")),
            Some(&MobileIconMeta::Lucide("Rocket".into()))
        );
        assert!(cache.is_idle());
        assert_eq!(cache.revision(), 0);
    }

    #[test]
    fn read_batch_rejects_excessive_keys() {
        let catalog = Catalog::default();
        let cache = RepoIconCache::default();
        let too_many: Vec<String> = (0..=MAX_BATCH_KEYS).map(|i| format!("{i:04x}")).collect();
        assert!(cache.read_batch(&catalog, &too_many, None).is_err());
    }

    #[test]
    fn rollback_inflight_preserves_other_active_threads_paths() {
        let cache = RepoIconCache::default();
        let repo1 = PathBuf::from("/tmp/repo-already-inflight");
        let repo2 = PathBuf::from("/tmp/repo-newly-scheduled");
        {
            let mut inflight = crate::session::lock(&cache.inflight);
            inflight.insert(repo1.clone());
            inflight.insert(repo2.clone());
        }
        cache.rollback_inflight(std::slice::from_ref(&repo2));
        let inflight = crate::session::lock(&cache.inflight);
        assert!(
            inflight.contains(&repo1),
            "pre-existing inflight path must not be removed on sibling spawn failure"
        );
        assert!(
            !inflight.contains(&repo2),
            "newly scheduled path must be removed on spawn failure"
        );
    }
}
