//! 워크트리 생성(MK-042, T4 §3.2.1). git 호출은 전부 [`GitQueue`]를 거치므로 같은 레포 작업은 직렬이다.
//!
//! 순서: 레포 설정(`minikrill.toml`) 읽기 → setup 신뢰 확인(신뢰하지 않으면 아무것도 만들지 않고 끝) → 이름 정리 →
//! 기준 ref 결정 → 이름 충돌이면 `-2`~`-100` → `git worktree add --no-track -b` → include 복사.
//!
//! ```toml
//! # <레포>/minikrill.toml
//! [worktree]
//! include = [".env", "config/local.json"]  # 레포 기준 리터럴 경로. gitignore된 것만 복사한다.
//! setup = "pnpm install"                    # 새 워크트리에서 `/bin/sh -c`로 돈다. 내용이 바뀌면 다시 묻는다.
//! ```
//!
//! Orca 호환(MK-080): `.worktreeinclude`를 include에 합치고, native setup이 없으면
//! `orca.yaml`의 `scripts.setup`을 같은 신뢰 절차로 실행한다.
//!
//! - include: `..`·절대 경로·경로 위의 심링크는 거부한다(레포 밖을 읽거나 워크트리 밖에 쓰지 못하게). 디렉터리는
//!   안의 심링크·특수 파일을 건너뛰고 APFS clonefile(불가하면 일반 복사)로 복사한다.
//! - setup 신뢰: 레포 경로와 스크립트 내용의 SHA-256을 신뢰 파일에 한 줄씩 남긴다. 같은 레포에서 같은 내용이면
//!   묻지 않는다.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Output;

use krill_proto::control::{Create, Failure, NewWorktree, Program, Reply, Setup, Worktree};
use krill_proto::holder::WinSize;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{GitQueue, queue};

mod compat;
pub mod prepare;
mod remove;
pub use compat::environment;
pub use remove::{remove, remove_with_state, serve_remove};

/// 레포 설정 파일(ARCHITECTURE §5 설정 호환: `minikrill.toml` 기본).
pub const CONFIG: &str = "minikrill.toml";
/// setup 신뢰 기록. 상태 디렉터리 안.
pub const TRUST_FILE: &str = "setup-trust";
/// 이름이 겹칠 때 붙여 보는 마지막 번호(T4 §3.2.1).
const MAX_SUFFIX: u32 = 100;
/// 기준 ref 감지 순서(T4 G2). 하나도 없으면 `HEAD`.
const BASES: [(&str, &str); 5] = [
    ("origin/HEAD", "refs/remotes/origin/HEAD"),
    ("origin/main", "refs/remotes/origin/main"),
    ("origin/master", "refs/remotes/origin/master"),
    ("main", "refs/heads/main"),
    ("master", "refs/heads/master"),
];

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    worktree: Section,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Section {
    base: Option<String>,
    include: Vec<String>,
    #[serde(alias = "shared_directories")]
    symlinks: Vec<String>,
    setup: Option<String>,
    archive: Option<String>,
}

/// [`create`]의 결과.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 만들었다. `setup`이 있으면 호출자가 새 워크트리에서 돌린다(`Worktree::setup`은 비어 있다).
    Created {
        worktree: Worktree,
        setup: Option<String>,
    },
    /// setup 스크립트를 이 내용으로 신뢰한 적이 없다. 아무것도 만들지 않았다.
    Untrusted { script: String, hash: Vec<u8> },
}

/// 워크트리 루트. `KRILL_WORKTREES_DIR`가 있으면 그것(개발·테스트 격리), 없으면 `~/Minikrill/worktrees`(T4 §3.2.1).
fn root() -> Result<PathBuf, String> {
    root_in(|key| std::env::var_os(key))
}

/// [`root`]의 본체. 환경변수를 직접 만지지 않아 병렬 테스트에 안전하다.
fn root_in(get: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    if let Some(d) = get("KRILL_WORKTREES_DIR") {
        return Ok(PathBuf::from(d));
    }
    get("HOME")
        .map(|h| PathBuf::from(h).join("Minikrill/worktrees"))
        .ok_or_else(|| "HOME is not set".to_owned())
}

/// setup 세션을 띄울 기본 크기. 요청한 세션 크기를 알 수 없을 때만 쓴다.
const DEFAULT_SETUP_SIZE: WinSize = WinSize { cols: 80, rows: 24 };

/// [`Request::NewWorktree`](krill_proto::control::Request::NewWorktree)에 답한다. setup은 `spawn`으로 새 세션에서
/// 돌린다(출력을 터미널로 볼 수 있게). `size`는 요청한 세션의 현재 크기(없으면 80×24). 세션을 못 띄워도
/// 워크트리는 남기고 경고로 알린다.
pub fn serve(
    req: &NewWorktree,
    size: Option<WinSize>,
    spawn: impl FnOnce(Create) -> Result<Reply, Failure>,
) -> Reply {
    let trust = krill_proto::daemon::state_dir().map(|d| d.join(TRUST_FILE));
    let outcome = match (queue(), root(), trust) {
        (Ok(q), Ok(root), Some(trust)) => {
            create_with_pool(q, req, &root, &trust, prepare::existing())
        }
        (Err(e), _, _) | (_, Err(e), _) => Err(e),
        (_, _, None) => Err("HOME is not set".to_owned()),
    };
    finish(outcome, size, spawn)
}

fn finish(
    outcome: Result<Outcome, String>,
    size: Option<WinSize>,
    spawn: impl FnOnce(Create) -> Result<Reply, Failure>,
) -> Reply {
    let (mut worktree, setup) = match outcome {
        Ok(Outcome::Created { worktree, setup }) => (worktree, setup),
        Ok(Outcome::Untrusted { script, hash }) => return Reply::SetupUntrusted { script, hash },
        Err(e) => return Reply::WorktreeFailed(e),
    };
    if let Some(script) = setup {
        let size = size.unwrap_or(DEFAULT_SETUP_SIZE);
        let c = Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![b"-c".to_vec(), script.into_bytes()],
            },
            cwd: worktree.path.clone(),
            cols: size.cols,
            rows: size.rows,
        };
        match spawn(c) {
            Ok(Reply::Created { session, .. }) => worktree.setup = Some(session),
            Ok(_) => worktree.warnings.push("setup did not start".to_owned()),
            Err(f) => worktree.warnings.push(format!("setup did not start: {f}")),
        }
    }
    Reply::WorktreeCreated(worktree)
}

/// `req.repo`에 워크트리를 `<root>/<레포 이름>/<이름>`으로 만든다. `trust`는 setup 신뢰 파일.
///
/// # Errors
/// 사람이 읽을 이유(git 실패는 git의 stderr).
pub fn create(
    q: &GitQueue,
    req: &NewWorktree,
    root: &Path,
    trust: &Path,
) -> Result<Outcome, String> {
    create_with_pool(q, req, root, trust, None)
}

/// 스케줄 격리 실행용 새 워크트리를 만든다(MK-186). 비대화형 크론이므로 setup 스크립트는 건너뛴다.
pub fn create_isolated(repo: &Path, name: &str, state_dir: &Path) -> Result<PathBuf, String> {
    let q = queue()?;
    let root = std::env::var_os("KRILL_WORKTREES_DIR")
        .or_else(|| std::env::var_os("MK_WORKTREES_DIR"))
        .map_or_else(|| state_dir.join("worktrees"), PathBuf::from);
    let trust = state_dir.join(TRUST_FILE);
    let req = NewWorktree {
        repo: repo.as_os_str().as_bytes().to_vec(),
        name: name.to_owned(),
        base: None,
        setup: Setup::Skip,
    };
    match create_with_pool(q, &req, &root, &trust, prepare::existing())? {
        Outcome::Created { worktree, .. } => Ok(PathBuf::from(OsString::from_vec(worktree.path))),
        Outcome::Untrusted { .. } => Err("worktree setup untrusted".to_owned()),
    }
}

/// Creation path shared by normal requests and the daemon preparation pool.
pub fn create_with_pool(
    q: &GitQueue,
    req: &NewWorktree,
    root: &Path,
    trust: &Path,
    pool: Option<&prepare::Pool>,
) -> Result<Outcome, String> {
    let given = Path::new(OsStr::from_bytes(&req.repo));
    // 넘겨받은 워크트리가 설정·include의 출처다. 경로는 비 UTF-8 바이트도 보존한다.
    let top = output_path(
        git(q, given, ["rev-parse", "--show-toplevel"])?,
        "not a git repository",
    )?;
    let _transaction = transaction(q, &top)?;
    let config = compat::read_config_with_state(&top, trust.parent())?;

    if let (Some(archive), Setup::Approve(h)) = (config.worktree.archive.as_deref(), &req.setup)
        && *h == digest(&top, archive)
    {
        remember(trust, h).map_err(|e| format!("cannot record setup trust: {e}"))?;
    }

    let setup = match (config.worktree.setup, &req.setup) {
        (None, _) | (_, Setup::Skip) => None,
        (Some(script), mode) => {
            let hash = digest(&top, &script);
            if matches!(mode, Setup::Approve(h) if *h == hash) {
                remember(trust, &hash).map_err(|e| format!("cannot record setup trust: {e}"))?;
            } else if !trusted(trust, &hash).map_err(|e| format!("cannot read setup trust: {e}"))? {
                return Ok(Outcome::Untrusted { script, hash });
            }
            Some(script)
        }
    };

    let leaf = leaf(&req.name)?;
    // `-`로 시작하는 이름은 `leaf`가 막는다. 남은 문법(`..`, `~`, `.lock` 등)은 git이 판정한다.
    text(
        git(q, &top, ["check-ref-format", "--branch", leaf.as_str()])?,
        "invalid branch name",
    )?;
    let base = base(
        q,
        &top,
        req.base.as_deref().or(config.worktree.base.as_deref()),
    )?;

    let repo_name = top.file_name().ok_or("repository has no name")?;
    let dir = root.join(repo_name);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // git이 기록하는 경로와 비교하려고 실제 경로로(`/tmp` → `/private/tmp`).
    let dir = fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (branch, path) = free_name(q, &top, &dir, &leaf)?;

    let promoted = match pool
        .map(|p| p.promote(q, &top, &base, &path, &branch))
        .transpose()
    {
        Ok(promoted) => promoted.unwrap_or(false),
        Err(error) => {
            // Remove only our empty reservation; a moved checkout must survive any later error.
            fs::remove_dir(&path).ok();
            return Err(error);
        }
    };
    if !promoted {
        let mut add: Vec<OsString> = ["worktree", "add", "--no-track", "-b", branch.as_str()]
            .map(OsString::from)
            .into();
        add.push(path.clone().into_os_string());
        add.push(base.clone().into());
        if let Err(e) = git(q, &top, add).and_then(|out| text(out, "git worktree add failed")) {
            // 직접 확보한 빈 디렉터리만 지운다. git이 중간까지 쓴 내용은 지우지 않는다.
            fs::remove_dir(&path).ok();
            return Err(e);
        }
    }

    let (mut copied, mut warnings) = link_symlinks(&top, &path, &config.worktree.symlinks);
    let include_entries: Vec<String> = config
        .worktree
        .include
        .iter()
        .filter(|e| !copied.iter().any(|l| l == e.as_str()))
        .cloned()
        .collect();
    let (inc_copied, inc_warnings) = copy_includes(q, &top, &path, &include_entries);
    copied.extend(inc_copied);
    warnings.extend(inc_warnings);
    crate::agent::pre_trust_all_agents(&path);
    Ok(Outcome::Created {
        worktree: Worktree {
            path: path.into_os_string().into_vec(),
            branch,
            base,
            copied,
            warnings,
            setup: None,
        },
        setup,
    })
}

/// 첫 프롬프트에서 슬러그를 뽑아 현재 워크트리의 기본 브랜치(`worktree-1`, `claude-1`, 동물 이름 등)를
/// 자동 개명한다(OWE-737).
///
/// - 보호 브랜치(`main`·`master` 등)나 이미 사용자가 지정한 브랜치 이름은 건드리지 않는다.
/// - upstream 추적 브랜치(`@{u}`)가 설정된 브랜치는 건드리지 않는다.
/// - 같은 이름의 로컬 브랜치가 이미 있으면 `-2`~`-100` 접미사를 붙인다.
pub fn rename_branch_from_prompt(
    q: &GitQueue,
    worktree: &Path,
    prompt: &str,
) -> Result<Option<String>, String> {
    let Some(slug) = krill_proto::git::prompt_branch_slug(prompt) else {
        return Ok(None);
    };
    let top = output_path(
        git(q, worktree, ["rev-parse", "--show-toplevel"])?,
        "not a git repository",
    )?;
    let _transaction = transaction(q, &top)?;
    let current = match git(q, &top, ["symbolic-ref", "--quiet", "--short", "HEAD"])
        .and_then(|out| text(out, "detached HEAD"))
    {
        Ok(b) if !b.is_empty() => b,
        _ => return Ok(None),
    };
    if !krill_proto::git::is_default_branch_or_worktree_name(&current) {
        return Ok(None);
    }
    if git(
        q,
        &top,
        ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_ok_and(|out| out.status.success())
    {
        return Ok(None);
    }
    let refs = text(
        git(
            q,
            &top,
            [
                "for-each-ref",
                "--format=%(refname:lstrip=2)",
                "refs/heads/",
            ],
        )?,
        "git for-each-ref failed",
    )?;
    let branches: HashSet<&str> = refs.lines().collect();
    for n in 1..=MAX_SUFFIX {
        let candidate = if n == 1 {
            slug.clone()
        } else {
            format!("{slug}-{n}")
        };
        if candidate == current {
            return Ok(Some(candidate));
        }
        if branches.contains(candidate.as_str()) {
            continue;
        }
        if !git(
            q,
            &top,
            ["check-ref-format", "--branch", candidate.as_str()],
        )
        .is_ok_and(|out| out.status.success())
        {
            continue;
        }
        text(
            git(
                q,
                &top,
                ["branch", "-m", current.as_str(), candidate.as_str()],
            )?,
            "git branch -m failed",
        )?;
        return Ok(Some(candidate));
    }
    Err(format!("branch name {slug:?} is taken up to -{MAX_SUFFIX}"))
}

/// 생성·삭제의 여러 git 호출을 공통 디렉터리에서 하나의 작업으로 잠근다.
fn transaction(q: &GitQueue, top: &Path) -> Result<fs::File, String> {
    let common = output_path(
        git(
            q,
            top,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?,
        "cannot find the common git directory",
    )?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(common.join("minikrill-worktree.lock"))
        .map_err(|e| format!("cannot open worktree lock: {e}"))?;
    file.lock()
        .map_err(|e| format!("cannot lock worktree transaction: {e}"))?;
    Ok(file)
}

fn git<I, S>(q: &GitQueue, repo: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    q.submit(repo, args)
        .recv()
        .map_err(|_| "git queue closed".to_owned())?
        .map_err(|e| format!("cannot run git: {e}"))
}

/// 성공했으면 stdout(끝 줄바꿈 제외), 아니면 git의 stderr(비었으면 `what`).
fn text(out: Output, what: &str) -> Result<String, String> {
    let Output {
        status,
        stdout,
        stderr,
    } = out;
    if status.success() {
        return Ok(String::from_utf8_lossy(&stdout)
            .trim_end_matches('\n')
            .to_owned());
    }
    let err = String::from_utf8_lossy(&stderr).trim().to_owned();
    Err(if err.is_empty() { what.to_owned() } else { err })
}

fn output_path(mut out: Output, what: &str) -> Result<PathBuf, String> {
    if !out.status.success() {
        return text(out, what).map(PathBuf::from);
    }
    if out.stdout.last() == Some(&b'\n') {
        out.stdout.pop();
    }
    Ok(PathBuf::from(OsString::from_vec(out.stdout)))
}

/// 신뢰 키: 레포 경로와 스크립트. 다른 레포의 같은 스크립트는 따로 묻는다.
pub(crate) fn digest(top: &Path, script: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(top.as_os_str().as_bytes());
    h.update([0]);
    h.update(script.as_bytes());
    h.finalize().to_vec()
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").ok();
        s
    })
}

pub(crate) fn trusted(file: &Path, hash: &[u8]) -> io::Result<bool> {
    match fs::read_to_string(file) {
        Ok(s) => Ok(s.lines().any(|l| l == hex(hash))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub(crate) fn remember(file: &Path, hash: &[u8]) -> io::Result<()> {
    if trusted(file, hash)? {
        return Ok(());
    }
    if let Some(d) = file.parent() {
        fs::create_dir_all(d)?;
    }
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(file)?
        .write_all(format!("{}\n", hex(hash)).as_bytes())
}

/// `state_dir/setup-trust`에 `(top, script)` SHA-256 해시가 승인돼 있거나
/// 사용자가 `state_dir`의 `WorktreeStore`에 직접 설정한 `archive` 스크립트인지 확인한다(OWE-751).
pub(crate) fn is_setup_trusted(
    state_dir: Option<&Path>,
    top: &Path,
    script: &str,
) -> Result<bool, String> {
    let Some(dir) = state_dir else {
        return Ok(false);
    };
    let trust_file = dir.join(TRUST_FILE);
    let hash = digest(top, script);
    if trusted(&trust_file, &hash).map_err(|e| format!("cannot read setup trust: {e}"))? {
        return Ok(true);
    }
    if let Ok(canon) = fs::canonicalize(top)
        && canon != top
    {
        let canon_hash = digest(&canon, script);
        if trusted(&trust_file, &canon_hash).map_err(|e| format!("cannot read setup trust: {e}"))? {
            return Ok(true);
        }
    }
    if let Ok(rest) = top.strip_prefix("/private") {
        let alias = Path::new("/").join(rest);
        let alias_hash = digest(&alias, script);
        if trusted(&trust_file, &alias_hash).map_err(|e| format!("cannot read setup trust: {e}"))? {
            return Ok(true);
        }
    }
    if let Ok(store) = krill_proto::orchestration::WorktreeStore::load(dir) {
        let top_key = top.to_string_lossy();
        if store.archive_script_for(&top_key).as_deref() == Some(script) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 사용자가 입력한 이름 → 디렉터리·브랜치 이름. 공백 묶음은 `-` 하나로. 한글 등 비ASCII는 그대로 둔다.
fn leaf(name: &str) -> Result<String, String> {
    let leaf = name.split_whitespace().collect::<Vec<_>>().join("-");
    if leaf.is_empty()
        || leaf.starts_with(['.', '-'])
        || leaf.contains(['/', '\\'])
        || leaf.chars().any(char::is_control)
    {
        return Err(format!(
            "invalid worktree name {name:?}: must not be empty, start with '.' or '-', or contain '/'"
        ));
    }
    Ok(leaf)
}

/// 사용자 지정이면 커밋으로 풀리는지 확인, 아니면 [`BASES`] 순서로 있는 첫 ref.
fn base(q: &GitQueue, top: &Path, given: Option<&str>) -> Result<String, String> {
    if let Some(b) = given {
        if b.starts_with('-') {
            return Err(format!("invalid base ref {b:?}"));
        }
        let commit = format!("{b}^{{commit}}");
        let args = [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &commit,
        ];
        text(git(q, top, args)?, "")
            .map_err(|_| format!("base ref {b:?} does not name a commit"))?;
        return Ok(b.to_owned());
    }
    let mut args = vec!["for-each-ref", "--format=%(refname)"];
    args.extend(BASES.map(|(_, full)| full));
    let found = text(git(q, top, args)?, "git for-each-ref failed")?;
    let found: HashSet<&str> = found.lines().collect();
    Ok(BASES
        .iter()
        .find(|(_, full)| found.contains(full))
        .map_or("HEAD", |(short, _)| short)
        .to_owned())
}

/// 브랜치·디렉터리·등록된 워크트리 어느 것과도 겹치지 않는 `leaf`, `leaf-2`, …, `leaf-100`.
fn free_name(
    q: &GitQueue,
    top: &Path,
    dir: &Path,
    leaf: &str,
) -> Result<(String, PathBuf), String> {
    let refs = text(
        git(
            q,
            top,
            [
                "for-each-ref",
                "--format=%(refname:lstrip=2)",
                "refs/heads/",
            ],
        )?,
        "git for-each-ref failed",
    )?;
    let branches: HashSet<&str> = refs.lines().collect();
    let list = text(
        git(q, top, ["worktree", "list", "--porcelain", "-z"])?,
        "git worktree list failed",
    )?;
    let registered: HashSet<&Path> = list
        .split('\0')
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(Path::new)
        .collect();
    for n in 1..=MAX_SUFFIX {
        let branch = if n == 1 {
            leaf.to_owned()
        } else {
            format!("{leaf}-{n}")
        };
        let path = dir.join(&branch);
        if branches.contains(branch.as_str()) || registered.contains(path.as_path()) {
            continue;
        }
        // 다른 레포라도 같은 basename을 쓸 수 있다. 경로는 원자적으로 확보한다.
        match fs::create_dir(&path) {
            Ok(()) => return Ok((branch, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("cannot reserve worktree path: {e}")),
        }
    }
    Err(format!("name {leaf:?} is taken up to -{MAX_SUFFIX}"))
}

/// symlinks 항목을 주 워크트리(`top`)에서 새 워크트리(`wt`)로 심볼릭 링크한다(MK-174).
fn link_symlinks(top: &Path, wt: &Path, entries: &[String]) -> (Vec<String>, Vec<String>) {
    let mut linked = Vec::new();
    let mut warnings = Vec::new();
    if entries.is_empty() {
        return (linked, warnings);
    }
    let (Ok(top_real), Ok(wt_real)) = (fs::canonicalize(top), fs::canonicalize(wt)) else {
        warnings.push("symlink: cannot resolve repository paths".to_owned());
        return (linked, warnings);
    };
    for e in entries {
        let trimmed = e.trim().trim_end_matches('/');
        let rel = Path::new(trimmed);
        if trimmed.is_empty()
            || trimmed.contains('\\')
            || !rel.components().all(|c| {
                matches!(c, Component::Normal(n) if !n.as_bytes().eq_ignore_ascii_case(b".git"))
            })
        {
            warnings.push(format!(
                "symlink {e:?}: refused, path leaves the repository"
            ));
            continue;
        }
        match safe_parent(&top_real, rel, false) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                warnings.push(format!("symlink {e:?}: {err}"));
                continue;
            }
        }
        let dst = wt_real.join(rel);
        if fs::symlink_metadata(&dst).is_ok() {
            warnings.push(format!("symlink {e:?}: skipped, already exists"));
            continue;
        }
        let parent = rel.parent().unwrap_or(Path::new(""));
        if safe_parent(&wt_real, parent, true).is_err() {
            warnings.push(format!("symlink {e:?}: refused, destination has a symlink"));
            continue;
        }
        match std::os::unix::fs::symlink(top_real.join(rel), &dst) {
            Ok(()) => linked.push(trimmed.to_owned()),
            Err(err) => warnings.push(format!("symlink {e:?}: {err}")),
        }
    }
    (linked, warnings)
}

/// include 항목을 `top`에서 `wt`로 복사한다. (복사한 항목, 경고). 실패는 생성을 막지 않고 경고로만 남는다.
fn copy_includes(
    q: &GitQueue,
    top: &Path,
    wt: &Path,
    entries: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut copied = Vec::new();
    let mut warnings = Vec::new();
    if entries.is_empty() {
        return (copied, warnings);
    }
    let (Ok(top_real), Ok(wt_real)) = (fs::canonicalize(top), fs::canonicalize(wt)) else {
        warnings.push("include: cannot resolve repository paths".to_owned());
        return (copied, warnings);
    };
    // 경로 검사를 먼저: check-ignore는 심링크 너머 경로가 하나라도 있으면 통째로 실패한다.
    let mut safe = Vec::new();
    for e in entries {
        let rel = Path::new(e);
        if e.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(n) if !n.as_bytes().eq_ignore_ascii_case(b".git"))) {
            warnings.push(format!(
                "include {e:?}: refused, path leaves the repository"
            ));
            continue;
        }
        match safe_parent(&top_real, rel, false) {
            Ok(()) => safe.push(e.as_str()),
            // 없는 항목(.env를 아직 안 만든 경우 등)은 조용히 건너뛴다.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => warnings.push(format!("include {e:?}: {err}")),
        }
    }
    if safe.is_empty() {
        return (copied, warnings);
    }
    for e in safe {
        // -z는 --stdin과 함께만 쓸 수 있다. 항목별 종료 코드로 판정하면 줄바꿈·한글 경로도 안전하다.
        match git(q, top, ["check-ignore", "--quiet", "--", e]) {
            Ok(out) if out.status.success() => {}
            Ok(out) if out.status.code() == Some(1) => {
                warnings.push(format!("include {e:?}: skipped, not ignored by git (tracked files come from the checkout)"));
                continue;
            }
            other => {
                let error = other
                    .and_then(|out| text(out, "git check-ignore failed"))
                    .err()
                    .unwrap_or_default();
                warnings.push(format!("include {e:?}: {error}"));
                continue;
            }
        }
        let rel = Path::new(e);
        let dst = wt_real.join(rel);
        if fs::symlink_metadata(&dst).is_ok() {
            warnings.push(format!("include {e:?}: skipped, already exists"));
            continue;
        }
        let parent = rel.parent().unwrap_or(Path::new(""));
        // 체크아웃된 심링크 디렉터리를 따라 워크트리 밖에 쓰지 않게.
        if safe_parent(&wt_real, parent, true).is_err() {
            warnings.push(format!("include {e:?}: refused, destination has a symlink"));
            continue;
        }
        match copy_tree(&top_real.join(rel), &dst, e, &mut warnings) {
            Ok(()) => copied.push(e.to_owned()),
            Err(err) => warnings.push(format!("include {e:?}: {err}")),
        }
    }
    (copied, warnings)
}

/// 경로를 한 요소씩 검사한다. `create_dir_all` 뒤 검사하면 이미 밖에 디렉터리를 만들 수 있다.
fn safe_parent(root: &Path, rel: &Path, create: bool) -> io::Result<()> {
    let mut path = root.to_path_buf();
    for part in rel.components() {
        path.push(part);
        let metadata = match fs::symlink_metadata(&path) {
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&path)?;
                fs::symlink_metadata(&path)?
            }
            m => m?,
        };
        if metadata.is_symlink() {
            return Err(io::Error::other("refused, path has a symlink"));
        }
    }
    Ok(())
}

/// 파일·디렉터리를 복사한다. 심링크·특수 파일은 건너뛰고 경고한다(`name`은 경고에 쓸 레포 기준 경로).
fn copy_tree(src: &Path, dst: &Path, name: &str, warnings: &mut Vec<String>) -> io::Result<()> {
    let m = fs::symlink_metadata(src)?;
    if m.is_file() {
        compat::copy_file(src, dst)?;
    } else if m.is_dir() {
        fs::create_dir(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = format!("{name}/{}", entry.file_name().to_string_lossy());
            copy_tree(&entry.path(), &dst.join(entry.file_name()), &name, warnings)?;
        }
    } else {
        warnings.push(format!(
            "include {name:?}: refused, symlink or special file"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// Best-effort startup cleanup; preparation failures never prevent session recovery.
pub fn recover_preparation() {
    if let Err(error) = prepare::recover() {
        tracing::warn!(%error, "cannot recover speculative worktree");
    }
}
