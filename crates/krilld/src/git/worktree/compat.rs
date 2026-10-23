//! Orca 저장소 설정 읽기 호환(MK-080). 원본 파일을 수정하지 않는다.

use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::os::fs::OpenOptionsExt as _;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use serde::Deserialize;

use super::{CONFIG, Config};

type Environment = Vec<(Vec<u8>, Vec<u8>)>;

/// 셸·에이전트·setup 세션의 자식 환경. ROOT는 주 워크트리, WORKTREE는 cwd가 속한 워크트리다.
/// git 아닌 디렉터리에서는 호환 변수를 만들지 않는다. 모든 git 호출은 데몬 큐를 거친다.
///
/// 새 칸 생성(`Create`)이 이 값을 기다리므로, 흔한 배치(`.git` 디렉터리·연결 워크트리)는 git 자식 없이
/// 파일시스템만 읽어 정한다. git 두 번(큐 대기 포함)이 생성 시간의 대부분이었다(디버그 빌드 측정 50~225ms).
/// 확신할 수 없는 배치(서브모듈·`core.worktree`·GIT_* 환경·파일시스템 경계)는 git에 맡긴다.
pub fn environment(cwd: &Path) -> Environment {
    match discover(cwd) {
        Some(Discovered::Repo { root, worktree }) => {
            return variables(root.as_os_str().as_bytes(), &worktree);
        }
        Some(Discovered::NotRepo) => return Vec::new(),
        None => {}
    }
    super::queue().map_or_else(
        |_| Vec::new(),
        |q| environment_in(q, cwd).unwrap_or_default(),
    )
}

/// [`discover`]가 git 없이 확정한 결과.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Discovered {
    /// `root`는 주 워크트리, `worktree`는 cwd가 속한 워크트리.
    Repo {
        root: std::path::PathBuf,
        worktree: std::path::PathBuf,
    },
    NotRepo,
}

/// cwd의 워크트리를 git 없이 찾는다. `None`이면 판단을 git에 넘긴다.
pub(super) fn discover(cwd: &Path) -> Option<Discovered> {
    const GIT_ENV: [&str; 5] = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ];
    if GIT_ENV.iter().any(|k| std::env::var_os(k).is_some()) {
        return None;
    }
    let start = std::fs::canonicalize(cwd).ok()?;
    let start_device = device(&start)?;
    for dir in start.ancestors() {
        if device(dir)? != start_device {
            return None; // git은 파일시스템 경계에서 멈춘다.
        }
        let dot = dir.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&dot) else {
            continue;
        };
        if meta.is_dir() {
            if !dot.join("HEAD").is_file() || dot.join("commondir").exists() {
                return None;
            }
            // `core.worktree`·`bare = true`·워크트리별 설정이 있으면 작업 트리가 `.git`의 부모가 아닐 수 있다.
            let config = std::fs::read(dot.join("config")).ok()?;
            if config.split(|b| *b == b'\n').any(unusual_config) {
                return None;
            }
            return Some(Discovered::Repo {
                root: dir.to_path_buf(),
                worktree: dir.to_path_buf(),
            });
        }
        if !meta.is_file() {
            return None;
        }
        // 연결 워크트리: `.git` 파일의 `gitdir: <main>/.git/worktrees/<name>`과 그 안 `commondir`.
        let text = std::fs::read(&dot).ok()?;
        let gitdir = text.strip_prefix(b"gitdir: ")?.trim_ascii_end();
        let gitdir = dir.join(OsStr::from_bytes(gitdir));
        let common = std::fs::read(gitdir.join("commondir")).ok()?; // 없으면 서브모듈
        let common =
            std::fs::canonicalize(gitdir.join(OsStr::from_bytes(common.trim_ascii_end()))).ok()?;
        if common.file_name() != Some(OsStr::new(".git")) {
            return None;
        }
        return Some(Discovered::Repo {
            root: common.parent()?.to_path_buf(),
            worktree: dir.to_path_buf(),
        });
    }
    Some(Discovered::NotRepo)
}

/// 빠른 경로가 틀릴 수 있는 설정 줄(`worktree = …`, `bare = true`, `worktreeConfig = …`).
fn unusual_config(line: &[u8]) -> bool {
    let key: Vec<u8> = line
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .map(u8::to_ascii_lowercase)
        .collect();
    key == b"bare=true" || key.starts_with(b"worktree=") || key.starts_with(b"worktreeconfig=")
}

/// 파일시스템 번호. Windows는 경로 규칙(`\\?\` 접두사 등)이 달라 빠른 경로를 쓰지 않는다(`None` → git).
#[cfg(unix)]
fn device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device(_: &Path) -> Option<u64> {
    None
}

pub(super) fn environment_in(q: &super::GitQueue, cwd: &Path) -> Result<Environment, String> {
    let worktree = super::output_path(
        super::git(q, cwd, ["rev-parse", "--show-toplevel"])?,
        "not a git repository",
    )?;
    let list = super::git(q, &worktree, ["worktree", "list", "--porcelain", "-z"])?;
    if !list.status.success() {
        return Err("cannot find main worktree".to_owned());
    }
    let root = list
        .stdout
        .split(|b| *b == 0)
        .find_map(|line| line.strip_prefix(b"worktree "))
        .ok_or("cannot find main worktree")?;
    Ok(variables(root, &worktree))
}

fn variables(root: &[u8], worktree: &Path) -> Environment {
    let name = worktree.file_name().unwrap_or(OsStr::new("")).as_bytes();
    let path = worktree.as_os_str().as_bytes();
    [
        ("KRILL_ROOT_PATH", root),
        ("KRILL_WORKTREE_PATH", path),
        ("KRILL_WORKSPACE_NAME", name),
        ("ORCA_ROOT_PATH", root),
        ("ORCA_WORKTREE_PATH", path),
        ("ORCA_WORKSPACE_NAME", name),
        ("CONDUCTOR_ROOT_PATH", root),
        ("GHOSTX_ROOT_PATH", root),
    ]
    .into_iter()
    .map(|(k, v)| (k.as_bytes().to_vec(), v.to_vec()))
    .collect()
}

const ORCA_CONFIG: &str = "orca.yaml";
const INCLUDE: &str = ".worktreeinclude";
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 1000;

#[derive(Default, Deserialize)]
#[serde(default)]
struct Orca {
    scripts: Scripts,
    worktree: OrcaWorktree,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Scripts {
    setup: Option<String>,
    archive: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct OrcaWorktree {
    #[serde(alias = "shared_directories")]
    shared_directories: Vec<String>,
    #[serde(default)]
    symlinks: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ImportedProjectsFile {
    projects: Vec<ImportedProjectHooks>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ImportedProjectHooks {
    path: String,
    hook_mode: Option<String>,
    setup_script: Option<String>,
    symlink_paths: Vec<String>,
}

fn repo_hook_config(repo_top: &Path) -> Option<ImportedProjectHooks> {
    let state_dir = krill_proto::daemon::state_dir()?;
    let raw = read(&state_dir.join("projects.toml")).ok()??;
    let parsed: ImportedProjectsFile = toml::from_str(&raw).ok()?;
    let mut aliases = vec![repo_top.to_string_lossy().into_owned()];
    if let Ok(canon) = std::fs::canonicalize(repo_top) {
        let s = canon.to_string_lossy().into_owned();
        if !aliases.contains(&s) {
            aliases.push(s);
        }
    }
    if let Ok(rest) = repo_top.strip_prefix("/private") {
        let s = Path::new("/").join(rest).to_string_lossy().into_owned();
        if !aliases.contains(&s) {
            aliases.push(s);
        }
    } else if let Ok(rest) = repo_top.strip_prefix("/") {
        let s = Path::new("/private")
            .join(rest)
            .to_string_lossy()
            .into_owned();
        if !aliases.contains(&s) {
            aliases.push(s);
        }
    }
    parsed
        .projects
        .into_iter()
        .find(|p| aliases.iter().any(|c| c == &p.path))
}

pub(super) fn read_config(top: &Path) -> Result<Config, String> {
    read_config_with_state(top, krill_proto::daemon::state_dir().as_deref())
}

pub(super) fn read_config_with_state(
    top: &Path,
    state_dir: Option<&Path>,
) -> Result<Config, String> {
    let mut config: Config = match read(&krill_proto::user_dirs::repo_config(top))? {
        Some(s) => toml::from_str(&s).map_err(|e| format!("{CONFIG}: {e}"))?,
        None => Config::default(),
    };
    let orca: Option<Orca> = match read(&top.join(ORCA_CONFIG))? {
        Some(s) => Some(serde_saphyr::from_str(&s).map_err(|e| format!("{ORCA_CONFIG}: {e}"))?),
        None => None,
    };
    let store = state_dir.and_then(|dir| krill_proto::orchestration::WorktreeStore::load(dir).ok());
    let top_key = top.to_string_lossy();
    let imported = repo_hook_config(top);

    // 네이티브 setup/archive가 우선한다. 빈 문자열도 명시적인 재정의다.
    // setup: 네이티브 > 가져온 프로젝트(override) > Krill 저장소별 설정 > orca.yaml > 가져온 프로젝트.
    if config.worktree.setup.is_none()
        && let Some(ref proj) = imported
        && proj.hook_mode.as_deref() == Some("override")
        && proj.setup_script.is_some()
    {
        config.worktree.setup.clone_from(&proj.setup_script);
    }
    if config.worktree.setup.is_none() {
        config.worktree.setup = store
            .as_ref()
            .and_then(|s| s.setup_script_for(&top_key))
            .or_else(|| orca.as_ref().and_then(|o| o.scripts.setup.clone()))
            .or_else(|| imported.as_ref().and_then(|p| p.setup_script.clone()));
    }
    if config.worktree.archive.is_none() {
        config.worktree.archive = store
            .as_ref()
            .and_then(|s| s.archive_script_for(&top_key))
            .or_else(|| orca.as_ref().and_then(|o| o.scripts.archive.clone()));
    }
    if let Some(o) = orca {
        config
            .worktree
            .symlinks
            .extend(o.worktree.shared_directories);
        config.worktree.symlinks.extend(o.worktree.symlinks);
    }
    if let Some(s) = store {
        config.worktree.symlinks.extend(s.symlinks_for(&top_key));
    }
    if config.worktree.symlinks.len() > MAX_ENTRIES {
        return Err(format!("symlinks: exceeds {MAX_ENTRIES} entries"));
    }
    let mut clean_symlinks = Vec::new();
    for item in &config.worktree.symlinks {
        let trimmed = item.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            clean_symlinks.push(trimmed.to_owned());
        }
    }
    clean_symlinks.sort();
    clean_symlinks.dedup();
    config.worktree.symlinks = clean_symlinks;

    if let Some(s) = read(&top.join(INCLUDE))? {
        config
            .worktree
            .include
            .extend(include_manifest_entries(&s)?);
    }
    if let Some(proj) = imported {
        for entry in proj.symlink_paths {
            let trimmed = entry.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('!') && !trimmed.contains(['{', '}']) {
                config.worktree.include.push(trimmed.to_owned());
            }
        }
    }
    config.worktree.include.sort();
    config.worktree.include.dedup();
    config.worktree.setup = config.worktree.setup.filter(|s| !s.trim().is_empty());
    config.worktree.archive = config.worktree.archive.filter(|s| !s.trim().is_empty());
    Ok(config)
}

fn read(path: &Path) -> Result<Option<String>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
        return Err(format!("{}: configuration exceeds 256 KiB", path.display()));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(all(test, unix))]
fn include_entries(s: &str) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    for (index, line) in s
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .enumerate()
    {
        if index >= MAX_ENTRIES {
            return Err(format!("{INCLUDE}: exceeds {MAX_ENTRIES} entries"));
        }
        if line.starts_with('!') || line.contains(['*', '?', '[', ']', '{', '}']) {
            continue;
        }
        entries.push(line.to_owned());
    }
    Ok(entries)
}

/// `.worktreeinclude` 항목 파서(#24675): 리터럴 경로와 glob 패턴(`**/.env*`, `*.key` 등)을 모두 허용하되,
/// 부정(`!`)과 브레이스 확장(`{a,b}`)은 제외한다.
fn include_manifest_entries(s: &str) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    for (index, line) in s
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .enumerate()
    {
        if index >= MAX_ENTRIES {
            return Err(format!("{INCLUDE}: exceeds {MAX_ENTRIES} entries"));
        }
        if line.starts_with('!') || line.contains(['{', '}']) {
            continue;
        }
        entries.push(line.to_owned());
    }
    Ok(entries)
}

/// clonefile이 실패하면 새 파일을 독점 생성해 복사한다. 기존 파일은 어느 경로에서도 덮어쓰지 않는다.
pub(super) fn copy_file(src: &Path, dst: &Path) -> io::Result<()> {
    copy_with(src, dst, crate::sys::clone_file)
}

fn copy_with(
    src: &Path,
    dst: &Path,
    clone: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    match clone(src, dst) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
        Err(_) => {}
    }
    let mut source = crate::os::open_no_follow(OpenOptions::new().read(true), src)?;
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)?;
    io::copy(&mut source, &mut target)?;
    target.set_permissions(source.metadata()?.permissions())
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
