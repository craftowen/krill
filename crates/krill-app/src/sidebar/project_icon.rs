//! 사이드바 프로젝트 헤더 아이콘·배지 색(Orca `RepoIcon` 패리티).
//!
//! 아이콘은 `projects.toml`의 명시 `repo_icon`(Orca 이관분) → 저장소 안 파비콘·로고 파일 →
//! `package.json` `homepage` 파비콘 → GitHub 소유자 아바타 순으로 고른다
//! (`.ref/orca/src/main/repo-icon-autodetect.ts`). 원격 이미지는 백그라운드에서 한 번 내려받아
//! `<state_dir>/repo-icons/`에 캐시하고, 받기 전까지는 폴더 아이콘으로 그린다.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use super::external::{ImportedRepoIcon, ImportedVisual};
use crate::ui::icons::ProjectIcon;

/// CLI `krill import orca`가 data URL 아이콘을 푸는 곳과 같은 디렉터리.
const REPO_ICON_DIR: &str = "repo-icons";
const MAX_SOURCE_BYTES: u64 = 64 * 1024;
/// Orca `MAX_REPO_ICON_UPLOAD_BYTES`와 같은 이미지 상한.
const MAX_IMAGE_BYTES: usize = 256 * 1024;

/// Orca `REPO_ICON_FILE_STEMS`(`.ref/orca/src/main/repo-icon-file-detection.ts`)와 같은 우선순위.
const FILE_STEMS: &[&str] = &[
    "favicon",
    "public/favicon",
    "app/favicon",
    "app/icon",
    "src/favicon",
    "src/app/icon",
    "assets/favicon",
    "assets/icon",
    "static/favicon",
    "logo",
    "public/logo",
    "public/icon",
    "src-tauri/icons/icon",
    "app-icon",
    "icon",
];

/// Orca `REPO_ICON_SOURCE_FILE_CANDIDATES`: `<link rel="icon">`를 찾을 소스 파일.
const SOURCE_FILES: &[&str] = &[
    "index.html",
    "public/index.html",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
    "app/root.tsx",
    "src/root.tsx",
    "src/index.html",
];

/// 사이드바 프로젝트 헤더에 그릴 배지 색·아이콘.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectVisual {
    pub badge_color: Option<String>,
    pub icon: Option<ProjectIcon>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Detected {
    Local(PathBuf),
    Remote(String),
    None,
}

/// 저장소 루트 mtime별 자동 감지 결과(폴링마다 수십 번의 파일 탐색을 반복하지 않는다).
/// ponytail: 루트 mtime만 보므로 하위 폴더에 아이콘을 새로 넣으면 앱 재시작 전까지 안 바뀐다.
type DetectCache = BTreeMap<(PathBuf, bool), (Option<SystemTime>, Detected)>;
static DETECT_CACHE: LazyLock<Mutex<DetectCache>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));
/// 이번 프로세스에서 이미 내려받기를 시도한 URL(실패해도 재시도하지 않는다).
static FETCH_TRIED: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));
/// 백그라운드 내려받기가 새 아이콘을 캐시에 썼다는 신호. 폴링이 [`take_fetched`]로 소비한다.
static FETCHED: AtomicBool = AtomicBool::new(false);

/// 새 원격 아이콘이 캐시에 들어왔으면 `true`를 한 번 돌려준다.
pub(super) fn take_fetched() -> bool {
    FETCHED.swap(false, Ordering::Relaxed)
}

/// PNG·WebP는 로컬 파일, 여기에 JPEG(GitHub 아바타)를 더한 것은 원격 이미지로 받는다.
fn image_magic_ok(bytes: &[u8], allow_jpeg: bool) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || (bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || (allow_jpeg && bytes.starts_with(&[0xFF, 0xD8, 0xFF]))
}

fn is_icon_file(path: &Path, allow_jpeg: bool) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if !file
        .metadata()
        .is_ok_and(|m| m.is_file() && m.len() <= MAX_IMAGE_BYTES as u64)
    {
        return false;
    }
    let mut header = [0u8; 12];
    file.read(&mut header)
        .is_ok_and(|n| image_magic_ok(&header[..n], allow_jpeg))
}

fn read_capped(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// 원격 URL이 캐시될 경로(`<cache_dir>/url-<sha256 앞 16자리>`). 형식은 내용으로 판별된다.
fn cached_url_path(cache_dir: &Path, url: &str) -> PathBuf {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(url.as_bytes());
    let mut name = String::from("url-");
    for b in digest.iter().take(8) {
        write!(name, "{b:02x}").ok();
    }
    cache_dir.join(name)
}

/// 캐시된 원격 아이콘이 있으면 그 경로, 없으면 백그라운드 내려받기를 한 번 걸고 `None`.
fn remote_icon(url: &str, cache_dir: &Path) -> Option<ProjectIcon> {
    if !url.starts_with("https://") {
        return None;
    }
    let target = cached_url_path(cache_dir, url);
    if is_icon_file(&target, true) {
        return Some(ProjectIcon::Image(target));
    }
    let first = FETCH_TRIED
        .lock()
        .is_ok_and(|mut tried| tried.insert(url.to_owned()));
    #[cfg(not(test))]
    if first {
        let url = url.to_owned();
        std::thread::spawn(move || {
            let Ok(bytes) = crate::updater::fetch_bytes(&url, MAX_IMAGE_BYTES) else {
                return;
            };
            if !image_magic_ok(&bytes, true) {
                return;
            }
            let tmp = target.with_extension(format!("tmp{}", std::process::id()));
            let written = target
                .parent()
                .is_some_and(|d| fs::create_dir_all(d).is_ok())
                && fs::write(&tmp, &bytes).is_ok()
                && fs::rename(&tmp, &target).is_ok();
            if written {
                FETCHED.store(true, Ordering::Relaxed);
            } else {
                fs::remove_file(&tmp).ok();
            }
        });
    }
    #[cfg(test)]
    let _ = first;
    None
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower.get(from..)?.find(name) {
        let idx = from + pos;
        from = idx + name.len();
        let boundary = idx == 0
            || lower
                .as_bytes()
                .get(idx - 1)
                .is_some_and(u8::is_ascii_whitespace);
        let Some(rest) = tag.get(from..).map(str::trim_start) else {
            continue;
        };
        if let (true, Some(rest)) = (boundary, rest.strip_prefix('=')) {
            let rest = rest.trim_start();
            let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
            let inner = rest.get(1..)?;
            return inner
                .find(quote)
                .and_then(|end| inner.get(..end))
                .map(|v| v.trim().to_owned());
        }
    }
    None
}

/// `<link rel="icon" href="...">`(`shortcut icon` 포함)의 `href`들.
fn icon_link_hrefs(source: &str) -> Vec<String> {
    let lower = source.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(start) = lower
        .get(from..)
        .and_then(|s| s.find("<link"))
        .map(|p| from + p)
    {
        let Some(end) = lower
            .get(start..)
            .and_then(|s| s.find('>'))
            .map(|p| start + p + 1)
        else {
            break;
        };
        from = end;
        let Some(tag) = source.get(start..end) else {
            continue;
        };
        let is_icon = attr_value(tag, "rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|t| t.eq_ignore_ascii_case("icon"))
        });
        if let Some(href) = attr_value(tag, "href").filter(|h| is_icon && !h.is_empty()) {
            out.push(href);
        }
    }
    out
}

/// `href`를 저장소 안 PNG·WebP 파일로 푼다. 저장소 밖(심링크·`..`)으로 나가는 경로는 버린다.
fn resolve_href(repo: &Path, source_file: &Path, href: &str) -> Option<PathBuf> {
    let href = href.split(['#', '?']).next()?.trim();
    if href.is_empty()
        || href.contains("://")
        || href.starts_with("data:")
        || href.starts_with("//")
    {
        return None;
    }
    let canon_repo = fs::canonicalize(repo).ok()?;
    let candidates = if let Some(abs) = href.strip_prefix('/') {
        vec![repo.join("public").join(abs), repo.join(abs)]
    } else {
        let mut v: Vec<PathBuf> = source_file
            .parent()
            .map(|d| d.join(href))
            .into_iter()
            .collect();
        v.extend([repo.join("public").join(href), repo.join(href)]);
        v
    };
    candidates.into_iter().find_map(|cand| {
        let canon = fs::canonicalize(&cand).ok()?;
        (canon.starts_with(&canon_repo) && is_icon_file(&canon, false)).then_some(canon)
    })
}

/// 저장소 안 파비콘·로고 이미지(`.png`·`.webp`)를 찾는다(Orca `detectRepoFileIcon`).
fn detect_local_file(repo: &Path) -> Option<PathBuf> {
    let direct = FILE_STEMS.iter().find_map(|stem| {
        ["png", "webp"]
            .iter()
            .map(|ext| repo.join(format!("{stem}.{ext}")))
            .find(|p| is_icon_file(p, false))
    });
    direct.or_else(|| {
        SOURCE_FILES.iter().find_map(|rel| {
            let source = repo.join(rel);
            let text = read_capped(&source)?;
            icon_link_hrefs(&text)
                .iter()
                .find_map(|href| resolve_href(repo, &source, href))
        })
    })
}

/// 웹사이트 주소의 Google S2 파비콘 URL(Orca `faviconUrlFromWebsite`). 코드 호스팅 주소는 건너뛴다.
fn favicon_url(website: &str) -> Option<String> {
    let raw = website.trim();
    let rest = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .to_ascii_lowercase();
    let skip = [
        "github.com",
        "www.github.com",
        "gitlab.com",
        "www.gitlab.com",
        "bitbucket.org",
        "www.bitbucket.org",
        "localhost",
    ];
    let valid = host.contains('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    (valid && !skip.contains(&host.as_str()))
        .then(|| format!("https://www.google.com/s2/favicons?domain={host}&sz=64"))
}

fn package_homepage_favicon(repo: &Path) -> Option<String> {
    let text = read_capped(&repo.join("package.json"))?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    favicon_url(json.get("homepage")?.as_str()?)
}

/// GitHub 원격 URL(`git@github.com:o/r.git`, `https://github.com/o/r`, `ssh://git@github.com/o/r`)의 소유자.
fn github_owner(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
        .or_else(|| url.strip_prefix("git://"))
    {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?.split(':').next()?;
        host.eq_ignore_ascii_case("github.com").then_some(path)?
    } else {
        let (authority, path) = url.split_once(':')?;
        let host = authority.rsplit('@').next()?;
        host.eq_ignore_ascii_case("github.com").then_some(path)?
    };
    let owner = path.trim_start_matches('/').split('/').next()?;
    let ok = !owner.is_empty()
        && owner.len() <= 39
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !owner.starts_with('-')
        && !owner.ends_with('-');
    ok.then(|| owner.to_ascii_lowercase())
}

/// `.git/config`의 `upstream` → `origin` 순으로 GitHub 소유자 아바타 URL을 만든다
/// (Orca `githubAvatarSlug`: 포크면 원본 저장소 쪽 소유자).
fn github_avatar_url(repo: &Path) -> Option<String> {
    let config = read_capped(&repo.join(".git/config"))?;
    let mut remote: Option<String> = None;
    let mut urls: BTreeMap<String, String> = BTreeMap::new();
    for line in config.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            remote = section
                .strip_prefix("remote")
                .map(|r| r.trim().trim_matches('"').to_owned());
        } else if let (Some(name), Some((key, value))) = (&remote, line.split_once('='))
            && key.trim() == "url"
        {
            urls.entry(name.clone())
                .or_insert_with(|| value.trim().to_owned());
        }
    }
    ["upstream", "origin"]
        .iter()
        .find_map(|name| github_owner(urls.get(*name)?))
        .map(|owner| format!("https://github.com/{owner}.png?size=64"))
}

fn detect(repo: &Path, is_git: bool) -> Detected {
    let mtime = fs::metadata(repo).and_then(|m| m.modified()).ok();
    let key = (repo.to_path_buf(), is_git);
    if let Ok(cache) = DETECT_CACHE.lock()
        && let Some((cached, found)) = cache.get(&key)
        && *cached == mtime
    {
        return found.clone();
    }
    let found = detect_local_file(repo).map_or_else(
        || {
            package_homepage_favicon(repo)
                .or_else(|| is_git.then(|| github_avatar_url(repo)).flatten())
                .map_or(Detected::None, Detected::Remote)
        },
        Detected::Local,
    );
    if let Ok(mut cache) = DETECT_CACHE.lock() {
        cache.insert(key, (mtime, found.clone()));
    }
    found
}

fn explicit_icon(spec: &ImportedRepoIcon, cache_dir: &Path) -> Option<ProjectIcon> {
    let value = spec.value.trim();
    match spec.kind.trim() {
        "lucide" if !value.is_empty() => Some(ProjectIcon::Lucide(value.to_owned())),
        "emoji" if !value.is_empty() => Some(ProjectIcon::Emoji(value.to_owned())),
        "image" if value.starts_with("https://") => remote_icon(value, cache_dir),
        "image" => {
            let path = PathBuf::from(value);
            (path.is_absolute() && is_icon_file(&path, false)).then_some(ProjectIcon::Image(path))
        }
        _ => None,
    }
}

/// 로컬 프로젝트(`repos` = git 저장소, `folders` = 폴더 프로젝트)별 배지 색·아이콘을 계산한다.
pub(super) fn resolve(
    state_dir: &Path,
    repos: &[PathBuf],
    folders: &[PathBuf],
) -> BTreeMap<PathBuf, ProjectVisual> {
    let imported = super::external::imported_visuals(state_dir);
    let cache_dir = state_dir.join(REPO_ICON_DIR);
    let mut out = BTreeMap::new();
    for (path, is_git) in repos
        .iter()
        .map(|p| (p, true))
        .chain(folders.iter().map(|p| (p, false)))
    {
        let meta: Option<&ImportedVisual> = imported
            .get(path)
            .or_else(|| imported.get(&super::external::normalize_imported_path(path)));
        let is_git = meta.map_or(is_git, |m| m.is_git);
        let icon = meta
            .and_then(|m| m.repo_icon.as_ref())
            .and_then(|spec| explicit_icon(spec, &cache_dir))
            .or_else(|| match detect(path, is_git) {
                Detected::Local(p) => Some(ProjectIcon::Image(p)),
                Detected::Remote(url) => remote_icon(&url, &cache_dir),
                Detected::None => None,
            });
        let badge_color = meta.and_then(|m| m.badge_color.clone());
        if icon.is_some() || badge_color.is_some() {
            out.insert(path.clone(), ProjectVisual { badge_color, icon });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\0";

    #[test]
    fn detects_local_files_links_homepage_and_github_owner() {
        let base = std::env::temp_dir().join(format!("mk-project-icons-{}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        let web = base.join("web");
        fs::create_dir_all(web.join("public/img")).unwrap();
        // 파일 우선순위: `favicon.*`이 없으면 `public/logo.png`.
        fs::write(web.join("public/logo.png"), PNG).unwrap();
        assert_eq!(detect_local_file(&web), Some(web.join("public/logo.png")));
        fs::remove_file(web.join("public/logo.png")).unwrap();
        // `<link rel="icon">`의 루트 기준 href는 `public/` 아래에서 찾는다. 저장소 밖은 버린다.
        fs::write(web.join("public/img/fav.png"), PNG).unwrap();
        fs::write(
            web.join("index.html"),
            r#"<link rel="stylesheet" href="/a.css"><link rel="shortcut icon" href="/img/fav.png?v=2"><link rel="icon" href="../../etc/x.png">"#,
        )
        .unwrap();
        assert_eq!(
            detect_local_file(&web),
            Some(fs::canonicalize(web.join("public/img/fav.png")).unwrap())
        );
        assert_eq!(
            icon_link_hrefs(r#"<link rel="icon" href="../x.png">"#),
            vec!["../x.png"]
        );
        assert_eq!(
            resolve_href(&web, &web.join("index.html"), "../x.png"),
            None
        );

        assert_eq!(
            favicon_url("https://www.aasoft.kr/docs").as_deref(),
            Some("https://www.google.com/s2/favicons?domain=www.aasoft.kr&sz=64")
        );
        assert_eq!(favicon_url("https://github.com/o/r"), None);
        assert_eq!(favicon_url("localhost:3000"), None);

        for (url, owner) in [
            ("git@github.com:CraftOwen/minikrill.git", Some("craftowen")),
            ("https://github.com/acme/repo", Some("acme")),
            ("ssh://git@github.com/acme/repo.git", Some("acme")),
            ("https://gitlab.com/acme/repo", None),
            ("git@github.com:-bad/repo", None),
        ] {
            assert_eq!(github_owner(url).as_deref(), owner, "{url}");
        }
        let gh = base.join("gh");
        fs::create_dir_all(gh.join(".git")).unwrap();
        fs::write(
            gh.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:me/fork.git\n[remote \"upstream\"]\n\turl = https://github.com/acme/fork\n",
        )
        .unwrap();
        assert_eq!(
            github_avatar_url(&gh).as_deref(),
            Some("https://github.com/acme.png?size=64")
        );
        assert_eq!(
            detect(&gh, true),
            Detected::Remote("https://github.com/acme.png?size=64".to_owned())
        );
        assert_eq!(
            detect(&gh, false),
            Detected::None,
            "폴더 프로젝트는 아바타를 쓰지 않는다"
        );
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn resolve_prefers_explicit_icon_and_uses_cached_remote_images() {
        let base = std::env::temp_dir().join(format!("mk-project-visuals-{}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        let state = base.join("state");
        let (lucide, remote, plain) =
            (base.join("lucide"), base.join("remote"), base.join("plain"));
        for dir in [&state, &lucide, &remote, &plain] {
            fs::create_dir_all(dir).unwrap();
        }
        let url = "https://github.com/craftowen.png?size=64";
        let cached = cached_url_path(&state.join(REPO_ICON_DIR), url);
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        fs::write(
            state.join(super::super::external::PROJECTS_FILE),
            format!(
                "[[projects]]\nid = \"a\"\npath = \"{}\"\nkind = \"git\"\nbadge_color = \"#3b82f6\"\n\
                 repo_icon = {{ kind = \"lucide\", value = \"Rocket\" }}\n\
                 [[projects]]\nid = \"b\"\npath = \"{}\"\nkind = \"git\"\n\
                 repo_icon = {{ kind = \"image\", value = \"{url}\" }}\n",
                lucide.display(),
                remote.display()
            ),
        )
        .unwrap();
        let visuals = resolve(
            &state,
            &[lucide.clone(), remote.clone()],
            std::slice::from_ref(&plain),
        );
        assert_eq!(
            visuals.get(&lucide),
            Some(&ProjectVisual {
                badge_color: Some("#3b82f6".to_owned()),
                icon: Some(ProjectIcon::Lucide("Rocket".to_owned())),
            })
        );
        assert_eq!(
            visuals.get(&remote).and_then(|v| v.icon.clone()),
            Some(ProjectIcon::Image(cached))
        );
        assert!(
            !visuals.contains_key(&plain),
            "아이콘·배지 없는 프로젝트는 기본 폴더 아이콘"
        );
        fs::remove_dir_all(&base).ok();
    }
}
