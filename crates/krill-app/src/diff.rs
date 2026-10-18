//! MK-054 · OWE-578(UI-4) · OWE-585(MK-053 감시 연결): 오른쪽 변경 사항·diff 패널.
//!
//! - **변경 감시(OWE-585)**: 패널이 열려 있는 동안(`visible == true`) 100ms 주기로 `krilld`의
//!   `Request::GitWatch`(`FEAT_GIT_WATCH`)를 폴링해 `FSEvents` 이벤트(200ms 디바운스 + 3초 최소 간격)가
//!   감지한 변경 스냅샷을 자동으로 받아 온다. 패널을 접으면(`set_visible(false)`) 즉시 폴링을 멈추고
//!   `visible: false`를 보내 진행 중인 git 자식을 취소·회수한다(숨김 동안 git 자식 0).
//! - **오른쪽 사이드바 UI(OWE-578)**: 상단 탭 바(`변경 사항` + 파일 수 배지 + 새로고침 아이콘 + 접기 아이콘 `⌥⌘B`),
//!   비교 모드 바(`작업 트리` / `스테이징`), 상태 배지(`M`/`A`/`D`/`R`/`U`/`C` + 파일 아이콘)·축약 경로·`+/-` 줄 수
//!   가상 리스트(`uniform_list`), 비-git 폴더·깨끗한 작업 트리·파일 미선택 빈 상태, 선택 시 읽기 전용 diff.
//! - **잠정 결정 의존**: OWE-575(UI-1) `krill-app::ui` 토큰·`gpui-component`, 거부 시 되돌릴 파일:
//!   `crates/krill-app/src/diff.rs`, `crates/krill-app/src/shell.rs`.
//! - UI 배치·상태 코드 표기 참고: `.ref/orca/src/renderer/src/components/right-sidebar/`
//!   (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    Context, Entity, EventEmitter, FontWeight, Subscription, Task, UniformListScrollHandle, Window,
    div, prelude::*, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::diff::{Diff, DiffFile, DiffState};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::control::{FEAT_CONTROL, FEAT_GIT_CHANGES, FEAT_GIT_WATCH, Reply, Request};
use krill_proto::daemon;
use krill_proto::git::{ChangedFile, Changes, GitWatchReply, GitWatchRequest};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

use crate::hosts::HostTarget;

mod checks;
pub use checks::PanelTab;
mod comments;
pub use comments::{CommentSource, SendComments, input_bytes};
mod explorer;
pub use explorer::{DirEntry, EditKind, Explorer, TreeRow};
pub mod search;
pub use search::{
    DEFAULT_MAX_RESULTS, SearchFileResult, SearchMatch, SearchOptions, SearchResult, SearchRow,
    WorkspaceSearch, fit_highlighted_preview_for_cols, glob_matches_path, max_cols_for_row_width,
    parse_glob_patterns, search_worktree, search_worktree_fallback, split_highlighted_preview,
};

/// Retained patch bound, independent of the number of visible rows.
pub const MAX_PATCH_BYTES: usize = 4 * 1024 * 1024;
mod edit;
const TIMEOUT: Duration = Duration::from_secs(5);
const WATCH_POLL: Duration = Duration::from_millis(100);
const WATCH_POLL_INACTIVE: Duration = Duration::from_millis(500);

pub use crate::i18n::catalog::diff as text;

/// 패널 상단 접기 버튼을 눌렀을 때 셸에 알리는 이벤트(`⌥⌘B`와 동일 동작).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToggleCollapse;

/// 파일 행 더블클릭·메뉴의 '에디터로 열기'(MK-119). 셸이 가운데 영역에 파일 탭으로 연다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile(pub PathBuf);

/// 검색 결과 행 클릭 시 중앙 에디터로 파일을 열고 1-based `(line, column)`으로 커서를 이동시키는 이벤트(OWE-733).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFileAt {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Comparison {
    #[default]
    Worktree,
    Index,
}

/// `FSEvents` 변경 감시 소스(프로덕션은 UDS 제어 연결, 테스트는 인메모리·커스텀 큐 주입 가능).
pub trait Source: Send + Sync {
    fn watch(&self, req: &GitWatchRequest) -> io::Result<GitWatchReply>;
}

struct Conn {
    stream: UnixStream,
    features: u64,
}

struct Socket {
    target: HostTarget,
    conn: Mutex<Option<Conn>>,
}

impl Socket {
    fn new(target: impl Into<HostTarget>) -> Self {
        Self {
            target: target.into(),
            conn: Mutex::new(None),
        }
    }

    fn connect(&self) -> io::Result<Conn> {
        let hello = daemon::client_hello(ClientRole::App, "krill-app-diff");
        if self.target.allow_local_launch() {
            let mut stream = UnixStream::connect(self.target.socket())?;
            stream.set_read_timeout(Some(TIMEOUT))?;
            stream.set_write_timeout(Some(TIMEOUT))?;
            let (kind, body) = ipc::read_frame(&mut stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
            let server = ipc::ServerHello::decode(kind, &body)?;
            let negotiated = ipc::negotiate(&server, &hello).ok_or(io::ErrorKind::Unsupported)?;
            if negotiated.features & FEAT_CONTROL == 0 {
                return Err(io::ErrorKind::Unsupported.into());
            }
            stream.write_all(&hello.encode()?)?;
            Ok(Conn {
                stream,
                features: negotiated.features,
            })
        } else {
            let conn = self.target.connect(&hello).map_err(|e| match e {
                krill_proto::daemon::Error::Io(io_err) => io_err,
                other => io::Error::other(other.to_string()),
            })?;
            if conn.negotiated.features & FEAT_CONTROL == 0 {
                return Err(io::ErrorKind::Unsupported.into());
            }
            conn.stream.set_read_timeout(Some(TIMEOUT))?;
            conn.stream.set_write_timeout(Some(TIMEOUT))?;
            Ok(Conn {
                stream: conn.stream,
                features: conn.negotiated.features,
            })
        }
    }

    fn exchange(conn: &mut Conn, req: &GitWatchRequest) -> io::Result<GitWatchReply> {
        if conn.features & FEAT_GIT_WATCH != 0 {
            conn.stream
                .write_all(&Request::GitWatch(req.clone()).encode()?)?;
            let (kind, bytes) =
                ipc::read_frame(&mut conn.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
            return match Reply::decode(kind, &bytes)? {
                Reply::GitWatch(reply) => Ok(reply),
                Reply::Failed(f) => Err(io::Error::other(f.to_string())),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected git watch reply",
                )),
            };
        }
        if conn.features & FEAT_GIT_CHANGES == 0 {
            return Err(io::ErrorKind::Unsupported.into());
        }
        if !req.visible {
            return Ok(GitWatchReply::Hidden);
        }
        if req.since != 0 && !req.force {
            return Ok(GitWatchReply::Unchanged { pending: false });
        }
        conn.stream.write_all(
            &Request::GitChanges {
                repo: req.repo.clone(),
            }
            .encode()?,
        )?;
        let (kind, bytes) =
            ipc::read_frame(&mut conn.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        let generation = req.since.wrapping_add(1).max(1);
        match Reply::decode(kind, &bytes)? {
            Reply::GitChanges(changes) => Ok(GitWatchReply::Changed {
                generation,
                changes,
            }),
            Reply::GitChangesFailed(error) => Ok(GitWatchReply::Failed { generation, error }),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected git changes reply",
            )),
        }
    }
}

impl Source for Socket {
    fn watch(&self, req: &GitWatchRequest) -> io::Result<GitWatchReply> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| io::Error::other("diff socket mutex poisoned"))?;
        if guard.is_none() {
            *guard = Some(self.connect()?);
        }
        let result = match guard.as_mut() {
            Some(conn) => Self::exchange(conn, req),
            None => unreachable!(),
        };
        match &result {
            Ok(_) => self.target.record_outcome(true, None),
            Err(e) => {
                *guard = None;
                self.target.record_outcome(false, Some(e));
            }
        }
        result
    }
}

#[cfg(test)]
struct TestHubSource(Mutex<krilld::git::watch::Hub>);

#[cfg(test)]
impl Source for TestHubSource {
    fn watch(&self, req: &GitWatchRequest) -> io::Result<GitWatchReply> {
        let mut hub = self
            .0
            .lock()
            .map_err(|_| io::Error::other("test hub poisoned"))?;
        Ok(hub.handle(req))
    }
}

/// Query MK-053 without spawning a daemon: the shell owns daemon lifecycle.
pub fn changes(target: impl Into<HostTarget>, repo: &Path) -> io::Result<Changes> {
    let target = target.into();
    let hello = daemon::client_hello(ClientRole::App, "krill-app-diff");
    let conn = target.connect(&hello).map_err(io::Error::other)?;
    let need = FEAT_CONTROL | FEAT_GIT_CHANGES;
    if conn.negotiated.features & need != need {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "git changes unavailable",
        ));
    }
    let mut sock = conn.stream;
    sock.set_read_timeout(Some(TIMEOUT))?;
    sock.set_write_timeout(Some(TIMEOUT))?;
    sock.write_all(
        &Request::GitChanges {
            repo: repo.as_os_str().as_bytes().to_vec(),
        }
        .encode()?,
    )?;
    let (kind, bytes) = ipc::read_frame(&mut sock)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match Reply::decode(kind, &bytes)? {
        Reply::GitChanges(changes) => Ok(changes),
        Reply::GitChangesFailed(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected git changes reply",
        )),
    }
}

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// Bounded read, timeout and kill+wait on every exit. Never runs external diff/textconv.
/// The path comes from MK-053 and is passed literally, including non-UTF-8 names.
pub fn patch(repo: &Path, file: &ChangedFile, comparison: Comparison) -> io::Result<Vec<DiffFile>> {
    let path = Path::new(OsStr::from_bytes(&file.path));
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid git path",
        ));
    }
    if comparison == Comparison::Index && file.status == *b"??" {
        return Ok(Vec::new());
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "--literal-pathspecs",
            "-c",
            "core.quotePath=false",
            "-c",
            "diff.autoRefreshIndex=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--unified=3",
        ]);
    let untracked = file.status == *b"??";
    if untracked {
        std::fs::symlink_metadata(repo.join(path))?;
        cmd.args(["--no-index", "--", "/dev/null"]).arg(path);
    } else {
        if comparison == Comparison::Index {
            cmd.arg("--cached");
        }
        cmd.arg("--").arg(path);
        if let Some(old) = &file.old_path {
            cmd.arg(OsStr::from_bytes(old));
        }
    }
    let mut child = Reap(cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?);
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing git stdout"))?;
    // The reader stops and closes the pipe even when git emits an unbounded patch.
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(u64::try_from(MAX_PATCH_BYTES).unwrap_or(u64::MAX) + 1)
            .read_to_end(&mut bytes)?;
        Ok::<_, io::Error>(bytes)
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "git diff timed out",
                ));
            }
            Err(e) => break Err(e),
        }
    };
    // Close the writer before joining, including on timeout/error.
    child.0.kill().ok();
    child.0.wait().ok();
    let bytes = reader
        .join()
        .map_err(|_| io::Error::other("git reader failed"))??;
    if bytes.len() > MAX_PATCH_BYTES {
        return Err(io::Error::other("diff exceeds 4 MiB limit"));
    }
    let status = status?;
    if !(status.success() || untracked && status.code() == Some(1)) {
        return Err(io::Error::other("git diff failed"));
    }
    // AS-06: UTF-8이 아닌 텍스트(EUC-KR·CP949)도 통째로 거부하지 않는다. 패치 헤더는
    // ASCII라 구조 파싱이 되고, 디코드 실패 바이트만 U+FFFD로 보인다(손실 변환).
    // Binary 패치는 git이 메타데이터만 내보내(`Binary files ... differ`) 그대로 표시된다.
    let source = String::from_utf8_lossy(&bytes);
    DiffFile::parse(&source).map_err(io::Error::other)
}

/// Porcelain v2 XY 2바이트를 Orca 오른쪽 사이드바 상태 문자(`M`/`A`/`D`/`R`/`C`/`U`/`!`)로 바꾼다.
#[must_use]
pub fn status_code(status: [u8; 2], comparison: Comparison) -> &'static str {
    if status == *b"??" {
        return "U";
    }
    if matches!(status, [b'U', _] | [_, b'U'] | [b'A', b'A'] | [b'D', b'D']) {
        return "!";
    }
    let primary = match comparison {
        Comparison::Index if status[0] != b'.' && status[0] != b'?' => status[0],
        Comparison::Worktree if status[1] != b'.' => status[1],
        _ if status[0] != b'.' => status[0],
        _ => status[1],
    };
    match primary {
        b'A' => "A",
        b'D' => "D",
        b'R' => "R",
        b'C' => "C",
        b'?' => "U",
        _ => "M",
    }
}

/// 파일 경로를 `(파일명, 축약된 상위 경로·이름 변경 힌트)`로 나눈다.
#[must_use]
pub fn abbreviate_path(path: &[u8], old_path: Option<&[u8]>) -> (String, Option<String>) {
    let full = String::from_utf8_lossy(path);
    let mut parts: Vec<&str> = full.split('/').filter(|s| !s.is_empty()).collect();
    let basename = parts.pop().unwrap_or(full.as_ref()).to_owned();
    let dir = match parts.as_slice() {
        [] => None,
        [one] => Some((*one).to_owned()),
        [first, second] => Some(format!("{first}/{second}")),
        [first, middle, last] => Some(format!("{first}/{middle}/{last}")),
        [first, .., last] => Some(format!("{first}/…/{last}")),
    };
    let old_hint = old_path.map(|old| {
        let old_str = String::from_utf8_lossy(old);
        let old_base = old_str.rsplit('/').next().unwrap_or(old_str.as_ref());
        format!("← {old_base}")
    });
    let hint = match (dir, old_hint) {
        (Some(d), Some(o)) => Some(format!("{d} · {o}")),
        (Some(d), None) => Some(d),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };
    (basename, hint)
}

fn is_not_git_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("not a git repository") || lower.contains("invalid .git")
}

fn probe_file_status(repo: &Path, clean_rel: &str, staged: bool) -> [u8; 2] {
    let rel_path = Path::new(clean_rel);
    if !rel_path.as_os_str().is_empty()
        && rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && let Ok(out) = crate::pull_request::git_output(
            repo,
            &[
                "--literal-pathspecs",
                "-c",
                "diff.autoRefreshIndex=false",
                "status",
                "--porcelain=v2",
                "--untracked-files=all",
                "--",
                clean_rel,
            ],
        )
    {
        for line in out.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("? ") {
                return *b"??";
            }
            if (trimmed.starts_with("1 ") || trimmed.starts_with("2 ") || trimmed.starts_with("u "))
                && let Some(xy) = trimmed
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| <[u8; 2]>::try_from(s.as_bytes()).ok())
            {
                return xy;
            }
        }
    }
    let full = repo.join(clean_rel);
    if !full.exists() {
        if staged { *b"D." } else { *b".D" }
    } else if staged {
        *b"M."
    } else {
        *b".M"
    }
}

/// AS-26: 패치 실패 문구를 카탈로그 한글로 바꾼다. `patch`의 원문 영어는 그대로 둬
/// 로그·테스트(`4 MiB` 단언)가 깨지지 않게 한다.
fn patch_error_message(err: &io::Error) -> String {
    if err.kind() == io::ErrorKind::TimedOut {
        return text::DIFF_TIMED_OUT.get().to_owned();
    }
    let msg = err.to_string();
    if msg.contains("4 MiB") {
        text::DIFF_TOO_LARGE.get().to_owned()
    } else {
        text::DIFF_FAILED.format(&[("reason", &msg)])
    }
}

/// `Changed` 감시 결과를 어떻게 반영할지(AS-05).
/// 미추적 파일은 numstat가 없고 같은 줄 수 수정도 목록이 같아 `changes`가 그대로인데,
/// 선택 파일의 패치는 낡는다. 목록이 같아도 선택 파일이 남아 있으면 다시 선택해
/// 패치를 새로 받고(`Reselect`), 수동 새로고침도 같은 경로로 동작한다.
#[derive(Debug, PartialEq, Eq)]
enum WatchAction {
    /// 그대로 둔다(목록이 같고 선택 없음).
    Ignore,
    /// `idx` 파일을 다시 선택해 패치를 새로 받는다.
    Reselect(usize),
    /// 목록이 바뀌고 선택 파일이 없으면 선택을 비운다.
    Reset,
}

fn watch_action(
    loaded: bool,
    repo_error: Option<&String>,
    old: &Changes,
    new: &Changes,
    selected_path: Option<&[u8]>,
) -> WatchAction {
    let changed = !loaded || repo_error.is_some() || old != new;
    if let Some(path) = selected_path
        && let Some(idx) = new.files.iter().position(|f| f.path == path)
    {
        return WatchAction::Reselect(idx);
    }
    if changed {
        WatchAction::Reset
    } else {
        WatchAction::Ignore
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "loaded, visible, active, force_next, and single-click preview are independent orthogonal panel flags (OWE-849)"
)]
pub struct Panel {
    repo: PathBuf,
    tab: PanelTab,
    explorer: Explorer,
    open_file_preview: bool,
    search: WorkspaceSearch,
    pending_diff_path: Option<Vec<u8>>,
    changes: Changes,
    selected: Option<usize>,
    comparison: Comparison,
    state: Entity<DiffState>,
    scroll: UniformListScrollHandle,
    message: String,
    repo_error: Option<String>,
    loaded: bool,
    visible: bool,
    active: bool,
    watch_gen: u64,
    force_next: bool,
    patch_gen: u64,
    patch_task: Option<Task<()>>,
    watch_task: Option<Task<()>>,
    source: Option<Arc<dyn Source>>,
    _observe: Subscription,
    review: comments::Review,
    editing: edit::State,
    checks: checks::State,
    #[cfg(test)]
    pub(crate) rendered_rows: usize,
    #[cfg(test)]
    pub(crate) renders: usize,
}

impl EventEmitter<ToggleCollapse> for Panel {}
impl EventEmitter<SendComments> for Panel {}
impl EventEmitter<OpenFile> for Panel {}
impl EventEmitter<OpenFileAt> for Panel {}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("tab", &self.tab)
            .field("files", &self.changes.files.len())
            .field("selected", &self.selected)
            .field("comparison", &self.comparison)
            .field("visible", &self.visible)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl Panel {
    pub fn new(repo: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::unconnected(repo, cx);
        #[cfg(test)]
        {
            panel.connect_source(
                Arc::new(TestHubSource(
                    Mutex::new(krilld::git::watch::Hub::default()),
                )),
                cx,
            );
        }
        #[cfg(not(test))]
        if let Ok(target) = HostTarget::local_dev() {
            panel.connect(target, cx);
        }
        panel
    }

    /// 데몬 연결 전에 셸이 만드는 초기 상태(`connect`가 호출될 때 감시를 시작한다).
    pub fn unconnected(repo: PathBuf, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| DiffState::new([], cx).with_inline_unit(None));
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let explorer = Explorer::new(repo.clone());
        let search = WorkspaceSearch::new(repo.clone());
        Self {
            repo,
            tab: PanelTab::default(),
            explorer,
            open_file_preview: false,
            search,
            pending_diff_path: None,
            changes: Changes::default(),
            selected: None,
            comparison: Comparison::default(),
            state,
            scroll: UniformListScrollHandle::new(),
            message: text::EMPTY.get().into(),
            repo_error: None,
            loaded: false,
            visible: true,
            active: true,
            watch_gen: 0,
            force_next: false,
            patch_gen: 0,
            patch_task: None,
            watch_task: None,
            source: None,
            _observe: observe,
            review: comments::Review::default(),
            editing: edit::State::default(),
            checks: checks::State::default(),
            #[cfg(test)]
            rendered_rows: 0,
            #[cfg(test)]
            renders: 0,
        }
    }

    /// 직전 `OpenFile` 이벤트가 단일 클릭 미리보기(preview) 열기인지 반환한다.
    #[must_use]
    pub fn last_open_file_preview(&self) -> bool {
        self.open_file_preview
    }

    /// 직전 `OpenFile` 이벤트의 미리보기(preview) 플래그를 꺼내고 `false`로 초기화한다.
    pub fn take_open_file_preview(&mut self) -> bool {
        std::mem::replace(&mut self.open_file_preview, false)
    }

    pub fn connect(&mut self, target: impl Into<HostTarget>, cx: &mut Context<Self>) {
        let target = target.into();
        self.editing.connect(target.clone());
        self.refresh_branch_sync(cx);
        self.connect_source(Arc::new(Socket::new(target)), cx);
    }

    pub fn connect_source(&mut self, source: Arc<dyn Source>, cx: &mut Context<Self>) {
        self.source = Some(source);
        self.watch_gen = 0;
        if self.visible {
            self.start_watch(cx);
        }
    }

    /// 활성 워크트리·폴더를 바꾼다. 이전 폴더 감시는 숨김으로 전환해 git 자식을 남기지 않는다.
    pub fn set_repo(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        if self.repo == repo {
            return;
        }
        self.review = comments::Review::default();
        self.editing.reset_repo();
        self.explorer.set_repo(repo.clone(), false);
        if self.tab == PanelTab::Files {
            self.ensure_explorer_loaded_async(cx);
        }
        self.search
            .set_repo(repo.clone(), self.tab == PanelTab::Search);
        if self.tab == PanelTab::Search && !self.search.options.query.trim().is_empty() {
            self.spawn_async_search(cx);
        }
        self.pending_diff_path = None;
        self.checks.reset();
        let old_repo = std::mem::replace(&mut self.repo, repo);
        if self.visible
            && let Some(source) = self.source.clone()
        {
            let hide = GitWatchRequest {
                repo: old_repo.as_os_str().as_bytes().to_vec(),
                visible: false,
                since: 0,
                force: false,
            };
            cx.background_executor()
                .spawn(async move {
                    source.watch(&hide).ok();
                })
                .detach();
        }
        self.changes = Changes::default();
        self.selected = None;
        self.repo_error = None;
        self.loaded = false;
        self.watch_gen = 0;
        self.force_next = false;
        self.patch_gen = self.patch_gen.wrapping_add(1);
        self.patch_task = None;
        self.message = text::EMPTY.get().into();
        self.state.update(cx, |s, cx| s.set_files([], cx));
        if self.visible {
            self.start_watch(cx);
            if self.tab == PanelTab::Checks {
                self.refresh_checks(cx);
            }
        }
        cx.notify();
    }

    /// 오른쪽 패널 가시성을 바꾼다(OWE-585). 숨기면 즉시 감시를 끄고 진행 중인 git 자식을 회수한다.
    /// OWE-898: 숨길 때 진행 중인 패치·Checks 조회도 취소해 최대 4MiB 패치 버퍼와 검사 결과가
    /// 백그라운드에 머물지 않게 한다(표시 중인 패치 화면은 유지).
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.watch_gen = 0;
            self.start_watch(cx);
        } else {
            self.watch_task = None;
            // 진행 중인 패치·Checks는 무효화만 하고 표시 중인 내용은 둔다.
            self.patch_gen = self.patch_gen.wrapping_add(1);
            self.patch_task = None;
            self.checks.generation = self.checks.generation.wrapping_add(1);
            self.checks.task = None;
            self.checks.loading = false;
            if let Some(source) = self.source.clone() {
                let req = GitWatchRequest {
                    repo: self.repo.as_os_str().as_bytes().to_vec(),
                    visible: false,
                    since: self.watch_gen,
                    force: false,
                };
                cx.background_executor()
                    .spawn(async move {
                        source.watch(&req).ok();
                    })
                    .detach();
            }
        }
        cx.notify();
    }

    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 창 활성 여부를 반영한다(OWE-844). 비포커스 상태에서도 패널이 열려 있으면(`visible`)
    /// 완화된 주기로 `GitWatch`를 유지하고, 포커스를 되찾으면 즉시 빠른 감시 주기로 복귀한다.
    /// OWE-898: 이미 감시 루프가 살아 있으면 재시작하지 않는다. 재시작은 진행 중인 `watch`
    /// IPC(최신 `Changes` 보유)와 새 루프의 조회가 겹쳐 피크를 2배로 만들기 때문이다.
    /// 살아 있는 루프는 다음 반복에서 빠른 주기로 자동 복귀한다.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active && self.visible && self.watch_task.is_none() {
            self.start_watch(cx);
        }
        cx.notify();
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    fn watch_poll_interval(&self) -> Duration {
        if self.active {
            WATCH_POLL
        } else {
            WATCH_POLL_INACTIVE
        }
    }

    /// 수동 새로고침: 가시·포커스 여부와 무관하게 즉시 강제 조회를 트리거한다(OWE-844).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.tab == PanelTab::Checks {
            self.refresh_checks(cx);
        }
        if self.editing.busy {
            return;
        }
        #[cfg(not(test))]
        if self.source.is_none()
            && let Ok(target) = HostTarget::local_dev()
        {
            self.source = Some(Arc::new(Socket::new(target)));
        }
        self.force_next = true;
        self.start_watch(cx);
    }

    fn start_watch(&mut self, cx: &mut Context<Self>) {
        if (!self.visible && !self.force_next) || self.source.is_none() {
            return;
        }
        self.watch_task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            loop {
                let Ok(Some((source, req))) = this.update(cx, |this, _| {
                    if !this.visible && !this.force_next {
                        return None;
                    }
                    let source = this.source.clone()?;
                    let force = std::mem::replace(&mut this.force_next, false);
                    let req = GitWatchRequest {
                        repo: this.repo.as_os_str().as_bytes().to_vec(),
                        visible: this.visible || force,
                        since: this.watch_gen,
                        force,
                    };
                    Some((source, req))
                }) else {
                    return;
                };
                let reply = bg.spawn(async move { source.watch(&req) }).await;
                let next_delay = this.update(cx, |this, cx| {
                    this.apply_watch(reply, cx);
                    this.visible.then(|| this.watch_poll_interval())
                });
                let Ok(Some(delay)) = next_delay else {
                    return;
                };
                bg.timer(delay).await;
            }
        }));
    }

    fn apply_watch(&mut self, reply: io::Result<GitWatchReply>, cx: &mut Context<Self>) {
        match reply {
            Ok(GitWatchReply::Hidden | GitWatchReply::Unchanged { .. }) => {}
            Ok(GitWatchReply::Changed {
                generation,
                changes,
            }) => {
                self.watch_gen = generation;
                let pending_path = self.pending_diff_path.take();
                let selected_path = self
                    .selected
                    .and_then(|i| self.changes.files.get(i))
                    .map(|f| f.path.clone())
                    .or(pending_path);
                self.refresh_branch_sync(cx);
                match watch_action(
                    self.loaded,
                    self.repo_error.as_ref(),
                    &self.changes,
                    &changes,
                    selected_path.as_deref(),
                ) {
                    WatchAction::Ignore => {}
                    WatchAction::Reselect(idx) => {
                        self.loaded = true;
                        self.repo_error = None;
                        self.changes = changes;
                        if self
                            .changes
                            .files
                            .get(idx)
                            .is_some_and(|f| Self::matches_comparison(f, self.comparison))
                        {
                            self.select(idx, cx);
                        } else {
                            let had_selection = self.selected.take().is_some();
                            self.patch_gen = self.patch_gen.wrapping_add(1);
                            self.patch_task = None;
                            self.message = text::EMPTY.get().into();
                            if had_selection {
                                self.state.update(cx, |s, cx| s.set_files([], cx));
                            }
                            cx.notify();
                        }
                    }
                    WatchAction::Reset => {
                        self.loaded = true;
                        self.repo_error = None;
                        self.changes = changes;
                        let had_selection = self.selected.take().is_some();
                        self.message = text::EMPTY.get().into();
                        if had_selection {
                            self.state.update(cx, |s, cx| s.set_files([], cx));
                        }
                        cx.notify();
                    }
                }
            }
            Ok(GitWatchReply::Failed { generation, error }) => {
                self.watch_gen = generation;
                let changed = !self.loaded
                    || self.repo_error.as_deref() != Some(error.as_str())
                    || !self.changes.files.is_empty();
                self.loaded = true;
                if changed {
                    let had_selection = self.selected.take().is_some();
                    self.changes = Changes::default();
                    self.repo_error = Some(error);
                    self.message = text::EMPTY.get().into();
                    if had_selection {
                        self.state.update(cx, |s, cx| s.set_files([], cx));
                    }
                    cx.notify();
                }
            }
            Err(_) => {
                // 소켓 재연결 시 전체 스냅샷을 다시 받도록 세대 번호를 초기화한다.
                self.watch_gen = 0;
            }
        }
    }

    #[must_use]
    pub fn comparison(&self) -> Comparison {
        self.comparison
    }

    #[must_use]
    pub fn selected_path(&self) -> Option<String> {
        let idx = self.selected?;
        let file = self.changes.files.get(idx)?;
        Some(String::from_utf8_lossy(&file.path).into_owned())
    }

    /// 파일 칸 헤더에 보여줄 경로(OWE-849): 워크트리 상대 경로를 우선하고,
    /// 홈 아래면 `~`로 축약하며, 그 외에만 절대 경로를 그대로 보인다.
    #[must_use]
    pub fn display_path(&self, path: &Path) -> String {
        if let Ok(rel) = path.strip_prefix(&self.repo) {
            let text = rel.to_string_lossy().replace('\\', "/");
            if !text.is_empty() {
                return text;
            }
        }
        let home = crate::accounts::default_home();
        if let Ok(rel) = path.strip_prefix(&home) {
            let text = rel.to_string_lossy().replace('\\', "/");
            if !text.is_empty() {
                return format!("~/{text}");
            }
            return String::from("~");
        }
        path.display().to_string()
    }

    /// 지정된 워크트리 내 상대 경로 파일의 Diff 뷰를 우측 패널(`Changes` 탭)에 연다(`krill file diff`).
    pub fn open_diff_file(
        &mut self,
        repo: PathBuf,
        relative_path: &str,
        staged: bool,
        cx: &mut Context<Self>,
    ) {
        if self.repo != repo {
            self.set_repo(repo, cx);
        }
        self.tab = PanelTab::Changes;
        self.comparison = if staged {
            Comparison::Index
        } else {
            Comparison::Worktree
        };
        let clean_rel = relative_path.trim_start_matches("./");
        let target_bytes = clean_rel.as_bytes().to_vec();
        self.pending_diff_path = Some(target_bytes.clone());
        if let Some(idx) = self
            .changes
            .files
            .iter()
            .position(|f| f.path == target_bytes)
        {
            if let Some(existing) = self.changes.files.get_mut(idx)
                && existing.staged.is_none()
                && existing.unstaged.is_none()
            {
                existing.status = probe_file_status(&self.repo, clean_rel, staged);
            } else {
                self.pending_diff_path = None;
            }
            self.select(idx, cx);
        } else {
            let status = probe_file_status(&self.repo, clean_rel, staged);
            self.changes.files.push(ChangedFile {
                path: target_bytes,
                old_path: None,
                status,
                staged: None,
                unstaged: None,
            });
            let idx = self.changes.files.len() - 1;
            self.select(idx, cx);
            self.force_next = true;
            if self.source.is_some() {
                self.refresh(cx);
            }
        }
        cx.notify();
    }

    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(file) = self.changes.files.get(index).cloned() else {
            return;
        };
        self.patch_gen = self.patch_gen.wrapping_add(1);
        let generation = self.patch_gen;
        self.selected = Some(index);
        self.message = text::LOADING.get().into();
        self.state.update(cx, |s, cx| s.set_files([], cx));
        let (repo, comparison) = (self.repo.clone(), self.comparison);
        self.patch_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { patch(&repo, &file, comparison) })
                .await;
            this.update(cx, |this, cx| {
                if this.patch_gen != generation {
                    return;
                }
                match result {
                    Ok(files) => {
                        this.message = if files.is_empty() {
                            text::NO_PATCH.get().into()
                        } else {
                            String::new()
                        };
                        this.state.update(cx, |s, cx| s.set_files(files, cx));
                    }
                    Err(e) => this.message = patch_error_message(&e),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn matches_comparison(file: &ChangedFile, comparison: Comparison) -> bool {
        match comparison {
            Comparison::Worktree => {
                file.unstaged.is_some() || file.status == *b"??" || file.status[1] != b'.'
            }
            Comparison::Index => {
                file.staged.is_some() || (file.status[0] != b'.' && file.status[0] != b'?')
            }
        }
    }

    pub(crate) fn visible_file_indices(&self, comparison: Comparison) -> Vec<usize> {
        self.changes
            .files
            .iter()
            .enumerate()
            .filter_map(|(i, f)| Self::matches_comparison(f, comparison).then_some(i))
            .collect()
    }

    fn count_for(&self, comparison: Comparison) -> usize {
        self.changes
            .files
            .iter()
            .filter(|f| Self::matches_comparison(f, comparison))
            .count()
    }

    fn render_tab_button(
        tab: PanelTab,
        active: bool,
        p: &crate::ui::Palette,
    ) -> gpui::Stateful<gpui::Div> {
        let selector = tab.selector();
        let active_selector = tab.active_selector();
        let tooltip_selector = tab.tooltip_selector();
        let tip = tab.tooltip();
        div()
            .id(selector)
            .debug_selector(move || selector.into())
            .role(gpui::Role::Tab)
            .aria_label(tip)
            .relative()
            .h(px(24.))
            .min_w(px(26.))
            .px_1p5()
            .flex_shrink_0()
            .rounded(crate::ui::RADIUS_XS)
            .cursor_pointer()
            .when(active, |d| d.bg(p.sidebar_accent))
            .hover(|d| d.bg(p.sidebar_accent))
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(Icon::new(tab.icon()).size(px(14.)).text_color(if active {
                p.sidebar_accent_foreground
            } else {
                p.muted_foreground
            }))
            .when(active, |d| {
                d.child(
                    div()
                        .id(active_selector)
                        .debug_selector(move || active_selector.into())
                        .absolute()
                        .bottom_0()
                        .left_1()
                        .right_1()
                        .h(px(2.))
                        .rounded_full()
                        .bg(p.sidebar_foreground),
                )
            })
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::element(move |_, _| {
                    div()
                        .debug_selector(move || tooltip_selector.into())
                        .child(tip)
                })
                .build(window, cx)
            })
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = crate::ui::palette(cx);
        let file_count = self.changes.files.len();
        let show_badge = self.repo_error.is_none();
        let changes_active = self.tab == PanelTab::Changes;
        let files_active = self.tab == PanelTab::Files;
        let search_active = self.tab == PanelTab::Search;
        let checks_active = self.tab == PanelTab::Checks;
        let checks_badge = self.checks_badge_count();
        let checks_dot = self.checks_status_color(p);
        div()
            .h(crate::ui::TAB_BAR_HEIGHT)
            .px_1p5()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.sidebar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .min_w_0()
                    .overflow_x_hidden()
                    .child(
                        Self::render_tab_button(PanelTab::Changes, changes_active, p)
                            .when(show_badge, |d| {
                                d.child(
                                    div()
                                        .id("diff-file-count")
                                        .debug_selector(|| "diff-file-count".into())
                                        .px_1()
                                        .rounded(crate::ui::RADIUS_XS)
                                        .bg(p.muted)
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_2XS)
                                        .text_color(if changes_active {
                                            p.sidebar_accent_foreground
                                        } else {
                                            p.muted_foreground
                                        })
                                        .child(file_count.to_string()),
                                )
                            })
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_tab(PanelTab::Changes, cx)),
                            ),
                    )
                    .child(
                        Self::render_tab_button(PanelTab::Files, files_active, p).on_click(
                            cx.listener(|this, _, _, cx| this.set_tab(PanelTab::Files, cx)),
                        ),
                    )
                    .child(
                        Self::render_tab_button(PanelTab::Search, search_active, p).on_click(
                            cx.listener(|this, _, window, cx| this.show_search(window, cx)),
                        ),
                    )
                    .child(
                        Self::render_tab_button(PanelTab::Checks, checks_active, p)
                            .when_some(checks_dot, |d, dot_color| {
                                d.child(
                                    div()
                                        .id("diff-checks-dot")
                                        .debug_selector(|| "diff-checks-dot".into())
                                        .size(crate::ui::STATE_DOT)
                                        .rounded_full()
                                        .bg(dot_color),
                                )
                            })
                            .when_some(checks_badge, |d, count| {
                                d.child(
                                    div()
                                        .id("diff-checks-count")
                                        .debug_selector(|| "diff-checks-count".into())
                                        .px_1()
                                        .rounded(crate::ui::RADIUS_XS)
                                        .bg(p.muted)
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_2XS)
                                        .text_color(if count > 0 {
                                            p.destructive
                                        } else if checks_active {
                                            p.sidebar_accent_foreground
                                        } else {
                                            p.muted_foreground
                                        })
                                        .child(count.to_string()),
                                )
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_tab(PanelTab::Checks, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .flex_shrink_0()
                    .child(
                        div().debug_selector(|| "diff-refresh".into()).child(
                            Button::new("diff-refresh")
                                .ghost()
                                .xsmall()
                                .icon(IconName::RefreshCw)
                                .tooltip(text::REFRESH.get())
                                .accessibility_label(text::REFRESH.get())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.tab == PanelTab::Files {
                                        this.refresh_explorer_async(cx);
                                    } else if this.tab == PanelTab::Search {
                                        this.spawn_async_search(cx);
                                    } else {
                                        this.refresh(cx);
                                    }
                                })),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "diff-collapse".into()).child(
                            Button::new("diff-collapse")
                                .ghost()
                                .xsmall()
                                .icon(IconName::PanelRightClose)
                                .tooltip(text::COLLAPSE.get())
                                .accessibility_label(text::COLLAPSE.get())
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(ToggleCollapse))),
                        ),
                    ),
            )
    }

    fn render_comparison_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = crate::ui::palette(cx);
        div()
            .px_2()
            .py_1()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(p.sidebar_border)
            .children(
                [Comparison::Worktree, Comparison::Index]
                    .into_iter()
                    .enumerate()
                    .map(|(i, comparison)| {
                        let active = self.comparison == comparison;
                        let count = self.count_for(comparison);
                        let label = if comparison == Comparison::Worktree {
                            text::WORKTREE.get()
                        } else {
                            text::INDEX.get()
                        };
                        div()
                            .id(("comparison", i))
                            .debug_selector(move || format!("comparison-{i}"))
                            .flex_1()
                            .h(px(24.))
                            .px_2()
                            .rounded(crate::ui::RADIUS_XS)
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_1p5()
                            .text_size(crate::ui::TEXT_XS)
                            .font_weight(if active {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(if active {
                                p.sidebar_accent_foreground
                            } else {
                                p.muted_foreground
                            })
                            .when(active, |d| d.bg(p.sidebar_accent))
                            .hover(|d| {
                                d.bg(p.sidebar_accent)
                                    .text_color(p.sidebar_accent_foreground)
                            })
                            .child(label)
                            .child(
                                div()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(count.to_string()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.comparison != comparison {
                                    this.comparison = comparison;
                                    if let Some(index) = this.selected {
                                        if this.changes.files.get(index).is_some_and(|f| {
                                            Self::matches_comparison(f, comparison)
                                        }) {
                                            this.select(index, cx);
                                        } else {
                                            this.selected = None;
                                            this.patch_gen = this.patch_gen.wrapping_add(1);
                                            this.patch_task = None;
                                            this.message = text::EMPTY.get().into();
                                            this.state.update(cx, |s, cx| s.set_files([], cx));
                                            cx.notify();
                                        }
                                    } else {
                                        cx.notify();
                                    }
                                }
                            }))
                    }),
            )
    }

    fn render_file_row(
        &self,
        i: usize,
        file: &ChangedFile,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let p = crate::ui::palette(cx);
        let stats = match self.comparison {
            Comparison::Index => file.staged.or(file.unstaged),
            Comparison::Worktree => file.unstaged.or(file.staged),
        };
        let status_color = crate::ui::git_status_color(file.status, cx);
        let code = status_code(file.status, self.comparison);
        let (basename, dir_hint) = abbreviate_path(&file.path, file.old_path.as_deref());
        let selected = self.selected == Some(i);
        let full = self.repo.join(OsStr::from_bytes(&file.path));
        let open = full.clone();
        let weak = cx.weak_entity();
        let comparison = self.comparison;

        div()
            .id(("changed-file", i))
            .debug_selector(move || format!("changed-file-{i}"))
            .h(px(26.))
            .px_2()
            .mx_1()
            .rounded(crate::ui::RADIUS_XS)
            .flex()
            .items_center()
            .gap_1p5()
            .overflow_hidden()
            .cursor_pointer()
            .text_size(crate::ui::TEXT_XS)
            .text_color(if selected {
                p.sidebar_accent_foreground
            } else {
                p.sidebar_foreground
            })
            .when(selected, |d| d.bg(p.sidebar_accent))
            .hover(|d| {
                d.bg(p.sidebar_accent)
                    .text_color(p.sidebar_accent_foreground)
            })
            .child(
                Icon::new(IconName::FileText)
                    .size(px(13.))
                    .text_color(status_color),
            )
            .child(
                div()
                    .w(px(12.))
                    .flex_none()
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_2XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(status_color)
                    .child(code),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .child(basename),
                    )
                    .children(dir_hint.map(|hint| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(hint)
                    })),
            )
            .children(stats.map(|s| {
                match (s.added, s.deleted) {
                    (Some(added), Some(deleted)) => div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_2XS)
                        .when(added > 0 || deleted == 0, |d| {
                            d.child(div().text_color(p.success).child(format!("+{added}")))
                        })
                        .when(deleted > 0 || added == 0, |d| {
                            d.child(div().text_color(p.destructive).child(format!("−{deleted}")))
                        }),
                    _ => div()
                        .flex_none()
                        .px_1()
                        .rounded(crate::ui::RADIUS_XS)
                        .bg(p.muted)
                        .text_size(crate::ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(text::BINARY.get()),
                }
            }))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .ml_1()
                    .when(comparison == Comparison::Worktree, |d| {
                        d.child(self.discard_button(i, cx)).child(self.edit_button(
                            i,
                            Comparison::Worktree,
                            cx,
                        ))
                    })
                    .when(comparison == Comparison::Index, |d| {
                        d.child(self.edit_button(i, Comparison::Index, cx))
                    }),
            )
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    this.open_file_preview = false;
                    cx.emit(OpenFile(open.clone()));
                } else {
                    this.select(i, cx);
                }
            }))
            .context_menu(move |menu, _, _| {
                let (weak, full) = (weak.clone(), full.clone());
                let w_stage = weak.clone();
                let w_discard = weak.clone();
                let w_unstage = weak.clone();
                let menu = menu.item(
                    gpui_component::menu::PopupMenuItem::new(text::OPEN_IN_EDITOR.get()).on_click(
                        move |_, _, cx| {
                            weak.update(cx, |this, cx| {
                                this.open_file_preview = false;
                                cx.emit(OpenFile(full.clone()));
                            })
                            .ok();
                        },
                    ),
                );
                if comparison == Comparison::Worktree {
                    menu.separator()
                        .item(
                            gpui_component::menu::PopupMenuItem::new(
                                crate::i18n::catalog::diff_edit::STAGE.get(),
                            )
                            .icon(IconName::Plus)
                            .on_click(move |_, _, cx| {
                                w_stage
                                    .update(cx, |this, cx| {
                                        this.edit_file(i, Comparison::Worktree, cx);
                                    })
                                    .ok();
                            }),
                        )
                        .item(
                            gpui_component::menu::PopupMenuItem::new(
                                crate::i18n::catalog::diff_edit::DISCARD.get(),
                            )
                            .icon(Icon::default().data(crate::ui::icons::ROTATE_CCW_SVG))
                            .on_click(move |_, window, cx| {
                                w_discard
                                    .update(cx, |this, cx| {
                                        this.confirm_discard_file(i, window, cx);
                                    })
                                    .ok();
                            }),
                        )
                } else {
                    menu.separator().item(
                        gpui_component::menu::PopupMenuItem::new(
                            crate::i18n::catalog::diff_edit::UNSTAGE.get(),
                        )
                        .icon(IconName::Minus)
                        .on_click(move |_, _, cx| {
                            w_unstage
                                .update(cx, |this, cx| {
                                    this.edit_file(i, Comparison::Index, cx);
                                })
                                .ok();
                        }),
                    )
                }
            })
            .into_any_element()
    }

    fn render_empty_state(
        id: &'static str,
        icon: IconName,
        title: &str,
        desc: &str,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex_1()
            .min_h_0()
            .px_4()
            .py_6()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(p.sidebar)
            .child(Icon::new(icon).size(px(22.)).text_color(p.muted_foreground))
            .child(
                div()
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.sidebar_foreground)
                    .child(title.to_owned()),
            )
            .when(!desc.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(desc.to_owned()),
                )
            })
    }
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let p = crate::ui::palette(cx);
        let top_bar = self.render_top_bar(cx);

        if self.tab == PanelTab::Files {
            let explorer_pane = self.render_explorer_pane(window, cx);
            return div()
                .size_full()
                .flex()
                .flex_col()
                .min_w_0()
                .min_h_0()
                .font_family(crate::ui::FONT_UI)
                .bg(p.sidebar)
                .text_color(p.sidebar_foreground)
                .child(top_bar)
                .child(explorer_pane);
        }

        if self.tab == PanelTab::Search {
            let search_pane = self.render_search_pane(window, cx);
            return div()
                .size_full()
                .flex()
                .flex_col()
                .min_w_0()
                .min_h_0()
                .font_family(crate::ui::FONT_UI)
                .bg(p.sidebar)
                .text_color(p.sidebar_foreground)
                .child(top_bar)
                .child(search_pane);
        }

        if self.tab == PanelTab::Checks {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .min_w_0()
                .min_h_0()
                .font_family(crate::ui::FONT_UI)
                .bg(p.sidebar)
                .text_color(p.sidebar_foreground)
                .child(top_bar)
                .child(self.render_checks_tab(window, cx))
                .children(
                    self.editing
                        .pr_dialog
                        .as_ref()
                        .map(|(dialog, _)| dialog.clone()),
                );
        }

        if let Some(err) = &self.repo_error {
            let body = if is_not_git_error(err) {
                Self::render_empty_state(
                    "diff-empty-not-git",
                    IconName::Folder,
                    text::NOT_GIT_TITLE.get(),
                    text::NOT_GIT_DESC.get(),
                    cx,
                )
            } else {
                Self::render_empty_state("diff-empty-error", IconName::CircleAlert, err, "", cx)
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .min_w_0()
                .min_h_0()
                .font_family(crate::ui::FONT_UI)
                .bg(p.sidebar)
                .text_color(p.sidebar_foreground)
                .child(top_bar)
                .child(body)
                .when(
                    self.selected.is_some() || self.review.has_active_ui(),
                    |d| d.child(self.render_comments(window, cx)),
                );
        }

        let comparison_bar = self.render_comparison_bar(cx);
        let editor = self.render_editor(window, cx);
        if self.changes.files.is_empty() {
            let empty_clean = Self::render_empty_state(
                "diff-empty-clean",
                IconName::CircleCheck,
                text::CLEAN_TITLE.get(),
                text::CLEAN_DESC.get(),
                cx,
            );
            return div()
                .size_full()
                .flex()
                .flex_col()
                .min_w_0()
                .min_h_0()
                .font_family(crate::ui::FONT_UI)
                .bg(p.sidebar)
                .text_color(p.sidebar_foreground)
                .child(top_bar)
                .child(comparison_bar)
                .child(editor)
                .child(empty_clean)
                .when(
                    self.selected.is_some() || self.review.has_active_ui(),
                    |d| d.child(self.render_comments(window, cx)),
                );
        }

        let selected_file = self
            .selected
            .and_then(|idx| self.changes.files.get(idx).cloned());

        let visible_indices = Arc::new(self.visible_file_indices(self.comparison));
        let has_visible_files = !visible_indices.is_empty();
        let list_indices = Arc::clone(&visible_indices);
        let list = uniform_list(
            "changed-files",
            visible_indices.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                #[cfg(test)]
                {
                    this.rendered_rows += range.len();
                }
                range
                    .filter_map(|row_ix| {
                        let file_idx = *list_indices.get(row_ix)?;
                        this.changes
                            .files
                            .get(file_idx)
                            .cloned()
                            .map(|file| this.render_file_row(file_idx, &file, cx))
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .h(px(176.))
        .py_1()
        .flex_none();

        let weak = cx.entity().downgrade();
        let bottom = match selected_file {
            None => Self::render_empty_state(
                "diff-empty-unselected",
                IconName::FileText,
                text::EMPTY.get(),
                "",
                cx,
            )
            .into_any_element(),
            Some(file) => {
                let code = status_code(file.status, self.comparison);
                let color = crate::ui::git_status_color(file.status, cx);
                let full_path = String::from_utf8_lossy(&file.path).into_owned();
                let header = div()
                    .h(px(26.))
                    .px_2p5()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .bg(p.sidebar)
                    .border_b_1()
                    .border_color(p.sidebar_border)
                    .child(
                        div()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(code),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.sidebar_foreground)
                            .child(full_path),
                    );
                let content = if self.message.is_empty() {
                    div()
                        .id("diff-viewer")
                        .debug_selector(|| "diff-viewer".into())
                        .flex_1()
                        .min_h_0()
                        .child(Diff::new(&self.state).size_full().on_add_annotation(
                            move |range, window, cx| {
                                weak.update(cx, |this, cx| {
                                    this.begin_comment(range.clone(), window, cx);
                                })
                                .ok();
                            },
                        ))
                        .into_any_element()
                } else {
                    Self::render_empty_state(
                        "diff-empty-patch",
                        IconName::File,
                        &crate::i18n::cached_notice(&self.message),
                        "",
                        cx,
                    )
                    .into_any_element()
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(p.sidebar_border)
                    .child(header)
                    .child(content)
                    .into_any_element()
            }
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .font_family(crate::ui::FONT_UI)
            .bg(p.sidebar)
            .text_color(p.sidebar_foreground)
            .child(top_bar)
            .child(comparison_bar)
            .when(has_visible_files, |d| d.child(list))
            .child(editor)
            .when(self.changes.limited, |d| {
                d.child(
                    div()
                        .px_3()
                        .py_1()
                        .flex_none()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.warning)
                        .child(text::LIMITED.get()),
                )
            })
            .child(bottom)
            .when(
                self.selected.is_some() || self.review.has_active_ui(),
                |d| d.child(self.render_comments(window, cx)),
            )
    }
}

#[cfg(test)]
use krill_proto::daemon::Paths;

#[cfg(test)]
mod tests;
