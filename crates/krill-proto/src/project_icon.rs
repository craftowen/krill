//! 프로젝트(저장소·폴더) 아이콘 탐지·검증·디스크 캐시 공용 모듈.
//! 데스크톱(`krill-app`)과 모바일 데몬(`krilld`)이 같은 우선순위와 캐시 키를 공유한다.
//! UI/design adapted from Orca (`src/main/repo-icon-file-detection.ts`).
//! Copyright (c) 2026 Lovecast Inc., MIT License; implemented here in Rust.

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use ring::digest;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// `<state_dir>` 아래 프로젝트 아이콘 캐시 디렉터리 이름.
pub const REPO_ICON_DIR: &str = "repo-icons";
/// HTML/package.json/Cargo.toml/.git/config 파싱 상한.
pub const MAX_ICON_SOURCE_BYTES: u64 = 64 * 1024;
/// 원격·캐시 아이콘 바이트 상한(256 KiB).
pub const MAX_ICON_IMAGE_BYTES: usize = 256 * 1024;
/// 로컬 저장소 안 이미지 파일 허용 상한(2 MiB, 데스크톱 `MAX_ICON_IMAGE_BYTES * 8`).
pub const MAX_ICON_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Orca `REPO_ICON_FILE_STEMS` 및 모노레포(`apps/web`, `packages/web`)·표준 파비콘 하위 폴더 우선순위.
/// 앞 16개 스템(× 4 확장자 = 64개)은 원격 `ProbePaths` 첫 배치(`MAX_PROBE_PATHS`)에서 한 번에 조회된다.
pub const REPO_ICON_FILE_STEMS: &[&str] = &[
    "favicon",
    "public/favicon",
    "public/apple-touch-icon",
    "public/favicon/favicon",
    "public/favicon/apple-touch-icon",
    "public/icons/icon-192x192",
    "app/favicon",
    "app/icon",
    "src/favicon",
    "src/app/icon",
    "logo",
    "public/logo",
    "public/icon",
    "src-tauri/icons/icon",
    "apps/web/public/favicon",
    "apps/web/public/icon-192",
    "apps/web/public/apple-touch-icon",
    "apps/web/public/favicon-192",
    "apps/web/public/icon",
    "apps/web/public/logo",
    "apps/web/app/favicon",
    "apps/web/app/icon",
    "apps/web/src/app/favicon",
    "apps/web/src/app/icon",
    "packages/web/public/favicon",
    "packages/web/public/icon",
    "packages/web/public/logo",
    "public/icons/apple-touch-icon",
    "app/apple-icon",
    "src/app/favicon",
    "src/app/apple-icon",
    "assets/favicon",
    "assets/icon",
    "assets/logo",
    "static/favicon",
    "static/logo",
    "public/icons",
    "app-icon",
    "icon",
    "packaging/macos/favicon",
    "packaging/windows/favicon",
    "packaging/favicon",
];

/// 저장소에서 자동 탐지하는 아이콘 파일 확장자.
pub const REPO_ICON_RASTER_EXTS: &[&str] = &["png", "webp", "ico", "svg"];

/// `<link rel="icon">` 탐색 대상 소스 파일 후보.
pub const REPO_ICON_SOURCE_FILES: &[&str] = &[
    "index.html",
    "public/index.html",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
    "app/root.tsx",
    "src/root.tsx",
    "src/index.html",
    "apps/web/index.html",
    "apps/web/public/index.html",
    "apps/web/app/root.tsx",
    "apps/web/src/index.html",
];

const RESERVED_GITHUB_PATHS: &[&str] = &[
    "orgs",
    "users",
    "settings",
    "marketplace",
    "explore",
    "topics",
    "collections",
    "events",
    "sponsors",
    "notifications",
    "login",
    "join",
    "about",
    "pricing",
    "security",
    "features",
];

/// 로컬 저장소 자동 탐지 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbedIconSource {
    LocalFile(PathBuf),
    RemoteUrl(String),
    None,
}

/// 명시적 `projects.toml` 지정 + 자동 탐지를 합친 최종 아이콘 원천(네트워크 요청 전).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedIconSource {
    Emoji(String),
    Lucide(String),
    LocalFile(PathBuf),
    RemoteUrl(String),
}

type IconProbeKey = (PathBuf, bool);
type IconProbeEntry = (Option<SystemTime>, ProbedIconSource);

static DETECTED_ICON_CACHE: LazyLock<Mutex<BTreeMap<IconProbeKey, IconProbeEntry>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// SVG XML 문서 시작부가 유효한 `<svg` 루트인지 확인한다(HTML·바이너리 거부).
#[must_use]
pub fn is_valid_svg_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 6 || bytes.contains(&0) {
        return false;
    }
    let Some(prefix) = bytes.get(..bytes.len().min(4096)) else {
        return false;
    };
    let s = match std::str::from_utf8(prefix) {
        Ok(s) => s,
        Err(e) if e.valid_up_to() > 0 => {
            let Some(valid) = prefix.get(..e.valid_up_to()) else {
                return false;
            };
            let Ok(s) = std::str::from_utf8(valid) else {
                return false;
            };
            s
        }
        Err(_) => return false,
    };
    let mut rest = s.trim_start_matches('\u{feff}').trim_start();
    loop {
        if let Some(after_xml) = rest.strip_prefix("<?") {
            let Some(end) = after_xml.find("?>") else {
                return false;
            };
            let Some(next) = end.checked_add(2).and_then(|i| after_xml.get(i..)) else {
                return false;
            };
            rest = next.trim_start();
        } else if let Some(after_comment) = rest.strip_prefix("<!--") {
            let Some(end) = after_comment.find("-->") else {
                return false;
            };
            let Some(next) = end.checked_add(3).and_then(|i| after_comment.get(i..)) else {
                return false;
            };
            rest = next.trim_start();
        } else if rest
            .get(..9)
            .is_some_and(|head| head.eq_ignore_ascii_case("<!doctype"))
        {
            let Some(after_dt_raw) = rest.get(9..) else {
                return false;
            };
            let after_dt = after_dt_raw.trim_start();
            if !after_dt
                .get(..3)
                .is_some_and(|dt| dt.eq_ignore_ascii_case("svg"))
            {
                return false;
            }
            let Some(end) = after_dt.find('>') else {
                return false;
            };
            let Some(next) = end.checked_add(1).and_then(|i| after_dt.get(i..)) else {
                return false;
            };
            rest = next.trim_start();
        } else {
            break;
        }
    }
    rest.get(..4)
        .is_some_and(|tag| tag.eq_ignore_ascii_case("<svg"))
        && rest
            .as_bytes()
            .get(4)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
}

/// PNG·JPEG·WebP·ICO·SVG 매직 바이트에서 실제 MIME 타입을 판별한다.
/// 확장자와 무관하게 실제 바이트로 판별하므로 GitHub 아바타(`.png` URL이 JPEG 반환)도 정확히 분류한다.
#[must_use]
pub fn detect_icon_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12
        && bytes.starts_with(b"RIFF")
        && bytes.get(8..12) == Some(b"WEBP".as_slice())
    {
        return Some("image/webp");
    }
    if bytes.len() >= 6
        && bytes.starts_with(&[0, 0, 1, 0])
        && bytes
            .get(4)
            .zip(bytes.get(5))
            .is_some_and(|(&lo, &hi)| u16::from_le_bytes([lo, hi]) > 0)
    {
        return Some("image/x-icon");
    }
    if is_valid_svg_bytes(bytes) {
        return Some("image/svg+xml");
    }
    None
}

/// PNG·JPEG·WebP·ICO·SVG 헤더인지 확인한다.
#[must_use]
pub fn has_valid_icon_bytes(bytes: &[u8]) -> bool {
    detect_icon_mime(bytes).is_some()
}

/// 파일 크기와 앞 4KiB 매직 바이트가 유효한 아이콘인지 확인한다.
#[must_use]
pub fn has_valid_icon_magic(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() < 6 || meta.len() > MAX_ICON_FILE_BYTES {
        return false;
    }
    let mut header = [0u8; 4096];
    let Ok(n) = file.read(&mut header) else {
        return false;
    };
    header.get(..n).is_some_and(has_valid_icon_bytes)
}

/// 임의 바이트의 SHA-256 앞 8바이트를 소문자 16자리 hex로 돌려준다.
#[must_use]
pub fn short_sha256_hex(input: &[u8]) -> String {
    let hash = digest::digest(&digest::SHA256, input);
    let mut out = String::with_capacity(16);
    for &b in hash.as_ref().iter().take(8) {
        write!(&mut out, "{b:02x}").ok();
    }
    out
}

/// 원격 아이콘 URL이 로컬 디스크에 캐시될 경로(`<cache_dir>/url-<sha256_prefix>.png`).
#[must_use]
pub fn cached_icon_path_for_url(cache_dir: &Path, url: &str) -> PathBuf {
    cache_dir.join(format!(
        "url-{}.png",
        short_sha256_hex(url.trim().as_bytes())
    ))
}

/// 아이콘 바이트를 검증하고 원자적으로 디스크에 기록한다(`0700` 디렉터리, `0600` 파일).
pub fn write_cached_icon_bytes(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if !has_valid_icon_bytes(bytes) || bytes.len() > MAX_ICON_IMAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid icon image bytes",
        ));
    }
    let Some(parent) = target.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "icon target has no parent",
        ));
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    builder.create(parent)?;
    let tmp = target.with_extension(format!("tmp.{}", std::process::id()));
    let res = (|| -> io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, target)?;
        Ok(())
    })();
    if res.is_err() {
        fs::remove_file(&tmp).ok();
    }
    res
}

fn decode_base64_bounded(input: &str) -> Option<Vec<u8>> {
    let cap = input.len().checked_mul(3).map_or(0, |n| n / 4);
    let mut out = Vec::with_capacity(cap.min(MAX_ICON_IMAGE_BYTES));
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b.checked_sub(b'A')?,
            b'a'..=b'z' => b.checked_sub(b'a')?.checked_add(26)?,
            b'0'..=b'9' => b.checked_sub(b'0')?.checked_add(52)?,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits = bits.checked_add(6)?;
        if bits >= 8 {
            bits = bits.checked_sub(8)?;
            let byte = u8::try_from((buf >> bits) & 0xFF).ok()?;
            out.push(byte);
            if out.len() > MAX_ICON_IMAGE_BYTES {
                return None;
            }
        }
    }
    Some(out)
}

/// `data:image/(png|webp|svg+xml);base64,...` 데이터 URL을 `<cache_dir>`에 디코드·저장하고 경로를 돌려준다.
#[must_use]
pub fn cache_data_url_icon(data_url: &str, cache_dir: &Path) -> Option<PathBuf> {
    let trimmed = data_url.trim();
    let (ext, b64) = if let Some(rest) = trimmed.strip_prefix("data:image/png;base64,") {
        ("png", rest)
    } else if let Some(rest) = trimmed.strip_prefix("data:image/svg+xml;base64,") {
        ("svg", rest)
    } else {
        let rest = trimmed.strip_prefix("data:image/webp;base64,")?;
        ("webp", rest)
    };
    let target = cache_dir.join(format!(
        "data-{}.{ext}",
        short_sha256_hex(trimmed.as_bytes())
    ));
    if has_valid_icon_magic(&target) {
        return Some(target);
    }
    let bytes = decode_base64_bounded(b64)?;
    write_cached_icon_bytes(&target, &bytes).ok()?;
    Some(target)
}

fn extract_attr_value(tag: &str, attr_name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(pos) = lower.get(search_from..)?.find(attr_name) {
        let idx = search_from.checked_add(pos)?;
        let before_ok = idx == 0
            || idx
                .checked_sub(1)
                .and_then(|i| lower.as_bytes().get(i))
                .is_some_and(|b| {
                    b.is_ascii_whitespace() || *b == b'<' || *b == b'"' || *b == b'\''
                });
        let after_idx = idx.checked_add(attr_name.len())?;
        let rest = tag.get(after_idx..)?.trim_start();
        if before_ok && let Some(after_eq) = rest.strip_prefix('=') {
            let after_eq = after_eq.trim_start();
            if let Some(quote) = after_eq.chars().next().filter(|&c| c == '"' || c == '\'') {
                let inner = after_eq.get(quote.len_utf8()..)?;
                if let Some(end) = inner.find(quote) {
                    return inner.get(..end).map(|s| s.trim().to_owned());
                }
            }
        }
        search_from = after_idx;
    }
    None
}

/// HTML/TSX 문서에서 `<link rel="...icon..." href="...">`의 `href` 목록을 추출한다.
#[must_use]
pub fn extract_icon_link_hrefs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = source.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(pos) = lower.get(from..).and_then(|s| s.find("<link")) {
        let Some(start) = from.checked_add(pos) else {
            break;
        };
        let Some(end_rel) = lower.get(start..).and_then(|s| s.find('>')) else {
            break;
        };
        let Some(end) = start.checked_add(end_rel).and_then(|n| n.checked_add(1)) else {
            break;
        };
        if let Some(tag) = source.get(start..end)
            && let Some(rel) = extract_attr_value(tag, "rel")
            && rel
                .split_ascii_whitespace()
                .any(|tok| tok.eq_ignore_ascii_case("icon") || tok.eq_ignore_ascii_case("shortcut"))
            && let Some(href) = extract_attr_value(tag, "href")
            && !href.is_empty()
        {
            out.push(href);
        }
        from = end;
    }
    out
}

fn resolve_local_icon_href(
    repo_path: &Path,
    source_file: &Path,
    raw_href: &str,
) -> Option<PathBuf> {
    let href = raw_href.trim().split('#').next()?.split('?').next()?.trim();
    if href.is_empty()
        || href.starts_with("http://")
        || href.starts_with("https://")
        || href.starts_with("data:")
        || href.starts_with("//")
    {
        return None;
    }
    let canon_repo = fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    let mut candidates = Vec::new();
    if let Some(stripped) = href.strip_prefix('/') {
        if let Some(parent) = source_file.parent() {
            candidates.push(parent.join("public").join(stripped));
            candidates.push(parent.join(stripped));
        }
        candidates.push(repo_path.join("public").join(stripped));
        candidates.push(repo_path.join(stripped));
    } else {
        if let Some(parent) = source_file.parent() {
            candidates.push(parent.join(href));
            candidates.push(parent.join("public").join(href));
        }
        candidates.push(repo_path.join("public").join(href));
        candidates.push(repo_path.join(href));
    }
    for cand in candidates {
        let ext_ok = cand.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            REPO_ICON_RASTER_EXTS
                .iter()
                .any(|ext| e.eq_ignore_ascii_case(ext))
        });
        if !ext_ok {
            continue;
        }
        let Ok(canon_cand) = fs::canonicalize(&cand) else {
            continue;
        };
        if canon_cand.starts_with(&canon_repo) && has_valid_icon_magic(&canon_cand) {
            return Some(canon_cand);
        }
    }
    None
}

fn read_bounded_bytes(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    u64::try_from(bytes.len())
        .ok()
        .filter(|&n| n <= limit)
        .map(|_| bytes)
}

/// 프로젝트 폴더에서 로컬 파비콘·로고 이미지 파일(PNG·WebP·ICO·SVG)을 찾는다.
#[must_use]
pub fn detect_local_file_icon(repo_path: &Path) -> Option<PathBuf> {
    for stem in REPO_ICON_FILE_STEMS {
        for ext in REPO_ICON_RASTER_EXTS {
            let candidate = repo_path.join(format!("{stem}.{ext}"));
            if has_valid_icon_magic(&candidate) {
                return Some(candidate);
            }
        }
    }
    for rel in REPO_ICON_SOURCE_FILES {
        let source_path = repo_path.join(rel);
        let Some(bytes) = read_bounded_bytes(&source_path, MAX_ICON_SOURCE_BYTES) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for href in extract_icon_link_hrefs(text) {
            if let Some(found) = resolve_local_icon_href(repo_path, &source_path, &href) {
                return Some(found);
            }
        }
    }
    None
}

fn is_valid_dns_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || !host.contains('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// 웹사이트 주소에서 Google S2 파비콘 URL을 만든다(Orca `faviconUrlFromWebsite` 패리티).
#[must_use]
pub fn favicon_url_from_website(raw: &str) -> Option<String> {
    if raw.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let authority = without_scheme.split(['/', '?', '#']).next()?.trim();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host_port.split(':').next()?.trim().to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if !is_valid_dns_hostname(host)
        || matches!(
            host,
            "localhost"
                | "127.0.0.1"
                | "0.0.0.0"
                | "::1"
                | "github.com"
                | "gitlab.com"
                | "bitbucket.org"
        )
    {
        return None;
    }
    Some(format!(
        "https://www.google.com/s2/favicons?domain={host}&sz=64"
    ))
}

/// `<repo_path>/package.json`의 `homepage` 필드에서 파비콘 URL을 추출한다.
#[must_use]
pub fn detect_package_homepage_favicon_url(repo_path: &Path) -> Option<String> {
    let bytes = read_bounded_bytes(&repo_path.join("package.json"), MAX_ICON_SOURCE_BYTES)?;
    let val: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let homepage = val.get("homepage")?.as_str()?;
    favicon_url_from_website(homepage)
}

fn read_icon_cargo_manifest(path: &Path) -> Option<toml::Value> {
    let bytes = read_bounded_bytes(path, MAX_ICON_SOURCE_BYTES)?;
    let source = std::str::from_utf8(&bytes).ok()?;
    toml::from_str(source).ok()
}

/// Rust 패키지·워크스페이스의 `homepage`에서 파비콘 URL을 찾는다.
#[must_use]
pub fn detect_cargo_homepage_favicon_url(repo_path: &Path) -> Option<String> {
    let manifest = read_icon_cargo_manifest(&repo_path.join("Cargo.toml"))?;
    let package = manifest.get("package");
    let homepage = package.and_then(|p| p.get("homepage"));
    if let Some(url) = homepage.and_then(toml::Value::as_str) {
        return favicon_url_from_website(url);
    }
    let workspace_homepage = |manifest: &toml::Value| {
        favicon_url_from_website(
            manifest
                .get("workspace")?
                .get("package")?
                .get("homepage")?
                .as_str()?,
        )
    };
    if package.is_none()
        || homepage
            .and_then(|h| h.get("workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true)
    {
        if manifest.get("workspace").is_some() {
            return workspace_homepage(&manifest);
        }
        if let Some(workspace) = package
            .and_then(|p| p.get("workspace"))
            .and_then(toml::Value::as_str)
        {
            return workspace_homepage(&read_icon_cargo_manifest(
                &repo_path.join(workspace).join("Cargo.toml"),
            )?);
        }
        for parent in repo_path.ancestors().skip(1) {
            if let Some(manifest) = read_icon_cargo_manifest(&parent.join("Cargo.toml"))
                && manifest.get("workspace").is_some()
            {
                return workspace_homepage(&manifest);
            }
        }
    }
    None
}

fn is_valid_github_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.is_empty() || bytes.len() > 39 {
        return false;
    }
    let first_last_ok = bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric);
    first_last_ok
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && !RESERVED_GITHUB_PATHS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(slug))
}

/// GitHub 원격 URL이나 소유자 문자열에서 계정/조직 슬러그를 추출한다(Orca `githubAvatarSlug` 패리티).
#[must_use]
pub fn github_avatar_slug(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('@').trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((host_part, path_part)) = trimmed.split_once(':')
        && !path_part.starts_with("//")
        && host_part
            .rsplit_once('@')
            .map_or(host_part, |(_, h)| h)
            .eq_ignore_ascii_case("github.com")
    {
        let owner = path_part
            .trim_start_matches('/')
            .split('/')
            .next()?
            .trim_end_matches(".git");
        return is_valid_github_slug(owner).then(|| owner.to_ascii_lowercase());
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .or_else(|| trimmed.strip_prefix("git://"))
        .unwrap_or(trimmed);
    if let Some((authority, rest)) = without_scheme.split_once('/') {
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, h)| h)
            .split(':')
            .next()
            .unwrap_or("")
            .trim();
        let host = host.strip_prefix("www.").unwrap_or(host);
        if host.eq_ignore_ascii_case("github.com") {
            let owner = rest
                .trim_start_matches('/')
                .split(['/', '?', '#'])
                .next()?
                .trim_end_matches(".git");
            return is_valid_github_slug(owner).then(|| owner.to_ascii_lowercase());
        }
    }
    None
}

/// `.git/config`에서 `origin`과 `upstream` 원격 URL을 추출한다.
#[must_use]
pub fn parse_git_config_remotes(config_text: &str) -> (Option<String>, Option<String>) {
    let mut current_remote: Option<&str> = None;
    let mut origin: Option<String> = None;
    let mut upstream: Option<String> = None;
    for raw_line in config_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let section = section.trim();
            current_remote = section
                .strip_prefix("remote")
                .map(str::trim)
                .and_then(|rest| rest.strip_prefix('"'))
                .and_then(|rest| rest.strip_suffix('"'));
            continue;
        }
        if let Some(remote_name) = current_remote
            && let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case("url")
        {
            let url = v.trim().trim_matches('"').trim();
            if !url.is_empty() {
                if remote_name == "origin" && origin.is_none() {
                    origin = Some(url.to_owned());
                } else if remote_name == "upstream" && upstream.is_none() {
                    upstream = Some(url.to_owned());
                }
            }
        }
    }
    (origin, upstream)
}

fn main_worktree_for_icon(tree: &Path) -> Option<PathBuf> {
    let dot = tree.join(".git");
    let git_dir = read_bounded_bytes(&dot, 4096)
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.strip_prefix("gitdir:").map(|p| tree.join(p.trim())))
        .unwrap_or(dot);
    let common_bytes = read_bounded_bytes(&git_dir.join("commondir"), 4096)?;
    let common_str = String::from_utf8(common_bytes).ok()?;
    let common = fs::canonicalize(git_dir.join(common_str.trim())).ok()?;
    (common.file_name()? == ".git")
        .then(|| common.parent().map(Path::to_path_buf))
        .flatten()
}

/// Git 저장소의 `.git/config`(`upstream` → `origin` 순)에서 GitHub 소유자 아바타 URL을 추출한다.
#[must_use]
pub fn detect_github_avatar_url(repo_path: &Path) -> Option<String> {
    let direct_cfg = repo_path.join(".git/config");
    let config_path = if direct_cfg.is_file() {
        direct_cfg
    } else {
        let main_repo = main_worktree_for_icon(repo_path)?;
        let main_cfg = main_repo.join(".git/config");
        if main_cfg.is_file() {
            main_cfg
        } else {
            return None;
        }
    };
    let bytes = read_bounded_bytes(&config_path, MAX_ICON_SOURCE_BYTES)?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let (origin, upstream) = parse_git_config_remotes(text);
    for remote_url in [upstream.as_deref(), origin.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(slug) = github_avatar_slug(remote_url) {
            return Some(format!("https://github.com/{slug}.png?size=64"));
        }
    }
    None
}

/// 로컬 프로젝트의 자동 아이콘 원천(디렉터리 mtime 단위 캐시)을 탐지한다.
#[must_use]
pub fn probe_project_icon_source(repo_path: &Path, is_git: bool) -> ProbedIconSource {
    let mtime = fs::metadata(repo_path).ok().and_then(|m| m.modified().ok());
    let key = (repo_path.to_path_buf(), is_git);
    if let Ok(cache) = DETECTED_ICON_CACHE.lock()
        && let Some((cached_mtime, outcome)) = cache.get(&key)
        && *cached_mtime == mtime
    {
        return outcome.clone();
    }
    let outcome = if let Some(local) = detect_local_file_icon(repo_path) {
        ProbedIconSource::LocalFile(local)
    } else if let Some(favicon_url) = detect_package_homepage_favicon_url(repo_path) {
        ProbedIconSource::RemoteUrl(favicon_url)
    } else if let Some(favicon_url) = detect_cargo_homepage_favicon_url(repo_path) {
        ProbedIconSource::RemoteUrl(favicon_url)
    } else if is_git && let Some(avatar_url) = detect_github_avatar_url(repo_path) {
        ProbedIconSource::RemoteUrl(avatar_url)
    } else {
        ProbedIconSource::None
    };
    if let Ok(mut cache) = DETECTED_ICON_CACHE.lock() {
        cache.insert(key, (mtime, outcome.clone()));
    }
    outcome
}

/// 이전 Orca 이관이 남긴 자동 GitHub 아바타 URL인지 판별한다(서비스 파비콘이 없을 때만 폴백).
#[must_use]
pub fn is_imported_github_avatar(kind: &str, value: &str) -> bool {
    kind.trim() == "image"
        && value
            .trim()
            .strip_prefix("https://github.com/")
            .and_then(|path| path.split('?').next())
            .and_then(|path| path.strip_suffix(".png"))
            .is_some_and(is_valid_github_slug)
}

/// `projects.toml` 명시 아이콘을 네트워크 호출 없이 로컬 파일·원격 URL·이모지·Lucide로 해석한다.
#[must_use]
pub fn resolve_explicit_icon_source(
    kind: &str,
    value: &str,
    repo_path: &Path,
    cache_dir: Option<&Path>,
) -> Option<ResolvedIconSource> {
    let val = value.trim();
    if val.is_empty() {
        return None;
    }
    match kind.trim() {
        "lucide" => Some(ResolvedIconSource::Lucide(val.to_owned())),
        "emoji" => Some(ResolvedIconSource::Emoji(val.to_owned())),
        "image" => {
            if val.starts_with("data:image/") {
                cache_data_url_icon(val, cache_dir?).map(ResolvedIconSource::LocalFile)
            } else if val.starts_with("https://") || val.starts_with("http://") {
                Some(ResolvedIconSource::RemoteUrl(val.to_owned()))
            } else {
                let raw_path = val.strip_prefix("file://").unwrap_or(val);
                let p = Path::new(raw_path);
                let candidate = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    repo_path.join(p)
                };
                if has_valid_icon_magic(&candidate) {
                    return Some(ResolvedIconSource::LocalFile(candidate));
                }
                if let Some(dir) = cache_dir
                    && let Some(name) = candidate.file_name()
                {
                    let cached_candidate = dir.join(name);
                    if has_valid_icon_magic(&cached_candidate) {
                        return Some(ResolvedIconSource::LocalFile(cached_candidate));
                    }
                }
                None
            }
        }
        _ => None,
    }
}

/// 데스크톱과 동일한 우선순위로 프로젝트 아이콘 원천을 해석한다(네트워크 I/O 없음).
#[must_use]
pub fn resolve_project_icon_source(
    repo_path: &Path,
    is_git: bool,
    cache_dir: Option<&Path>,
    explicit: Option<(&str, &str)>,
) -> Option<ResolvedIconSource> {
    if let Some((kind, value)) = explicit
        && !is_imported_github_avatar(kind, value)
        && let Some(resolved) = resolve_explicit_icon_source(kind, value, repo_path, cache_dir)
    {
        return Some(resolved);
    }
    match probe_project_icon_source(repo_path, is_git) {
        ProbedIconSource::LocalFile(path) => Some(ResolvedIconSource::LocalFile(path)),
        ProbedIconSource::RemoteUrl(url) => Some(ResolvedIconSource::RemoteUrl(url)),
        ProbedIconSource::None => explicit.and_then(|(kind, value)| {
            resolve_explicit_icon_source(kind, value, repo_path, cache_dir)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_icon_mime_identifies_all_supported_formats_and_rejects_junk() {
        assert_eq!(
            detect_icon_mime(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0d"),
            Some("image/png")
        );
        assert_eq!(
            detect_icon_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16]),
            Some("image/jpeg")
        );
        assert_eq!(
            detect_icon_mime(b"RIFF\x10\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(
            detect_icon_mime(&[0, 0, 1, 0, 1, 0, 16, 16]),
            Some("image/x-icon")
        );
        assert_eq!(
            detect_icon_mime(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
            Some("image/svg+xml")
        );
        assert_eq!(detect_icon_mime(b"<html><body><svg/></body></html>"), None);
        assert_eq!(detect_icon_mime(&[0, 0, 1, 0, 0, 0]), None);
        assert_eq!(detect_icon_mime(b"plain text"), None);
    }

    #[test]
    fn short_sha256_hex_matches_standard_sha256_prefix() {
        // SHA-256("https://github.com/craftowen.png?size=64") starts with 6de14c6af705a140
        // (matching existing desktop repo-icons disk cache filenames).
        assert_eq!(short_sha256_hex(b"hello"), "2cf24dba5fb0a30e");
        let dir = Path::new("/tmp/cache");
        let url = "https://github.com/owner.png?size=64";
        let path = cached_icon_path_for_url(dir, url);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(name.starts_with("url-"));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(name.len(), 4 + 16 + 4);
    }

    #[test]
    fn resolve_project_icon_source_matches_desktop_priority() {
        let root = std::env::temp_dir().join(format!("krill-proto-icon-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(root.join("public")).unwrap();
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\"/>";
        let fav = root.join("public/favicon.svg");
        fs::write(&fav, svg).unwrap();

        // Imported GitHub avatar yields to local repo favicon.
        let avatar = ("image", "https://github.com/craftowen.png?size=64");
        assert_eq!(
            resolve_project_icon_source(&root, true, None, Some(avatar)),
            Some(ResolvedIconSource::LocalFile(fav.clone()))
        );
        // Explicit emoji or lucide beats local repo favicon.
        assert_eq!(
            resolve_project_icon_source(&root, true, None, Some(("emoji", "🦀"))),
            Some(ResolvedIconSource::Emoji("🦀".into()))
        );
        assert_eq!(
            resolve_project_icon_source(&root, true, None, Some(("lucide", "Rocket"))),
            Some(ResolvedIconSource::Lucide("Rocket".into()))
        );

        // Empty repo falls back to imported GitHub avatar.
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            resolve_project_icon_source(&empty, true, None, Some(avatar)),
            Some(ResolvedIconSource::RemoteUrl(avatar.1.into()))
        );
        fs::remove_dir_all(&root).ok();
    }
}
