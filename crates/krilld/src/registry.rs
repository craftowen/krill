//! 세션 레지스트리(OWE-486): krilld가 들고 있는 세션과 holder 링크. 연결 하나를 끝까지 처리하는 [`Registry::serve`]가
//! 첫 요청으로 화면 스트림([`crate::attach`], MK-023)과 제어 요청([`krill_proto::control`])을 가른다.
//!
//! - 생성: holder `Spawn` → 받은 master fd로 [`Session`]을 띄우고 세그먼트 기록을 붙인다(MK-024). 프로그램은 로그인
//!   셸·지정 실행 파일(MK-034) 또는 에이전트 프로필(MK-037).
//! - 닫기: 돌고 있으면 holder v3 `Terminate`(v2 이하는 프로세스 그룹에 SIGHUP `Kill`), `Exited`가 오면 `Release`하고
//!   지운다. 이미 끝났으면 바로 `Release`.
//! - 권한: 연결마다 [`Peer`]로 계보를 판정한다(MK-036). 세션 안 피어도 외부 셸처럼 모든 세션을 다루고(Orca 소유자
//!   모델), 계보를 확인하지 못한 피어만 거부하고 감사 로그에 남긴다.
//! - krilld 재기동: holder가 다시 넘긴 세션([`Kept`])을 같은 id로 다시 띄운다. seq는 디스크 세그먼트 끝에서 이어진다.
//!   holder를 잃은 이전 krilld가 넘긴 세션도 같은 길로 들어온다([`crate::rejoin`]).
//! - 에이전트 상태([`crate::status`], MK-039): hook 연결(첫 프레임이 hook 이벤트)과 spool을 세션에 붙여 반영한다.
//!   세션은 피어가 세션 후손이면 그 세션, 외부(전체 범위)·spool이면 이벤트의 `ppid` 계보로 정한다.

use crate::os::AsFd;
#[cfg(all(test, unix))]
use crate::os::OwnedFd;
use krill_proto::os::net::UnixStream;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::holder::link::Link;
use krill_proto::SessionId;
use krill_proto::control::{
    self, Create, FEAT_CONTROL, Failure, Info, Program, Reply, Request, SessionAccount,
    SessionStatus,
};
use krill_proto::daemon::{FEAT_HOLDER_WATCH, HolderStatus};
use krill_proto::holder::{FromHolder, ToHolder, WinSize};
use krill_proto::hook::{self, FEAT_HOOK, HookEvent};
use krill_proto::ipc::{self, ServerHello};
use krill_proto::resources::{Action, FEAT_RESOURCES};
use krill_proto::roles::{self, Ack, FEAT_ROLES, Role};
use krill_proto::screen::{self, Attach, FEAT_SCREEN, Msg};

use crate::appearance::Appearance;
use crate::attach::{self, ATTACH_TIMEOUT, Tap};
use crate::holder::{Holder, Kept, Master};
use crate::scope::{self, Audit, Peer, Roots, Scope};
use crate::session::{Session, lock};
use crate::shell::{Launch, Shell};
use crate::status::{self, Statuses};
use crate::store::{Limits, Store};

mod account_switch;
pub mod admission;
mod agent_history;
mod closed_sessions;
mod hibernation;
mod hook_identity;
mod orch_cache;
pub(crate) mod persistence;
pub(crate) mod prompt_delivery;
mod schedule;
mod session_ids;
mod sidebar;
#[cfg(all(test, unix))]
mod stale_sessions_tests;
pub mod usage_tick;

struct RemoteFsConnGuard(u64);

impl Drop for RemoteFsConnGuard {
    fn drop(&mut self) {
        crate::remote_fs::cleanup_connection(self.0);
    }
}

/// `Terminate`가 없는 holder(v2 이하)에 닫기를 보내는 방법.
const TERMINATE_SINCE: u16 = 3;
/// v3 미만 holder에 보내는 `Kill` 신호.
#[cfg(unix)]
const SIGHUP: i32 = libc::SIGHUP;
/// holder 프로토콜 값(POSIX 번호). Windows holder는 아직 없다(Phase 3).
#[cfg(windows)]
const SIGHUP: i32 = 1;
/// 감시 스레드에서 보존 기간·전역 디스크 상한을 확인하는 간격.
const GC_INTERVAL: Duration = Duration::from_secs(60);
/// holder가 `Session`에 크기를 싣지 않아 PTY에서 읽지 못했을 때.
const FALLBACK_SIZE: WinSize = WinSize { cols: 80, rows: 24 };
/// 경고 파일에 남기는 최대 줄 수. 넘으면 오래된 것부터 버린다.
const RECORD_WARNINGS_MAX_LINES: usize = 64;

/// 기록 실패 한 줄을 상태 디렉터리에 남긴다(없으면 만든다). 쓰기 실패는 무시한다(거부가 판정을 바꾸지 않듯이
/// 경고 실패도 동작을 바꾸지 않는다). [`krill_proto::diag::record_warnings`]로 읽어 `krill doctor`가 보인다.
fn note_record_warning(dir: &Path, msg: &str) {
    use krill_proto::diag::RECORD_WARNINGS_FILE;
    use std::fmt::Write as _;
    let path = dir.join(RECORD_WARNINGS_FILE);
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    let secs = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut last = String::new();
    write!(last, "{secs} {msg}").ok();
    lines.push(last);
    let skip = lines.len().saturating_sub(RECORD_WARNINGS_MAX_LINES);
    let kept: Vec<&str> = lines.iter().skip(skip).map(String::as_str).collect();
    std::fs::write(
        &path,
        kept.join(
            "
",
        ) + "
",
    )
    .ok();
}

/// 이번 기동에서 기록 저장소를 여는 데 성공했으면 지난 경고를 지운다. 이후 실패는 다시 쌓인다.
fn clear_record_warnings(dir: &Path) {
    use krill_proto::diag::RECORD_WARNINGS_FILE;
    std::fs::remove_file(dir.join(RECORD_WARNINGS_FILE)).ok();
}

struct Entry {
    session: Session,
    pid: u32,
    cwd: PathBuf,
    /// 루트가 에이전트 셸 복귀 래퍼(`agent::PREFLIGHT`)다. 에이전트 종료를 루트 종료 대신 래퍼로 판정한다.
    wrapped: bool,
}

/// argv가 에이전트 셸 복귀 래퍼(`<shell> -l -i -c PREFLIGHT mk-agent ...`)인가.
pub(crate) fn is_wrapper_argv<S: AsRef<[u8]>>(argv: &[S]) -> bool {
    #[cfg(unix)]
    return argv
        .get(5)
        .is_some_and(|a| a.as_ref() == crate::agent::WRAPPER_ARG0.as_bytes())
        && argv.get(3).is_some_and(|a| a.as_ref() == b"-c");
    #[cfg(not(unix))]
    {
        let _ = argv;
        false
    }
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<SessionId, Entry>,
    /// Holder still owns these masters; engine startup failure is not process exit.
    unregistered_kept: HashSet<SessionId>,
    /// 끝난 세션의 상태. `Spawned` 답보다 `Exited`가 먼저 처리될 수 있어 세션과 따로 둔다.
    exited: HashMap<SessionId, i32>,
    /// 닫기를 요청받은 세션. `Exited`가 오면 바로 지운다.
    closing: HashSet<SessionId>,
    /// 명시적 `Close`로 닫힌 세션. 재부팅 소실 세션과 구분해 `Cold`·`Resume` 부활을 막는다.
    closed: HashSet<SessionId>,
    /// 이미 `Resume`으로 새 세션에 인계된 옛 세션. 중복 `Resume`이 빈 로그인 셸을 또 띄우지 않게 한다.
    resumed: HashSet<SessionId>,
    roots: Roots,
}

type IdleMailScope = (u64, String, Option<String>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum MailEpochState {
    InFlight(u64),
    Delivered(u64),
}

#[derive(Debug)]
pub(crate) struct MobileSessionRow {
    pub info: Info,
    pub cwd: Vec<u8>,
    pub status: control::AgentStatus,
}

#[derive(Debug)]
pub(crate) struct MobileDashboardInputs {
    pub sessions: Vec<MobileSessionRow>,
    pub repositories: Vec<control::RepositoryTrees>,
}

/// krilld 전체에 하나.
pub struct Registry {
    holder: Option<Arc<Holder>>,
    watch: crate::watch::Watch,
    store: Option<Arc<Store>>,
    look: Arc<Appearance>,
    audit: Audit,
    /// 사용자 에이전트 프로필(`agents.toml`).
    agents: crate::agent::control::Control,
    status: Statuses,
    /// Serializes status/launch commits with deletion, outside the registry state lock.
    hook_commit: Mutex<()>,
    /// `krill hook`의 spool 파일.
    spool: Option<PathBuf>,
    state: Mutex<State>,
    closed_ledger: closed_sessions::ClosedLedger,
    ids: Mutex<session_ids::SessionIds>,
    sidebar: Arc<Mutex<sidebar::Cache>>,
    hibernation: Mutex<hibernation::Clock>,
    schedule_minute: Mutex<Option<u64>>,
    git_watch: Mutex<crate::git::watch::Hub>,
    /// 업그레이드를 시작했다(MK-062). 잠금이 동시 요청을 줄 세운다.
    upgrading: Mutex<bool>,
    /// 진행 중인 `Resume`의 `old` id. 같은 세션의 동시 재개는 먼저 잡은 쪽만 진행한다(K-25).
    resuming: Mutex<HashSet<SessionId>>,
    /// Serializes concurrent `AgentHistory::Resume`/`ResumeVault` calls for the same conversation.
    resuming_conversations: (
        Mutex<HashSet<(krill_proto::agent_history::VaultAgent, String)>>,
        std::sync::Condvar,
    ),
    account_work: Arc<crate::usage::accounts::switch_work::SwitchWork>,
    account_handoffs: Mutex<account_switch::ledger::HandoffLedger>,
    account_recovered: std::sync::atomic::AtomicBool,
    account_processing: std::sync::atomic::AtomicBool,
    account_replacements: Mutex<std::collections::BTreeSet<SessionId>>,
    account_switch_opt_out: Mutex<std::collections::BTreeSet<SessionId>>,
    /// main에 프로세스 종료를 알린다(업그레이드 답을 보낸 뒤).
    exit: OnceLock<Sender<()>>,
    /// 디스크 사용량을 잴 상태 디렉터리(MK-071).
    dir: Option<PathBuf>,
    last_gc: Mutex<Instant>,
    /// 기동 GC는 첫 감시 바퀴(별도 스레드)에서 돈다. 연결 수락 전 경로에서 세그먼트 수만 개를 stat하지 않는다.
    startup_gc: std::sync::atomic::AtomicBool,
    app_opened: OnceLock<()>,
    mobile: OnceLock<Arc<crate::mobile::gateway::Gateway>>,
    self_ref: OnceLock<std::sync::Weak<Self>>,
    bots: OnceLock<Arc<crate::bots::BotsManager>>,
    bots_boot_started: OnceLock<()>,
    voice: OnceLock<Arc<crate::voice::VoiceService>>,
    orch_signal: (Mutex<u64>, std::sync::Condvar),
    orch_cache: Mutex<orch_cache::Cache>,
    orch_mail_notified_done: Mutex<BTreeMap<IdleMailScope, MailEpochState>>,
    /// Serializes concurrent worker launches for the same `dispatch_id` without holding
    /// the global `orchestration.json` flock across holder spawn.
    orch_launching: (Mutex<HashSet<String>>, std::sync::Condvar),
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("holder", &self.holder)
            .finish_non_exhaustive()
    }
}

fn kept_session_size(
    live_pty: Option<WinSize>,
    reported: Option<WinSize>,
    stored: Option<WinSize>,
) -> (WinSize, bool) {
    let actual = live_pty.or(reported);
    let size = crate::term::clamp_size(actual.or(stored).unwrap_or(FALLBACK_SIZE));
    (size, actual != Some(size))
}

impl Registry {
    /// `dir`(상태 디렉터리)이 있으면 세그먼트 저장소·테마·감사 로그·에이전트 프로필을 그 아래에 둔다. `holder`가
    /// 다시 넘긴 세션은 바로 다시 띄운다. 기록을 열지 못해도 세션은 띄운다(그 세션만 디스크 기록이 없다).
    pub fn new(holder: Option<(Holder, Vec<Kept>)>, dir: Option<&Path>) -> Self {
        let started = Instant::now();
        // Only the daemon that owns a state folder upgrades account registrations. `cfg!(test)` does
        // not cover other crates' tests that link krilld.
        if !cfg!(test)
            && let Some(dir) = dir
            && let Err(error) = crate::usage::accounts::codex_home::migrate(
                &crate::usage::accounts::default_home(),
                Some(dir),
            )
        {
            tracing::warn!(kind = ?error.kind(), "Codex account home migration failed");
        }
        if let Some(d) = dir {
            crate::agent::refresh_zsh_sync_env(d);
        }
        let ids = session_ids::SessionIds::open(
            dir,
            holder
                .as_ref()
                .into_iter()
                .flat_map(|(_, kept)| kept.iter().map(|k| k.id)),
        );
        let at = |name: &str| dir.map_or_else(PathBuf::new, |d| d.join(name));
        // 기록 저장소를 열지 못해도 세션은 띄운다. 조용히 넘기지 않고 경고를 남겨 `krill doctor`가 보인다.
        let store = dir.and_then(|d| match Store::open(d, Limits::default()) {
            Ok(store) => {
                clear_record_warnings(d);
                Some(store)
            }
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), errno = e.raw_os_error(), "segment store unavailable");
                note_record_warning(d, &format!("store open failed: {e}"));
                None
            }
        });
        let (holder, kept) = holder.map_or((None, Vec::new()), |(h, k)| (Some(Arc::new(h)), k));
        let kept_count = kept.len();
        let kept_ids: Vec<_> = kept.iter().map(|session| session.id).collect();
        let closed_ledger = closed_sessions::ClosedLedger::open(dir, &kept_ids);
        let reg = Self {
            app_opened: OnceLock::new(),
            last_gc: Mutex::new(Instant::now()),
            startup_gc: std::sync::atomic::AtomicBool::new(true),
            sidebar: Arc::default(),
            hibernation: Mutex::default(),
            schedule_minute: Mutex::default(),
            git_watch: Mutex::default(),
            upgrading: Mutex::new(false),
            resuming: Mutex::default(),
            resuming_conversations: (Mutex::default(), std::sync::Condvar::new()),
            account_work: Arc::new(crate::usage::accounts::switch_work::SwitchWork::default()),
            account_handoffs: Mutex::new(account_switch::load(dir)),
            account_recovered: std::sync::atomic::AtomicBool::new(false),
            account_processing: std::sync::atomic::AtomicBool::new(false),
            account_replacements: Mutex::default(),
            account_switch_opt_out: Mutex::default(),
            exit: OnceLock::new(),
            dir: dir.map(Path::to_path_buf),
            mobile: OnceLock::new(),
            self_ref: OnceLock::new(),
            bots: OnceLock::new(),
            bots_boot_started: OnceLock::new(),
            voice: OnceLock::new(),
            orch_signal: (Mutex::new(0), std::sync::Condvar::new()),
            orch_cache: Mutex::default(),
            orch_mail_notified_done: Mutex::default(),
            orch_launching: (Mutex::default(), std::sync::Condvar::new()),
            watch: crate::watch::Watch::new(if holder.is_some() {
                HolderStatus::Linked
            } else {
                HolderStatus::Standalone
            }),
            holder,
            store,
            look: Arc::new(Appearance::load(at("theme"))),
            audit: Audit::new(at("audit.log")),
            agents: crate::agent::control::Control::new(dir),
            hook_commit: Mutex::new(()),
            status: Statuses::open(dir.map(|d| d.join(status::FILE_NAME))),
            spool: dir.map(|d| d.join(hook::SPOOL_FILE)),
            ids: Mutex::new(ids),
            closed_ledger,
            state: Mutex::new(State::default()),
        };
        let mut st = lock(&reg.state);
        for k in kept {
            // 재기동 공백의 provider roster는 알 수 없다. 이 프로세스는 자동 종료에서 제외한다.
            reg.agents.block_hibernation(k.id);
            if k.status.is_none() {
                // 종료 요청 전후에 krilld가 죽었으면 살아 있는 프로세스를 우선 보존한다.
                reg.clear_sleeping(k.id);
            }
            let size = reg.resolve_kept_size(&k);
            let Ok(session) = reg.start(k.id, k.master, size) else {
                // master는 holder가 계속 쥔다. 다음 krilld 기동 때 다시 온다.
                // 조용히 넘기면 그 krilld 수명 동안 세션이 목록에서 사라진다(K-26).
                tracing::warn!(session = k.id.0, "kept session failed to start");
                st.unregistered_kept.insert(k.id);
                continue;
            };
            st.roots.insert(k.id, k.pid).ok();
            if let Some(s) = k.status {
                st.exited.insert(k.id, s);
                reg.status.exited(k.id);
            }
            st.sessions.insert(
                k.id,
                Entry {
                    session,
                    pid: k.pid,
                    cwd: reg
                        .store
                        .as_ref()
                        .and_then(|s| s.launch_cwd(k.id))
                        .unwrap_or_default(),
                    wrapped: crate::sys::process_args(k.pid).is_ok_and(|a| is_wrapper_argv(&a)),
                },
            );
        }
        let live: Vec<_> = st.sessions.keys().copied().collect();
        if let Some(d) = dir {
            crate::agent::claude::sweep(d, &live);
        }
        drop(st);
        // A missing recorder must not turn recoverable launch metadata into an orphan.
        if reg.store.is_some()
            && let Err(error) = reg.agents.sweep(&kept_ids, SystemTime::now())
        {
            tracing::warn!(kind = ?error.kind(), "orphan agent launch cleanup failed");
        }
        reg.status.retain(&live);
        reg.drain_spool();
        if kept_count > 0 {
            reg.emit_telemetry(krill_proto::telemetry::Event::DaemonReattached(
                krill_proto::telemetry::DaemonReattached {
                    recovered: live.len() == kept_count,
                    delay_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
                },
            ));
        }
        reg
    }

    /// 감시 루프의 가상 시계 입력. 레지스트리 잠금은 live 목록을 복사하는 동안에만 쥔다.
    pub(crate) fn gc_due(&self, now: Instant, wall: SystemTime) {
        let due = {
            let mut last = lock(&self.last_gc);
            let due = now.saturating_duration_since(*last) >= GC_INTERVAL;
            if due {
                *last = now;
            }
            due
        };
        let startup = self
            .startup_gc
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        if !due && !startup {
            return;
        }
        let ids_ready = lock(&self.ids).ready();
        if due && ids_ready {
            let retired: Vec<_> = {
                let state = lock(&self.state);
                state
                    .closed
                    .iter()
                    .filter(|id| state.exited.contains_key(id))
                    .copied()
                    .collect()
            };
            for id in retired {
                if let Some(holder) = &self.holder {
                    holder.notify(|req| ToHolder::Release { req, id }).ok();
                }
                self.remove(id);
            }
        }
        if ids_ready && let Some(store) = &self.store {
            let live: Vec<_> = lock(&self.state).sessions.keys().copied().collect();
            if let Err(e) = store.gc(&live, wall) {
                tracing::warn!(kind = ?e.kind(), errno = e.raw_os_error(), "history gc failed");
            }
        }
    }

    /// `krill hook` spool을 수거해 상태에 반영한다(기동 때, 그 뒤 [`status::SPOOL_POLL`]마다). 세션을 정하지 못한
    /// 이벤트(에이전트가 이미 끝남 등)는 버린다.
    pub fn drain_spool(&self) {
        let Some(path) = &self.spool else { return };
        let mut hooked = Vec::new();
        // Spawn 등록을 기다리는 동안 spool 파일을 잠그면 CLI fallback의 append 예산도 소진한다.
        let commit = lock(&self.hook_commit);
        status::take_spool(path, |events| {
            let found: Vec<_> = {
                let st = lock(&self.state);
                events
                    .iter()
                    .filter_map(|e| {
                        let id = scope::session_of(e.ppid, &st.roots)?;
                        let entry = st.sessions.get(&id)?;
                        Some((id, entry.cwd.clone(), entry.session.tap(), e))
                    })
                    .collect()
            };
            for (id, cwd, tap, e) in found {
                if self.apply_agent_hook(id, e, Some(&cwd), Some(&tap))? {
                    hooked.push((id, e.clone()));
                }
            }
            Ok(())
        })
        .ok();
        drop(commit);
        for (id, e) in &hooked {
            self.discover_hook_identity(*id, e);
            self.maybe_rename_from_hook(*id, &e.payload, e.id);
            if let Some(bots) = self.bots_if_initialized() {
                bots.on_hook_event(self, *id, e);
            }
            if status::is_turn_stop(&e.payload)
                && let Some(dir) = self.state_dir()
                && let Err(error) = self.notify_idle_orchestration_mail_for(&dir, Some(*id))
            {
                tracing::warn!(
                    kind = ?error.kind(),
                    "orchestration idle mail notification failed"
                );
            }
        }
    }

    /// holder 리더 스레드 본체. 링크가 끊길 때까지 돌아오지 않는다.
    pub fn run_holder(&self, link: Link) {
        if let Some(h) = &self.holder {
            h.run(link, |id, status| self.on_exited(id, status));
        }
    }

    /// 감시 스냅샷 뒤 세션이 지워졌으면 그 상태를 다시 만들지 않는다.
    /// 셸이 남은 세션에서 에이전트만 끝났다. 그때까지의 프롬프트를 다음 에이전트에 보여 주지 않는다.
    pub(crate) fn agent_exited(&self, id: SessionId) {
        if self.with_status(id, |s| s.exited(id)).is_some()
            && let Some(dir) = self.state_dir()
        {
            krill_proto::git::forget_session_prompts(&dir, id, status::now_ns()).ok();
        }
    }

    pub(crate) fn with_status<T>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Statuses) -> T,
    ) -> Option<T> {
        let _commit = lock(&self.hook_commit);
        let allowed = {
            let st = lock(&self.state);
            st.sessions.contains_key(&id) && !st.exited.contains_key(&id)
        };
        allowed.then(|| f(&self.status))
    }

    pub fn monitor_status(&self) {
        status::monitor(self);
    }

    /// 현재 화면 규칙. 서명 규칙이 유효하면 그것, 아니면 내장(MK-091).
    pub(crate) fn screen_rules(&self) -> krill_proto::agent::rules::ScreenRules {
        self.agents.screen_rules()
    }

    /// 루트가 에이전트 셸 복귀 래퍼인 세션인가([`Entry::wrapped`]).
    pub(crate) fn wrapped(&self, id: SessionId) -> bool {
        lock(&self.state)
            .sessions
            .get(&id)
            .is_some_and(|e| e.wrapped)
    }

    pub fn holder(&self) -> Option<&Holder> {
        self.holder.as_deref()
    }

    /// 업그레이드 답을 보낸 뒤 `done`으로 main에 종료를 알린다(MK-062). 한 번만 설정된다.
    pub fn exit_with(self: &Arc<Self>, done: Sender<()>) {
        self.bind_self_arc();
        self.exit.set(done).ok();
    }

    /// Binds a weak self-reference so lazy background subsystems (e.g., Bots connectors) can
    /// upgrade back to this [`Registry`], and triggers automatic startup recovery for bots and
    /// lost orchestration terminal workers if state exists on disk.
    pub fn bind_self_arc(self: &Arc<Self>) {
        self.self_ref.set(Arc::downgrade(self)).ok();
        self.maybe_spawn_bot_boot();
        crate::rejoin::recovery::recover_lost_terminal_workers_on_boot(self);
    }

    /// Spawns background Bot startup recovery once if `bots.json` already exists in `state_dir()`.
    pub(crate) fn maybe_spawn_bot_boot(self: &Arc<Self>) {
        let Some(dir) = self.state_dir() else {
            return;
        };
        if !crate::bots::store::BotStore::index_exists_at(&dir) {
            return;
        }
        if self.bots_boot_started.set(()).is_err() {
            return;
        }
        let reg = Arc::clone(self);
        if std::thread::Builder::new()
            .name("bots-boot".to_owned())
            .stack_size(256 * 1024)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Ok(mgr) = reg.bots() {
                        mgr.boot_recover(&reg);
                    }
                }))
                .is_err()
                {
                    tracing::warn!("bot startup recovery panicked");
                }
            })
            .is_err()
            && let Ok(mgr) = self.bots()
        {
            mgr.boot_recover(self);
        }
    }

    /// 재결합 소켓 준비가 끝난 뒤 감시 클라이언트에 holder 상실을 알린다.
    pub(crate) fn holder_lost(&self) {
        self.watch.orphan();
    }

    /// 아직 끝나지 않은 세션의 id·pid.
    pub(crate) fn running(&self) -> Vec<(SessionId, u32)> {
        let st = lock(&self.state);
        st.sessions
            .iter()
            .filter(|(id, _)| !st.exited.contains_key(id))
            .map(|(&id, e)| (id, e.pid))
            .collect()
    }

    /// Wait for in-flight reader creation to finish registration before a handoff snapshots it.
    pub(crate) fn session_start_barrier(&self) -> std::sync::MutexGuard<'_, ()> {
        lock(&self.hook_commit)
    }

    /// 세션마다 `f(id, pid, 끝났으면 상태, 세션)`. 상태 잠금 안에서 부른다.
    pub(crate) fn each(&self, mut f: impl FnMut(SessionId, u32, Option<i32>, &Session)) {
        let st = lock(&self.state);
        for (&id, e) in &st.sessions {
            f(id, e.pid, st.exited.get(&id).copied(), &e.session);
        }
    }

    /// 모바일 제어는 하나의 게이트웨이 인스턴스로 직렬화한다.
    pub fn mobile_gateway(&self) -> Option<&Arc<crate::mobile::gateway::Gateway>> {
        self.mobile.get()
    }

    fn resolve_kept_size(&self, k: &Kept) -> WinSize {
        #[cfg(unix)]
        let live_pty = crate::session::pty::size(&k.master).ok();
        #[cfg(windows)]
        let live_pty = None;
        let stored = if live_pty.is_none() && k.size.is_none() {
            self.store.as_deref().and_then(|s| s.last_size(k.id))
        } else {
            None
        };
        let (size, needs_holder_resize) = kept_session_size(live_pty, k.size, stored);
        #[cfg(unix)]
        let _ = needs_holder_resize;
        #[cfg(windows)]
        if k.status.is_none()
            && needs_holder_resize
            && let Some(h) = &self.holder
            && h.version() >= 4
        {
            h.notify(|req| ToHolder::Resize {
                req,
                id: k.id,
                size,
            })
            .ok();
        }
        size
    }

    pub(crate) fn session_size(&self, id: SessionId) -> Option<WinSize> {
        let tap = lock(&self.state).sessions.get(&id).map(|e| e.session.tap());
        tap?.size().ok()
    }

    pub fn start_mobile(self: &Arc<Self>, hello: ServerHello) -> io::Result<()> {
        self.bind_self_arc();
        if self.mobile.get().is_some() {
            return Ok(());
        }
        let gateway = crate::mobile::gateway::Gateway::start(
            Arc::downgrade(self),
            self.dir.as_deref(),
            hello,
        )?;
        self.mobile.set(gateway).map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "mobile gateway already started",
            )
        })
    }

    /// 연결 하나를 끝까지 처리한다: Hello 협상 → 첫 프레임이 [`Attach`]면 화면 스트림, [`Request`]면 제어 요청을
    /// 클라이언트가 닫을 때까지, [`HookEvent`]면 반영하고 ACK. 첫 프레임 없이 닫으면(진단용 연결) 그냥 끝난다.
    /// 화면·제어·hook feature 없이 협상만 했으면 감시 연결(MK-026)이다. holder 감시 기능을 협상한 연결에는
    /// 초기 상태를 보내고, 재결합 소켓 준비 뒤 holder 상실을 알린다(OWE-493).
    pub fn serve(&self, s: UnixStream, hello: &ServerHello) -> io::Result<()> {
        let permit = admission::handshake().ok_or_else(|| {
            io::Error::new(io::ErrorKind::WouldBlock, "local connection admission full")
        })?;
        self.serve_admitted(s, hello, permit)
    }

    /// Called by the accept loop after reserving a slot before spawning a thread.
    pub fn serve_admitted(
        &self,
        s: UnixStream,
        hello: &ServerHello,
        permit: admission::Permit,
    ) -> io::Result<()> {
        self.serve_as(s, hello, false, Some(permit))
    }

    /// 모바일 인증을 마친 내부 pair에만 사용한다. 원격 PID 대신 명시적 전체 범위를 부여한다.
    pub(crate) fn serve_mobile(&self, s: UnixStream, hello: &ServerHello) -> io::Result<()> {
        self.serve_as(s, hello, true, None)
    }

    pub(crate) fn handle_control_mobile(&self, req: Request) -> Reply {
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let reply = match req {
            Request::Orchestration(krill_proto::orchestration::OrchAction::SnapshotPage {
                generation,
                offset,
            }) => self.orchestration_page(&peer, generation, offset, ipc::FEATURES),
            other => self.handle_as(&peer, "mobile-pwa", other, &mut HashMap::new()),
        };
        match reply {
            Reply::Orchestration(response) => {
                Reply::Orchestration(response.for_features(ipc::FEATURES))
            }
            reply => reply,
        }
    }

    pub(crate) fn handle_usage_mobile(
        &self,
        req: krill_proto::usage::UsageRequest,
    ) -> krill_proto::usage::UsageReply {
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let required = req.required_features();
        if ipc::FEATURES & required == required {
            self.on_usage_negotiated(
                &peer,
                Role::InteractiveWithSize { owns_size: false },
                req,
                ipc::FEATURES,
            )
        } else {
            krill_proto::usage::UsageReply::Failed(Failure::BadRequest)
        }
        .for_features(ipc::FEATURES)
    }

    fn serve_as(
        &self,
        mut s: UnixStream,
        hello: &ServerHello,
        mobile: bool,
        admission: Option<admission::Permit>,
    ) -> io::Result<()> {
        crate::alloc::mobile_thread().ok();
        // macOS는 닫힌 피어의 LOCAL_PEERPID도 EINVAL로 거부하므로 ServerHello를 보내기 전에 피어를 읽는다.
        let mut peer = if mobile {
            Peer {
                pid: None,
                scope: Scope::Full,
            }
        } else {
            // Spawn 직후의 피어가 아직 미등록 루트나 상위 세션으로 분류되지 않게 한다.
            let _commit = lock(&self.hook_commit);
            Peer::of(&s, &lock(&self.state).roots)
        };
        // 서명 정책이 강제되면 모든 로컬 피어의 코드서명을 핸드셰이크 전에 검증한다(MK-096).
        // 계보 조회에 실패한 Scope::Restricted 피어도 강제 정책을 우회할 수 없다.
        if !mobile
            && self.gate_connection_signing(&mut peer, || {
                crate::sys::peer_token(s.as_fd())
                    .ok()
                    .map(|token| crate::sys::verify_audit_token(&token))
            })
        {
            return Ok(());
        }
        let (client, n) = crate::ipc::handshake(&mut s, hello)?;
        if peer.scope.full()
            && client.role == ipc::ClientRole::App
            && self.app_opened.set(()).is_ok()
        {
            self.emit_telemetry(krill_proto::telemetry::Event::AppOpened(
                krill_proto::telemetry::AppOpened {},
            ));
        }
        if n.features
            & (FEAT_SCREEN
                | FEAT_CONTROL
                | FEAT_HOOK
                | krill_proto::history::FEAT_HISTORY
                | krill_proto::theme::FEAT_THEME
                | krill_proto::usage::FEAT_USAGE
                | krill_proto::mobile::FEAT_MOBILE
                | ipc::FEAT_BOTS)
            == 0
        {
            let app = peer.pid.filter(|_| client.role == ipc::ClientRole::App);
            drop(admission);
            if n.features & FEAT_HOLDER_WATCH != 0 {
                return self.watch.serve(s, app);
            }
            return attach::watch(s, app);
        }
        // macOS는 닫힌 피어의 setsockopt를 EINVAL로 거부한다(OWE-487). 이미 받은 요청은 처리해야 한다.
        s.set_read_timeout(Some(if mobile {
            attach::MOBILE_ATTACH_TIMEOUT
        } else {
            ATTACH_TIMEOUT
        }))
        .ok();
        let (mut role, first) = attach::roles::first(&mut s, n.features)?;
        if mobile && role != Role::Observer {
            role = Role::InteractiveWithSize { owns_size: false };
        }
        let Some((k, body)) = first else {
            return Ok(());
        };
        drop(admission);
        if k == krill_proto::mobile::MOBILE_REQ
            && n.features & krill_proto::mobile::FEAT_MOBILE != 0
        {
            use krill_proto::mobile::{MobileReply, MobileRequest};
            s.set_read_timeout(None).ok();
            let mut frame = Some((k, body));
            while let Some((kind, body)) = frame {
                let request = MobileRequest::decode(kind, &body)?;
                let allowed = peer.scope.full()
                    && if mobile {
                        matches!(request, MobileRequest::Status)
                            || (role != Role::Observer
                                && matches!(request, MobileRequest::Workflow(_)))
                    } else {
                        role != Role::Observer
                            || matches!(
                                request,
                                MobileRequest::Status
                                    | MobileRequest::ListDevices
                                    | MobileRequest::ConnectedDevices
                            )
                    };
                let reply = if !allowed {
                    MobileReply::Failed("permission denied".into())
                } else if let Some(gateway) = self.mobile.get() {
                    if mobile {
                        gateway.request(request)
                    } else {
                        gateway.request_local(request)
                    }
                } else {
                    MobileReply::Failed("mobile gateway unavailable".into())
                };
                s.write_all(&reply.encode()?)?;
                frame = ipc::read_frame(&mut s)?;
            }
            return Ok(());
        }
        if k == krill_proto::theme::THEME && n.features & krill_proto::theme::FEAT_THEME != 0 {
            if !peer.scope.full() || role == Role::Observer || client.role != ipc::ClientRole::App {
                peer.deny("theme", &self.audit);
                return Ok(());
            }
            s.set_read_timeout(None).ok();
            let mut frame = Some((k, body));
            while let Some((k, body)) = frame {
                let push = krill_proto::theme::Push::decode(k, &body)?;
                if push.cell.is_some_and(|c| c.width == 0 || c.height == 0) {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                // 전체 레지스트리 잠금으로 테마 적용·저장·세션 알림 순서를 직렬화한다.
                let state = lock(&self.state);
                let changed = self.look.theme() != push.theme;
                let saved = self.look.set_theme(push.theme);
                if let Some(cell) = push.cell {
                    self.look.set_cell_size(cell);
                }
                if changed {
                    for entry in state.sessions.values() {
                        entry.session.theme_changed();
                    }
                }
                saved?;
                drop(state);
                frame = ipc::read_frame(&mut s)?;
            }
            return Ok(());
        }
        if k == screen::kind::ATTACH && n.features & FEAT_SCREEN != 0 {
            let a = Attach::decode(k, &body)?;
            let tap = self.tap(&peer, a.session);
            if tap.is_some() && role != Role::Observer {
                self.acknowledge_retained_handoff(a.session);
            }
            let frame_min = if mobile {
                attach::MOBILE_FRAME_MIN
            } else {
                attach::FRAME_MIN
            };
            return attach::run_as(s, a, hello.instance_id, n.features, tap, role, frame_min);
        }
        if k == hook::kind::EVENT && n.features & FEAT_HOOK != 0 {
            if role == Role::Observer {
                return s.write_all(&Ack::Denied.encode()?);
            }
            let e = HookEvent::decode(k, &body)?;
            let session = self.on_hook(&peer, &e)?;
            // 세션을 못 정한 이벤트도 받은 것이다(spool에 써 봐야 같은 결과).
            // OWE-737: hook 클라이언트의 30ms 소켓 예산을 막지 않도록 ACK를 먼저 보낸 뒤 브랜치·제목 개명을 수행한다.
            let ack = s.write_all(&hook::ack());
            drop(s);
            if let Some(id) = session {
                self.discover_hook_identity(id, &e);
                self.maybe_rename_from_hook(id, &e.payload, e.id);
                if let Some(bots) = self.bots_if_initialized() {
                    bots.on_hook_event(self, id, &e);
                }
                if status::is_turn_stop(&e.payload)
                    && let Some(dir) = self.state_dir()
                    && let Err(error) = self.notify_idle_orchestration_mail_for(&dir, Some(id))
                {
                    tracing::warn!(
                        kind = ?error.kind(),
                        "orchestration idle mail notification failed"
                    );
                }
            }
            return ack;
        }
        if k == krill_proto::history::kind::HISTORY_REQ
            && n.features & krill_proto::history::FEAT_HISTORY != 0
        {
            use krill_proto::history::{HistoryReply, HistoryRequest};
            let request = HistoryRequest::decode(k, &body)?;
            let reply = if peer.scope.full() && role != Role::Observer {
                match &self.store {
                    Some(store) => match request {
                        HistoryRequest::Clear if !lock(&self.ids).ensure_ready() => {
                            HistoryReply::Failed(Failure::Os(libc::EIO))
                        }
                        HistoryRequest::Clear => match store.clear() {
                            Ok(()) => HistoryReply::Cleared,
                            Err(e) => HistoryReply::Failed(os(&e)),
                        },
                        HistoryRequest::SetPaused { paused } => {
                            if n.features & krill_proto::history::FEAT_HISTORY_PAUSE == 0 {
                                HistoryReply::Failed(Failure::BadRequest)
                            } else {
                                store.set_paused(paused);
                                HistoryReply::Paused { paused }
                            }
                        }
                    },
                    None => HistoryReply::Failed(Failure::Os(libc::EIO)),
                }
            } else {
                peer.deny("history-clear", &self.audit);
                HistoryReply::Failed(Failure::Denied)
            };
            return s.write_all(&reply.encode()?);
        }
        if k == krill_proto::usage::kind::USAGE_REQ
            && n.features & krill_proto::usage::FEAT_USAGE != 0
        {
            use krill_proto::usage::UsageRequest;
            let req = UsageRequest::decode(k, &body)?;
            let required = req.required_features();
            let reply = if n.features & required == required {
                self.on_usage_negotiated(&peer, role, req, n.features)
            } else {
                krill_proto::usage::UsageReply::Failed(Failure::BadRequest)
            }
            .for_features(n.features);
            return s.write_all(&reply.encode()?);
        }
        if k == krill_proto::bots::BOT_REQ && n.features & ipc::FEAT_BOTS != 0 {
            use krill_proto::bots::{BotRpcRequest, BotRpcResponse};
            let req = BotRpcRequest::decode(k, &body)?;
            let is_observer = role == Role::Observer;
            let reply = if !peer.scope.full() || (is_observer && !req.is_read_only()) {
                peer.deny("bots", &self.audit);
                BotRpcResponse::Error("permission denied".to_owned())
            } else {
                self.on_bot_rpc_for_peer(&peer, is_observer, req)
            };
            let frame = match reply.encode() {
                Ok(f) => f,
                Err(krill_proto::ipc::Error::BadLength(bytes)) => {
                    BotRpcResponse::Error(krill_proto::orchestration::frame_limit_error(bytes))
                        .encode()?
                }
                Err(e) => return Err(e.into()),
            };
            return s.write_all(&frame);
        }
        if k != control::kind::REQUEST || n.features & FEAT_CONTROL == 0 {
            return Ok(());
        }
        s.set_read_timeout(None).ok();
        let mut body = body;
        let mut clients = HashMap::new();
        let mut control_slot = None;
        let remote_fs_conn = RemoteFsConnGuard(crate::remote_fs::next_connection_id());
        loop {
            let decoded = Request::decode(control::kind::REQUEST, &body);
            if !mobile {
                if decoded.as_ref().is_ok_and(|request| {
                    admission::protected(request)
                        || matches!(
                            request,
                            Request::Orchestration(
                                krill_proto::orchestration::OrchAction::CheckMessagesLegacy {
                                    wait_ms: 1..,
                                    ..
                                }
                            )
                        )
                }) {
                    control_slot = None;
                } else if control_slot.is_none() {
                    control_slot = Some(admission::control().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::WouldBlock, "local control admission full")
                    })?);
                }
            }
            let restart = matches!(decoded, Ok(Request::Resources(Action::RestartDaemon)));
            let reply = match decoded {
                Ok(Request::States) if n.features & FEAT_HOOK == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::NewWorktree(_)) if n.features & control::FEAT_WORKTREE == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::RemoveWorktree(_))
                    if n.features & control::FEAT_WORKTREE_REMOVE == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::GitChanges { .. }) if n.features & control::FEAT_GIT_CHANGES == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::GitEdit { ref edit, .. })
                    if n.features & edit.required_features() != edit.required_features() =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(
                    Request::Capture { .. } | Request::Resize { .. } | Request::SendInput { .. },
                ) if n.features & control::FEAT_CLI == 0 => Reply::Failed(Failure::BadRequest),
                Ok(Request::CaptureSince { .. })
                    if n.features & (control::FEAT_CAPTURE_SINCE | control::FEAT_CLI)
                        != (control::FEAT_CAPTURE_SINCE | control::FEAT_CLI) =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::WaitStatus { .. }) if n.features & control::FEAT_WAIT_STATUS == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Sidebar) if n.features & control::FEAT_SIDEBAR == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Cold { .. }) if n.features & control::FEAT_COLD == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }

                Ok(Request::AgentSettings { .. } | Request::AgentLaunch { .. })
                    if n.features & control::FEAT_AGENTS == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Upgrade(_)) if n.features & control::FEAT_UPGRADE == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Resources(_)) if n.features & FEAT_RESOURCES == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Resume { .. }) if n.features & control::FEAT_RESUME == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Hibernation { .. }) if n.features & control::FEAT_HIBERNATION == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::PrepareWorktree(_))
                    if n.features & control::FEAT_WORKTREE_PREPARE == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::GitWatch(_)) if n.features & control::FEAT_GIT_WATCH == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::AgentHistory(_))
                    if n.features & krill_proto::agent_history::FEAT_AGENT_HISTORY == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::AgentHistory(
                    krill_proto::agent_history::Action::Search(_)
                    | krill_proto::agent_history::Action::ResumeVault(_),
                )) if n.features & krill_proto::agent_history::FEAT_AI_VAULT == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::AgentHistory(krill_proto::agent_history::Action::ChatPoll {
                    ..
                })) if n.features & krill_proto::agent_history::FEAT_CHAT_POLL == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(
                    Request::Create(ref c)
                    | Request::Resume { create: ref c, .. }
                    | Request::Hibernation { create: ref c, .. },
                ) if c.account.is_some()
                    && n.features & krill_proto::usage::FEAT_ACCOUNT_POLICY == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::SessionAccounts) if n.features & control::FEAT_SESSION_ACCOUNT == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Create(ref c))
                    if matches!(&c.program, Program::Agent { id } if id.starts_with("fork:"))
                        && n.features & control::FEAT_SESSION_FORK == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::ClearScrollback { .. })
                    if n.features & control::FEAT_CLEAR_SCROLLBACK == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Orchestration(
                    krill_proto::orchestration::OrchAction::RetainWorker { .. },
                )) if n.features & krill_proto::orchestration::FEAT_ORCH_RETAIN == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Orchestration(
                    krill_proto::orchestration::OrchAction::SnapshotPage { .. },
                )) if n.features & krill_proto::orchestration::FEAT_ORCH_SNAPSHOT_PAGE == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::WorktreeRefs { .. })
                    if n.features & control::FEAT_WORKTREE_REFS == 0 =>
                {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::RemoteFs(_)) if n.features & control::FEAT_REMOTE_FS == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::Bot(_)) if n.features & ipc::FEAT_BOTS == 0 => {
                    Reply::Failed(Failure::BadRequest)
                }
                Ok(Request::RemoteFs(krill_proto::remote_fs::RemoteFsRequest::PortForward {
                    port,
                })) => {
                    if role == Role::Observer || !peer.scope.full() {
                        peer.deny("port-forward", &self.audit);
                        s.write_all(&Reply::Failed(Failure::Denied).encode()?)?;
                        return Ok(());
                    }
                    match crate::remote_fs::connect_loopback_port(port) {
                        Ok(tcp) => {
                            let ok = Reply::RemoteFs(
                                krill_proto::remote_fs::RemoteFsReply::PortForwarded(Ok(())),
                            );
                            s.write_all(&ok.encode()?)?;
                            s.flush().ok();
                            drop(control_slot);
                            crate::remote_fs::bridge_streams(s, tcp);
                            return Ok(());
                        }
                        Err(err) => {
                            let failed = Reply::RemoteFs(
                                krill_proto::remote_fs::RemoteFsReply::PortForwarded(Err(err)),
                            );
                            s.write_all(&failed.encode()?)?;
                            return Ok(());
                        }
                    }
                }
                Ok(req) if role == Role::Observer && !read_only(&req) => {
                    if matches!(req, Request::Bot(_)) {
                        peer.deny("bots", &self.audit);
                    }
                    Reply::Failed(Failure::Denied)
                }
                // 페이지 내용은 이 연결의 협상 기능으로 내려 보내야 하므로 여기서 다룬다.
                Ok(Request::Orchestration(
                    krill_proto::orchestration::OrchAction::SnapshotPage { generation, offset },
                )) => self.orchestration_page(&peer, generation, offset, n.features),
                Ok(Request::Bot(req)) => {
                    if peer.scope.full() {
                        Reply::Bot(self.on_bot_rpc_for_peer(&peer, role == Role::Observer, req))
                    } else {
                        peer.deny("bots", &self.audit);
                        Reply::Failed(Failure::Denied)
                    }
                }
                Ok(Request::RemoteFs(req)) => {
                    if peer.scope.full() {
                        Reply::RemoteFs(crate::remote_fs::serve_on_conn(remote_fs_conn.0, req))
                    } else {
                        peer.deny("remote-fs", &self.audit);
                        Reply::Failed(Failure::Denied)
                    }
                }
                Ok(req) => self.handle_as(&peer, &client.name, req, &mut clients),
                Err(_) => Reply::Failed(Failure::BadRequest),
            };
            let reply = match reply {
                Reply::Orchestration(response) => {
                    Reply::Orchestration(response.for_features(n.features))
                }
                reply => reply,
            };
            let frame = match reply.encode() {
                Ok(frame) => frame,
                // 1MiB 프레임을 넘는 응답은 스트림을 닫지 않고 N-1 와이어의 작은 명시 오류로 바꾼다
                // (foundation-02). 클라이언트는 EOF 대신 이유를 받고 페이지 스냅샷·디스크 읽기로 넘어간다.
                Err(krill_proto::ipc::Error::BadLength(bytes)) => {
                    oversized_reply(&reply, bytes).encode()?
                }
                Err(e) => return Err(e.into()),
            };
            let sent = s.write_all(&frame);
            if reply == Reply::Upgrade(control::Upgraded::Started)
                || (restart && matches!(reply, Reply::Resources(_)))
            {
                // 리더는 멈췄거나(업그레이드) holder가 교체를 승인했다(MK-071). 답을 보낸 뒤(실패해도) 끝나야
                // holder가 새 krilld를 띄운다(세대 공존 없음).
                if let Some(exit) = self.exit.get() {
                    exit.send(()).ok();
                }
                return sent;
            }
            sent?;
            loop {
                match ipc::read_frame(&mut s)? {
                    Some((control::kind::REQUEST, b)) => {
                        body = b;
                        break;
                    }
                    Some((roles::kind::ROLE, b)) if n.features & FEAT_ROLES != 0 => {
                        let ack = if let roles::Request::Set(next) =
                            roles::Request::decode(roles::kind::ROLE, &b)?
                        {
                            role = next;
                            for client in clients.values() {
                                client.request(roles::Request::Set(role));
                            }
                            Ack::Applied
                        } else {
                            Ack::Invalid // 제어 연결은 대상 세션이 없어 Focus/Resize를 받지 않는다.
                        };
                        s.write_all(&ack.encode()?)?;
                    }
                    _ => return Ok(()),
                }
            }
        }
    }

    #[cfg(test)]
    #[cfg(unix)]
    fn handle(
        &self,
        peer: &Peer,
        req: Request,
        clients: &mut HashMap<SessionId, attach::roles::Client>,
    ) -> Reply {
        self.handle_as(peer, "", req, clients)
    }

    fn handle_as(
        &self,
        peer: &Peer,
        client_name: &str,
        req: Request,
        clients: &mut HashMap<SessionId, attach::roles::Client>,
    ) -> Reply {
        let r = match req {
            Request::AgentSettings { permission } => {
                if peer.scope.full() {
                    self.agents
                        .settings(permission)
                        .map(Reply::AgentSettings)
                        .map_err(|e| os(&e))
                } else {
                    peer.deny("agent-settings", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::AgentLaunch { session } => {
                let check = {
                    let st = lock(&self.state);
                    if !peer.authorize("agent-launch", session, &self.audit) {
                        Err(Failure::Denied)
                    } else if !st.sessions.contains_key(&session) {
                        Err(Failure::NoSession)
                    } else {
                        Ok(())
                    }
                };
                check.and_then(|()| {
                    self.agents
                        .launch(session)
                        .map(Reply::AgentLaunch)
                        .map_err(|e| os(&e))
                })
            }
            Request::WaitStatus { session } => self
                .tap(peer, session)
                .ok_or(Failure::NoSession)
                .map(|tap| Reply::WaitStatus {
                    state: if lock(&self.state).exited.contains_key(&session) {
                        control::AgentState::Exited
                    } else {
                        self.status.get(session).state
                    },
                    quiet_ms: tap.quiet_ms(),
                }),
            Request::CaptureSince { session, since } => self
                .tap(peer, session)
                .ok_or(Failure::NoSession)
                .and_then(|tap| {
                    tap.capture_plain_since(since)
                        .map_err(|_| Failure::BadRequest)
                }),
            Request::Capture { session, format } => {
                let tap = self.tap(peer, session).ok_or(Failure::NoSession);
                tap.and_then(|tap| {
                    tap.capture_cli(format)
                        .map(|(seq, bytes)| Reply::Captured { seq, bytes })
                        .map_err(|_| Failure::BadRequest)
                })
            }
            Request::Resize {
                session,
                cols,
                rows,
            } => self
                .tap(peer, session)
                .ok_or(Failure::NoSession)
                .and_then(|tap| {
                    if cols == 0 || rows == 0 || cols > screen::MAX_COLS || rows > screen::MAX_LINES
                    {
                        return Err(Failure::BadRequest);
                    }
                    let client = match clients.entry(session) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => e.insert(
                            attach::roles::Client::new(tap, Role::Interactive)
                                .map_err(|e| os(&e))?,
                        ),
                    };
                    // 세션을 지정하는 CLI resize는 명시적 포커스도 겸한다.
                    if client.request(roles::Request::Resize { cols, rows }) == Ack::Applied
                        && client.request(roles::Request::Focus) == Ack::Applied
                    {
                        Ok(Reply::Resized)
                    } else {
                        Err(Failure::Os(libc::EIO))
                    }
                }),
            Request::Cold { session } => self.cold(peer, session),
            Request::ClearScrollback { session } => self.clear_scrollback(peer, session),
            Request::Resume { session, create } => self.resume(peer, session, create),
            Request::Hibernation { session, create } => self.wake_hibernated(peer, session, create),
            Request::Create(c) => self.create(peer, c),
            Request::List => Ok(self.list(peer)),
            Request::Close { session } => self.close(peer, session),
            Request::States => Ok(self.states(peer)),
            Request::SessionAccounts => Ok(self.session_accounts(peer)),
            Request::Sidebar => {
                let st = lock(&self.state);
                let entries: Vec<_> = st
                    .sessions
                    .keys()
                    .filter(|_| peer.scope.full())
                    .copied()
                    .collect();
                drop(st);
                // Use the same fallback as session creation/resume. Windows cannot query another
                // process's cwd, and a restored engine may not have received a shell report yet.
                let entries: Vec<_> = entries
                    .into_iter()
                    .map(|id| (id, self.session_cwd(id)))
                    .collect();
                sidebar::snapshot(&self.sidebar, &entries).map(|mut data| {
                    let state = self.state_dir();
                    if peer.scope.full() {
                        if let Some(dir) = &state {
                            sidebar::include_projects(&mut data, dir);
                        }
                        data.closed_sessions = self.closed_sidebar_ranges();
                    }
                    if let Some(dir) = state {
                        data.prompt_histories =
                            krill_proto::git::load_session_prompt_histories(&dir)
                                .into_iter()
                                .filter(|(id, _)| data.sessions.iter().any(|s| s.session == *id))
                                .collect();
                    }
                    Reply::Sidebar(data)
                })
            }
            Request::WorktreeRefs { repo } => {
                if peer.authorize("worktree-refs", SessionId(0), &self.audit) {
                    Ok(crate::git::worktree_refs::serve(&repo))
                } else {
                    Err(Failure::Denied)
                }
            }
            Request::PrepareWorktree(w) => {
                if peer.authorize("worktree-prepare", SessionId(0), &self.audit) {
                    Ok(crate::git::worktree::prepare::serve(&w))
                } else {
                    Err(Failure::Denied)
                }
            }
            Request::NewWorktree(w) => {
                if peer.authorize("worktree-new", SessionId(0), &self.audit) {
                    let size = self.request_size(peer);
                    let reply = crate::git::worktree::serve(&w, size, |c| self.create(peer, c));
                    if matches!(reply, Reply::WorktreeCreated(_)) {
                        lock(&self.sidebar).invalidate();
                    }
                    Ok(reply)
                } else {
                    Err(Failure::Denied)
                }
            }
            Request::RemoveWorktree(w) => {
                if peer.authorize("worktree-rm", SessionId(0), &self.audit) {
                    let reply = crate::git::worktree::serve_remove(&w);
                    if matches!(reply, Reply::WorktreeRemoved(_)) {
                        lock(&self.sidebar).invalidate();
                    }
                    Ok(reply)
                } else {
                    Err(Failure::Denied)
                }
            }
            Request::Upgrade(u) => match (&self.holder, peer.scope.full()) {
                (Some(holder), true) => {
                    let mut started = lock(&self.upgrading);
                    if *started {
                        Ok(Reply::Upgrade(control::Upgraded::Started))
                    } else {
                        let r = crate::upgrade::prepare(self, holder, u);
                        *started = r == Ok(control::Upgraded::Started);
                        r.map(Reply::Upgrade)
                    }
                }
                (None, true) => Err(Failure::NoHolder),
                _ => {
                    peer.deny("upgrade", &self.audit);
                    Err(Failure::Denied)
                }
            },
            Request::AgentHistory(action) => self.agent_history(peer, action),
            Request::Resources(action) => self.resources(peer, action),
            Request::GitChanges { repo } => {
                if peer.scope.full() {
                    Ok(crate::git::changes::serve(&repo))
                } else {
                    peer.deny("git-changes", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::GitWatch(req) => {
                if peer.scope.full() {
                    Ok(Reply::GitWatch(
                        lock(&self.git_watch).handle_for(client_name, &req),
                    ))
                } else {
                    peer.deny("git-watch", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::GitEdit { repo, edit } => {
                if peer.scope.full() {
                    Ok(crate::git::edit::serve(&repo, edit))
                } else {
                    peer.deny("git-edit", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::SendInput {
                session,
                bytes,
                submit,
                wait_submit,
            } => self.send_input(peer, session, &bytes, submit, wait_submit),
            Request::Orchestration(action) => self.orchestration(peer, action),
            Request::RemoteFs(req) => {
                if peer.scope.full() {
                    Ok(Reply::RemoteFs(crate::remote_fs::serve(req)))
                } else {
                    peer.deny("remote-fs", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::Bot(req) => {
                if peer.scope.full() {
                    Ok(Reply::Bot(self.on_bot_rpc_for_peer(peer, false, req)))
                } else {
                    peer.deny("bots", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::Voice(req) => {
                if peer.scope.full() {
                    let voice = self.voice();
                    let reply =
                        voice.handle_request(req, |kind, id| self.voice_target_cwd(kind, id));
                    Ok(Reply::Voice(reply))
                } else {
                    peer.deny("voice", &self.audit);
                    Err(Failure::Denied)
                }
            }
        };
        r.unwrap_or_else(Reply::Failed)
    }

    fn resources(&self, peer: &Peer, action: Action) -> Result<Reply, Failure> {
        if action == Action::RestartDaemon {
            if !peer.scope.full() {
                peer.deny("restart-daemon", &self.audit);
                return Err(Failure::Denied);
            }
            if self.exit.get().is_none() || self.holder.is_none() {
                return Err(Failure::NoHolder);
            }
        }
        let st = lock(&self.state);
        let sessions: Vec<_> = st
            .sessions
            .iter()
            .filter(|_| peer.scope.full())
            .map(|(&id, e)| (id, (!st.exited.contains_key(&id)).then_some(e.pid)))
            .collect();
        drop(st);
        let r = crate::resources::measure(
            peer.pid,
            self.holder.is_some(),
            &sessions,
            self.dir.as_deref(),
        );
        if action == Action::RestartDaemon {
            crate::monitor::restart(self).map_err(|e| os(&e))?;
            tracing::info!("daemon restart requested by client");
        }
        Ok(Reply::Resources(r))
    }

    /// setup 세션에 쓸 요청 세션의 현재 크기. 세션 안에서 온 요청이 아니거나 크기를 읽지 못하면 `None`(80x24 기본값).
    fn request_size(&self, peer: &Peer) -> Option<WinSize> {
        let Scope::Session(id) = peer.scope else {
            return None;
        };
        let st = lock(&self.state);
        st.sessions.get(&id)?.session.tap().size().ok()
    }

    fn session_cwd(&self, id: SessionId) -> Option<PathBuf> {
        use krill_proto::os::ffi::OsStrExt as _;

        let (pid, live, initial, tap) = {
            let st = lock(&self.state);
            let e = st.sessions.get(&id)?;
            let live =
                !st.exited.contains_key(&id) && scope::session_of(e.pid, &st.roots) == Some(id);
            (e.pid, live, e.cwd.clone(), e.session.tap())
        };
        if live && let Some(dir) = tap.cwd(pid) {
            return Some(krill_proto::os::canonicalize(&dir).unwrap_or(dir));
        }
        // Keep the last observed cwd when a session exits or its shell stops reporting.
        if let Some(dir) = lock(&self.sidebar).directory(id) {
            return Some(dir);
        }
        // A manually launched agent's hook records its workspace even when the shell does not
        // emit OSC 7/9;9. This metadata also survives daemon replacement, unlike Entry::cwd.
        if let Some(dir) = self
            .agents
            .stored(id)
            .ok()
            .flatten()
            .map(|s| PathBuf::from(std::ffi::OsStr::from_bytes(&s.cwd)))
            .filter(|p| p.is_absolute())
        {
            return Some(krill_proto::os::canonicalize(&dir).unwrap_or(dir));
        }
        (!initial.as_os_str().is_empty())
            .then(|| krill_proto::os::canonicalize(&initial).unwrap_or(initial))
    }

    /// 화면 스트림이 붙을 세션. 범위 밖이면 감사 로그에 남기고 없는 세션처럼 답한다.
    fn tap(&self, peer: &Peer, id: SessionId) -> Option<Tap> {
        let st = lock(&self.state);
        if !peer.authorize("attach", id, &self.audit) {
            return None;
        }
        st.sessions.get(&id).map(|e| e.session.tap())
    }

    /// 세션 `id`의 핫·콜드 스크롤백을 비운다(OWE-873b). 세션·PTY·보이는 화면은 건드리지 않는다.
    fn clear_scrollback(&self, peer: &Peer, id: SessionId) -> Result<Reply, Failure> {
        let tap = {
            let st = lock(&self.state);
            if !peer.authorize("clear-scrollback", id, &self.audit) {
                return Err(Failure::Denied);
            }
            st.sessions.get(&id).map(|e| e.session.tap())
        };
        if let Some(tap) = tap {
            tap.clear_scrollback();
            return Ok(Reply::ScrollbackCleared);
        }
        if self
            .store
            .as_ref()
            .is_some_and(|store| store.clear_cold_scrollback(id))
        {
            return Ok(Reply::ScrollbackCleared);
        }
        Err(Failure::NoSession)
    }

    /// 디스크 세그먼트에서 재구성한 마지막 화면과 스크롤백(MK-032 콜드 복원). 범위 밖이면 거부한다.
    ///
    /// 잠정 결정 의존: MK-024 세그먼트 저장소(`Store::history`·`History::restore`). 거부되면 이 함수와
    /// `Request::Cold` 분기를 걷어 낸다.
    fn cold(&self, peer: &Peer, id: SessionId) -> Result<Reply, Failure> {
        if !peer.authorize("cold", id, &self.audit) {
            return Err(Failure::Denied);
        }
        if lock(&self.state).unregistered_kept.contains(&id) {
            return Err(Failure::BadRequest);
        }
        if self.is_closed(id) {
            return Err(Failure::NoSession);
        }
        let store = self.store.as_ref().ok_or(Failure::NoSession)?;
        let restored = store
            .history(id)
            .and_then(|h| h.restore())
            .map_err(|_| Failure::NoSession)?;
        let mut cold = control::Cold {
            screen: attach::snapshot(&restored.screen()),
            history: restored.history(),
        };
        if cold.fit() {
            Ok(Reply::Cold(cold))
        } else {
            Err(Failure::BadRequest)
        }
    }

    fn start(&self, id: SessionId, master: Master, size: WinSize) -> io::Result<Session> {
        let rec = self
            .store
            .as_ref()
            .and_then(|s| match s.recorder(id, size) {
                Ok(rec) => Some(rec),
                Err(e) => {
                    tracing::warn!(
                        session = id.0,
                        kind = ?e.kind(),
                        errno = e.raw_os_error(),
                        "session recorder unavailable"
                    );
                    if let Some(dir) = self.dir.as_deref() {
                        note_record_warning(dir, &format!("session {} recorder failed: {e}", id.0));
                    }
                    None
                }
            });
        Session::start(
            id,
            master,
            #[cfg(windows)]
            self.holder.clone(),
            size,
            Arc::clone(&self.look),
            rec,
        )
    }

    /// Decodes a query-encoded `Program::Agent { id }` (e.g. `claude?model=...&effort=...`),
    /// folds Antigravity level model IDs, and applies launch overrides onto a cloned profile.
    fn resolve_agent_profile_with_overrides(
        &self,
        id: &str,
    ) -> Result<
        (
            String,
            krill_proto::agent::Profile,
            krill_proto::agent::Permission,
        ),
        Failure,
    > {
        let (base_id, mut overrides) =
            krill_proto::agent::decode_agent_program_id(id).map_err(|_| Failure::BadRequest)?;
        let agents = self.agents.agents().map_err(|_| Failure::UnknownAgent)?;
        let (canonical, base_profile) = agents.resolve(base_id).ok_or(Failure::UnknownAgent)?;
        let mut profile = base_profile.clone();
        if canonical == "agy"
            && let Some(ref model) = overrides.model
            && let Some((family, eff)) =
                crate::mobile::fold_agy_model_selection(None, model, overrides.effort.as_deref())
        {
            overrides.model = Some(family);
            overrides.effort = Some(eff);
        }
        if !overrides.is_empty() {
            krill_proto::agent::apply_launch_overrides(canonical, &mut profile, &overrides)
                .map_err(|_| Failure::BadRequest)?;
        }
        Ok((canonical.to_owned(), profile, agents.permission))
    }

    fn gate_connection_signing(
        &self,
        peer: &mut Peer,
        sign: impl FnOnce() -> Option<crate::sys::CodeSign>,
    ) -> bool {
        let policy = scope::SigningPolicy::load(self.dir.as_deref());
        if !policy.enforced() {
            return false;
        }
        if !peer.scope.full() {
            peer.deny("peer-sign", &self.audit);
            return true;
        }
        let sign = sign();
        peer.gate_signing(&policy, sign.as_ref(), &self.audit)
    }

    fn create(&self, peer: &Peer, mut c: Create) -> Result<Reply, Failure> {
        if !peer.authorize("create", SessionId(0), &self.audit) {
            return Err(Failure::Denied);
        }
        sanitize_spawn_cwd(&mut c.cwd);
        if c.account.is_some() && !matches!(c.program, Program::Agent { .. }) {
            return Err(Failure::BadRequest);
        }
        if let Program::Agent { ref id } = c.program
            && let Some(spec) = id.strip_prefix("fork:")
        {
            let spec = spec.to_owned();
            return self.fork(peer, &spec, c);
        }
        let holder = self.holder.as_ref().ok_or(Failure::NoHolder)?;
        // 세션 id를 먼저 잡는다. Claude settings 파일 이름(`<id>-<uuid>.json`)에 넣어
        // 세션이 끝나면 그 세션의 파일만 지운다(OWE-499). id 재사용은 `next_id`가 막는다.
        let session = self.reserve_session_id(None).map_err(|error| os(&error))?;
        let state = self.state_dir();
        let mut agy_prewarm = None;
        let Launch { argv, env } = match c.program {
            Program::Login => crate::agent::login_in(state.as_deref()),
            Program::Custom { program, args } => Shell::Custom { program, args }.launch(),
            Program::Agent { id } => {
                let (canonical, profile, permission) =
                    self.resolve_agent_profile_with_overrides(&id)?;
                let (launch, session_id, account) = crate::agent::launch_selected_in(
                    &profile,
                    permission,
                    session,
                    state.as_deref(),
                    c.account.as_deref(),
                )
                .map_err(|e| {
                    self.discard_settings(session);
                    os(&e)
                })?;
                crate::agent::pre_trust_workspace(&profile, &c.cwd, account.as_ref());
                if !cfg!(test) && canonical == "agy" {
                    agy_prewarm = Some(
                        account
                            .as_ref()
                            .filter(|a| !a.dir.is_empty())
                            .map(|a| PathBuf::from(&a.dir))
                            .or_else(|| {
                                profile
                                    .env
                                    .get("ANTIGRAVITY_CLI_HOME")
                                    .filter(|p| !p.is_empty())
                                    .map(PathBuf::from)
                            }),
                    );
                }
                let metadata = control::AgentLaunch {
                    profile: canonical,
                    permission,
                };
                self.agents
                    .remember_session(session, &metadata, session_id, c.cwd.clone(), account)
                    .map_err(|e| {
                        self.discard_settings(session);
                        os(&e)
                    })?;
                launch
            }
        };
        let (session, pid) =
            self.spawn_holder(holder, session, argv, env, c.cwd, c.cols, c.rows)?;
        if let Some(config) = agy_prewarm
            && let Some(home) = krill_proto::os::home_dir()
        {
            crate::mobile::prewarm_agy_launch(&home, config);
        }
        Ok(Reply::Created { session, pid })
    }

    /// 기존 에이전트 세션을 포크해 새 세션으로 띄운다(OWE-736, MK-182).
    /// 원 세션 PTY는 절대 닫지 않고 그대로 둔다.
    fn fork(&self, peer: &Peer, spec: &str, mut c: Create) -> Result<Reply, Failure> {
        let (agent_hint, old_str) = match spec.rsplit_once(':') {
            Some((agent, id)) => (Some(agent).filter(|s| !s.is_empty()), id),
            None => (None, spec),
        };
        let old = old_str
            .strip_prefix('s')
            .unwrap_or(old_str)
            .parse::<u64>()
            .ok()
            .filter(|&n| n > 0)
            .map(SessionId)
            .ok_or(Failure::BadRequest)?;
        if !peer.authorize("fork", old, &self.audit) {
            return Err(Failure::Denied);
        }
        let holder = self.holder.as_ref().ok_or(Failure::NoHolder)?;
        let stored = self.agents.stored(old).map_err(|e| os(&e))?;
        let state = self.state_dir();
        let fallback_sid = state
            .as_deref()
            .and_then(|d| crate::agent::claude::session_id(d, old));
        let has_old = lock(&self.state).sessions.contains_key(&old);
        if !has_old && stored.is_none() && fallback_sid.is_none() {
            return Err(Failure::NoSession);
        }
        if c.cwd.is_empty() {
            if let Some(s) = &stored
                && !s.cwd.is_empty()
            {
                c.cwd.clone_from(&s.cwd);
            } else if let Some(dir) = self.session_cwd(old) {
                use krill_proto::os::ffi::OsStrExt as _;
                c.cwd = dir.as_os_str().as_bytes().to_vec();
            }
        }
        sanitize_spawn_cwd(&mut c.cwd);
        let agents = self.agents.agents().map_err(|_| Failure::UnknownAgent)?;
        let status_agent = self.status.get(old).agent;
        let profile_id = stored
            .as_ref()
            .map(|s| s.profile.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| (!status_agent.is_empty()).then_some(status_agent.as_str()))
            .or(agent_hint)
            .or_else(|| fallback_sid.as_ref().map(|_| "claude"))
            .ok_or(Failure::UnknownAgent)?;
        let (canonical, profile) = agents.resolve(profile_id).ok_or(Failure::UnknownAgent)?;
        let permission = stored.as_ref().map_or(agents.permission, |s| s.permission);
        let source_sid = stored
            .as_ref()
            .and_then(|s| s.session_id.clone())
            .or(fallback_sid);
        let recorded_account = stored.as_ref().and_then(|s| s.account.as_ref());
        let account = crate::agent::resume_target(
            profile,
            state.as_deref(),
            c.account.as_deref(),
            recorded_account,
            source_sid.as_deref(),
        )
        .map_err(|e| os(&e))?;
        let session = self
            .reserve_session_id(Some(old))
            .map_err(|error| os(&error))?;
        crate::agent::pre_trust_workspace(profile, &c.cwd, account.as_ref());
        let (launch, session_id, account) = crate::agent::fork_in(
            profile,
            permission,
            session,
            state.as_deref(),
            source_sid.as_deref(),
            account.as_ref(),
        )
        .map_err(|e| {
            self.discard_settings(session);
            os(&e)
        })?;
        let metadata = control::AgentLaunch {
            profile: canonical.to_owned(),
            permission,
        };
        self.agents
            .remember_session(session, &metadata, session_id, c.cwd.clone(), account)
            .map_err(|e| {
                self.discard_settings(session);
                os(&e)
            })?;
        let (session, pid) = self.spawn_holder(
            holder,
            session,
            launch.argv,
            launch.env,
            c.cwd,
            c.cols,
            c.rows,
        )?;
        Ok(Reply::Created { session, pid })
    }
}

/// 진행 중인 `Resume` 표시. 살아 있는 동안 같은 `old`의 `resume`은 거부된다.
/// drop하면 표시가 풀린다(K-25).
struct ResumeClaim<'a> {
    busy: &'a Mutex<HashSet<SessionId>>,
    id: SessionId,
}

impl Drop for ResumeClaim<'_> {
    fn drop(&mut self) {
        lock(self.busy).remove(&self.id);
    }
}

impl Registry {
    /// 종료·콜드 세션 `old`를 새 세션으로 이어서 띄운다(MK-052). 에이전트 기동 기록이 있으면 그 프로필·권한과
    /// 저장된 `session_id`로 `claude --resume <id>`·`codex resume [<id>]`를 실행하고, 없으면 `c`로 띄운다.
    ///
    /// 잠정 결정 의존: OWE-397 [MK-032] 콜드 복원 뷰(`Request::Cold`·`Shell::restart`). 거부되면 이 함수와
    /// `Request::Resume` 분기를 걷어 낸다.
    ///
    /// 같은 세션의 동시 `Resume`는 [`Self::claim_resume`]으로 먼저 잡은 쪽만 진행한다(K-25).
    fn resume(&self, peer: &Peer, old: SessionId, mut c: Create) -> Result<Reply, Failure> {
        if !peer.authorize("resume", old, &self.audit) {
            return Err(Failure::Denied);
        }
        {
            let st = lock(&self.state);
            if st.resumed.contains(&old) {
                return Err(Failure::NoSession);
            }
            // K-26: 이 PTY는 holder에 살아 있다. 목록에 없다고 되살리면 같은 대화의 에이전트가 둘이 된다.
            if st.unregistered_kept.contains(&old)
                || st.sessions.contains_key(&old) && !st.exited.contains_key(&old)
            {
                return Err(Failure::BadRequest);
            }
        }
        if self.is_closed(old) {
            return Err(Failure::NoSession);
        }
        // 같은 세션의 동시 재개는 먼저 잡은 쪽만 진행한다. 두 창이 동시에 눌러도
        // `claude --resume`이 두 번 뜨지 않는다(K-25).
        let _claim = self.claim_resume(old).ok_or(Failure::BadRequest)?;
        if lock(&self.state).unregistered_kept.contains(&old) {
            return Err(Failure::BadRequest);
        }
        // A competing resume may have completed between the first check and claim.
        if self.is_closed(old) || lock(&self.state).resumed.contains(&old) {
            return Err(Failure::NoSession);
        }
        let holder = self.holder.as_ref().ok_or(Failure::NoHolder)?;
        let stored = self.agents.stored(old).map_err(|e| os(&e))?;
        let state = self.state_dir();
        let fallback_sid = state
            .as_deref()
            .and_then(|d| crate::agent::claude::session_id(d, old));
        if let Some(s) = &stored
            && c.cwd.is_empty()
            && !s.cwd.is_empty()
        {
            c.cwd.clone_from(&s.cwd);
        }
        sanitize_spawn_cwd(&mut c.cwd);
        let session = self
            .reserve_session_id(Some(old))
            .map_err(|error| os(&error))?;
        let cwd_path = {
            use krill_proto::os::ffi::OsStrExt as _;
            PathBuf::from(std::ffi::OsStr::from_bytes(&c.cwd))
        };
        let bots_mgr = if let Some(existing) = self.bots_if_initialized() {
            Some(Arc::clone(existing))
        } else if stored
            .as_ref()
            .is_some_and(|s| s.bot_instructions.is_some())
            || state
                .as_deref()
                .is_some_and(|d| d.join("bots").join("runtime-bindings.json").is_file())
        {
            self.bots().ok().cloned()
        } else {
            None
        };
        let mut bot_launch = if let Some(ref bots) = bots_mgr {
            bots.ensure_runtime_rehydrated(self);
            match bots.prepare_resume_launch(
                old,
                session,
                &cwd_path,
                stored.as_ref().and_then(|s| s.bot_instructions.as_deref()),
                stored
                    .as_ref()
                    .and_then(|s| s.launched_at)
                    .unwrap_or(0)
                    .saturating_mul(1000),
            ) {
                Ok(bl) => bl,
                Err(err) => {
                    tracing::warn!(
                        session = old.0,
                        error = %err,
                        "bot resume launch preparation failed; falling back to stored instructions"
                    );
                    None
                }
            }
        } else {
            None
        };
        let revoke_bot_grant = |bl: &mut Option<crate::bots::runtime::BotAccountSwitchLaunch>| {
            if let (Some(bots), Some(launch)) = (bots_mgr.as_ref(), bl.take()) {
                bots.revoke_launch_token(launch.grant_token.as_ref());
            }
        };
        let Launch { argv, mut env } = match stored {
            Some(s) => {
                let Ok(agents) = self.agents.agents() else {
                    revoke_bot_grant(&mut bot_launch);
                    return Err(Failure::UnknownAgent);
                };
                let Some((canonical, profile)) = agents.resolve(&s.profile) else {
                    revoke_bot_grant(&mut bot_launch);
                    return Err(Failure::UnknownAgent);
                };
                let resume_id = s.session_id.or(fallback_sid);
                // 실행 당시 계정으로 이어 간다(OWE-698). 휴면 깨우기도 이 경로다.
                // 요청이 계정을 지정하면 그 계정으로 이어 간다(OWE-699, 다른 계정으로 이어가기).
                let account = match crate::agent::resume_target(
                    profile,
                    state.as_deref(),
                    c.account.as_deref(),
                    s.account.as_ref(),
                    resume_id.as_deref(),
                ) {
                    Ok(a) => a,
                    Err(e) => {
                        revoke_bot_grant(&mut bot_launch);
                        return Err(os(&e));
                    }
                };
                let mut effective_profile = profile.clone();
                if let Some(ref bl) = bot_launch {
                    let mut overrides = krill_proto::agent::LaunchOverrides {
                        model: bl.model.clone(),
                        effort: bl.effort.clone(),
                    };
                    if canonical == "agy"
                        && let Some(ref m) = overrides.model
                        && let Some((family, eff)) = crate::mobile::fold_agy_model_selection(
                            None,
                            m,
                            overrides.effort.as_deref(),
                        )
                    {
                        overrides.model = Some(family);
                        overrides.effort = Some(eff);
                    }
                    if !overrides.is_empty()
                        && let Err(err) = krill_proto::agent::apply_launch_overrides(
                            canonical,
                            &mut effective_profile,
                            &overrides,
                        )
                    {
                        tracing::warn!(
                            session = old.0,
                            error = %err,
                            "ignoring invalid bot launch override during resume"
                        );
                    }
                }
                crate::agent::pre_trust_workspace(&effective_profile, &c.cwd, account.as_ref());
                let instructions = bot_launch
                    .as_ref()
                    .map(|bl| bl.instructions.as_str())
                    .or(s.bot_instructions.as_deref());
                let (launch, session_id, account) = match crate::agent::resume_in_with_bot_mcp(
                    &effective_profile,
                    s.permission,
                    session,
                    state.as_deref(),
                    resume_id.as_deref(),
                    account.as_ref(),
                    bot_launch.as_ref().map(|_| cwd_path.as_path()),
                    bot_launch.as_ref().map(|bl| &bl.mcp_cfg),
                    instructions,
                ) {
                    Ok(res) => res,
                    Err(e) => {
                        self.discard_settings(session);
                        revoke_bot_grant(&mut bot_launch);
                        return Err(os(&e));
                    }
                };
                if canonical == "agy"
                    && account.as_ref().is_none_or(|a| a.dir.is_empty())
                    && let Err(err) = preflight_agy_background_launch(&launch.env)
                {
                    self.discard_settings(session);
                    revoke_bot_grant(&mut bot_launch);
                    return Err(os(&err));
                }
                let metadata = control::AgentLaunch {
                    profile: canonical.to_owned(),
                    permission: s.permission,
                };
                if let Err(e) = self
                    .agents
                    .remember_session(session, &metadata, session_id, c.cwd.clone(), account)
                    .and_then(|()| match instructions {
                        Some(text) => self.agents.remember_bot_instructions(session, text),
                        None => Ok(()),
                    })
                {
                    self.discard_settings(session);
                    self.agents.forget(session);
                    revoke_bot_grant(&mut bot_launch);
                    return Err(os(&e));
                }
                launch
            }
            None => match c.program {
                Program::Login => crate::agent::login_in(state.as_deref()),
                Program::Custom { program, args } => Shell::Custom { program, args }.launch(),
                Program::Agent { id } => {
                    let (canonical, profile, permission) =
                        match self.resolve_agent_profile_with_overrides(&id) {
                            Ok(resolved) => resolved,
                            Err(e) => {
                                revoke_bot_grant(&mut bot_launch);
                                return Err(e);
                            }
                        };
                    let account = match crate::agent::resume_target(
                        &profile,
                        state.as_deref(),
                        c.account.as_deref(),
                        None,
                        fallback_sid.as_deref(),
                    ) {
                        Ok(a) => a,
                        Err(e) => {
                            revoke_bot_grant(&mut bot_launch);
                            return Err(os(&e));
                        }
                    };
                    crate::agent::pre_trust_workspace(&profile, &c.cwd, account.as_ref());
                    let (launch, session_id, account) = match crate::agent::resume_in_with_bot_mcp(
                        &profile,
                        permission,
                        session,
                        state.as_deref(),
                        fallback_sid.as_deref(),
                        account.as_ref(),
                        bot_launch.as_ref().map(|_| cwd_path.as_path()),
                        bot_launch.as_ref().map(|bl| &bl.mcp_cfg),
                        bot_launch.as_ref().map(|bl| bl.instructions.as_str()),
                    ) {
                        Ok(res) => res,
                        Err(e) => {
                            self.discard_settings(session);
                            revoke_bot_grant(&mut bot_launch);
                            return Err(os(&e));
                        }
                    };
                    if canonical == "agy"
                        && account.as_ref().is_none_or(|a| a.dir.is_empty())
                        && let Err(err) = preflight_agy_background_launch(&launch.env)
                    {
                        self.discard_settings(session);
                        revoke_bot_grant(&mut bot_launch);
                        return Err(os(&err));
                    }
                    let metadata = control::AgentLaunch {
                        profile: canonical,
                        permission,
                    };
                    if let Err(e) = self
                        .agents
                        .remember_session(session, &metadata, session_id, c.cwd.clone(), account)
                        .and_then(|()| match bot_launch.as_ref() {
                            Some(bl) => self
                                .agents
                                .remember_bot_instructions(session, &bl.instructions),
                            None => Ok(()),
                        })
                    {
                        self.discard_settings(session);
                        revoke_bot_grant(&mut bot_launch);
                        return Err(os(&e));
                    }
                    launch
                }
            },
        };
        if let Some(ref bl) = bot_launch {
            env.extend(bl.extra_env.clone());
        }
        if let Some(store) = &self.store {
            // holder Spawn 전에 연결을 남겨 데몬이 응답 전에 죽어도 이력을 잇는다.
            if let Err(e) = store.inherit_history(session, old) {
                self.discard_settings(session);
                self.agents.forget(session);
                revoke_bot_grant(&mut bot_launch);
                return Err(os(&e));
            }
        }
        let (session, pid) =
            match self.spawn_holder(holder, session, argv, env, c.cwd, c.cols, c.rows) {
                Ok(spawned) => spawned,
                Err(e) => {
                    if let Some(store) = &self.store {
                        store.discard_inherited_history(session);
                    }
                    revoke_bot_grant(&mut bot_launch);
                    return Err(e);
                }
            };
        // Rebind bot ownership BEFORE `self.remove(old)` so old's removal never drops the
        // resurrected session from `runtime-bindings.json`.
        if let (Some(bots), Some(bl)) = (bots_mgr.as_ref(), bot_launch.take()) {
            if let Some(rebound) = bots.rebind_session(old, session, pid, bl.grant_token.clone()) {
                bots.revoke_launch_token(rebound.previous_token.as_ref());
            } else {
                bots.adopt_resumed_session(
                    bl.bot_id,
                    session,
                    pid,
                    bl.grant_token,
                    bl.created_at_ms,
                );
            }
        }
        let (has_old, exited_old) = {
            let mut st = lock(&self.state);
            st.resumed.insert(old);
            (st.sessions.contains_key(&old), st.exited.contains_key(&old))
        };
        if has_old && exited_old {
            holder
                .request(|req| ToHolder::Release { req, id: old })
                .ok();
            self.remove(old);
        } else if !has_old {
            self.discard_settings(old);
            self.agents.forget(old);
        }
        self.clear_sleeping(old);
        Ok(Reply::Resumed { session, pid })
    }

    /// 같은 세션의 `Resume` 동시 요청을 직렬화한다. 먼저 잡은 쪽만 `Some(표시)`을 받는다.
    /// 표시는 `resume`이 끝날 때 풀린다. 진행 중이거나 겹치면 기다리지 않고 `None`이다.
    /// 기다리면 느린 holder 왕복 동안 다른 연결이 멈추고, 풀린 뒤에는 같은 세션을
    /// 두 번 띄우게 된다. 두 번째 요청은 `Failure::BadRequest`다(K-25).
    fn claim_resume(&self, old: SessionId) -> Option<ResumeClaim<'_>> {
        // 집합 접근만 직렬화한다. holder 왕복 동안 이 잠금을 보유하지 않는다.
        let mut busy = lock(&self.resuming);
        if busy.insert(old) {
            Some(ResumeClaim {
                busy: &self.resuming,
                id: old,
            })
        } else {
            None
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "holder Spawn 요청에 필요한 세션·프로세스·화면 인자를 한 번에 넘긴다"
    )]
    fn spawn_holder(
        &self,
        holder: &Holder,
        id: SessionId,
        argv: Vec<Vec<u8>>,
        env: Vec<(Vec<u8>, Vec<u8>)>,
        cwd: Vec<u8>,
        cols: u16,
        rows: u16,
    ) -> Result<(SessionId, u32), Failure> {
        self.spawn_holder_outcome(holder, id, argv, env, cwd, cols, rows)
            .map_err(|(error, _)| error)
    }

    /// Returns `(failure, definitively_not_spawned)` on error so callers can distinguish
    /// a definitive `FromHolder::Failed` rejection from uncertain spawn outcomes.
    #[expect(
        clippy::too_many_arguments,
        reason = "holder Spawn 요청에 필요한 세션·프로세스·화면 인자를 한 번에 넘긴다"
    )]
    fn spawn_holder_outcome(
        &self,
        holder: &Holder,
        id: SessionId,
        mut argv: Vec<Vec<u8>>,
        mut env: Vec<(Vec<u8>, Vec<u8>)>,
        mut cwd: Vec<u8>,
        cols: u16,
        rows: u16,
    ) -> Result<(SessionId, u32), (Failure, bool)> {
        use krill_proto::os::ffi::OsStrExt as _;
        sanitize_spawn_cwd(&mut cwd);
        let wrapped = is_wrapper_argv(&argv);
        env.extend(crate::agent::pane_env_in(id, self.state_dir().as_deref()));
        // daemon `Create`의 거대 cols·rows도 여기서 자른다(K-01, CLI만 막는다).
        let size = crate::term::clamp_size(WinSize { cols, rows });
        let mut initial_cwd = PathBuf::from(std::ffi::OsStr::from_bytes(&cwd));
        env.extend(crate::git::worktree::environment(&initial_cwd));
        crate::shell::dedupe_env(&mut env);
        // 자식은 Spawned 응답보다 먼저 hook을 보낼 수 있다. 루트·세션을 모두 등록하기 전에는
        // socket hook 적용과 spool 소거를 막아 최초 이벤트가 미귀속 상태로 ACK/폐기되지 않게 한다.
        // holder 응답 리더는 on_exited 워커와 분리돼 있어 이 잠금을 기다리지 않는다.
        let commit = lock(&self.hook_commit);
        let Ok(mut reply) = holder.request(|req| ToHolder::Spawn {
            req,
            id,
            argv: argv.clone(),
            env: env.clone(),
            cwd: cwd.clone(),
            size,
        }) else {
            self.discard_settings(id);
            self.agents.forget(id);
            return Err((Failure::NoHolder, false));
        };
        if matches!(
            reply,
            (FromHolder::Failed { errno, .. }, _)
                if missing_path_errno(errno)
        ) && fallback_spawn_on_enoent(&mut argv, &mut cwd, &mut env)
        {
            initial_cwd = PathBuf::from(std::ffi::OsStr::from_bytes(&cwd));
            let Ok(next_reply) = holder.request(|req| ToHolder::Spawn {
                req,
                id,
                argv,
                env,
                cwd,
                size,
            }) else {
                self.discard_settings(id);
                self.agents.forget(id);
                return Err((Failure::NoHolder, false));
            };
            reply = next_reply;
        }
        match reply {
            (FromHolder::Spawned { pid, .. }, Some(master)) => {
                let session = self.start(id, master, size).map_err(|e| {
                    // krilld가 master를 버렸으니 holder 것까지 놓으면 세션은 SIGHUP으로 끝난다.
                    tracing::warn!(
                        id = id.0,
                        kind = ?e.kind(),
                        errno = e.raw_os_error(),
                        wrapped,
                        "session start failed"
                    );
                    record_session_start_failed(
                        self.dir.as_deref(),
                        "daemon_start",
                        &format!("{:?}", e.kind()),
                        e.raw_os_error(),
                        wrapped,
                        file!(),
                        line!(),
                    );
                    holder.notify(|req| ToHolder::Release { req, id }).ok();
                    self.discard_settings(id);
                    self.agents.forget(id);
                    (os(&e), false)
                })?;
                if let Some(store) = &self.store {
                    store.remember_cwd(id, &initial_cwd);
                }
                let mut st = lock(&self.state);
                st.roots.insert(id, pid).ok();
                st.sessions.insert(
                    id,
                    Entry {
                        session,
                        pid,
                        cwd: initial_cwd,
                        wrapped,
                    },
                );
                Ok((id, pid))
            }
            (FromHolder::Failed { errno, .. }, _) => {
                // spawn 실패: 세션이 없으니 주입 settings만 지우고 만들다 만 계보 기록도 걷는다.
                let kind = session_start_failure_kind(errno);
                tracing::warn!(
                    id = id.0,
                    errno,
                    kind,
                    wrapped,
                    "holder session spawn failed"
                );
                lock(&self.state).roots.remove(id);
                self.discard_settings(id);
                self.agents.forget(id);
                drop(commit);
                record_session_start_failed(
                    self.dir.as_deref(),
                    "holder_spawn",
                    kind,
                    Some(errno),
                    wrapped,
                    file!(),
                    line!(),
                );
                Err((Failure::from_holder_errno(errno), true))
            }
            _ => {
                tracing::warn!(id = id.0, wrapped, "unexpected holder spawn reply");
                holder.notify(|req| ToHolder::Release { req, id }).ok();
                lock(&self.state).roots.remove(id);
                self.discard_settings(id);
                self.agents.forget(id);
                drop(commit);
                record_session_start_failed(
                    self.dir.as_deref(),
                    "holder_reply",
                    "UnexpectedReply",
                    Some(libc::EIO),
                    wrapped,
                    file!(),
                    line!(),
                );
                Err((Failure::Os(libc::EIO), false))
            }
        }
    }

    fn emit_telemetry(&self, event: krill_proto::telemetry::Event) {
        struct LogTransport;
        impl krill_proto::telemetry::Transport for LogTransport {
            fn send(&mut self, body: &str) {
                tracing::debug!(telemetry = body, "telemetry event emitted");
            }
        }
        let mut transport = LogTransport;
        self.emit_telemetry_to(&krill_proto::telemetry::Env::live(), event, &mut transport);
    }

    fn emit_telemetry_to(
        &self,
        env: &krill_proto::telemetry::Env,
        event: krill_proto::telemetry::Event,
        transport: &mut impl krill_proto::telemetry::Transport,
    ) -> bool {
        let consent = krill_proto::telemetry::consent(self.dir.as_deref(), env);
        krill_proto::telemetry::emit(
            &consent,
            event,
            krill_proto::telemetry::now_unix(),
            transport,
        )
    }

    /// Persist a reservation before settings, launch metadata, or holder Spawn.
    fn reserve_session_id(&self, prior: Option<SessionId>) -> io::Result<SessionId> {
        let floor = lock(&self.state)
            .sessions
            .keys()
            .map(|id| id.0)
            .max()
            .unwrap_or_default();
        lock(&self.ids).reserve_after(prior, floor)
    }

    fn list(&self, peer: &Peer) -> Reply {
        // An unconfirmed account replacement is published as its original's successor, never as a
        // separate session; account snapshots reveal it at the same moment.
        let replacements = lock(&self.account_replacements).clone();
        let st = lock(&self.state);
        Reply::Sessions(
            st.sessions
                .iter()
                .filter(|(session, _)| peer.scope.full() && !replacements.contains(session))
                .map(|(&session, e)| Info {
                    session,
                    pid: e.pid,
                    status: st.exited.get(&session).copied(),
                })
                .collect(),
        )
    }

    /// 세션별 실행 계정 표시용 목록(OWE-707). OWE-698 `StoredLaunch::account`를
    /// 읽어 범위 내 세션만 담는다. 셸·구 기록·라벨 없는 세션은 빼 배지를 숨긴다.
    /// PTY에는 손대지 않는다. 데몬 재시작 뒤에도 디스크 기록에서 복원된다.
    fn session_accounts(&self, peer: &Peer) -> Reply {
        use crate::usage::accounts::{default_home, load_file};
        let home = default_home();
        let state = self.state_dir();
        let file = load_file(&home, state.as_deref());
        let ids: Vec<SessionId> = {
            let st = lock(&self.state);
            st.sessions
                .keys()
                .copied()
                .filter(|_| peer.scope.full())
                .collect()
        };
        let mut out = Vec::new();
        for session in ids {
            let stored = self.agents.stored(session).ok().flatten();
            let Some(launch) = stored.and_then(|s| s.account) else {
                continue;
            };
            if launch.id.is_empty() && launch.dir.is_empty() {
                continue;
            }
            let label = file
                .accounts
                .iter()
                .find(|a| a.id == launch.id)
                .map(|a| a.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| launch.id.clone());
            if label.is_empty() {
                continue;
            }
            out.push(SessionAccount {
                session,
                account_id: launch.id,
                label,
            });
        }
        out.sort_by_key(|a| a.session);
        Reply::SessionAccounts(out)
    }

    fn states(&self, peer: &Peer) -> Reply {
        let sessions: Vec<_> = {
            let st = lock(&self.state);
            st.sessions
                .iter()
                .filter(|_| peer.scope.full())
                .map(|(&session, e)| {
                    let tap = e.session.tap();
                    // Reader completion also occurs during daemon handoff. Only the
                    // published EOF flag (or holder exit) proves the terminal ended.
                    let exited = st.exited.contains_key(&session) || tap.pulse().closed;
                    (session, e.pid, exited, tap, e.wrapped)
                })
                .collect()
        };
        Reply::States(
            sessions
                .into_iter()
                .map(|(session, root, exited, tap, wrapped)| SessionStatus {
                    session,
                    status: self.resolve_session_status(session, root, exited, &tap, wrapped),
                })
                .collect(),
        )
    }

    fn resolve_session_status(
        &self,
        session: SessionId,
        root: u32,
        exited: bool,
        tap: &crate::session::Tap,
        wrapped: bool,
    ) -> control::AgentStatus {
        self.resolve_session_status_inner(session, root, exited, tap, wrapped, AgyLookup::Sync)
    }

    fn resolve_session_status_inner(
        &self,
        session: SessionId,
        root: u32,
        exited: bool,
        tap: &crate::session::Tap,
        wrapped: bool,
        agy_lookup: AgyLookup<'_>,
    ) -> control::AgentStatus {
        let mut status = self.status.get(session);
        // EOF/holder 종료는 저장된 hook보다 우선한다. 종료 콜백이 늦거나
        // 더 최신 시각의 hook이 도착해도 끝난 세션을 Done으로 되살리지 않는다.
        if exited {
            status.state = control::AgentState::Exited;
            status.source = Some(control::Source::Process);
            status.confidence = control::Confidence::High;
            status.subagents.clear();
        }
        // Pi has no hook identity. Its executable may be reported as `node`,
        // and its terminal title can be renamed. Keep the explicit launch identity.
        if status.agent.is_empty()
            && let Ok(Some(launch)) = self.agents.launch(session)
            && launch.profile == "pi"
        {
            status.agent = launch.profile;
        }
        status.foreground = status::session_foreground(root, tap.foreground(), exited);
        status.title = tap.title();
        // Only a launch with no agent evidence yet can still be in shell startup.
        status.startup_stall = (wrapped
            && !exited
            && status.state == control::AgentState::Unknown
            && status.source.is_none()
            && status
                .foreground
                .as_deref()
                .is_none_or(|fg| !krill_proto::agent::is_known_agent_process(fg)))
        .then(|| status::startup::startup_stall(root))
        .flatten();
        status.root_shell = (!exited)
            .then(|| crate::sys::process_name(root).ok())
            .flatten()
            .and_then(|program| {
                krill_proto::control::RootShell::new(
                    if cfg!(windows) {
                        krill_proto::control::ShellOs::Windows
                    } else {
                        krill_proto::control::ShellOs::Unix
                    },
                    program,
                )
            })
            .map(Box::new);
        let is_agy_fg = status.agent.eq_ignore_ascii_case("agy")
            || status.agent.eq_ignore_ascii_case("antigravity")
            || status.foreground.as_deref().is_some_and(|fg| {
                fg.eq_ignore_ascii_case("agy") || fg.eq_ignore_ascii_case("antigravity")
            });
        let stored = if is_agy_fg || status.agent.is_empty() {
            self.agents.stored(session).ok().flatten()
        } else {
            None
        };
        // Hook 실행 전(기동 직후·계정 전환·재개·Pi 등)에도 저장된 런치 프로필이 있으면 에이전트 정체성을 유지한다.
        if status.agent.is_empty()
            && let Some(s) = &stored
            && !s.profile.is_empty()
        {
            krill_proto::agent::canonical_id(&s.profile).clone_into(&mut status.agent);
        }
        let is_agy_stored = stored.as_ref().is_some_and(|s| {
            s.profile.eq_ignore_ascii_case("agy") || s.profile.eq_ignore_ascii_case("antigravity")
        });
        if is_agy_fg || is_agy_stored {
            if status.agent.is_empty() {
                "agy".clone_into(&mut status.agent);
            }
            let (title, discovered) = match agy_lookup {
                AgyLookup::Nonblocking(agy_jobs) => {
                    let cached = crate::agent_history::vault::cached_live_agy_title(
                        session,
                        stored.as_ref(),
                    );
                    let needs_binding = cached.discovered.is_some()
                        && stored.as_ref().is_some_and(|s| {
                            s.session_id.is_none()
                                || s.transcript_path.is_none()
                                || s.transcript_path
                                    .as_deref()
                                    .is_some_and(crate::agent::control::is_agy_conversation_db_path)
                        });
                    if (cached.stale || needs_binding)
                        && let Some(jobs) = agy_jobs
                    {
                        jobs.push(AgyRefreshJob {
                            session,
                            root,
                            tap_fg: tap.foreground(),
                            exited,
                            stored: stored.clone(),
                        });
                    }
                    (cached.title, cached.discovered)
                }
                AgyLookup::Sync => {
                    let agy_fg =
                        account_switch::effective_foreground_pid(root, tap.foreground(), exited);
                    let (title, discovered) = crate::agent_history::vault::resolve_live_agy_title(
                        session,
                        root,
                        agy_fg,
                        exited,
                        stored.as_ref(),
                    );
                    if let Some((ref conv_id, ref tp)) = discovered {
                        if tp.is_some() {
                            self.agents
                                .bind_discovered_session(session, conv_id, tp.as_deref());
                        } else if let Ok(Some(found)) = account_switch::identity::discover(
                            krill_proto::usage::ProviderKind::Antigravity,
                            root,
                            agy_fg,
                        ) {
                            self.agents.bind_discovered_session(
                                session,
                                &found.id,
                                Some(&found.path),
                            );
                        } else {
                            self.agents.bind_discovered_session(session, conv_id, None);
                        }
                    }
                    (title, discovered)
                }
            };
            if !exited
                && status.state == control::AgentState::Unknown
                && (title.is_some()
                    || discovered.is_some()
                    || stored.as_ref().is_some_and(|s| s.session_id.is_some()))
            {
                status.state = control::AgentState::Done;
            }
            let title = title
                .as_deref()
                .and_then(krill_proto::control::display_title);
            if title.is_some() || status.title.is_none() {
                status.title = title;
            }
        }
        status
    }

    /// 소켓으로 온 hook 이벤트. 세션 후손이 보냈으면 그 세션만, 외부에서 왔으면 `ppid` 계보의 세션.
    fn on_hook(&self, peer: &Peer, e: &HookEvent) -> io::Result<Option<SessionId>> {
        let _commit = lock(&self.hook_commit);
        let target = {
            let st = lock(&self.state);
            let id = match peer.scope {
                Scope::Session(id) => Some(id),
                Scope::Full => scope::session_of(e.ppid, &st.roots),
                Scope::Restricted => None,
            };
            id.and_then(|id| {
                st.sessions
                    .get(&id)
                    .map(|entry| (id, entry.cwd.clone(), entry.session.tap()))
            })
        };
        if let Some((id, cwd, tap)) = target {
            if self.apply_agent_hook(id, e, Some(&cwd), Some(&tap))? {
                Ok(Some(id))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// OWE-737: 첫 프롬프트 hook(`UserPromptSubmit` 등)이 오면 세션 자동 제목을 기록하고,
    /// 현재 디렉터리가 기본 이름(`worktree-1`, `claude-1` 등)의 git 브랜치이면 슬러그로 개명한다.
    /// 또한 매 턴 프롬프트 히스토리를 `session-prompts.json`에 기록한다.
    pub(crate) fn maybe_rename_from_hook(
        &self,
        id: SessionId,
        payload: &[u8],
        event_id: hook::EventId,
    ) {
        use krill_proto::os::ffi::OsStrExt as _;
        if self.agents.is_subagent_or_foreign_hook(id, payload) {
            return;
        }
        let Some(prompt) = krill_proto::git::extract_prompt_from_hook_payload(payload) else {
            return;
        };
        let state_dir = self.state_dir();
        if let Some(dir) = state_dir.as_deref() {
            krill_proto::git::record_session_prompt_from_hook(dir, id, payload, event_id).ok();
        }
        self.voice().observe_prompt(
            krill_proto::voice::VoiceTargetKind::Session,
            &id.0.to_string(),
            &prompt,
            self.session_cwd(id),
        );
        if !krill_proto::git::is_auto_rename_enabled(state_dir.as_deref()) {
            return;
        }
        let Some(slug) = krill_proto::git::prompt_branch_slug(&prompt) else {
            return;
        };
        if let Some(dir) = state_dir.as_deref()
            && !krill_proto::git::record_session_auto_title(dir, id, &slug).unwrap_or(false)
        {
            return;
        }
        let cwd = self
            .session_cwd(id)
            .or_else(|| {
                self.agents
                    .stored(id)
                    .ok()
                    .flatten()
                    .map(|s| PathBuf::from(std::ffi::OsStr::from_bytes(&s.cwd)))
            })
            .filter(|p| !p.as_os_str().is_empty());
        if let (Some(cwd), Ok(q)) = (cwd, crate::git::queue()) {
            crate::git::worktree::rename_branch_from_prompt(q, &cwd, &prompt).ok();
        }
    }

    fn apply_agent_hook(
        &self,
        id: SessionId,
        e: &HookEvent,
        cwd: Option<&Path>,
        tap: Option<&Tap>,
    ) -> io::Result<bool> {
        self.apply_agent_hook_in(&crate::usage::accounts::default_home(), id, e, cwd, tap)
    }

    fn apply_agent_hook_in(
        &self,
        home: &Path,
        id: SessionId,
        e: &HookEvent,
        cwd: Option<&Path>,
        tap: Option<&Tap>,
    ) -> io::Result<bool> {
        use krill_proto::os::ffi::OsStrExt as _;
        let cwd_hint = cwd.map(|cwd| cwd.as_os_str().as_bytes());
        let root = lock(&self.state).sessions.get(&id).map(|entry| entry.pid);
        let origin = root.map_or(hook_identity::Origin::Unverified, |root| {
            hook_identity::origin(root, tap.and_then(Tap::foreground), e)
        });
        let is_subagent_or_foreign = match origin {
            hook_identity::Origin::Foreground => self
                .agents
                .record_foreground_hook_session(id, &e.payload, &e.agent, cwd_hint),
            hook_identity::Origin::Auxiliary => {
                self.agents.block_hibernation(id);
                true
            }
            hook_identity::Origin::Unverified => self.agents.record_hook_session_with_context(
                id,
                &e.payload,
                Some(&e.agent),
                cwd_hint,
            ),
        };
        if !is_subagent_or_foreign
            && let Ok(Some(stored)) = self.agents.stored(id)
            && stored.permission == krill_proto::agent::Permission::Manual
            && self.live_agent_has_yolo_args(id, &stored.profile)
        {
            let yolo = crate::agent::control::StoredLaunch {
                permission: krill_proto::agent::Permission::Yolo,
                ..stored
            };
            self.agents.bind_switch_identity(id, &yolo).ok();
        }
        self.status.subagent_hook(id, e, !is_subagent_or_foreign);
        let allow_status_hook = !is_subagent_or_foreign
            || crate::agent::control::Control::is_explicit_subagent_hook(&e.payload);
        if allow_status_hook && self.status.hook(&[(id, e)])? > 0 {
            let status = self.status.get(id);
            if status.state != control::AgentState::Done {
                lock(&self.orch_mail_notified_done).retain(|(sid, _, _), _| *sid != id.0);
            }
            if status.state == control::AgentState::Working {
                self.agents.clear_rate_limited(id);
            }
            if status.state == control::AgentState::Done
                && let Some(tap) = tap
            {
                tap.agent_done(status.since_unix_ns);
            }
        }
        // 한도 도달이면 실행 당시 계정(OWE-698 기록)을 RateLimited로 기록해
        // UI 한도 배지·만료 시각·채팅 한도 배너에 반영한다.
        if status::stop_failure_rate_limited(&e.payload) || status::agy_rate_limited(&e.payload) {
            if !is_subagent_or_foreign {
                self.agents.mark_rate_limited(id);
            }
            let stored = self.agents.stored(id).ok().flatten();
            let event_provider = krill_proto::usage::ProviderKind::parse(&e.agent);
            let provider = stored
                .as_ref()
                .and_then(|s| {
                    s.account
                        .as_ref()
                        .and_then(|launch| {
                            krill_proto::usage::ProviderKind::from_env_var(&launch.env_key)
                        })
                        .or_else(|| self.provider_for_profile(&s.profile))
                        .or_else(|| krill_proto::usage::ProviderKind::parse(&s.profile))
                })
                .or(event_provider);
            if let Some(provider) = provider
                && (!is_subagent_or_foreign
                    || event_provider.is_none_or(|event_p| event_p == provider))
            {
                let account_id = self.resolve_session_account_id(home, provider, stored.as_ref());
                crate::usage::accounts::record_rate_limited(provider, &account_id);
            }
        }
        Ok(!is_subagent_or_foreign)
    }

    /// Slow live-process identity discovery (open vnodes, `/bin/ps`, transcript walks).
    /// Must run after releasing `hook_commit` / spool lock and after `hook::ack()`.
    fn discover_hook_identity(&self, id: SessionId, e: &HookEvent) {
        self.discover_hook_identity_in(&crate::usage::accounts::default_home(), id, e);
    }

    fn discover_hook_identity_in(&self, home: &Path, id: SessionId, e: &HookEvent) {
        let Ok(Some(stored)) = self.agents.stored(id) else {
            return;
        };
        if stored.session_id.is_some() && stored.transcript_path.is_some() {
            return;
        }
        let Some((root_pid, tap)) = ({
            let st = lock(&self.state);
            (!st.exited.contains_key(&id))
                .then(|| {
                    st.sessions
                        .get(&id)
                        .map(|entry| (entry.pid, entry.session.tap()))
                })
                .flatten()
        }) else {
            return;
        };
        if root_pid <= 1 {
            return;
        }
        let fg_pid = (e.ppid > 1).then_some(e.ppid).or_else(|| {
            account_switch::effective_foreground_pid(root_pid, tap.foreground(), false)
        });
        let provider = krill_proto::usage::ProviderKind::parse(&stored.profile)
            .or_else(|| krill_proto::usage::ProviderKind::parse(&e.agent));
        let Some(provider) = provider else {
            return;
        };
        let discovered = self
            .discover_live_identity(provider, home, Some(&stored), root_pid, fg_pid)
            .ok()
            .flatten()
            .map(|found| (found.id, found.path))
            .or_else(|| {
                (provider == krill_proto::usage::ProviderKind::Antigravity).then(|| {
                    crate::agent_history::vault::discover_live_agy_identity(root_pid, fg_pid)
                        .or_else(|| crate::agent_history::vault::cached_live_agy_identity(id))
                        .map(|(_, conv_id, path)| (conv_id, path))
                })?
            });
        if let Some((conv_id, path)) = discovered {
            let _commit = lock(&self.hook_commit);
            let still_live = {
                let st = lock(&self.state);
                !st.exited.contains_key(&id)
                    && st
                        .sessions
                        .get(&id)
                        .is_some_and(|entry| entry.pid == root_pid)
            };
            if still_live {
                self.agents
                    .bind_discovered_session(id, &conv_id, Some(&path));
                self.agents.observe_account_resume(id, &conv_id);
            }
        }
    }

    fn close(&self, peer: &Peer, id: SessionId) -> Result<Reply, Failure> {
        if !peer.authorize("close", id, &self.audit) {
            return Err(Failure::Denied);
        }
        let holder = self.holder.as_ref().ok_or(Failure::NoHolder)?;
        {
            let mut st = lock(&self.state);
            if !st.sessions.contains_key(&id) {
                return Err(Failure::NoSession);
            }
            if !st.exited.contains_key(&id) {
                // 끝나면 on_exited가 지운다. 잠금 안에서 표시해 그사이 온 Exited를 놓치지 않는다.
                let first = st.closing.insert(id);
                drop(st);
                self.cancel_queued_account_switch(id);
                let end = |req| {
                    if holder.version() >= TERMINATE_SINCE {
                        ToHolder::Terminate { req, id }
                    } else {
                        ToHolder::Kill {
                            req,
                            id,
                            signal: SIGHUP,
                        }
                    }
                };
                match holder.request(end) {
                    // ESRCH: 그사이 끝났다. Exited가 먼저 와서 on_exited가 지운다.
                    Ok((FromHolder::Done { .. }, _)) => {
                        self.mark_closed(id);
                        return Ok(Reply::Closed);
                    }
                    Ok((FromHolder::Failed { errno, .. }, _)) if errno == libc::ESRCH => {
                        self.mark_closed(id);
                        return Ok(Reply::Closed);
                    }
                    r => {
                        let mut st = lock(&self.state);
                        if !st.sessions.contains_key(&id) {
                            return Ok(Reply::Closed);
                        }
                        if !st.exited.contains_key(&id) {
                            if first {
                                st.closing.remove(&id);
                            }
                            return Err(match r {
                                Ok((FromHolder::Failed { errno, .. }, _)) => Failure::Os(errno),
                                Ok(_) => Failure::Os(libc::EIO),
                                Err(_) => Failure::NoHolder,
                            });
                        }
                    }
                }
            }
        }
        // 이미 끝난 세션: holder가 보관하던 master·상태를 놓게 한다. holder가 없거나 이미 놓았어도 지운다.
        if !lock(&self.ids).ensure_ready() {
            // An exited holder record may be the last evidence of a legacy unrecorded ID.
            // Termination remains allowed; defer only releasing its inventory entry.
            self.mark_closed(id);
            return Ok(Reply::Closed);
        }
        holder.request(|req| ToHolder::Release { req, id }).ok();
        self.mark_closed(id);
        self.remove(id);
        Ok(Reply::Closed)
    }

    fn mark_closed(&self, id: SessionId) {
        lock(&self.state).closed.insert(id);
        self.closed_ledger.record_closed(id);
        if let Some(store) = &self.store {
            store.mark_closed(id);
        }
    }

    fn is_closed(&self, id: SessionId) -> bool {
        lock(&self.state).closed.contains(&id)
            || self.closed_ledger.contains(id)
            || self.store.as_ref().is_some_and(|s| s.is_closed(id))
    }

    fn closed_sidebar_ranges(&self) -> krill_proto::control::ClosedSessionRanges {
        let mut ranges = self.closed_ledger.snapshot();
        {
            let st = lock(&self.state);
            for &id in &st.closed {
                ranges.insert(id);
            }
            for &id in st.sessions.keys() {
                ranges.remove(id);
            }
            for &id in &st.unregistered_kept {
                ranges.remove(id);
            }
        }
        {
            let handoffs = lock(&self.account_handoffs);
            let max_hops = handoffs.len().saturating_add(1);
            for (&from, h) in handoffs.iter() {
                if h.to.is_none() && h.error.is_none() {
                    ranges.remove(from);
                } else if let Some(mut target) = h.to {
                    for _ in 0..max_hops {
                        let Some(next) = handoffs.get(&target).and_then(|nh| nh.to) else {
                            break;
                        };
                        if next == target {
                            break;
                        }
                        target = next;
                    }
                    if !ranges.contains(target) {
                        ranges.remove(from);
                    }
                }
            }
        }
        ranges
    }

    /// holder `Exited` 송신 큐의 워커. 응답 리더와 분리해 `Release`가 exec 잠금을 기다려도 응답을 계속 읽는다.
    /// holder를 잃은 뒤에는 [`crate::rejoin`]의 종료 감시가 부른다.
    pub(crate) fn on_exited(&self, id: SessionId, status: i32) {
        let commit = lock(&self.hook_commit);
        let closing = {
            let mut st = lock(&self.state);
            st.exited.insert(id, status);
            st.closing.contains(&id)
        };
        self.status.exited(id);
        drop(commit);
        if !closing {
            return;
        }
        if !lock(&self.ids).ensure_ready() {
            self.mark_closed(id);
            return;
        }
        // `Release`를 링크에 쓴 뒤에 지운다. 목록에서 사라진 뒤 krilld가 죽어도 holder는 소켓에 남은 `Release`를
        // 먼저 읽고 끝난 세션을 다음 krilld에 다시 넘기지 않는다(OWE-502).
        if let Some(h) = &self.holder {
            h.notify(|req| ToHolder::Release { req, id }).ok();
        }
        self.mark_closed(id);
        self.remove(id);
    }

    /// 자기 상태 폴더만 쓴다. 폴더 없는 레지스트리(단독 테스트)가 운영 폴더로 넘어가지 않게 기본값으로 폴백하지 않는다.
    pub fn state_dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    /// 상태 디렉터리가 있으면 세션 `id`의 Claude 주입 settings를 지운다.
    fn discard_settings(&self, id: SessionId) {
        if let Some(dir) = self.state_dir() {
            crate::agent::claude::discard(&dir, id);
        }
    }

    /// 레지스트리에서 지운다. PTY 리더는 남은 출력을 EOF까지 읽어 기록한 뒤 master를 닫는다.
    /// 지운 세션의 Claude 주입 settings도 함께 지운다(살아 있는 세션의 파일은 남는다).
    fn remove(&self, id: SessionId) {
        let _commit = lock(&self.hook_commit);
        let ids_durable = lock(&self.ids).ensure_ready();
        if ids_durable {
            self.discard_settings(id);
            self.agents.forget(id);
        }
        let mut st = lock(&self.state);
        st.sessions.remove(&id);
        st.exited.remove(&id);
        st.closing.remove(&id);
        if ids_durable {
            st.roots.remove(id);
        }
        drop(st);
        self.status.remove(id);
        lock(&self.orch_mail_notified_done).retain(|(sid, _, _), _| *sid != id.0);
        self.clear_sleeping(id);
        self.forget_failed_account_handoff(id, ids_durable);
        if let Some(bots) = self.bots_if_initialized() {
            bots.on_session_removed(id);
        }
    }

    #[cfg(all(test, unix))]
    fn on_usage(
        &self,
        peer: &Peer,
        role: Role,
        req: krill_proto::usage::UsageRequest,
    ) -> krill_proto::usage::UsageReply {
        self.on_usage_negotiated(peer, role, req, ipc::FEATURES)
    }

    fn on_usage_negotiated(
        &self,
        peer: &Peer,
        role: Role,
        req: krill_proto::usage::UsageRequest,
        features: u64,
    ) -> krill_proto::usage::UsageReply {
        self.on_usage_negotiated_in(
            peer,
            role,
            req,
            features,
            &crate::usage::accounts::default_home(),
        )
    }

    fn on_usage_negotiated_in(
        &self,
        peer: &Peer,
        role: Role,
        req: krill_proto::usage::UsageRequest,
        features: u64,
        home: &Path,
    ) -> krill_proto::usage::UsageReply {
        use crate::usage::accounts;
        use krill_proto::usage::{UsageReply, UsageRequest};

        if !peer.scope.full() {
            peer.deny("usage", &self.audit);
            return UsageReply::Failed(Failure::Denied);
        }
        if req.contains_removed_provider() {
            return UsageReply::Failed(Failure::BadRequest);
        }
        if role == Role::Observer && !req.is_read() {
            return UsageReply::Failed(Failure::Denied);
        }
        let home = home.to_path_buf();
        let state = self.state_dir();
        let state_ref = state.as_deref();
        let switch_control = krill_proto::usage::FEAT_ACCOUNT_SWITCH_CONTROL
            | krill_proto::usage::FEAT_PROVIDER_DETAILS;
        let mut reply = match req {
            UsageRequest::SwitchAccountOnly { .. }
            | UsageRequest::InspectSessionSwitches { .. }
            | UsageRequest::SwitchSelectedSessions { .. }
                if features & switch_control != switch_control =>
            {
                UsageReply::Failed(Failure::BadRequest)
            }
            UsageRequest::AntigravityAccounts(_)
                if features & (switch_control | krill_proto::usage::FEAT_USAGE_AGY)
                    != (switch_control | krill_proto::usage::FEAT_USAGE_AGY) =>
            {
                UsageReply::Failed(Failure::BadRequest)
            }
            UsageRequest::AntigravityAccounts(op) => UsageReply::AntigravityAccounts(
                accounts::agy_native::handle_op(&home, state_ref, &op),
            ),
            // Default only: running sessions keep their account until the user picks them.
            UsageRequest::SwitchAccountOnly {
                provider,
                account_id,
            } => match accounts::switch_account(&home, state_ref, provider, &account_id) {
                Ok(snap) => UsageReply::Accounts(snap),
                Err(e) => UsageReply::Failed(os(&e)),
            },
            UsageRequest::InspectSessionSwitches {
                provider,
                previous_account_id,
                target_account_id,
            } => {
                let snapshot = accounts::refresh_accounts(&home, state_ref);
                match self.inspect_session_switches_in(
                    &home,
                    provider,
                    previous_account_id,
                    target_account_id,
                    &snapshot,
                ) {
                    Ok(plan) => UsageReply::SessionSwitchPlan(plan),
                    Err(error) => UsageReply::Failed(error),
                }
            }
            UsageRequest::SwitchSelectedSessions {
                provider,
                account_id,
                sessions,
                interrupt_busy,
            } => {
                let snapshot = accounts::refresh_accounts(&home, state_ref);
                match self.queue_selected_account_switches_in(
                    &sessions,
                    provider,
                    &account_id,
                    interrupt_busy,
                    &snapshot,
                    &home,
                ) {
                    Ok(()) => UsageReply::Accounts(snapshot),
                    Err(error) => UsageReply::Failed(error),
                }
            }
            UsageRequest::ImportOrcaAntigravity {
                source_dir,
                dry_run,
            } => {
                match accounts::orca_vault::import(
                    &home,
                    state_ref,
                    Path::new(&source_dir),
                    dry_run,
                ) {
                    Ok(snap) => {
                        if !dry_run
                            && features & krill_proto::usage::FEAT_PROVIDER_DETAILS != 0
                            && let Some(active) = snap.accounts.iter().find(|a| {
                                a.provider == krill_proto::usage::ProviderKind::Antigravity
                                    && a.active
                            })
                        {
                            self.queue_account_switch(active.provider, &active.id);
                        }
                        UsageReply::Accounts(snap)
                    }
                    Err(e) => {
                        tracing::warn!(kind = ?e.kind(), "Orca account vault import failed");
                        UsageReply::Failed(os(&e))
                    }
                }
            }
            UsageRequest::SetProvisioning(enabled) => {
                match accounts::set_provisioning(&home, state_ref, enabled) {
                    Ok(enabled) => UsageReply::Provisioning(enabled),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
            UsageRequest::GetProvisioning => match accounts::provisioning(&home, state_ref) {
                Ok(enabled) => UsageReply::Provisioning(enabled),
                Err(e) => UsageReply::Failed(os(&e)),
            },
            // 구버전 요청의 opcode는 유지한다. 사용량 자동 수집은 끌 수 없고, 프로필 자동전환은 제거됐다.
            UsageRequest::SetDirectUsage(_)
            | UsageRequest::SetAdvancedPolicy(_)
            | UsageRequest::SetAutoSwitch(_)
            | UsageRequest::UpsertAutoSwitchRule(_)
            | UsageRequest::RemoveAutoSwitchRule { .. } => {
                UsageReply::Accounts(accounts::refresh_accounts(&home, state_ref))
            }
            UsageRequest::Get => UsageReply::Current(accounts::primary_snapshot(&home, state_ref)),
            UsageRequest::ListAccounts => {
                if role != Role::Observer {
                    self.adopt_profile_names(&home, state_ref);
                }
                UsageReply::Accounts(accounts::refresh_accounts(&home, state_ref))
            }
            UsageRequest::CancelAccountSwitch { session } => {
                if features & krill_proto::usage::FEAT_ACCOUNT_CANCEL == 0 {
                    UsageReply::Failed(Failure::BadRequest)
                } else {
                    match self.cancel_failed_account_switch(session) {
                        Ok(()) => {
                            UsageReply::Accounts(accounts::refresh_accounts(&home, state_ref))
                        }
                        Err(error) => UsageReply::Failed(error),
                    }
                }
            }
            UsageRequest::RecoverAccountInput {
                session,
                account_id,
            } => {
                if features & krill_proto::usage::FEAT_ACCOUNT_INPUT_RECOVERY == 0 {
                    UsageReply::Failed(Failure::BadRequest)
                } else {
                    match self.recover_failed_account_input(session, &account_id) {
                        Ok(()) => {
                            UsageReply::Accounts(accounts::refresh_accounts(&home, state_ref))
                        }
                        Err(error) => UsageReply::Failed(error),
                    }
                }
            }
            UsageRequest::RefreshUsage => {
                crate::usage::direct::refresh_manual(&home, state_ref);
                UsageReply::Accounts(accounts::refresh_accounts(&home, state_ref))
            }
            UsageRequest::SwitchSessionAccount {
                session,
                provider,
                account_id,
            } => {
                let required = krill_proto::usage::FEAT_SESSION_ACCOUNT_SWITCH
                    | krill_proto::usage::FEAT_PROVIDER_DETAILS;
                if features & required == required {
                    let snapshot = accounts::refresh_accounts(&home, state_ref);
                    match self.queue_session_account_switch(
                        session,
                        provider,
                        &account_id,
                        &snapshot,
                    ) {
                        Ok(()) => UsageReply::Accounts(snapshot),
                        Err(error) => UsageReply::Failed(error),
                    }
                } else {
                    UsageReply::Failed(Failure::BadRequest)
                }
            }
            UsageRequest::SwitchAccount {
                provider,
                account_id,
            } => match accounts::switch_account(&home, state_ref, provider, &account_id) {
                Ok(snap) => {
                    if features & krill_proto::usage::FEAT_PROVIDER_DETAILS != 0 {
                        self.queue_account_switch(provider, &account_id);
                    }
                    UsageReply::Accounts(snap)
                }
                Err(e) => UsageReply::Failed(os(&e)),
            },
            UsageRequest::AddAccount {
                provider,
                label,
                email,
                profile_dir,
            } => {
                match accounts::add_account(&home, state_ref, provider, &label, email, &profile_dir)
                {
                    Ok(snap) => UsageReply::Accounts(snap),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
            UsageRequest::RemoveAccount {
                provider,
                account_id,
            } => match accounts::remove_account(&home, state_ref, provider, &account_id) {
                Ok(snap) => UsageReply::Accounts(snap),
                Err(e) => UsageReply::Failed(os(&e)),
            },
            UsageRequest::ConsumeResetCredit {
                provider,
                account_id,
            } => match accounts::consume_reset_credit(&home, state_ref, provider, &account_id) {
                Ok((snap, outcome, windows_reset)) => UsageReply::ResetConsumed {
                    snapshot: snap,
                    outcome,
                    windows_reset,
                },
                Err(e) => UsageReply::Failed(e.to_failure()),
            },
            UsageRequest::RenameAccount {
                provider,
                account_id,
                label,
                email,
            } => {
                match accounts::rename_account(
                    &home,
                    state_ref,
                    provider,
                    &account_id,
                    &label,
                    email,
                ) {
                    Ok(_) => {
                        // 로그인 직후 이메일이 확인되는 시점이라 임시 이름 폴더를 바로 옮긴다.
                        self.adopt_profile_names(&home, state_ref);
                        UsageReply::Accounts(accounts::refresh_accounts(&home, state_ref))
                    }
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
            UsageRequest::DeleteAccount {
                provider,
                account_id,
            } => {
                let in_use = self.profiles_in_use();
                match accounts::delete_account(&home, state_ref, provider, &account_id, &in_use) {
                    Ok(snap) => UsageReply::Accounts(snap),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
            UsageRequest::ListOrphanProfiles => {
                UsageReply::Orphans(accounts::orphan_profiles(&home, state_ref))
            }
            UsageRequest::PurgeOrphanProfiles => {
                let in_use = self.profiles_in_use();
                match accounts::purge_orphan_profiles(&home, state_ref, &in_use) {
                    Ok(left) => UsageReply::Orphans(left),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
        };
        match &mut reply {
            UsageReply::Accounts(snap) => self.account_switch_snapshot(snap),
            UsageReply::ResetConsumed { snapshot, .. } => {
                self.account_switch_snapshot(snapshot);
            }
            _ => {}
        }
        reply
    }

    /// 이메일이 확인된 임시 이름(`claude-code-profile-n`) 프로필 폴더를 이메일 이름으로 옮긴다.
    /// 세션이 쓴 폴더는 건너뛴다. 실패는 기록만 하고 요청 응답에는 영향을 주지 않는다.
    fn adopt_profile_names(&self, home: &Path, state: Option<&Path>) {
        use crate::usage::accounts;
        let in_use = self.profiles_in_use();
        if let Err(error) = accounts::adopt_email_dirs(home, state, &in_use, &accounts::Keychain) {
            tracing::debug!(kind = ?error.kind(), "profile rename unavailable");
        }
    }

    /// 레지스트리에 남은 세션(실행·휴면·종료 표시 포함)이 실행에 쓴 계정 프로필 폴더(OWE-704).
    /// 이 폴더는 계정 데이터 삭제·고아 정리에서 건드리지 않는다. 기록은 OWE-698 `StoredLaunch::account`.
    fn profiles_in_use(&self) -> Vec<PathBuf> {
        let ids: Vec<SessionId> = lock(&self.state).sessions.keys().copied().collect();
        ids.into_iter()
            .filter_map(|id| self.agents.stored(id).ok().flatten()?.account)
            .filter(|a| !a.dir.is_empty())
            .map(|a| PathBuf::from(a.dir))
            .collect()
    }

    /// 활성 세션 터미널이 현재 Bracketed Paste 모드(`?2004h`)를 켰는지 확인한다(OWE-944 / MK-241).
    pub(crate) fn session_bracketed_paste(&self, id: SessionId) -> bool {
        self.session_screen_and_bracketed_paste(id).1
    }

    pub(crate) fn agent_control(&self) -> &crate::agent::control::Control {
        &self.agents
    }

    pub(crate) fn mobile_repositories(&self) -> io::Result<Vec<control::RepositoryTrees>> {
        let ids: Vec<_> = lock(&self.state).sessions.keys().copied().collect();
        let entries: Vec<_> = ids
            .into_iter()
            .map(|id| (id, self.session_cwd(id)))
            .collect();
        sidebar::mobile_repositories(&self.sidebar, &entries, self.state_dir().as_deref())
            .map(|(_, repos)| repos)
            .map_err(|e| io::Error::other(e.to_string()))
    }

    #[cfg(all(test, unix))]
    pub(crate) fn mobile_dashboard_inputs(&self) -> io::Result<MobileDashboardInputs> {
        self.mobile_dashboard_inputs_inner(None)
    }

    pub(crate) fn mobile_dashboard_inputs_swr(
        self: &Arc<Self>,
    ) -> io::Result<MobileDashboardInputs> {
        self.mobile_dashboard_inputs_inner(Some(Arc::downgrade(self)))
    }

    fn mobile_dashboard_inputs_inner(
        &self,
        refresh_reg: Option<std::sync::Weak<Self>>,
    ) -> io::Result<MobileDashboardInputs> {
        let raw: Vec<_> = {
            let st = lock(&self.state);
            st.sessions
                .iter()
                .map(|(&session, e)| {
                    (
                        session,
                        e.pid,
                        st.exited.get(&session).copied(),
                        e.session.tap(),
                        e.wrapped,
                    )
                })
                .collect()
        };
        let entries: Vec<_> = raw
            .iter()
            .map(|(id, ..)| (*id, self.session_cwd(*id)))
            .collect();
        let (dirs, repositories) =
            sidebar::mobile_repositories(&self.sidebar, &entries, self.state_dir().as_deref())
                .map_err(|e| io::Error::other(e.to_string()))?;
        let mut sessions = Vec::with_capacity(raw.len());
        let mut agy_jobs = Vec::new();
        for ((session, root, exit_code, tap, wrapped), dir) in raw.into_iter().zip(dirs) {
            let exited = exit_code.is_some() || tap.pulse().closed;
            let status = self.resolve_session_status_inner(
                session,
                root,
                exited,
                &tap,
                wrapped,
                AgyLookup::Nonblocking(refresh_reg.is_some().then_some(&mut agy_jobs)),
            );
            sessions.push(MobileSessionRow {
                info: Info {
                    session,
                    pid: root,
                    status: exit_code,
                },
                cwd: dir.cwd,
                status,
            });
        }
        if !agy_jobs.is_empty()
            && let Some(weak_reg) = refresh_reg
        {
            spawn_agy_swr_refresh(weak_reg, agy_jobs);
        }
        Ok(MobileDashboardInputs {
            sessions,
            repositories,
        })
    }

    pub(crate) fn mobile_session_cwd(&self, id: SessionId) -> io::Result<PathBuf> {
        if !lock(&self.state).sessions.contains_key(&id) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Session not found"));
        }
        let path = self
            .session_cwd(id)
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| io::Error::other("session working directory unavailable"))?;
        lock(&self.sidebar).record_directory(id, path.clone());
        Ok(path)
    }

    pub(crate) fn mobile_session_status(&self, id: SessionId) -> Option<control::AgentStatus> {
        let (tap, exited, root, wrapped) = {
            let st = lock(&self.state);
            let e = st.sessions.get(&id)?;
            let tap = e.session.tap();
            let exited = st.exited.contains_key(&id) || tap.pulse().closed;
            (tap, exited, e.pid, e.wrapped)
        };
        Some(self.resolve_session_status(id, root, exited, &tap, wrapped))
    }

    /// An existing live tap only; mobile model control must never spawn/resume a replacement.
    pub(crate) fn mobile_composer_tap(&self, id: SessionId) -> io::Result<Tap> {
        let state = lock(&self.state);
        let entry = state.sessions.get(&id).ok_or(io::ErrorKind::NotFound)?;
        if state.exited.contains_key(&id)
            || entry.session.is_closed()
            || self.agents.account_binding_retiring(id)
            || matches!(
                self.status.get(id).state,
                control::AgentState::Waiting
                    | control::AgentState::Blocked
                    | control::AgentState::Exited
            )
        {
            return Err(io::Error::other(
                "Wait for the running agent to become idle",
            ));
        }
        Ok(entry.session.tap())
    }

    pub(crate) fn agent_working(&self, id: SessionId) -> bool {
        self.status.get(id).state == control::AgentState::Working
    }

    /// 모바일 정지 요청: 살아 있는 세션이 현재 `AgentState::Working`이고 열린 붙여넣기가 없을 때만
    /// 단일 `Ctrl+C`(`\x03`)를 기록한다(OWE-1349). 이미 턴이 끝났으면 PTY에 아무것도 쓰지 않는다.
    pub(crate) fn mobile_interrupt(&self, id: SessionId) -> io::Result<bool> {
        let (tap, root, wrapped) = {
            let st = lock(&self.state);
            let Some(entry) = st.sessions.get(&id) else {
                return Ok(false);
            };
            if st.exited.contains_key(&id)
                || entry.session.is_closed()
                || self.agents.account_binding_retiring(id)
            {
                return Ok(false);
            }
            let tap = entry.session.tap();
            if tap.pulse().closed {
                return Ok(false);
            }
            (tap, entry.pid, entry.wrapped)
        };
        if self
            .resolve_session_status(id, root, false, &tap, wrapped)
            .state
            != control::AgentState::Working
        {
            return Ok(false);
        }
        tap.write_interrupt(|| {
            !tap.pulse().closed
                && !self.agents.account_binding_retiring(id)
                && self.status.get(id).state == control::AgentState::Working
        })
    }

    #[cfg(all(test, unix))]
    pub(crate) fn record_test_hook(
        &self,
        id: SessionId,
        event: &krill_proto::hook::HookEvent,
    ) -> io::Result<usize> {
        self.status.hook(&[(id, event)])
    }

    /// Read-only and not gated on the agent being idle, unlike [`Self::mobile_composer_tap`].
    pub(crate) fn session_capture(&self, id: SessionId) -> Option<krill_proto::screen::Screen> {
        let tap = lock(&self.state).sessions.get(&id)?.session.tap();
        Some(tap.capture().0)
    }

    /// 활성 세션의 현재 평문 화면과 Bracketed Paste 모드 여부를 함께 반환한다(OWE-944·OWE-946).
    pub(crate) fn session_screen_and_bracketed_paste(&self, id: SessionId) -> (String, bool) {
        let st = lock(&self.state);
        st.sessions
            .get(&id)
            .map_or((String::new(), false), |entry| {
                let (text, modes) = entry.session.tap().screen_text_and_modes();
                (
                    text,
                    modes.contains(krill_proto::term_modes::Modes::BRACKETED_PASTE),
                )
            })
    }

    /// Resolves a session's transcript identity (`bound`, `agent`, `session_id`, `transcript_path`)
    /// without reading or parsing transcript content from byte offset 0.
    pub(crate) fn resolve_chat_transcript(
        &self,
        id: SessionId,
    ) -> krill_proto::agent_history::ChatPollSnapshot {
        self.chat_poll_inner(id, 0, true)
    }

    /// 세션의 에이전트 트랜스크립트를 `offset` 바이트부터 증분 정규화해 읽는다(OWE-945 / MK-242).
    pub(crate) fn chat_poll(
        &self,
        id: SessionId,
        offset: u64,
    ) -> krill_proto::agent_history::ChatPollSnapshot {
        self.chat_poll_inner(id, offset, false)
    }

    fn chat_poll_inner(
        &self,
        id: SessionId,
        offset: u64,
        resolve_only: bool,
    ) -> krill_proto::agent_history::ChatPollSnapshot {
        use krill_proto::os::ffi::OsStrExt as _;
        let (root_pid, raw_fg, exited) = {
            let st = lock(&self.state);
            if let Some(e) = st.sessions.get(&id) {
                let ex = st.exited.contains_key(&id) || e.session.is_closed();
                (e.pid, e.session.tap().foreground(), ex)
            } else {
                (0, None, true)
            }
        };
        let fg_pid = account_switch::effective_foreground_pid(root_pid, raw_fg, exited);
        let cwd = self
            .session_cwd(id)
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .unwrap_or_default();
        let mut stored = self.agents.stored(id).ok().flatten().or_else(|| {
            let mut status_agent = self.status.get(id).agent;
            if status_agent.is_empty()
                && let Some(fg_name) = fg_pid.and_then(|pid| crate::sys::process_name(pid).ok())
            {
                status_agent = fg_name;
            }
            let agent = krill_proto::agent_history::VaultAgent::parse(&status_agent)?;
            Some(crate::agent::control::StoredLaunch {
                profile: agent.profile().to_owned(),
                permission: krill_proto::agent::Permission::Manual,
                session_id: None,
                transcript_path: None,
                cwd: cwd.clone(),
                hibernation_blocked: true,
                account: None,
                bot_instructions: None,
                launched_at: None,
            })
        });

        let provider = stored.as_ref().and_then(|s| {
            use krill_proto::{agent_history::VaultAgent, usage::ProviderKind};
            match VaultAgent::parse(&s.profile)? {
                VaultAgent::Claude => Some(ProviderKind::Claude),
                VaultAgent::Codex => Some(ProviderKind::Codex),
                VaultAgent::Agy => Some(ProviderKind::Antigravity),
                VaultAgent::OpenCode => None,
            }
        });
        if let Some(provider) = provider
            && !exited
            && root_pid > 0
            && stored
                .as_ref()
                .is_some_and(|s| s.session_id.is_none() || s.transcript_path.is_none())
        {
            let discovered = self.discover_live_identity_with_mode(
                provider,
                &crate::usage::accounts::default_home(),
                stored.as_ref(),
                root_pid,
                fg_pid,
                true,
            );
            if let Ok(Some(found)) = discovered {
                // A process-owned file also covers external account homes that are
                // absent from the daemon's configured transcript search roots.
                // Never replace a launch/hook-bound conversation with another ID.
                if stored
                    .as_ref()
                    .is_some_and(|s| s.session_id.as_deref().is_some_and(|id| id != found.id))
                {
                    return krill_proto::agent_history::ChatPollSnapshot {
                        agent: stored
                            .as_ref()
                            .map_or_else(String::new, |s| s.profile.clone()),
                        reset: !resolve_only && offset > 0,
                        ..Default::default()
                    };
                }
                self.agents
                    .bind_discovered_session(id, &found.id, Some(&found.path));
                if let Some(s) = &mut stored {
                    if s.session_id.is_none() {
                        s.session_id = Some(found.id);
                    }
                    let should_upgrade_db = s
                        .transcript_path
                        .as_deref()
                        .zip(found.path.to_str())
                        .is_some_and(|(old, new_path)| {
                            crate::agent::control::should_upgrade_agy_transcript_path(old, new_path)
                        });
                    if (s.transcript_path.is_none() || should_upgrade_db)
                        && let Some(p) = found
                            .path
                            .to_str()
                            .filter(|p| crate::agent::control::valid_transcript_path(p))
                    {
                        s.transcript_path = Some(p.to_owned());
                    }
                }
            } else if provider == krill_proto::usage::ProviderKind::Antigravity
                && stored
                    .as_ref()
                    .is_some_and(|s| s.session_id.is_none() && s.transcript_path.is_none())
                && !crate::sys::open_vnode_paths(root_pid).is_empty()
            {
                return krill_proto::agent_history::ChatPollSnapshot {
                    agent: "agy".to_owned(),
                    session_id: String::new(),
                    transcript_path: String::new(),
                    next_offset: 0,
                    reset: !resolve_only && offset > 0,
                    bound: false,
                    items: Vec::new(),
                };
            }
        }

        let launched_at = self.agents.launched_at(id);
        // A shell-launched agent has no launch record; its transcript cannot predate its process.
        // Windows `start_us` counts from 1601, 11,644,473,600 s before the Unix epoch.
        #[cfg(windows)]
        let launched_at = launched_at.or_else(|| {
            let start = crate::sys::start_us(fg_pid?).ok()?;
            (start / 1_000_000).checked_sub(11_644_473_600)
        });
        let claimed = self.agents.claimed_transcripts(id);
        let (snapshot, resolved) = if resolve_only {
            crate::agent_history::chat::resolve_session_bounded(
                self.state_dir().as_deref(),
                stored.as_ref(),
                &cwd,
                launched_at,
                &claimed,
            )
        } else {
            crate::agent_history::chat::poll_session_bounded(
                self.state_dir().as_deref(),
                stored.as_ref(),
                &cwd,
                offset,
                launched_at,
                &claimed,
            )
        };
        if let Some((path, sid)) = resolved
            && stored.as_ref().is_some_and(|s| {
                s.session_id.as_deref() == Some(&sid)
                    && (s.transcript_path.is_none()
                        || s.transcript_path.as_deref().zip(path.to_str()).is_some_and(
                            |(old, new_path)| {
                                crate::agent::control::should_upgrade_agy_transcript_path(
                                    old, new_path,
                                )
                            },
                        ))
            })
        {
            // Cwd/recency fallback is for displaying history only. It must never become
            // authority to replace a live conversation during an account switch.
            self.agents
                .bind_discovered_session(id, &sid, Some(path.as_path()));
        }
        if !snapshot.items.is_empty() {
            use krill_proto::agent_history::ChatItemKind;
            let turns: Vec<(String, String)> = snapshot
                .items
                .iter()
                .filter_map(|item| match item.kind {
                    ChatItemKind::User if !item.text.trim().is_empty() => {
                        Some(("user".to_owned(), item.text.clone()))
                    }
                    ChatItemKind::Assistant if !item.text.trim().is_empty() => {
                        Some(("assistant".to_owned(), item.text.clone()))
                    }
                    _ => None,
                })
                .collect();
            if !turns.is_empty() {
                self.voice().observe_turns(
                    krill_proto::voice::VoiceTargetKind::Session,
                    &id.0.to_string(),
                    &turns,
                    self.session_cwd(id),
                );
            }
        }
        snapshot
    }

    pub(crate) fn deliver_input_to_tap(tap: &Tap, bytes: &[u8], submit: bool) -> io::Result<()> {
        let _ = Self::deliver_input_to_tap_inner(tap, bytes, submit, None)?;
        Ok(())
    }

    /// Delivers `bytes` (and optional submit `\r`) only if no other input has touched `tap` since
    /// `expected_last_input_ns`. Returns `Ok(true)` if delivered, `Ok(false)` if suppressed.
    pub(crate) fn deliver_input_to_tap_if_untouched(
        tap: &Tap,
        bytes: &[u8],
        submit: bool,
        expected_last_input_ns: u64,
    ) -> io::Result<bool> {
        Self::deliver_input_to_tap_inner(tap, bytes, submit, Some(expected_last_input_ns))
    }

    fn deliver_input_to_tap_inner(
        tap: &Tap,
        bytes: &[u8],
        submit: bool,
        expected_last_input_ns: Option<u64>,
    ) -> io::Result<bool> {
        let (screen, _, _, modes) = tap.capture_shared_with_modes();
        if krill_proto::screen::agy::auth_login_screen(&screen) {
            tracing::warn!(
                "refused automated PTY input delivery while Antigravity sign-in/OAuth screen is active"
            );
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "antigravity session requires sign-in (auth_required)",
            ));
        }
        let bracketed = modes.contains(krill_proto::term_modes::Modes::BRACKETED_PASTE);
        let payload = if submit {
            bytes
                .strip_suffix(b"\r\n")
                .or_else(|| bytes.strip_suffix(b"\n"))
                .or_else(|| bytes.strip_suffix(b"\r"))
                .unwrap_or(bytes)
        } else {
            bytes
        };
        let is_control_seq = is_terminal_control_seq(payload);
        let use_bracketed = bracketed
            && !payload.is_empty()
            && !is_control_seq
            && (submit || payload.contains(&b'\n') || payload.contains(&b'\r'));
        let single_line;
        let payload = if submit
            && !use_bracketed
            && !is_control_seq
            && (payload.contains(&b'\n') || payload.contains(&b'\r'))
        {
            single_line = Self::flatten_unbracketed_submit_lines(payload);
            single_line.as_slice()
        } else {
            payload
        };
        let before_end = tap.pulse().end;
        let mut expected_stamp = expected_last_input_ns;
        let mut write_checked = |chunk: &[u8]| -> io::Result<bool> {
            if let Some(expected) = expected_stamp {
                if let Some(next_stamp) = tap.write_input_if_untouched(chunk, expected)? {
                    expected_stamp = Some(next_stamp);
                    Ok(true)
                } else {
                    Ok(false)
                }
            } else {
                tap.write_input(chunk, None)?;
                Ok(true)
            }
        };
        if use_bracketed {
            let mut framed = Vec::with_capacity(payload.len() + 12);
            framed.extend_from_slice(b"\x1b[200~");
            Self::strip_bracketed_paste_end(payload, &mut framed);
            framed.extend_from_slice(b"\x1b[201~");
            if !write_checked(&framed)? {
                return Ok(false);
            }
        } else if !payload.is_empty() && !write_checked(payload)? {
            return Ok(false);
        }
        if submit {
            // ConPTY without VT passthrough never exposes DECSET 2004, and busy TUIs can echo a
            // paste tens or hundreds of ms after write_input returns. Always separate text and CR
            // until after echo settles so the TUI does not fold Enter into the paste burst.
            if !payload.is_empty() && !is_control_seq {
                Self::wait_for_paste_submit_settle(tap, before_end);
            }
            if !write_checked(b"\r")? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Strips `ESC [ 201 ~` (bracketed-paste end) to a fixed point in linear time so nested
    /// sequences like `\x1b[\x1b[201~201~` cannot splice back into a literal terminator.
    fn strip_bracketed_paste_end(payload: &[u8], out: &mut Vec<u8>) {
        const PASTE_END: &[u8] = b"\x1b[201~";
        let body_start = out.len();
        for &byte in payload {
            out.push(byte);
            while out.len() >= body_start + PASTE_END.len() && out.ends_with(PASTE_END) {
                out.truncate(out.len() - PASTE_END.len());
            }
        }
    }

    fn flatten_unbracketed_submit_lines(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 16);
        let mut pending_sep = false;
        for &b in payload {
            match b {
                b'\r' | b'\n' => {
                    if !out.is_empty() {
                        pending_sep = true;
                    }
                }
                _ => {
                    if pending_sep {
                        if !out.ends_with(b" ") {
                            out.push(b' ');
                        }
                        out.extend_from_slice(b"| ");
                        pending_sep = false;
                    }
                    out.push(b);
                }
            }
        }
        out
    }

    pub(crate) fn wait_for_paste_submit_settle(tap: &Tap, before_end: u64) {
        const ECHO_WAIT_MAX: Duration = Duration::from_millis(500);
        const ECHO_DRAIN_MAX: Duration = Duration::from_millis(800);

        let write_at = Instant::now();
        let first = tap.wait_timeout(ECHO_WAIT_MAX, |p| p.closed || p.end > before_end);
        if first.closed {
            return;
        }
        if first.end == before_end {
            let remaining =
                krill_proto::input::PASTE_SUBMIT_DELAY.saturating_sub(write_at.elapsed());
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
            return;
        }
        let mut last_end = first.end;
        let mut last_echo_at = Instant::now();
        let drain_deadline = last_echo_at + ECHO_DRAIN_MAX;
        loop {
            let quiet_remaining =
                krill_proto::input::PASTE_SUBMIT_DELAY.saturating_sub(last_echo_at.elapsed());
            if quiet_remaining.is_zero() {
                break;
            }
            let budget = drain_deadline
                .saturating_duration_since(Instant::now())
                .min(quiet_remaining);
            if budget.is_zero() {
                break;
            }
            let next = tap.wait_timeout(budget, |p| p.closed || p.end > last_end);
            if next.closed {
                return;
            }
            if next.end > last_end {
                last_end = next.end;
                last_echo_at = Instant::now();
            } else {
                break;
            }
        }
        let remaining =
            krill_proto::input::PASTE_SUBMIT_DELAY.saturating_sub(last_echo_at.elapsed());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
    }

    fn send_input(
        &self,
        peer: &Peer,
        session: SessionId,
        bytes: &[u8],
        submit: bool,
        wait_submit: bool,
    ) -> Result<Reply, Failure> {
        if bytes.len() > 256 * 1024 {
            return Err(Failure::BadRequest);
        }
        let tap = self.tap(peer, session).ok_or(Failure::NoSession)?;
        if wait_submit {
            let idle_deadline = Instant::now() + Duration::from_millis(2500);
            while Instant::now() < idle_deadline {
                if tap.pulse().closed
                    || self.status.get(session).state != control::AgentState::Working
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let before_end = tap.pulse().end;
        let status = self.status.get(session);
        let was_working = status.state == control::AgentState::Working;
        Self::deliver_input_to_tap(&tap, bytes, submit).map_err(|e| os(&e))?;
        self.acknowledge_retained_handoff(session);
        if submit && let Ok(text) = std::str::from_utf8(bytes) {
            self.voice().observe_prompt(
                krill_proto::voice::VoiceTargetKind::Session,
                &session.0.to_string(),
                text,
                self.session_cwd(session),
            );
            if let Some(bots) = self.bots.get() {
                bots.on_owner_prompt_submitted(session, text);
            }
            if !status.agent.is_empty()
                && status.state != control::AgentState::Exited
                && let Some(dir) = self.state_dir()
            {
                krill_proto::git::record_session_prompt(&dir, session, text, status::now_ns()).ok();
            }
        }
        if submit && wait_submit {
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline {
                let p = tap.pulse();
                if p.closed
                    || p.end > before_end
                    || (!was_working
                        && self.status.get(session).state == control::AgentState::Working)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
        Ok(Reply::InputSent)
    }

    pub(crate) fn orchestration_snapshot(
        &self,
    ) -> io::Result<krill_proto::orchestration::OrchestrationStore> {
        match self.state_dir() {
            Some(dir) => lock(&self.orch_cache).load(&dir),
            None => Ok(krill_proto::orchestration::OrchestrationStore::default()),
        }
    }

    /// `FEAT_ORCH_SNAPSHOT_PAGE` 요청 하나. 범위 검사는 [`Self::orchestration`]과 같다.
    fn orchestration_page(
        &self,
        peer: &Peer,
        generation: Option<u64>,
        offset: u32,
        features: u64,
    ) -> Reply {
        if matches!(peer.scope, Scope::Restricted) {
            peer.deny("orchestration", &self.audit);
            return Reply::Failed(Failure::Denied);
        }
        Reply::Orchestration(self.snapshot_page_response(generation, offset, features))
    }

    /// 단일 프레임 `SnapshotV2`와 같은 바이트(`features`로 내린 뒤 postcard)의 한 조각. 페이지마다 다시
    /// 직렬화해 세대(바이트 해시)를 비교하므로 핀 캐시 없이도(데몬 메모리 상한 안) 바뀐 저장소의 조각이
    /// 섞이지 않는다.
    // ponytail: 페이지마다 재직렬화(≤2MiB, 수 ms). 느리면 orch_cache에 (stamp, features)별 바이트를 캐시한다.
    fn snapshot_page_response(
        &self,
        generation: Option<u64>,
        offset: u32,
        features: u64,
    ) -> krill_proto::orchestration::OrchResponse {
        use krill_proto::orchestration::{OrchResponse, snapshot_page};
        self.orchestration_snapshot()
            .and_then(|store| {
                postcard::to_stdvec(&OrchResponse::SnapshotV2(store).for_features(features))
                    .map_err(io::Error::other)
            })
            .map_or_else(
                |e| OrchResponse::Error(e.to_string()),
                |bytes| snapshot_page(&bytes, generation, offset),
            )
    }

    pub(crate) fn dashboard_tail(&self, id: SessionId) -> Option<Vec<String>> {
        let tap = lock(&self.state)
            .sessions
            .get(&id)
            .map(|entry| entry.session.tap())?;
        Some(tap.preview_tail())
    }

    /// A cheap server revision without any grid capture or Git discovery.
    pub(crate) fn dashboard_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut revision = std::collections::hash_map::DefaultHasher::new();
        let entries: Vec<_> = {
            let state = lock(&self.state);
            state
                .sessions
                .iter()
                .map(|(&id, entry)| {
                    (
                        id,
                        entry.pid,
                        entry.cwd.clone(),
                        state.exited.get(&id).copied(),
                        entry.session.tap(),
                    )
                })
                .collect()
        };
        for (id, pid, cwd, exited, tap) in entries {
            id.hash(&mut revision);
            pid.hash(&mut revision);
            cwd.hash(&mut revision);
            exited.hash(&mut revision);
            let pulse = tap.pulse();
            pulse.revision.hash(&mut revision);
            pulse.end.hash(&mut revision);
            pulse.closed.hash(&mut revision);
            tap.cwd(pid).hash(&mut revision);
            tap.foreground().hash(&mut revision);
            if let Some(dir) = &self.dir {
                crate::agent::control::cache::stamp(
                    &dir.join("agent-launches").join(format!("{}.json", id.0)),
                )
                .hash(&mut revision);
            }
            // AgentStatus has no Hash implementation; its small metadata serialization is
            // independent of terminal dimensions/history.
            if let Ok(bytes) = postcard::to_stdvec(&self.status.get(id)) {
                bytes.hash(&mut revision);
            }
        }
        revision.finish()
            ^ sidebar::revision(&self.sidebar).wrapping_mul(0xBF58_476D_1CE4_E5B9)
            ^ crate::agent_history::vault::agy_swr_revision().wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    pub(crate) fn notify_orch(&self) {
        let (lock_gen, cvar) = &self.orch_signal;
        let mut g = lock(lock_gen);
        *g = g.wrapping_add(1);
        drop(g);
        cvar.notify_all();
    }

    /// Detaches a session from Krill Bot ownership (e.g. `TakeOverWorkspace`) so
    /// future resumes treat it as a normal human-owned session.
    pub(crate) fn release_bot_session(&self, id: SessionId) {
        if let Err(error) = self.agents.clear_bot_instructions(id) {
            tracing::warn!(
                session = id.0,
                kind = ?error.kind(),
                "failed to clear bot instructions on workspace takeover"
            );
        }
    }
    /// Returns the [`crate::bots::BotsManager`] only if already lazily initialized (zero side effects).
    pub fn bots_if_initialized(&self) -> Option<&Arc<crate::bots::BotsManager>> {
        self.bots.get()
    }

    /// 데몬 종료 시 대기 중인 상태·닫힘 원장·세그먼트 표식을 제한 시간 내에 안전하게 비운다.
    pub fn flush_persistence(&self, timeout: Duration) {
        if let Some(store) = &self.store {
            store.retry_pending_markers();
        }
        self.status.shutdown(timeout);
        self.closed_ledger.flush(timeout);
    }

    /// Lazily initializes the [`crate::bots::BotsManager`] when a bot RPC or periodic bot tick needs it.
    pub(crate) fn bots(&self) -> Result<&Arc<crate::bots::BotsManager>, String> {
        if let Some(existing) = self.bots.get() {
            return Ok(existing);
        }
        let dir = self
            .state_dir()
            .ok_or_else(|| "daemon state directory unavailable for bots".to_owned())?;
        let weak_reg = self.self_ref.get().cloned().unwrap_or_default();
        Ok(self
            .bots
            .get_or_init(|| crate::bots::BotsManager::new(dir, weak_reg)))
    }

    /// Krill + Pipito 로컬 음성 서비스를 반환한다. 첫 접근 시 기존 프롬프트·워크스페이스·봇 이름으로 백그라운드 시드한다.
    pub fn voice(&self) -> Arc<crate::voice::VoiceService> {
        let svc = Arc::clone(
            self.voice
                .get_or_init(|| Arc::new(crate::voice::VoiceService::new(self.dir.as_deref()))),
        );
        if let Some(dir) = self.state_dir() {
            let mut prompts = Vec::new();
            let histories = krill_proto::git::load_session_prompt_histories(&dir);
            for history in histories.values() {
                for turn in &history.turns {
                    if !turn.text.trim().is_empty() {
                        prompts.push(turn.text.clone());
                    }
                }
            }
            let mut names = Vec::new();
            {
                let st = lock(&self.state);
                for entry in st.sessions.values() {
                    if let Some(name) = entry.cwd.file_name().and_then(|s| s.to_str()) {
                        names.push(name.to_owned());
                    }
                }
            }
            if let Some(bots) = self.bots_if_initialized()
                && let krill_proto::bots::BotRpcResponse::Bots(list) = bots.handle_rpc_for_peer(
                    self,
                    None,
                    true,
                    krill_proto::bots::BotRpcRequest::ListBots,
                )
            {
                for bot in list {
                    names.push(bot.name);
                }
            }
            svc.bootstrap_once(prompts, names);
        }
        svc
    }

    pub(crate) fn voice_target_cwd(
        &self,
        kind: krill_proto::voice::VoiceTargetKind,
        id: &str,
    ) -> Option<PathBuf> {
        if kind != krill_proto::voice::VoiceTargetKind::Session {
            return None;
        }
        let sid = id.trim().parse::<u64>().ok()?;
        self.session_cwd(SessionId(sid))
    }

    /// Dispatches a [`krill_proto::bots::BotRpcRequest`] carrying connection peer/observer context.
    fn on_bot_rpc_for_peer(
        &self,
        peer: &Peer,
        is_observer: bool,
        req: krill_proto::bots::BotRpcRequest,
    ) -> krill_proto::bots::BotRpcResponse {
        use krill_proto::bots::{BotRpcRequest, BotRpcResponse};
        use krill_proto::voice::VoiceTargetKind;
        // 봇 채팅·그룹 채팅·항목 댓글 제출 시 자동 단어장 및 문맥 캐시 갱신(비차단).
        match &req {
            BotRpcRequest::AddItemComment { item_id, text, .. }
            | BotRpcRequest::AnswerBotItem {
                item_id,
                answer_text: Some(text),
                ..
            } => {
                self.voice()
                    .observe_prompt(VoiceTargetKind::Item, item_id, text, None);
            }
            BotRpcRequest::SendGroupMessage { group_id, text, .. }
            | BotRpcRequest::SendGroupMessageWithAudience { group_id, text, .. } => {
                self.voice()
                    .observe_prompt(VoiceTargetKind::GroupChat, group_id, text, None);
            }
            BotRpcRequest::PostBotReply { bot_id, text, .. } => {
                self.voice()
                    .observe_prompt(VoiceTargetKind::BotChat, bot_id.as_str(), text, None);
            }
            _ => {}
        }
        let peer_session = match peer.scope {
            Scope::Session(sid) => Some(sid),
            Scope::Full | Scope::Restricted => None,
        };
        let resp = match self.bots() {
            Ok(mgr) => mgr.handle_rpc_for_peer(self, peer_session, is_observer, req),
            Err(msg) => BotRpcResponse::Error(msg),
        };
        if let BotRpcResponse::GroupView(ref view) = resp {
            let turns: Vec<(String, String)> = view
                .messages
                .iter()
                .filter(|m| !m.body.trim().is_empty())
                .map(|m| (m.sender_name.clone(), m.body.clone()))
                .collect();
            if !turns.is_empty() {
                self.voice().observe_turns(
                    VoiceTargetKind::GroupChat,
                    &view.group.id,
                    &turns,
                    None,
                );
            }
        } else if let BotRpcResponse::GroupRoom(ref room) = resp {
            let turns: Vec<(String, String)> = room
                .messages
                .iter()
                .filter(|m| !m.body.trim().is_empty())
                .map(|m| (m.sender_name.clone(), m.body.clone()))
                .collect();
            if !turns.is_empty() {
                self.voice().observe_turns(
                    VoiceTargetKind::GroupChat,
                    &room.group.id,
                    &turns,
                    None,
                );
            }
        }
        resp
    }

    /// Runs periodic bot tasks (memory tidy, look around, bot cron schedules, lazy poller reconcile)
    /// ONLY if bots already exist on disk or the bots subsystem is already active.
    pub fn tick_bots_periodic(&self, dir: &Path, now: SystemTime) {
        if self.bots.get().is_none() && !crate::bots::store::BotStore::index_exists_at(dir) {
            return;
        }
        if let Ok(mgr) = self.bots() {
            mgr.tick_periodic(self, now);
        }
    }

    /// Returns a [`Tap`] for a live session (internal daemon helper for Bots runtime).
    pub(crate) fn session_tap(&self, id: SessionId) -> Option<Tap> {
        lock(&self.state).sessions.get(&id).map(|e| e.session.tap())
    }

    /// Returns the OS root PID for `id`, if registered in `Registry`.
    pub(crate) fn session_pid(&self, id: SessionId) -> Option<u32> {
        lock(&self.state).sessions.get(&id).map(|e| e.pid)
    }

    /// Closes/terminates a bot-managed PTY session cleanly, or permanently marks a dormant
    /// post-reboot bot session closed so it cannot be resurrected on a later restart.
    pub(crate) fn kill_bot_session(&self, id: SessionId) -> io::Result<()> {
        if !lock(&self.state).sessions.contains_key(&id) {
            self.close_dormant_bot_session(id);
            return Ok(());
        }
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        match self.close(&peer, id) {
            Ok(_) => Ok(()),
            Err(Failure::NoSession) => {
                self.close_dormant_bot_session(id);
                Ok(())
            }
            Err(fail) => Err(io::Error::other(format!(
                "failed to close bot session: {fail:?}"
            ))),
        }
    }

    fn close_dormant_bot_session(&self, id: SessionId) {
        self.closed_ledger.record_closed(id);
        if let Some(store) = &self.store {
            store.mark_closed(id);
        }
        self.discard_settings(id);
        self.agents.forget(id);
        self.clear_sleeping(id);
    }

    /// Returns `true` if `id` is recorded as explicitly closed.
    pub(crate) fn is_session_closed(&self, id: SessionId) -> bool {
        self.is_closed(id)
    }

    /// Returns `true` if `id` is not closed/resumed and still has durable launch metadata or
    /// cold terminal history on disk that `Request::Resume` can resurrect after a reboot.
    pub(crate) fn is_session_resumable(&self, id: SessionId) -> bool {
        if self.is_closed(id) || lock(&self.state).resumed.contains(&id) {
            return false;
        }
        if self.agents.stored(id).ok().flatten().is_some() {
            return true;
        }
        if let Some(store) = &self.store
            && store.session_dir(id).is_dir()
        {
            return true;
        }
        self.dir.as_ref().is_some_and(|d| {
            d.join("sessions")
                .join(format!("{}.noindex", id.0))
                .is_dir()
        })
    }

    /// Returns live non-exited sessions whose stored launch still carries `bot_instructions`,
    /// enabling self-healing adoption when `runtime-bindings.json` lost their entry.
    pub(crate) fn live_bot_launch_candidates(&self) -> Vec<(SessionId, u32, PathBuf, String, u64)> {
        use krill_proto::os::ffi::OsStrExt as _;
        let live: Vec<(SessionId, u32, PathBuf)> = {
            let st = lock(&self.state);
            st.sessions
                .iter()
                .filter(|(id, entry)| !st.exited.contains_key(id) && !entry.session.is_closed())
                .map(|(&id, entry)| (id, entry.pid, entry.cwd.clone()))
                .collect()
        };
        let mut out = Vec::new();
        for (sid, pid, live_cwd) in live {
            let Ok(Some(stored)) = self.agents.stored(sid) else {
                continue;
            };
            let Some(instructions) = stored.bot_instructions else {
                continue;
            };
            let cwd = if stored.cwd.is_empty() {
                live_cwd
            } else {
                PathBuf::from(std::ffi::OsStr::from_bytes(&stored.cwd))
            };
            out.push((
                sid,
                pid,
                cwd,
                instructions,
                stored.launched_at.unwrap_or(0).saturating_mul(1000),
            ));
        }
        out
    }

    /// Returns `true` if `id` is a live, non-exited session.
    pub(crate) fn is_session_live(&self, id: SessionId) -> bool {
        let st = lock(&self.state);
        !st.exited.contains_key(&id)
            && st
                .sessions
                .get(&id)
                .is_some_and(|entry| !entry.session.is_closed())
    }

    #[cfg(all(test, unix))]
    pub(crate) fn insert_mock_live_session_for_test(
        &self,
        id: SessionId,
        cwd: PathBuf,
    ) -> io::Result<UnixStream> {
        let (master, peer) = UnixStream::pair()?;
        let session = Session::start(
            id,
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&self.look),
            None,
        )?;
        let mut st = lock(&self.state);
        st.exited.remove(&id);
        st.sessions.insert(
            id,
            Entry {
                session,
                pid: std::process::id(),
                cwd,
                wrapped: false,
            },
        );
        Ok(peer)
    }

    /// Returns the account ID recorded for `id`, if known from stored launch metadata.
    pub(crate) fn session_account_id(&self, id: SessionId) -> Option<String> {
        self.agents
            .stored(id)
            .ok()
            .flatten()
            .and_then(|s| s.account)
            .map(|a| a.id)
            .filter(|id| !id.trim().is_empty())
    }

    /// Returns `true` when `id` has saved, resumable agent conversation metadata on disk
    /// (used by terminal-worker restart recovery under `resume_work_on_restart`, Orca #46643).
    pub(crate) fn has_resumable_worker_conversation(&self, id: SessionId) -> bool {
        let Ok(Some(stored)) = self.agents.stored(id) else {
            return false;
        };
        let fallback_sid = self
            .state_dir()
            .as_deref()
            .and_then(|d| crate::agent::claude::session_id(d, id));
        let has_sid = stored
            .session_id
            .as_deref()
            .or(fallback_sid.as_deref())
            .is_some_and(|s| !s.trim().is_empty());
        if !has_sid {
            return false;
        }
        self.agents
            .agents()
            .is_ok_and(|agents| agents.resolve(&stored.profile).is_some())
    }

    /// Attempts to resume a lost orchestration terminal worker's saved agent conversation into a
    /// new live PTY session, preserving `KRILL_ORCH_DEPTH` / `KRILL_ORCH_MAX_DEPTH` and segment
    /// history (Orca #46643).
    pub(crate) fn resume_lost_worker_session(
        &self,
        old: SessionId,
        dispatch: &krill_proto::orchestration::Dispatch,
    ) -> io::Result<SessionId> {
        let holder = self
            .holder
            .as_ref()
            .ok_or_else(|| io::Error::other("no holder"))?;
        let stored = self
            .agents
            .stored(old)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing stored launch"))?;
        let state = self.state_dir();
        let fallback_sid = state
            .as_deref()
            .and_then(|d| crate::agent::claude::session_id(d, old));
        let resume_id = stored
            .session_id
            .or(fallback_sid)
            .filter(|s| !s.trim().is_empty());
        if resume_id.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no resumable agent conversation id",
            ));
        }
        let agents = self.agents.agents()?;
        let (canonical, profile) = agents
            .resolve(&stored.profile)
            .ok_or_else(|| io::Error::other("unknown agent profile"))?;
        let mut cwd = if stored.cwd.is_empty() {
            dispatch.cwd.as_bytes().to_vec()
        } else {
            stored.cwd.clone()
        };
        sanitize_spawn_cwd(&mut cwd);
        let session = self.reserve_session_id(Some(old))?;
        let account = crate::agent::resume_target(
            profile,
            state.as_deref(),
            None,
            stored.account.as_ref(),
            resume_id.as_deref(),
        )?;
        crate::agent::pre_trust_workspace(profile, &cwd, account.as_ref());
        let (mut launch, session_id, account) = crate::agent::resume_in_with_bot_instructions(
            profile,
            stored.permission,
            session,
            state.as_deref(),
            resume_id.as_deref(),
            account.as_ref(),
            stored.bot_instructions.as_deref(),
        )
        .inspect_err(|_| self.discard_settings(session))?;
        if canonical == "agy" && account.as_ref().is_none_or(|a| a.dir.is_empty()) {
            preflight_agy_background_launch(&launch.env)
                .inspect_err(|_| self.discard_settings(session))?;
        }
        for (key, value) in [
            ("KRILL_ORCH_DEPTH", dispatch.depth.saturating_add(1)),
            (
                "KRILL_ORCH_MAX_DEPTH",
                krill_proto::orchestration::MAX_WORKER_DEPTH,
            ),
        ] {
            launch
                .env
                .push((key.as_bytes().to_vec(), value.to_string().into_bytes()));
        }
        let metadata = control::AgentLaunch {
            profile: canonical.to_owned(),
            permission: stored.permission,
        };
        self.agents
            .remember_session(session, &metadata, session_id, cwd.clone(), account)
            .inspect_err(|_| self.discard_settings(session))?;
        if let Some(store) = &self.store {
            store.inherit_history(session, old).inspect_err(|_| {
                self.discard_settings(session);
                self.agents.forget(session);
            })?;
        }
        let (spawned_session, _) = self
            .spawn_holder(holder, session, launch.argv, launch.env, cwd, 120, 36)
            .map_err(|e| {
                if let Some(store) = &self.store {
                    store.discard_inherited_history(session);
                }
                io::Error::other(e.to_string())
            })?;
        lock(&self.state).resumed.insert(old);
        self.discard_settings(old);
        self.agents.forget(old);
        self.clear_sleeping(old);
        Ok(spawned_session)
    }

    /// Delivers input bytes to a live bot session PTY.
    pub(crate) fn deliver_bot_input(
        &self,
        id: SessionId,
        bytes: &[u8],
        submit: bool,
    ) -> io::Result<()> {
        let tap = self
            .session_tap(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "bot session not live"))?;
        if !submit {
            return Self::deliver_input_to_tap(&tap, bytes, false);
        }
        let canonical = self
            .agents
            .stored(id)
            .ok()
            .flatten()
            .map(|stored| krill_proto::agent::canonical_id(&stored.profile).to_owned())
            .or_else(|| {
                let agent = self.status.get(id).agent;
                (!agent.is_empty()).then(|| krill_proto::agent::canonical_id(&agent).to_owned())
            });
        prompt_delivery::deliver_and_confirm_submit(&tap, canonical.as_deref(), bytes, &|| {
            self.status.get(id).state == control::AgentState::Working
        })
    }

    /// Captures the trailing non-empty plain-text lines from a session's PTY screen (reply fallback),
    /// stripping box-drawing borders and interactive agent TUI status chrome.
    pub(crate) fn capture_session_plain_tail(&self, id: SessionId) -> Option<String> {
        let tap = self.session_tap(id)?;
        let (_, bytes) = tap.capture_cli(control::CaptureFormat::Plain).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|l| {
                let trimmed = l.trim();
                !trimmed.is_empty() && !is_tui_chrome_line(trimmed)
            })
            .collect();
        if lines.is_empty() {
            return None;
        }
        let start = lines.len().saturating_sub(40);
        let joined = lines.get(start..).unwrap_or_default().join("\n");
        (!joined.trim().is_empty()).then_some(joined)
    }

    /// Launches a bot agent session via normal `holder::Spawn` with bot env vars and optional MCP config.
    #[expect(clippy::too_many_arguments, reason = "bot agent spawn configuration")]
    pub(crate) fn spawn_bot_agent_session(
        &self,
        agent_id: &str,
        model: Option<&str>,
        effort: Option<&str>,
        permission: krill_proto::agent::Permission,
        cwd: &Path,
        extra_env: Vec<(Vec<u8>, Vec<u8>)>,
        bot_mcp: Option<&crate::agent::BotMcpLaunchConfig>,
    ) -> Result<(SessionId, u32, bool), crate::bots::runtime::BotStartError> {
        use crate::bots::runtime::BotStartError;
        use krill_proto::bots::BotSessionRefusal;
        use krill_proto::os::ffi::OsStrExt as _;
        let holder = self.holder.as_ref().ok_or_else(|| {
            BotStartError::Refused(BotSessionRefusal::host_unsupported(
                "Daemon session holder is unavailable.",
            ))
        })?;
        let agents = self
            .agents
            .agents()
            .map_err(|_| BotStartError::Other("failed to load agent profiles".to_owned()))?;
        let (canonical, base_profile) = agents
            .resolve(agent_id)
            .ok_or_else(|| BotStartError::Refused(BotSessionRefusal::unknown_agent(agent_id)))?;
        let mut profile = base_profile.clone();
        let mut overrides = krill_proto::agent::LaunchOverrides {
            model: model.map(ToOwned::to_owned),
            effort: effort.map(ToOwned::to_owned),
        };
        if canonical == "agy"
            && let Some(ref m) = overrides.model
            && let Some((family, eff)) =
                crate::mobile::fold_agy_model_selection(None, m, overrides.effort.as_deref())
        {
            overrides.model = Some(family);
            overrides.effort = Some(eff);
        }
        if !overrides.is_empty() {
            krill_proto::agent::apply_launch_overrides(canonical, &mut profile, &overrides)
                .map_err(|err| {
                    BotStartError::Refused(BotSessionRefusal::invalid_launch_config(canonical, err))
                })?;
        }
        // Admission check: verify the agent CLI is installed and meets minimum version before spawn.
        if let Some(program) = profile.program() {
            let exe = resolve_bot_cli_executable(program, &profile.env);
            let Some(exe) = exe else {
                return Err(BotStartError::Refused(
                    BotSessionRefusal::cli_not_installed(canonical),
                ));
            };
            if canonical == "agy"
                && let Some((installed, minimum)) = crate::mobile::agy_version_problem(&exe, false)
            {
                return Err(BotStartError::Refused(BotSessionRefusal::cli_too_old(
                    canonical, installed, minimum,
                )));
            }
        }
        let session = self
            .reserve_session_id(None)
            .map_err(|err| BotStartError::Other(format!("failed to reserve session id: {err}")))?;
        let state = self.state_dir();
        let mut cwd_bytes = cwd.as_os_str().as_bytes().to_vec();
        sanitize_spawn_cwd(&mut cwd_bytes);

        let (mut launch, session_id, account) = crate::agent::launch_selected_in_with_bot_mcp(
            &profile,
            permission,
            session,
            state.as_deref(),
            None,
            Some(cwd),
            bot_mcp,
        )
        .map_err(|err| {
            self.discard_settings(session);
            BotStartError::Refused(BotSessionRefusal::spawn_failed(canonical, err.to_string()))
        })?;
        launch.env.extend(extra_env);
        if canonical == "agy"
            && account.as_ref().is_none_or(|a| a.dir.is_empty())
            && let Err(err) = preflight_agy_background_launch(&launch.env)
        {
            self.discard_settings(session);
            tracing::warn!(
                agent = canonical,
                error = %err,
                "refused bot agy session launch before spawn: Antigravity credentials unavailable"
            );
            return Err(BotStartError::Refused(BotSessionRefusal::auth_required(
                canonical,
            )));
        }
        crate::agent::pre_trust_workspace(&profile, &cwd_bytes, account.as_ref());
        let metadata = control::AgentLaunch {
            profile: canonical.to_owned(),
            permission,
        };
        self.agents
            .remember_session(session, &metadata, session_id, cwd_bytes.clone(), account)
            .map_err(|err| {
                self.discard_settings(session);
                BotStartError::Other(format!("failed to persist session metadata: {err}"))
            })?;
        // 지침이 시스템 프롬프트로 갔는지: 아니면 호출자가 첫 입력으로 보낸다. 재개 때 다시 넘기도록 남긴다.
        let given = bot_mcp
            .and_then(|m| m.instructions.as_deref())
            .filter(|t| !t.trim().is_empty() && crate::agent::takes_bot_instructions(&profile));
        if let Some(text) = given {
            self.agents
                .remember_bot_instructions(session, text)
                .map_err(|err| {
                    self.discard_settings(session);
                    self.agents.forget(session);
                    BotStartError::Other(format!("failed to persist session metadata: {err}"))
                })?;
        }
        let instructions_given = given.is_some();
        self.spawn_holder(holder, session, launch.argv, launch.env, cwd_bytes, 120, 40)
            .map(|(sid, pid)| (sid, pid, instructions_given))
            .map_err(|fail| match fail {
                Failure::Os(errno)
                    if io::Error::from_raw_os_error(errno).kind() == io::ErrorKind::NotFound =>
                {
                    BotStartError::Refused(BotSessionRefusal::cli_not_installed(canonical))
                }
                other => BotStartError::Refused(BotSessionRefusal::spawn_failed(
                    canonical,
                    other.to_string(),
                )),
            })
    }

    fn check_messages_wait(
        &self,
        dir: &Path,
        scope: (Option<&str>, Option<&str>),
        types: &[krill_proto::orchestration::MessageType],
        unread_only: bool,
        ack: bool,
        wait_ms: u64,
    ) -> io::Result<krill_proto::orchestration::OrchResponse> {
        use krill_proto::orchestration::{MAX_CHECK_WAIT_MS, OrchResponse, OrchestrationStore};
        let (recipient, run_id) = scope;
        let deadline = Instant::now() + Duration::from_millis(wait_ms.min(MAX_CHECK_WAIT_MS));
        let mut waited = false;
        loop {
            let seen_gen = *lock(&self.orch_signal.0);
            let preview = self.orchestration_snapshot()?.check_messages_filtered(
                recipient,
                run_id,
                types,
                unread_only,
                false,
            );
            if !preview.is_empty() {
                if waited {
                    let coalesce = deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50));
                    if !coalesce.is_zero() {
                        std::thread::sleep(coalesce);
                    }
                }
                if ack {
                    let claimed = OrchestrationStore::mutate(dir, |s| {
                        Ok(s.check_messages_filtered(recipient, run_id, types, unread_only, true))
                    })?;
                    if !claimed.is_empty() {
                        self.notify_orch();
                        return Ok(OrchResponse::Messages(claimed));
                    }
                    if Instant::now() >= deadline {
                        return Ok(OrchResponse::Messages(Vec::new()));
                    }
                    continue;
                }
                let msgs = if waited {
                    self.orchestration_snapshot()?.check_messages_filtered(
                        recipient,
                        run_id,
                        types,
                        unread_only,
                        false,
                    )
                } else {
                    preview
                };
                return Ok(OrchResponse::Messages(msgs));
            }
            let rem = deadline.saturating_duration_since(Instant::now());
            if rem.is_zero() {
                return Ok(OrchResponse::Messages(Vec::new()));
            }
            let (lock_gen, cvar) = &self.orch_signal;
            let g = lock(lock_gen);
            if *g == seen_gen {
                drop(cvar.wait_timeout(g, rem.min(Duration::from_millis(250))));
            }
            waited = true;
        }
    }

    /// Launch from a dispatch recorded before spawn. Overrides affect only this profile
    /// clone, so configured accounts, trust, hooks and launch metadata remain intact.
    ///
    /// Serializes duplicate launches for the same `dispatch_id` in-process and releases
    /// the global `orchestration.json` flock before spawning the holder process.
    fn launch_orchestration_worker(
        &self,
        dispatch_id: &str,
        account: Option<&str>,
    ) -> io::Result<krill_proto::orchestration::Dispatch> {
        use krill_proto::orchestration::{
            DispatchSessionOrigin, DispatchStatus, OrchestrationStore, worker_launch_profile,
        };
        struct DispatchLaunchGuard<'a> {
            gate: &'a (Mutex<HashSet<String>>, std::sync::Condvar),
            dispatch_id: String,
        }
        impl Drop for DispatchLaunchGuard<'_> {
            fn drop(&mut self) {
                lock(&self.gate.0).remove(&self.dispatch_id);
                self.gate.1.notify_all();
            }
        }
        enum CommitOutcome {
            Committed(krill_proto::orchestration::Dispatch),
            AlreadyLaunched(krill_proto::orchestration::Dispatch),
            Ineligible(&'static str),
        }

        let dir = self
            .state_dir()
            .ok_or_else(|| io::Error::other("missing state directory"))?;

        // Phase 0: serialize concurrent launch attempts for the same dispatch_id without
        // blocking unrelated dispatches or other orchestration RPCs on the global flock.
        let _claim = {
            let (mtx, cvar) = &self.orch_launching;
            let mut active = lock(mtx);
            while active.contains(dispatch_id) {
                active = cvar
                    .wait(active)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            active.insert(dispatch_id.to_owned());
            DispatchLaunchGuard {
                gate: &self.orch_launching,
                dispatch_id: dispatch_id.to_owned(),
            }
        };

        // Phase 1: read & validate the dispatch without holding the orchestration flock across spawn.
        let dispatch = {
            let store = OrchestrationStore::load(&dir)?;
            let dispatch = store
                .dispatches
                .iter()
                .find(|d| d.id == dispatch_id)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "dispatch not found"))?;
            if dispatch.status != DispatchStatus::Active
                || dispatch.session_origin == DispatchSessionOrigin::Borrowed
            {
                return Err(io::Error::other(
                    "dispatch is not eligible for a new session",
                ));
            }
            if dispatch.session_id.is_some() {
                return Ok(dispatch);
            }
            dispatch
        };

        // Phase 2: prepare profile/settings and spawn the holder completely outside the flock.
        let agents = self.agents.agents()?;
        let (canonical, profile) = agents
            .resolve(&dispatch.agent)
            .ok_or_else(|| io::Error::other("unknown agent profile"))?;
        let profile = worker_launch_profile(
            profile,
            canonical,
            dispatch.model.as_deref(),
            dispatch.effort.as_deref(),
        )?;
        let holder = self
            .holder
            .as_ref()
            .ok_or_else(|| io::Error::other("no holder"))?;
        let session = self.reserve_session_id(None)?;
        let cwd = dispatch.cwd.as_bytes().to_vec();
        let (mut launch, agent_session_id, account) = crate::agent::launch_selected_in(
            &profile,
            agents.permission,
            session,
            Some(&dir),
            account,
        )
        .inspect_err(|_| self.discard_settings(session))?;
        crate::agent::pre_trust_workspace(&profile, &cwd, account.as_ref());
        let metadata = control::AgentLaunch {
            profile: canonical.to_owned(),
            permission: agents.permission,
        };
        self.agents
            .remember_session(session, &metadata, agent_session_id, cwd.clone(), account)
            .inspect_err(|_| self.discard_settings(session))?;
        for (key, value) in [
            ("KRILL_ORCH_DEPTH", dispatch.depth.saturating_add(1)),
            (
                "KRILL_ORCH_MAX_DEPTH",
                krill_proto::orchestration::MAX_WORKER_DEPTH,
            ),
        ] {
            launch
                .env
                .push((key.as_bytes().to_vec(), value.to_string().into_bytes()));
        }
        let (session, _) = self
            .spawn_holder(holder, session, launch.argv, launch.env, cwd, 120, 36)
            .map_err(|e| io::Error::other(e.to_string()))?;

        // Phase 3: short flock commit; if the dispatch became ineligible or already got a session
        // while we were spawning, tear down the orphan session outside the flock.
        let commit = OrchestrationStore::mutate(&dir, |store| {
            let Some(stored) = store.dispatches.iter_mut().find(|d| d.id == dispatch_id) else {
                return Ok(CommitOutcome::Ineligible("dispatch missing after launch"));
            };
            if stored.session_id.is_some() {
                return Ok(CommitOutcome::AlreadyLaunched(stored.clone()));
            }
            if stored.status != DispatchStatus::Active
                || stored.session_origin == DispatchSessionOrigin::Borrowed
            {
                return Ok(CommitOutcome::Ineligible(
                    "dispatch is not eligible for a new session",
                ));
            }
            stored.session_id = Some(session.0);
            let run_id = stored.run_id.clone();
            let result = stored.clone();
            if let Ok(sid) = u32::try_from(session.0) {
                store.session_runs.insert(sid, run_id);
            }
            let dispatch_msg_ids: Vec<String> = store
                .messages
                .iter()
                .filter(|m| {
                    m.dispatch_id.as_deref() == Some(dispatch_id)
                        && m.msg_type == krill_proto::orchestration::MessageType::Dispatch
                })
                .map(|m| m.id.clone())
                .collect();
            if !dispatch_msg_ids.is_empty() {
                store
                    .idle_mail_notified
                    .entry(session.0)
                    .or_default()
                    .extend(dispatch_msg_ids);
            }
            Ok(CommitOutcome::Committed(result))
        });

        match commit {
            Ok(CommitOutcome::Committed(result)) => Ok(result),
            Ok(CommitOutcome::AlreadyLaunched(existing)) => {
                self.close(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    session,
                )
                .ok();
                Ok(existing)
            }
            Ok(CommitOutcome::Ineligible(reason)) => {
                self.close(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    session,
                )
                .ok();
                Err(io::Error::other(reason))
            }
            Err(err) => {
                self.close(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    session,
                )
                .ok();
                Err(err)
            }
        }
    }

    /// Submit only a mailbox pointer to an idle live PTY, once per unacknowledged mail.
    /// Message content stays in the durable mailbox, not in terminal input.
    fn notify_idle_orchestration_mail(&self, dir: &Path) -> io::Result<()> {
        self.notify_idle_orchestration_mail_for(dir, None)
    }

    fn notify_idle_orchestration_mail_for(
        &self,
        dir: &Path,
        only_session: Option<SessionId>,
    ) -> io::Result<()> {
        use krill_proto::orchestration::{
            DispatchReleaseState, DispatchStatus, ORCH_FILE, OrchestrationStore,
        };
        if !dir.join(ORCH_FILE).exists() {
            return Ok(());
        }
        if let Some(session) = only_session
            && self.status.get(session).state != control::AgentState::Done
        {
            return Ok(());
        }
        let store = OrchestrationStore::load(dir)?;
        let mut targets: BTreeMap<IdleMailScope, Vec<String>> = BTreeMap::new();
        for message in store.messages.iter().filter(|m| !m.acked) {
            let session = if message.to == "coordinator" {
                store
                    .runs
                    .iter()
                    .find(|r| Some(&r.id) == message.run_id.as_ref())
                    .and_then(|r| r.coordinator_session_id)
            } else {
                store
                    .dispatches
                    .iter()
                    .rev()
                    .find(|d| {
                        d.worker_name == message.to
                            && message.run_id.as_ref().is_none_or(|id| &d.run_id == id)
                            && d.release_state != DispatchReleaseState::Released
                            && !matches!(
                                d.status,
                                DispatchStatus::Abandoned
                                    | DispatchStatus::Failed
                                    | DispatchStatus::Released
                            )
                    })
                    .and_then(|d| d.session_id)
            };
            let Some(sid) = session else {
                continue;
            };
            if only_session.is_some_and(|only| only.0 != sid) {
                continue;
            }
            if store
                .idle_mail_notified
                .get(&sid)
                .is_some_and(|ids| ids.contains(&message.id))
            {
                continue;
            }
            targets
                .entry((sid, message.to.clone(), message.run_id.clone()))
                .or_default()
                .push(message.id.clone());
        }
        if targets.is_empty() {
            return Ok(());
        }
        let mut delivered: Vec<(u64, Vec<String>)> = Vec::new();
        for (scope, ids) in targets {
            let (sid, ref recipient, ref run_id) = scope;
            let session = SessionId(sid);
            // Waiting includes approval/questions; never type mail into those prompts.
            let initial = self.status.get(session);
            if initial.state != control::AgentState::Done {
                continue;
            }
            let epoch = initial.since_unix_ns;
            let tap = lock(&self.state)
                .sessions
                .get(&session)
                .map(|entry| entry.session.tap());
            let Some(tap) = tap.filter(|tap| !tap.pulse().closed) else {
                continue;
            };
            {
                let mut epochs = lock(&self.orch_mail_notified_done);
                if matches!(
                    epochs.get(&scope),
                    Some(MailEpochState::InFlight(e) | MailEpochState::Delivered(e)) if *e == epoch
                ) {
                    continue;
                }
                epochs.insert(scope.clone(), MailEpochState::InFlight(epoch));
            }
            if !self.wait_for_idle_mail_prompt(session, epoch, &tap) {
                let mut epochs = lock(&self.orch_mail_notified_done);
                if epochs.get(&scope) == Some(&MailEpochState::InFlight(epoch)) {
                    epochs.remove(&scope);
                }
                continue;
            }
            // Avoid interpreting names as shell fragments or raw terminal controls, even for a login shell.
            let quote = |s: &str| {
                let clean: String = s.chars().filter(|c| !c.is_control()).collect();
                format!("'{}'", clean.replace('\'', "'\\''"))
            };
            let run_flag = run_id
                .as_deref()
                .map(|id| format!(" --run {}", quote(id)))
                .unwrap_or_default();
            // Senders and types only (the chat draws them as cards); content stays in the mailbox.
            let mut senders: Vec<String> = Vec::new();
            for m in store.messages.iter().filter(|m| ids.contains(&m.id)) {
                let sender = format!("{} ({})", quote(&m.from), m.msg_type.as_str());
                if !senders.contains(&sender) {
                    senders.push(sender);
                }
            }
            let pointer = format!(
                "[Krill orchestration] Unread mail. From: {}. Read with: krill orchestration check --to {}{} --unread --json",
                senders.join(", "),
                quote(recipient),
                run_flag
            );
            let sent = Self::deliver_input_to_tap(&tap, pointer.as_bytes(), true).is_ok();
            {
                let mut epochs = lock(&self.orch_mail_notified_done);
                if epochs.get(&scope) == Some(&MailEpochState::InFlight(epoch)) {
                    if sent {
                        epochs.insert(scope, MailEpochState::Delivered(epoch));
                    } else {
                        epochs.remove(&scope);
                    }
                }
            }
            if sent {
                delivered.push((sid, ids));
            }
        }
        if !delivered.is_empty() {
            OrchestrationStore::mutate(dir, |store| {
                let unread: std::collections::BTreeSet<&str> = store
                    .messages
                    .iter()
                    .filter(|m| !m.acked)
                    .map(|m| m.id.as_str())
                    .collect();
                for (sid, ids) in delivered {
                    let notified = store.idle_mail_notified.entry(sid).or_default();
                    for id in ids {
                        if unread.contains(id.as_str()) {
                            notified.insert(id);
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    fn wait_for_idle_mail_prompt(&self, session: SessionId, epoch: u64, tap: &Tap) -> bool {
        const MIN_DONE_AGE_NS: u64 = 250_000_000;
        const MIN_QUIET_MS: u64 = 200;
        const MAX_SETTLE: Duration = Duration::from_millis(450);
        const POLL: Duration = Duration::from_millis(20);

        let started = Instant::now();
        loop {
            if tap.pulse().closed {
                return false;
            }
            let current = self.status.get(session);
            if current.state != control::AgentState::Done || current.since_unix_ns != epoch {
                return false;
            }
            let done_age_ns = status::now_ns().saturating_sub(epoch);
            if (done_age_ns >= MIN_DONE_AGE_NS && tap.quiet_ms() >= MIN_QUIET_MS)
                || started.elapsed() >= MAX_SETTLE
            {
                return true;
            }
            std::thread::sleep(POLL);
        }
    }

    fn orchestration(
        &self,
        peer: &Peer,
        action: krill_proto::orchestration::OrchAction,
    ) -> Result<Reply, Failure> {
        use krill_proto::orchestration::{OrchAction, OrchResponse, OrchestrationStore};
        if matches!(peer.scope, Scope::Restricted) {
            peer.deny("orchestration", &self.audit);
            return Err(Failure::Denied);
        }
        let Some(dir) = self.state_dir() else {
            return Err(Failure::Os(libc::EIO));
        };
        let v2 = action.requires_v2();
        let action = match action {
            OrchAction::SetCurrentRunLegacy { run_id } => OrchAction::SetCurrentRun {
                run_id,
                session_id: None,
            },
            OrchAction::CheckMessagesLegacy {
                recipient,
                types,
                unread_only,
                ack,
                wait_ms,
            } => OrchAction::CheckMessages {
                recipient,
                types,
                unread_only,
                ack,
                wait_ms,
                run_id: None,
                delivery_id: None,
            },
            action => action,
        };
        let res: io::Result<OrchResponse> = match action {
            OrchAction::Snapshot => self.orchestration_snapshot().map(OrchResponse::Snapshot),
            OrchAction::SnapshotV2 => self.orchestration_snapshot().map(OrchResponse::SnapshotV2),
            // 제어 루프가 협상 기능과 함께 먼저 처리한다. 직접 호출은 이 빌드의 전체 기능으로 답한다.
            OrchAction::SnapshotPage { generation, offset } => {
                Ok(self.snapshot_page_response(generation, offset, krill_proto::ipc::FEATURES))
            }
            OrchAction::SetCurrentRunLegacy { .. } | OrchAction::CheckMessagesLegacy { .. } => {
                Err(io::Error::other("legacy action normalization failed"))
            }
            OrchAction::CreateRun {
                title,
                coordinator_session_id,
            } => OrchestrationStore::mutate(&dir, |s| s.create_run(&title, coordinator_session_id))
                .map(|r| {
                    self.notify_orch();
                    OrchResponse::Run(r)
                }),
            OrchAction::SetCurrentRun { run_id, session_id } => {
                OrchestrationStore::mutate(&dir, |s| {
                    s.set_session_run(session_id, run_id.as_deref())?;
                    Ok(OrchResponse::CurrentRun(s.current_run_id.clone()))
                })
            }
            OrchAction::CreateTask {
                run_id,
                title,
                description,
                deps,
            } => OrchestrationStore::mutate(&dir, |s| {
                s.create_task(&run_id, &title, &description, deps)
            })
            .map(|t| {
                self.notify_orch();
                OrchResponse::Task(t)
            }),
            OrchAction::UpdateTask {
                task_id,
                status,
                summary,
            } => OrchestrationStore::mutate(&dir, |s| s.update_task(&task_id, status, summary))
                .map(|t| {
                    self.notify_orch();
                    OrchResponse::Task(t)
                }),
            OrchAction::StartWorker {
                run_id,
                task_id,
                worker_name,
                agent,
                session_id,
                cwd,
                prompt,
                depth,
                max_depth,
            } => OrchestrationStore::mutate(&dir, |s| {
                s.start_worker_with_depth(
                    &run_id,
                    task_id.as_deref(),
                    &worker_name,
                    &agent,
                    session_id,
                    &cwd,
                    &prompt,
                    depth,
                    max_depth,
                )
            })
            .map(|d| {
                self.notify_orch();
                OrchResponse::Dispatch(Box::new(d))
            }),
            OrchAction::StartWorkerConfigured { spec } => {
                OrchestrationStore::mutate(&dir, |s| s.start_worker_configured(&spec)).map(
                    |dispatch| {
                        self.notify_orch();
                        OrchResponse::DispatchV2(Box::new(dispatch))
                    },
                )
            }
            OrchAction::LaunchWorker {
                dispatch_id,
                account,
            } => self
                .launch_orchestration_worker(&dispatch_id, account.as_deref())
                .map(|dispatch| {
                    self.notify_orch();
                    OrchResponse::DispatchV2(Box::new(dispatch))
                }),
            OrchAction::WorkerStartFailed {
                dispatch_id,
                reason,
            } => OrchestrationStore::mutate(&dir, |s| s.worker_start_failed(&dispatch_id, &reason))
                .map(|dispatch| {
                    self.notify_orch();
                    OrchResponse::DispatchV2(Box::new(dispatch))
                }),
            OrchAction::ManageWorker {
                worker_or_dispatch,
                operation,
                archived_output,
            } => {
                use krill_proto::orchestration::WorkerOperation;
                OrchestrationStore::mutate(&dir, |store| {
                    let result = match operation {
                        WorkerOperation::Stop => {
                            store.stop_or_release_worker(&worker_or_dispatch, false)
                        }
                        WorkerOperation::Release => {
                            store.stop_or_release_worker(&worker_or_dispatch, true)
                        }
                        WorkerOperation::Retain => store.retain_worker(&worker_or_dispatch),
                        WorkerOperation::Abandon => store.abandon_worker(&worker_or_dispatch),
                    }?;
                    let stored = store
                        .dispatches
                        .iter_mut()
                        .find(|d| d.id == result.id)
                        .ok_or_else(|| io::Error::other("dispatch missing"))?;
                    if archived_output.is_some() {
                        stored.archived_output = archived_output;
                    }
                    Ok(stored.clone())
                })
                .map(|dispatch| {
                    self.notify_orch();
                    OrchResponse::DispatchV2(Box::new(dispatch))
                })
            }
            OrchAction::SendMessage(params) => {
                if has_control_chars(&params.from)
                    || has_control_chars(&params.to)
                    || params.run_id.as_deref().is_some_and(has_control_chars)
                {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "orchestration message identifiers must not contain control characters",
                    ))
                } else {
                    OrchestrationStore::mutate(&dir, |s| s.send_message(&params)).map(|msgs| {
                        self.notify_orch();
                        // Mail is committed even when a best-effort PTY pointer cannot be sent.
                        if let Err(error) = self.notify_idle_orchestration_mail(&dir) {
                            tracing::warn!(kind = ?error.kind(), "orchestration idle mail notification failed");
                        }
                        OrchResponse::Messages(msgs)
                    })
                }
            }
            OrchAction::CheckMessages {
                recipient,
                types,
                unread_only,
                ack,
                wait_ms,
                run_id,
                delivery_id,
            } => {
                let deadline = Instant::now() + Duration::from_millis(wait_ms.min(300_000));
                loop {
                    let seen_gen = *lock(&self.orch_signal.0);
                    // 빈 폴링은 캐시 사본만 확인한다. ack=false도 명시적 ACK에 쓸
                    // 영속 delivery가 필요하므로, 전달할 때는 잠금 안에서 다시 선택한다.
                    let check = |s: &mut OrchestrationStore| {
                        s.check_delivery(
                            recipient.as_deref(),
                            &types,
                            unread_only,
                            ack,
                            run_id.as_deref(),
                            delivery_id.as_deref(),
                        )
                    };
                    let checked = self.orchestration_snapshot().and_then(|mut snapshot| {
                        let preview = check(&mut snapshot)?;
                        if !preview.1.is_empty() || delivery_id.is_some() {
                            let result = OrchestrationStore::mutate(&dir, check)?;
                            self.notify_orch();
                            Ok(result)
                        } else {
                            Ok(preview)
                        }
                    });
                    match checked {
                        Ok((delivery, messages)) => {
                            let now = Instant::now();
                            if delivery_id.is_some() || !messages.is_empty() || now >= deadline {
                                break Ok(
                                    if !v2 || (ack && delivery_id.is_none() && delivery.is_none()) {
                                        OrchResponse::Messages(messages)
                                    } else {
                                        OrchResponse::Delivery { delivery, messages }
                                    },
                                );
                            }
                            let rem = deadline.saturating_duration_since(now);
                            if rem.is_zero() {
                                break Ok(if !v2 || (ack && delivery_id.is_none()) {
                                    OrchResponse::Messages(Vec::new())
                                } else {
                                    OrchResponse::Delivery {
                                        delivery: None,
                                        messages: Vec::new(),
                                    }
                                });
                            }
                            let (lock_gen, cvar) = &self.orch_signal;
                            let g = lock(lock_gen);
                            if *g == seen_gen {
                                drop(cvar.wait_timeout(g, rem.min(Duration::from_millis(250))));
                            }
                        }
                        Err(e) => break Err(e),
                    }
                }
            }
            OrchAction::CheckMessagesFiltered {
                recipient,
                run_id,
                types,
                unread_only,
                ack,
                wait_ms,
            } => self.check_messages_wait(
                &dir,
                (recipient.as_deref(), run_id.as_deref()),
                &types,
                unread_only,
                ack,
                wait_ms,
            ),
            OrchAction::AckMessageIds {
                recipient,
                run_id,
                types,
                unread_only,
                ack_ids,
            } => OrchestrationStore::mutate(&dir, |s| {
                s.ack_message_ids(
                    recipient.as_deref(),
                    run_id.as_deref(),
                    &types,
                    unread_only,
                    &ack_ids,
                )
            })
            .map(|msgs| {
                self.notify_orch();
                OrchResponse::Messages(msgs)
            }),
            OrchAction::WaitAnswer {
                question_id,
                wait_ms,
            } => {
                let deadline = Instant::now() + Duration::from_millis(wait_ms.min(300_000));
                loop {
                    let seen_gen = *lock(&self.orch_signal.0);
                    let found = self.orchestration_snapshot().ok().and_then(|s| {
                        s.messages
                            .iter()
                            .find(|m| m.reply_to.as_deref() == Some(question_id.as_str()))
                            .cloned()
                    });
                    if let Some(ans) = found {
                        // 예산 도입 전의 초과 답장은 확인 처리 전에 명시 오류로 끝낸다(인코딩 실패 뒤 ACK 금지).
                        if !krill_proto::orchestration::response_fits_frame(&OrchResponse::Answer(
                            Some(ans.clone()),
                        )) {
                            break Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                format!(
                                    "answer {} exceeds the IPC frame limit; read it with `inbox --all --json`",
                                    ans.id
                                ),
                            ));
                        }
                        let ans_id = ans.id.clone();
                        drop(OrchestrationStore::mutate(&dir, |s| {
                            for m in &mut s.messages {
                                if m.id == ans_id {
                                    m.acked = true;
                                }
                            }
                            Ok(())
                        }));
                        break Ok(OrchResponse::Answer(Some(ans)));
                    }
                    let rem = deadline.saturating_duration_since(Instant::now());
                    if rem.is_zero() {
                        break Ok(OrchResponse::Answer(None));
                    }
                    let (lock_gen, cvar) = &self.orch_signal;
                    let g = lock(lock_gen);
                    if *g == seen_gen {
                        drop(cvar.wait_timeout(g, rem.min(Duration::from_millis(250))));
                    }
                }
            }
            OrchAction::ReplyMessage {
                question_id,
                from,
                body,
            } => {
                if has_control_chars(&from) {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "orchestration message identifiers must not contain control characters",
                    ))
                } else {
                    OrchestrationStore::mutate(&dir, |s| {
                        s.reply_message(&question_id, &from, &body)
                    })
                    .map(|m| {
                        self.notify_orch();
                        if let Err(error) = self.notify_idle_orchestration_mail(&dir) {
                            tracing::warn!(
                                kind = ?error.kind(),
                                "orchestration idle mail notification failed"
                            );
                        }
                        OrchResponse::Message(m)
                    })
                }
            }
            OrchAction::CreateGate {
                from,
                question,
                options,
                run_id,
                task_id,
            } => OrchestrationStore::mutate(&dir, |s| {
                s.create_gate(&from, &question, options, run_id, task_id)
            })
            .map(|g| {
                self.notify_orch();
                OrchResponse::Gate(g)
            }),
            OrchAction::ResolveGate {
                gate_id,
                decision,
                rationale,
                resolved_by,
            } => OrchestrationStore::mutate(&dir, |s| {
                s.resolve_gate(&gate_id, &decision, rationale, &resolved_by)
            })
            .map(|g| {
                self.notify_orch();
                OrchResponse::Gate(g)
            }),
            OrchAction::StopOrReleaseWorker {
                worker_or_dispatch,
                release,
                archived_output,
            } => OrchestrationStore::mutate(&dir, |s| {
                if let Some(text) = archived_output
                    && let Some(stored) =
                        s.dispatches.iter_mut().rev().find(|d| {
                            d.id == worker_or_dispatch || d.worker_name == worker_or_dispatch
                        })
                {
                    stored.archived_output = Some(text);
                }
                s.stop_or_release_worker(&worker_or_dispatch, release)
            })
            .map(|d| {
                self.notify_orch();
                OrchResponse::Dispatch(Box::new(d))
            }),
            OrchAction::AbandonWorker { worker_or_dispatch } => {
                OrchestrationStore::mutate(&dir, |s| s.abandon_worker(&worker_or_dispatch)).map(
                    |d| {
                        self.notify_orch();
                        OrchResponse::Dispatch(Box::new(d))
                    },
                )
            }
            OrchAction::RetainWorker { worker_or_dispatch } => {
                OrchestrationStore::mutate(&dir, |s| s.retain_worker(&worker_or_dispatch)).map(
                    |d| {
                        self.notify_orch();
                        OrchResponse::Dispatch(Box::new(d))
                    },
                )
            }
            OrchAction::Reset { run_id } => OrchestrationStore::mutate(&dir, |s| {
                s.reset(run_id.as_deref());
                Ok(())
            })
            .map(|()| {
                self.notify_orch();
                OrchResponse::Reset
            }),
        };
        let resp = match res {
            Ok(resp) => resp,
            Err(e) => OrchResponse::Error(e.to_string()),
        };
        Ok(Reply::Orchestration(resp))
    }
}

fn os(e: &io::Error) -> Failure {
    Failure::Os(e.raw_os_error().unwrap_or(libc::EIO))
}

/// [`krill_proto::ipc::MAX_FRAME`]을 넘는 `reply`(본문 `bytes`) 대신 보내는 작은 응답. 어떤 Reply 종류든
/// 스트림을 닫지 않고, 기존 와이어 variant만 써서 N-1 클라이언트도 읽는다.
enum AgyLookup<'a> {
    Sync,
    Nonblocking(Option<&'a mut Vec<AgyRefreshJob>>),
}

struct AgyRefreshJob {
    session: SessionId,
    root: u32,
    tap_fg: Option<u32>,
    exited: bool,
    stored: Option<crate::agent::control::StoredLaunch>,
}

fn spawn_agy_swr_refresh(weak_reg: std::sync::Weak<Registry>, jobs: Vec<AgyRefreshJob>) {
    let ids: Vec<SessionId> = jobs.iter().map(|job| job.session).collect();
    let claimed = crate::agent_history::vault::try_claim_agy_swr(&ids);
    if claimed.is_empty() {
        return;
    }
    let claimed_set: HashSet<SessionId> = claimed.iter().copied().collect();
    let work: Vec<AgyRefreshJob> = jobs
        .into_iter()
        .filter(|job| claimed_set.contains(&job.session))
        .collect();
    let claimed_guard_ids = claimed.clone();
    if std::thread::Builder::new()
        .name("mobile-agy-swr".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            struct ReleaseGuard(Vec<SessionId>);
            impl Drop for ReleaseGuard {
                fn drop(&mut self) {
                    crate::agent_history::vault::release_agy_swr(&self.0);
                    crate::alloc::purge();
                }
            }
            crate::alloc::mobile_thread().ok();
            let _guard = ReleaseGuard(claimed_guard_ids);
            for job in work {
                let Some(reg) = weak_reg.upgrade() else {
                    break;
                };
                let agy_fg =
                    account_switch::effective_foreground_pid(job.root, job.tap_fg, job.exited);
                let (_title, discovered) = crate::agent_history::vault::resolve_live_agy_title(
                    job.session,
                    job.root,
                    agy_fg,
                    job.exited,
                    job.stored.as_ref(),
                );
                if let Some((ref conv_id, ref tp)) = discovered {
                    if tp.is_some() {
                        reg.agents
                            .bind_discovered_session(job.session, conv_id, tp.as_deref());
                    } else if let Ok(Some(found)) = account_switch::identity::discover(
                        krill_proto::usage::ProviderKind::Antigravity,
                        job.root,
                        agy_fg,
                    ) {
                        reg.agents.bind_discovered_session(
                            job.session,
                            &found.id,
                            Some(&found.path),
                        );
                    } else {
                        reg.agents
                            .bind_discovered_session(job.session, conv_id, None);
                    }
                    crate::agent_history::vault::bump_agy_swr_revision();
                }
            }
        })
        .is_err()
    {
        crate::agent_history::vault::release_agy_swr(&claimed);
    }
}

fn unwrap_win32_code(errno: i32) -> i32 {
    if (errno.cast_unsigned() & 0xFFFF_0000) == 0x8007_0000 {
        errno & 0xFFFF
    } else {
        errno
    }
}

fn session_start_failure_kind(errno: i32) -> &'static str {
    if Failure::from_holder_errno(errno) == Failure::SessionLimit {
        return "SessionLimit";
    }
    let code = if cfg!(windows) {
        unwrap_win32_code(errno)
    } else {
        errno
    };
    match std::io::Error::from_raw_os_error(code).kind() {
        io::ErrorKind::NotFound => "NotFound",
        io::ErrorKind::PermissionDenied => "PermissionDenied",
        io::ErrorKind::InvalidInput => "InvalidInput",
        io::ErrorKind::AlreadyExists => "AlreadyExists",
        io::ErrorKind::TimedOut => "TimedOut",
        io::ErrorKind::ResourceBusy => "ResourceBusy",
        io::ErrorKind::OutOfMemory => "OutOfMemory",
        _ => "Other",
    }
}

fn format_session_start_detail(
    phase: &str,
    kind: &str,
    raw_code: Option<i32>,
    wrapped: bool,
) -> String {
    let mut detail = match raw_code {
        Some(code) if !(0..=0xFFFF).contains(&code) => {
            format!("phase={phase} kind={kind} errno={code} hex=0x{code:08x} wrapped={wrapped}")
        }
        Some(code) => format!("phase={phase} kind={kind} errno={code} wrapped={wrapped}"),
        None => format!("phase={phase} kind={kind} wrapped={wrapped}"),
    };
    detail.retain(|c| !c.is_ascii_control());
    if detail.len() > 256 {
        let mut end = 256;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
    }
    detail
}

fn record_session_start_failed(
    dir: Option<&Path>,
    phase: &str,
    kind: &str,
    raw_code: Option<i32>,
    wrapped: bool,
    file: &str,
    line: u32,
) {
    use krill_proto::error_reporting::{self, Code, Event, Process};

    let Some(state) = dir else {
        return;
    };
    if let Ok(mut event) = Event::new(
        Process::Daemon,
        Code::SessionStartFailed,
        error_reporting::source_location(file, line),
    ) {
        let detail = format_session_start_detail(phase, kind, raw_code, wrapped);
        if !detail.is_empty() {
            event.detail = Some(detail);
        }
        error_reporting::record_at(state, event).ok();
    }
}

fn is_tui_chrome_line(trimmed: &str) -> bool {
    let only_box_or_rule = trimmed.chars().all(|c| {
        matches!(
            c,
            '─' | '│'
                | '╭'
                | '╮'
                | '╰'
                | '╯'
                | '┌'
                | '┐'
                | '└'
                | '┘'
                | '├'
                | '┤'
                | '┬'
                | '┴'
                | '┼'
                | '═'
                | '║'
                | '╔'
                | '╗'
                | '╚'
                | '╝'
                | '-'
                | '_'
                | '='
                | ' '
        )
    });
    if only_box_or_rule {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("esc to interrupt")
        || lower.contains("shift+tab to")
        || lower.contains("ctrl+c to")
        || lower.contains("% context")
        || lower == "?"
        || lower == ">"
        || lower == "›"
}

fn oversized_reply(reply: &Reply, bytes: u64) -> Reply {
    if matches!(reply, Reply::Orchestration(_)) {
        Reply::Orchestration(krill_proto::orchestration::OrchResponse::Error(
            krill_proto::orchestration::frame_limit_error(bytes),
        ))
    } else if matches!(reply, Reply::Bot(_)) {
        Reply::Bot(krill_proto::bots::BotRpcResponse::Error(
            krill_proto::orchestration::frame_limit_error(bytes),
        ))
    } else {
        Reply::Failed(Failure::Os(libc::EMSGSIZE))
    }
}

/// Resolves a bot agent profile's executable path against `profile.env["PATH"]`, the login `PATH`,
/// process `PATH`, and `~/.local/bin` so missing CLIs produce a clear refusal before spawning a PTY.
/// Path-qualified executables use `krill_proto::agent::which` so Windows `PATHEXT` resolution and
/// Unix `+x` permission checks match normal PATH lookups.
fn resolve_bot_cli_executable(
    program: &str,
    profile_env: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    use krill_proto::os::ffi::OsStrExt as _;
    let path = Path::new(program);
    if program.contains(std::path::is_separator) {
        return krill_proto::agent::which(program, std::ffi::OsStr::new(""));
    }
    if path.components().count() > 1 {
        let file_name = path.file_name().and_then(|n| n.to_str())?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        return krill_proto::agent::which(file_name, parent.as_os_str());
    }
    if let Some(profile_path) = profile_env
        .iter()
        .find(|(k, _)| {
            #[cfg(windows)]
            {
                k.eq_ignore_ascii_case("PATH")
            }
            #[cfg(not(windows))]
            {
                k.as_str() == "PATH"
            }
        })
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
        && let Some(found) = krill_proto::agent::which(program, std::ffi::OsStr::new(profile_path))
    {
        return Some(found);
    }
    let login_path = std::ffi::OsStr::from_bytes(crate::shell::login_path());
    if let Some(found) = krill_proto::agent::which(program, login_path) {
        return Some(found);
    }
    if let Some(env_path) = std::env::var_os("PATH")
        && let Some(found) = krill_proto::agent::which(program, &env_path)
    {
        return Some(found);
    }
    krill_proto::os::home_dir()
        .and_then(|home| krill_proto::agent::which(program, home.join(".local/bin").as_os_str()))
}

/// Preflights native Antigravity credentials before spawning a background/bot/scheduled/resumed
/// `agy` session so unauthenticated or transiently locked reboot states never enter the OAuth TUI
/// and launch a browser window.
#[cfg_attr(
    test,
    expect(
        clippy::unnecessary_wraps,
        reason = "matches non-test Result signature"
    )
)]
pub(super) fn preflight_agy_background_launch(env: &[(Vec<u8>, Vec<u8>)]) -> io::Result<()> {
    #[cfg(test)]
    {
        let _ = env;
        Ok(())
    }
    #[cfg(not(test))]
    {
        let mut launch_env: std::collections::BTreeMap<String, String> = env
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    std::str::from_utf8(k).ok()?.to_owned(),
                    std::str::from_utf8(v).ok()?.to_owned(),
                ))
            })
            .collect();
        for key in [
            "HOME",
            "USERPROFILE",
            "AGY_CLI_FORCE_FILE_TOKEN_STORAGE",
            "SSH_CONNECTION",
            "SSH_TTY",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            if !launch_env.contains_key(key)
                && let Ok(val) = std::env::var(key)
            {
                launch_env.insert(key.to_owned(), val);
            }
        }
        let home = launch_env
            .get("HOME")
            .or_else(|| launch_env.get("USERPROFILE"))
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(krill_proto::os::home_dir)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Antigravity is not signed in (auth_required).",
                )
            })?;
        crate::usage::accounts::agy_native::preflight_auth_for_background_launch(&home, &launch_env)
            .map_err(|msg| io::Error::new(io::ErrorKind::PermissionDenied, msg))
    }
}

fn sanitize_spawn_cwd(cwd: &mut Vec<u8>) {
    if let Ok(text) = std::str::from_utf8(cwd) {
        let cleaned = krill_proto::orchestration::clean_workspace_path_str(text);
        if cleaned != text {
            *cwd = cleaned.into_bytes();
        }
    }
    #[cfg(windows)]
    {
        use krill_proto::os::ffi::OsStrExt as _;
        // Canonical paths arrive from the CLI and persisted layouts. cmd.exe rejects
        // verbatim drive paths as UNC and otherwise starts in C:\Windows instead.
        let path = krill_proto::os::shell_path(Path::new(std::ffi::OsStr::from_bytes(cwd)));
        *cwd = path.into_os_string().into_encoded_bytes();
    }
}

/// holder Spawn이 ENOENT로 실패했을 때:
/// - `cwd` 디렉터리가 삭제·유실되어 존재하지 않는 경우 존재하는 상위 폴더·홈·빈 cwd로 폴백한다.
/// - 로그인 셸(`-l`) 실행 파일이 삭제된 경우(`/opt/homebrew/bin/fish` 등) 시스템 기본 셸로 폴백한다.
///
/// 사용자 지정 실행 파일(`Program::Custom`) 자체가 없는 경우는 건드리지 않고 ENOENT를 그대로 돌려준다.
/// holder `Spawn` 실패가 "경로 없음"인가. Windows holder는 Win32 코드를 그대로 보낸다: 지워진 cwd는
/// `ERROR_DIRECTORY`(267), 중간 폴더까지 없으면 `ERROR_PATH_NOT_FOUND`(3), 파일은 `ERROR_FILE_NOT_FOUND`(2=ENOENT).
fn missing_path_errno(errno: i32) -> bool {
    let win32 = unwrap_win32_code(errno);
    Failure::from_holder_errno(errno) == Failure::Os(libc::ENOENT)
        || cfg!(windows) && matches!(win32, 2 | 3 | 15 | 21 | 53 | 67 | 123 | 206 | 267)
}

#[cfg_attr(
    unix,
    expect(
        clippy::ptr_arg,
        reason = "Windows fallback replaces argv and pushes to env"
    )
)]
fn fallback_spawn_on_enoent(
    argv: &mut Vec<Vec<u8>>,
    cwd: &mut Vec<u8>,
    env: &mut Vec<(Vec<u8>, Vec<u8>)>,
) -> bool {
    use krill_proto::os::ffi::OsStrExt as _;
    let mut changed = false;
    if !cwd.is_empty() {
        let cwd_path = Path::new(std::ffi::OsStr::from_bytes(cwd));
        if !cwd_path.is_dir() {
            let next_cwd = cwd_path
                .ancestors()
                .skip(1)
                .find(|a| !a.as_os_str().is_empty() && a.is_dir())
                .map(|a| a.as_os_str().as_bytes().to_vec())
                .or_else(|| {
                    env.iter()
                        .find(|(k, _)| {
                            k.as_slice() == b"HOME" || k.eq_ignore_ascii_case(b"USERPROFILE")
                        })
                        .map(|(_, v)| Path::new(std::ffi::OsStr::from_bytes(v)))
                        .filter(|p| p.is_dir())
                        .map(|p| p.as_os_str().as_bytes().to_vec())
                })
                .or_else(|| {
                    krill_proto::os::home_dir()
                        .filter(|p| p.is_dir())
                        .map(|p| p.as_os_str().as_bytes().to_vec())
                })
                .unwrap_or_default();
            *cwd = next_cwd;
            changed = true;
        }
    }
    #[cfg(unix)]
    if argv.get(1).is_some_and(|a| a.as_slice() == b"-l")
        && let Some(prog) = argv.first_mut()
    {
        let prog_path = Path::new(std::ffi::OsStr::from_bytes(prog));
        if prog_path.is_absolute() && !prog_path.is_file() {
            let old_prog = prog.clone();
            for candidate in [&b"/bin/zsh"[..], &b"/bin/sh"[..]] {
                if Path::new(std::ffi::OsStr::from_bytes(candidate)).is_file()
                    && prog.as_slice() != candidate
                {
                    *prog = candidate.to_vec();
                    for (k, v) in env.iter_mut() {
                        if k.as_slice() == b"SHELL" && v.as_slice() == old_prog.as_slice() {
                            *v = candidate.to_vec();
                        }
                    }
                    changed = true;
                    break;
                }
            }
        }
    }
    #[cfg(windows)]
    if !is_wrapper_argv(argv)
        && argv.first().is_some_and(|prog| {
            env.iter()
                .any(|(k, v)| k.eq_ignore_ascii_case(b"SHELL") && v.eq_ignore_ascii_case(prog))
                && !Path::new(std::ffi::OsStr::from_bytes(prog)).is_file()
        })
    {
        let fallback = Shell::Login.launch();
        if fallback.argv != *argv {
            *argv = fallback.argv;
            for (k, v) in fallback.env {
                if k.eq_ignore_ascii_case(b"PROMPT") || k.eq_ignore_ascii_case(b"SHELL") {
                    env.push((k, v));
                }
            }
            crate::shell::dedupe_env(env);
            changed = true;
        }
    }
    changed
}

fn has_control_chars(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn is_terminal_control_seq(payload: &[u8]) -> bool {
    if payload.is_empty() || payload.contains(&b'\n') || payload.contains(&b'\r') {
        return false;
    }
    if payload
        .iter()
        .all(|&b| (b < 0x20 && b != b'\t') || b == 0x7f)
    {
        return true;
    }
    if payload.len() <= 16 && payload.first() == Some(&0x1b) {
        if payload.len() <= 2 {
            return true;
        }
        if payload == b"\x1b[200~" || payload == b"\x1b[201~" {
            return false;
        }
        if let Some(rest) = payload.strip_prefix(b"\x1b[")
            && let Some((&final_byte, params)) = rest.split_last()
        {
            return (0x40..=0x7e).contains(&final_byte)
                && params.iter().all(|&b| (0x20..=0x3f).contains(&b));
        }
        if let Some(rest) = payload.strip_prefix(b"\x1bO")
            && let [final_byte] = rest
        {
            return (0x40..=0x7e).contains(final_byte);
        }
    }
    false
}

fn read_only(req: &Request) -> bool {
    matches!(
        req,
        Request::List
            | Request::States
            | Request::SessionAccounts
            | Request::CaptureSince { .. }
            | Request::Capture { .. }
            | Request::GitChanges { .. }
            | Request::WorktreeRefs { .. }
            | Request::WaitStatus { .. }
            | Request::Sidebar
            | Request::AgentLaunch { .. }
            | Request::AgentSettings { permission: None }
            | Request::Cold { .. }
            | Request::Resources(Action::Query)
            | Request::GitWatch(_)
            | Request::AgentHistory(
                krill_proto::agent_history::Action::Scan
                    | krill_proto::agent_history::Action::Search(_)
                    | krill_proto::agent_history::Action::ChatPoll { .. }
            )
            | Request::Orchestration(
                krill_proto::orchestration::OrchAction::Snapshot
                    | krill_proto::orchestration::OrchAction::SnapshotV2
                    | krill_proto::orchestration::OrchAction::SnapshotPage { .. }
                    | krill_proto::orchestration::OrchAction::CheckMessagesFiltered {
                        ack: false,
                        ..
                    }
            )
            | Request::RemoteFs(_)
            | Request::Bot(_)
            | Request::Voice(_)
    ) && !matches!(req, Request::RemoteFs(r) if !r.is_read_only())
        && !matches!(req, Request::Bot(r) if !r.is_read_only())
        && !matches!(req, Request::Voice(r) if !r.is_read_only())
}

#[cfg(test)]
#[cfg(unix)]
mod tests;

#[cfg(all(test, unix))]
mod orchestration_delivery_tests {
    use super::*;
    use krill_proto::orchestration::{MessageType, OrchAction, OrchestrationStore, SendParams};
    use std::io::Read as _;

    #[test]
    fn empty_orchestration_waiters_preserve_file_and_response_shapes() -> io::Result<()> {
        use crate::agent::control::cache::stamp;
        use krill_proto::orchestration::{ORCH_FILE, OrchResponse};
        let dir = std::env::temp_dir().join(format!("krill-empty-waits-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        OrchestrationStore::mutate(&dir, |s| s.create_run("empty wait", None))?;
        let before = stamp(&dir.join(ORCH_FILE)).unwrap();
        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let start = Arc::new(std::sync::Barrier::new(5));
        let waiters: Vec<_> = (0..4)
            .map(|i| {
                let reg = Arc::clone(&reg);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    reg.orchestration(
                        &Peer {
                            pid: None,
                            scope: Scope::Full,
                        },
                        if i < 2 {
                            OrchAction::CheckMessagesLegacy {
                                recipient: Some("empty".into()),
                                types: vec![],
                                unread_only: true,
                                ack: i == 1,
                                wait_ms: 800,
                            }
                        } else {
                            OrchAction::CheckMessages {
                                recipient: Some("empty".into()),
                                types: vec![],
                                unread_only: true,
                                ack: i == 3,
                                wait_ms: 800,
                                run_id: None,
                                delivery_id: None,
                            }
                        },
                    )
                })
            })
            .collect();
        start.wait();
        // 250ms 재확인을 여러 번 거쳐도 inode/mtime이 바뀌지 않아야 한다.
        for _ in 0..35 {
            std::thread::sleep(Duration::from_millis(25));
            assert_eq!(stamp(&dir.join(ORCH_FILE)).as_ref(), Some(&before));
        }
        for (i, waiter) in waiters.into_iter().enumerate() {
            let response = waiter.join().unwrap().unwrap();
            if i == 2 {
                assert!(
                    matches!(response, Reply::Orchestration(OrchResponse::Delivery { delivery: None, messages }) if messages.is_empty())
                );
            } else {
                assert!(
                    matches!(response, Reply::Orchestration(OrchResponse::Messages(messages)) if messages.is_empty())
                );
            }
        }
        std::fs::remove_dir_all(dir)
    }

    /// foundation-02: 예산 도입 전의 초과 답장(>1MiB)은 확인 처리 전에 명시 오류로 끝난다(ACK 없음).
    #[test]
    fn oversized_legacy_answer_is_reported_before_acknowledging() -> io::Result<()> {
        use krill_proto::orchestration::{OrchMessage, OrchResponse};
        let dir =
            std::env::temp_dir().join(format!("krill-answer-oversize-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let question = OrchestrationStore::mutate(&dir, |s| {
            s.send_message(&SendParams {
                from: "worker".into(),
                to: "coordinator".into(),
                msg_type: MessageType::Question,
                summary: "question".into(),
                body: String::new(),
                run_id: None,
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })
            .map(|messages| messages[0].clone())
        })?;
        OrchestrationStore::mutate(&dir, |s| {
            s.messages.push(OrchMessage {
                id: "msg-legacy-answer".into(),
                run_id: None,
                task_id: None,
                dispatch_id: None,
                from: "coordinator".into(),
                to: "worker".into(),
                msg_type: MessageType::Answer,
                summary: "answer".into(),
                body: "y".repeat(1_100_000),
                reply_to: Some(question.id.clone()),
                acked: false,
                created_at_ms: 2,
            });
            Ok(())
        })?;
        let reg = Registry::new(None, Some(&dir));
        let reply = reg
            .orchestration(
                &Peer {
                    pid: None,
                    scope: Scope::Full,
                },
                OrchAction::WaitAnswer {
                    question_id: question.id,
                    wait_ms: 0,
                },
            )
            .unwrap();
        assert!(matches!(
            reply,
            Reply::Orchestration(OrchResponse::Error(msg)) if msg.contains("exceeds")
        ));
        let stored = OrchestrationStore::load(&dir)?;
        assert!(
            stored
                .messages
                .iter()
                .any(|m| m.id == "msg-legacy-answer" && !m.acked)
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn cached_answer_wait_observes_external_reply_without_idle_writes() -> io::Result<()> {
        use crate::agent::control::cache::stamp;
        use krill_proto::orchestration::{ORCH_FILE, OrchResponse};
        let dir = std::env::temp_dir().join(format!("krill-answer-wait-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let question = OrchestrationStore::mutate(&dir, |s| {
            s.send_message(&SendParams {
                from: "worker".into(),
                to: "coordinator".into(),
                msg_type: MessageType::Question,
                summary: "question".into(),
                body: String::new(),
                run_id: None,
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })
            .map(|messages| messages[0].clone())
        })?;
        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let waiter = {
            let reg = Arc::clone(&reg);
            let question_id = question.id.clone();
            std::thread::spawn(move || {
                reg.orchestration(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    OrchAction::WaitAnswer {
                        question_id,
                        wait_ms: 5000,
                    },
                )
            })
        };
        let before = stamp(&dir.join(ORCH_FILE));
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(stamp(&dir.join(ORCH_FILE)), before);
        // 외부 프로세스의 저장처럼 notify 없이 파일 교체만으로 캐시를 무효화한다.
        let reply = OrchestrationStore::mutate(&dir, |s| {
            s.reply_message(&question.id, "coordinator", "answer")
        })?;
        assert_eq!(
            waiter.join().unwrap().unwrap(),
            Reply::Orchestration(OrchResponse::Answer(Some(reply.clone())))
        );
        assert!(
            OrchestrationStore::load(&dir)?
                .messages
                .iter()
                .any(|m| m.id == reply.id && m.acked)
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn cached_wait_wakes_and_replays_durable_delivery_before_explicit_ack() -> io::Result<()> {
        use crate::agent::control::cache::stamp;
        use krill_proto::orchestration::{ORCH_FILE, OrchResponse};
        let dir = std::env::temp_dir().join(format!("krill-delivery-wait-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let run = OrchestrationStore::mutate(&dir, |s| s.create_run("delivery wait", None))?;
        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let action = OrchAction::CheckMessages {
            recipient: Some("receiver".into()),
            types: vec![MessageType::Status],
            unread_only: true,
            ack: false,
            wait_ms: 5000,
            run_id: Some(run.id.clone()),
            delivery_id: None,
        };
        let waiter = {
            let reg = Arc::clone(&reg);
            let action = action.clone();
            std::thread::spawn(move || {
                reg.orchestration(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    action,
                )
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        let send = |summary: &str| {
            reg.orchestration(
                &peer,
                OrchAction::SendMessage(SendParams {
                    from: "sender".into(),
                    to: "receiver".into(),
                    msg_type: MessageType::Status,
                    summary: summary.into(),
                    body: String::new(),
                    run_id: Some(run.id.clone()),
                    task_id: None,
                    dispatch_id: None,
                    outcome: None,
                    reply_to: None,
                }),
            )
            .unwrap()
        };
        let sent_at = Instant::now();
        send("first");
        let Reply::Orchestration(OrchResponse::Delivery {
            delivery: Some(batch),
            messages,
        }) = waiter.join().unwrap().unwrap()
        else {
            panic!("expected durable delivery")
        };
        assert!(sent_at.elapsed() < Duration::from_secs(2));
        assert_eq!(messages.len(), 1);
        assert!(!messages[0].acked);
        assert_eq!(
            OrchestrationStore::load(&dir)?.deliveries.as_slice(),
            std::slice::from_ref(&batch)
        );
        let before = stamp(&dir.join(ORCH_FILE));
        assert_eq!(
            reg.orchestration(&peer, action.clone()).unwrap(),
            Reply::Orchestration(OrchResponse::Delivery {
                delivery: Some(batch.clone()),
                messages: messages.clone()
            })
        );
        assert_eq!(stamp(&dir.join(ORCH_FILE)), before);
        send("later");
        let OrchAction::CheckMessages {
            recipient,
            types,
            unread_only,
            wait_ms,
            run_id,
            ..
        } = action
        else {
            unreachable!()
        };
        let ack = OrchAction::CheckMessages {
            recipient,
            types,
            unread_only,
            wait_ms,
            run_id,
            ack: true,
            delivery_id: Some(batch.id.clone()),
        };
        let Reply::Orchestration(OrchResponse::Delivery {
            delivery: Some(acked),
            messages,
        }) = reg.orchestration(&peer, ack).unwrap()
        else {
            panic!("expected explicit ack delivery")
        };
        assert_eq!(acked.id, batch.id);
        assert!(acked.acked);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].acked);
        let store = OrchestrationStore::load(&dir)?;
        assert_eq!(store.messages.iter().filter(|m| !m.acked).count(), 1);
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn idle_headless_mail_pointer_is_delivered_once_and_busy_mail_is_deferred() -> io::Result<()> {
        let dir =
            std::env::temp_dir().join(format!("krill-idle-mail-pointer-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let reg = Registry::new(None, Some(&dir));
        let id = SessionId(411);
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_millis(100)))?;
        let session = Session::start(
            id,
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&reg.look),
            None,
        )?;
        lock(&reg.state).sessions.insert(
            id,
            Entry {
                session,
                pid: 0,
                cwd: PathBuf::from("/tmp"),
                wrapped: false,
            },
        );
        let run = OrchestrationStore::mutate(&dir, |s| {
            let run = s.create_run("mail pointers", Some(id.0))?;
            s.start_worker(&run.id, None, "worker", "codex", Some(id.0), "/tmp", "work")?;
            Ok(run)
        })?;
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let status = |name: &str, ns| -> io::Result<()> {
            reg.status.hook(&[(
                id,
                &HookEvent {
                    id: hook::EventId {
                        pid: 99,
                        unix_ns: ns,
                    },
                    agent: "claude".into(),
                    ppid: 0,
                    input: hook::Input::Complete,
                    payload: serde_json::to_vec(&serde_json::json!({"hook_event_name": name}))?,
                },
            )])?;
            Ok(())
        };
        let send = |to: &str, summary: &str| -> io::Result<()> {
            reg.orchestration(
                &peer,
                OrchAction::SendMessage(SendParams {
                    from: "sender".into(),
                    to: to.into(),
                    msg_type: MessageType::Status,
                    summary: summary.into(),
                    body: "private body stays in mailbox".into(),
                    run_id: Some(run.id.clone()),
                    task_id: None,
                    dispatch_id: None,
                    outcome: None,
                    reply_to: None,
                }),
            )
            .map_err(|e| io::Error::other(e.to_string()))?;
            Ok(())
        };
        status("UserPromptSubmit", 1)?;
        send("worker", "busy mail")?;
        let mut bytes = [0_u8; 4096];
        assert!(terminal.read(&mut bytes).is_err());
        assert!(
            OrchestrationStore::load(&dir)?
                .idle_mail_notified
                .get(&id.0)
                .is_some_and(|ids| ids.len() == 1)
        );
        status("Stop", 2)?;
        // A subsequent send retries previously unnotified mail for newly idle recipients.
        send("coordinator", "wake idle coordinator")?;
        let count = terminal.read(&mut bytes)?;
        let pointer = String::from_utf8_lossy(bytes.get(..count).unwrap());
        assert!(pointer.contains("orchestration check --to"));
        assert!(pointer.contains(&run.id));
        assert!(!pointer.contains("private body"));
        let recorded = OrchestrationStore::load(&dir)?.idle_mail_notified;
        assert!(recorded.get(&id.0).is_some_and(|ids| ids.len() == 3));
        // Drain the second mailbox pointer if the socket split the writes.
        while terminal.read(&mut bytes).is_ok() {}
        reg.notify_idle_orchestration_mail(&dir)?;
        assert!(terminal.read(&mut bytes).is_err());
        assert_eq!(OrchestrationStore::load(&dir)?.idle_mail_notified, recorded);
        drop(reg);
        drop(terminal);
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn deliver_input_to_tap_refuses_agy_oauth_login_screen_with_zero_bytes_written()
    -> io::Result<()> {
        use std::io::Write as _;

        let look = Arc::new(Appearance::load(PathBuf::new()));
        for (idx, banner) in [
            b"Welcome to the Antigravity CLI. You are currently not signed in.\r\n\
              Select login method:\r\n\
              > 1. Google OAuth\r\n\
                2. Use a Google Cloud project\r\n"
                .as_slice(),
            b"Your browser should open automatically.\r\n\
              -> Click here to authenticate\r\n\
              Enter authorization code:\r\n"
                .as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let (master, mut terminal) = UnixStream::pair()?;
            terminal.set_read_timeout(Some(Duration::from_millis(120)))?;
            let session = Session::start(
                SessionId(41_900 + idx as u64),
                OwnedFd::from(master),
                WinSize { cols: 80, rows: 24 },
                Arc::clone(&look),
                None,
            )?;
            let tap = session.tap();
            terminal.write_all(banner)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while !krill_proto::screen::agy::auth_login_screen(&tap.capture().0) {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for login banner on screen"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let err = Registry::deliver_input_to_tap(&tap, b"run bot task", true)
                .expect_err("automated input must be refused on agy auth login screen");
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
            assert!(err.to_string().contains("auth_required"), "err={err}");

            let mut buf = [0_u8; 64];
            let read_res = terminal.read(&mut buf);
            assert!(
                matches!(
                    read_res,
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        )
                ),
                "expected 0 bytes written to PTY, got {read_res:?}"
            );
            drop(session);
        }
        Ok(())
    }

    #[test]
    fn deliver_input_to_tap_allows_agy_1517_authenticated_composer_with_leftover_welcome_banner()
    -> io::Result<()> {
        use std::io::Write as _;

        let look = Arc::new(Appearance::load(PathBuf::new()));
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_secs(2)))?;
        let session = Session::start(
            SessionId(41_909),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&look),
            None,
        )?;
        let tap = session.tap();
        terminal.write_all(
            b"Welcome to the Antigravity CLI. You are currently not signed in.\r\n\
              Antigravity CLI 1.5.17\r\n\
              ----------------------------------------------------------------\r\n\
              > \r\n",
        )?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !tap.capture().0.rows.iter().any(|row| {
            row.iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("Antigravity CLI 1.5.17")
        }) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for agy 1.5.17 authenticated banner"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !krill_proto::screen::agy::auth_login_screen(&tap.capture().0),
            "leftover welcome banner above authenticated agy session must not block input"
        );
        Registry::deliver_input_to_tap(&tap, b"run bot task", true)?;
        let mut buf = [0_u8; 64];
        let n = terminal.read(&mut buf)?;
        assert!(n > 0, "expected prompt bytes delivered to PTY");
        drop(session);
        Ok(())
    }

    #[test]
    fn deliver_input_to_tap_paces_unbracketed_and_delayed_echo_submit() -> io::Result<()> {
        use std::io::Write as _;

        let look = Arc::new(Appearance::load(PathBuf::new()));

        // 1. Unbracketed mode (ConPTY without VT passthrough / rejoined PTY): payload and CR must
        //    not coalesce into one paste burst.
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_secs(2)))?;
        let session = Session::start(
            SessionId(412),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&look),
            None,
        )?;
        let tap = session.tap();
        let tui = std::thread::spawn(move || -> io::Result<(Vec<u8>, Vec<u8>, Duration)> {
            let mut buf = [0_u8; 512];
            let n1 = terminal.read(&mut buf)?;
            let first = buf[..n1].to_vec();
            terminal.write_all(b"> [Krill orchestration] Unread mail.")?;
            let echoed_at = Instant::now();
            let n2 = terminal.read(&mut buf)?;
            let gap = echoed_at.elapsed();
            let second = buf[..n2].to_vec();
            Ok((first, second, gap))
        });
        let text = b"[Krill orchestration]\nUnread mail.\r\n\nCheck now.";
        Registry::deliver_input_to_tap(&tap, text, true)?;
        let (first, second, gap) = tui.join().unwrap()?;
        assert_eq!(first, b"[Krill orchestration] | Unread mail. | Check now.");
        assert_eq!(second, b"\r");
        assert!(
            gap >= Duration::from_millis(220),
            "expected PASTE_SUBMIT_DELAY after unbracketed echo, got {gap:?}"
        );
        drop(session);

        // 1b. Unbracketed mode where OpenConsole emits an immediate cursor-hide byte and the TUI
        //     echoes the pasted text 120ms later (> 60ms): CR must wait PASTE_SUBMIT_DELAY after
        //     the late echo chunk instead of firing right after a blind sleep.
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_secs(2)))?;
        let session = Session::start(
            SessionId(415),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&look),
            None,
        )?;
        let tap = session.tap();
        let tui = std::thread::spawn(move || -> io::Result<(Vec<u8>, Vec<u8>, Duration)> {
            let mut buf = [0_u8; 512];
            let n1 = terminal.read(&mut buf)?;
            let first = buf[..n1].to_vec();
            terminal.write_all(b"\x1b[?25l")?;
            std::thread::sleep(Duration::from_millis(120));
            terminal.write_all(b"> [Krill orchestration] | Unread mail. | Check now.\x1b[?25h")?;
            let late_echo_at = Instant::now();
            let n2 = terminal.read(&mut buf)?;
            let gap = late_echo_at.elapsed();
            let second = buf[..n2].to_vec();
            Ok((first, second, gap))
        });
        Registry::deliver_input_to_tap(&tap, text, true)?;
        let (first, second, gap) = tui.join().unwrap()?;
        assert_eq!(first, b"[Krill orchestration] | Unread mail. | Check now.");
        assert_eq!(second, b"\r");
        assert!(
            gap >= Duration::from_millis(220),
            "expected PASTE_SUBMIT_DELAY after late ConPTY echo, got {gap:?}"
        );
        drop(session);

        // 2. Bracketed mode with delayed multi-chunk echo: CR must wait PASTE_SUBMIT_DELAY after
        //    the final echoed chunk, not merely from write_input enqueue time.
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_secs(2)))?;
        let session = Session::start(
            SessionId(413),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            look,
            None,
        )?;
        let tap = session.tap();
        terminal.write_all(b"\x1b[?2004h")?;
        tap.wait_timeout(Duration::from_secs(1), |p| p.end >= 8);
        assert!(
            tap.capture_shared_with_modes()
                .3
                .contains(krill_proto::term_modes::Modes::BRACKETED_PASTE)
        );
        let tui = std::thread::spawn(move || -> io::Result<(Vec<u8>, Vec<u8>, Duration)> {
            let mut buf = [0_u8; 512];
            let n1 = terminal.read(&mut buf)?;
            let first = buf[..n1].to_vec();
            std::thread::sleep(Duration::from_millis(200));
            terminal.write_all(b"> [Krill orchestration] ")?;
            std::thread::sleep(Duration::from_millis(30));
            terminal.write_all(b"Unread mail.")?;
            let last_echo_at = Instant::now();
            let n2 = terminal.read(&mut buf)?;
            let gap = last_echo_at.elapsed();
            let second = buf[..n2].to_vec();
            Ok((first, second, gap))
        });
        Registry::deliver_input_to_tap(&tap, text, true)?;
        let (first, second, gap) = tui.join().unwrap()?;
        assert!(first.starts_with(b"\x1b[200~") && first.ends_with(b"\x1b[201~"));
        assert_eq!(second, b"\r");
        assert!(
            gap >= Duration::from_millis(220),
            "expected PASTE_SUBMIT_DELAY after delayed echo, got {gap:?}"
        );
        Ok(())
    }

    #[test]
    fn idle_mail_pointer_skips_mid_turn_post_invocation_and_dedupes_same_done_epoch()
    -> io::Result<()> {
        use std::io::Write as _;

        let dir = std::env::temp_dir().join(format!("krill-idle-mail-agy-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let id = SessionId(414);
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_millis(120)))?;
        let session = Session::start(
            id,
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&reg.look),
            None,
        )?;
        let root_pid = std::process::id();
        {
            let mut st = lock(&reg.state);
            st.roots.insert(id, root_pid).unwrap();
            st.sessions.insert(
                id,
                Entry {
                    session,
                    pid: root_pid,
                    cwd: PathBuf::from("/tmp"),
                    wrapped: false,
                },
            );
        }
        let run = OrchestrationStore::mutate(&dir, |s| s.create_run("agy mail", Some(id.0)))?;
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let emit_hook = move |reg: &Registry, name: &str, pid: u32| -> io::Result<()> {
            let (mut client, server) = UnixStream::pair()?;
            let hello = krill_proto::daemon::client_hello(ipc::ClientRole::Cli, "test");
            client.write_all(&hello.encode()?)?;
            let ev = HookEvent {
                id: hook::EventId {
                    pid,
                    unix_ns: status::now_ns(),
                },
                agent: "agy".into(),
                ppid: root_pid,
                input: hook::Input::Complete,
                payload: serde_json::to_vec(&serde_json::json!({"hook_event_name": name}))?,
            };
            client.write_all(&ev.encode()?)?;
            let server_hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            };
            let res = reg.serve(server, &server_hello);
            drop(client);
            res
        };
        let send = |summary: &str| -> io::Result<()> {
            reg.orchestration(
                &peer,
                OrchAction::SendMessage(SendParams {
                    from: "worker-1".into(),
                    to: "coordinator".into(),
                    msg_type: MessageType::Status,
                    summary: summary.into(),
                    body: "details".into(),
                    run_id: Some(run.id.clone()),
                    task_id: None,
                    dispatch_id: None,
                    outcome: None,
                    reply_to: None,
                }),
            )
            .map_err(|e| io::Error::other(e.to_string()))?;
            Ok(())
        };

        // 1. Mid-turn PostInvocation followed quickly by PreInvocation must defer, not inject.
        emit_hook(&reg, "PostInvocation", 101)?;
        let resume_reg = Arc::clone(&reg);
        let resume = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            emit_hook(&resume_reg, "PreInvocation", 102)
        });
        send("mid-turn report")?;
        resume.join().unwrap()?;
        let mut buf = [0_u8; 4096];
        assert!(terminal.read(&mut buf).is_err());
        assert!(
            OrchestrationStore::load(&dir)?
                .idle_mail_notified
                .is_empty()
        );

        // 2. Final Stop hook delivers the deferred pointer once, and additional sends in the same
        //    Done epoch (before PreInvocation) do not type duplicate pointers onto the composer.
        emit_hook(&reg, "Stop", 103)?;
        send("second report in same idle epoch")?;
        let mut captured = Vec::new();
        while let Ok(n) = terminal.read(&mut buf) {
            if n == 0 {
                break;
            }
            captured.extend_from_slice(&buf[..n]);
        }
        let text = String::from_utf8_lossy(&captured);
        assert_eq!(
            text.matches("[Krill orchestration] Unread mail.").count(),
            1,
            "expected exactly one pointer in same Done epoch, got: {text:?}"
        );
        assert!(text.contains("Unread mail. From: 'worker-1' (status). Read with:"));
        assert!(text.ends_with('\r'));

        // 3. After a new turn runs and finishes without acking the second message, the new Done
        //    epoch notifies again cleanly.
        emit_hook(&reg, "PreInvocation", 104)?;
        std::thread::sleep(Duration::from_millis(5));
        emit_hook(&reg, "Stop", 105)?;
        captured.clear();
        while let Ok(n) = terminal.read(&mut buf) {
            if n == 0 {
                break;
            }
            captured.extend_from_slice(&buf[..n]);
        }
        let second_text = String::from_utf8_lossy(&captured);
        assert_eq!(
            second_text
                .matches("[Krill orchestration] Unread mail.")
                .count(),
            1,
            "expected unacked second message to notify on next Stop, got: {second_text:?}"
        );
        assert!(second_text.ends_with('\r'));

        drop(reg);
        drop(terminal);
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn unbound_agy_ignores_startup_conversation_files_until_presence_lock() -> io::Result<()> {
        use std::fmt::Write as _;
        use std::io::Read as _;
        use std::process::{Child, Command, Stdio};

        struct ChildGuard(Option<Child>);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(mut child) = self.0.take() {
                    child.kill().ok();
                    child.wait().ok();
                }
            }
        }

        fn spawn_holding(paths: &[&Path]) -> io::Result<ChildGuard> {
            let mut script = String::from("exec");
            for (idx, _) in paths.iter().enumerate() {
                write!(script, " {}<\"${}\"", idx + 3, idx + 1).map_err(io::Error::other)?;
            }
            script.push_str("; printf ready; exec sleep 30");
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c").arg(script).arg("krill-test-holder");
            for path in paths {
                cmd.arg(path);
            }
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut child = cmd.spawn()?;
            let mut ready = [0_u8; 5];
            if let Some(mut stdout) = child.stdout.take() {
                stdout.read_exact(&mut ready)?;
            }
            assert_eq!(&ready, b"ready");
            Ok(ChildGuard(Some(child)))
        }

        let dir = std::env::temp_dir().join(format!("krill-agy-unbound-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let agy_home = dir.join(".gemini/antigravity-cli");
        let old_conv = "0373fc46-0d68-498c-9c95-c8dd17bd4352";
        let new_conv = "1f11082a-215d-4b11-941a-28f6acdc1b75";

        let old_db = agy_home
            .join("conversations")
            .join(format!("{old_conv}.db"));
        let old_pbtxt = agy_home
            .join("annotations")
            .join(format!("{old_conv}.pbtxt"));
        let old_log = agy_home
            .join("brain")
            .join(old_conv)
            .join(".system_generated/logs/transcript.jsonl");
        std::fs::create_dir_all(old_db.parent().unwrap())?;
        std::fs::create_dir_all(old_pbtxt.parent().unwrap())?;
        std::fs::create_dir_all(old_log.parent().unwrap())?;
        std::fs::write(&old_db, b"sqlite")?;
        std::fs::write(&old_pbtxt, b"title: \"Old Session Title\"\n")?;
        std::fs::write(
            &old_log,
            "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nOld session prompt\\n</USER_REQUEST>\"}\n",
        )?;

        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let id = SessionId(416);
        let (master, _terminal) = UnixStream::pair()?;
        let session = Session::start(
            id,
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&reg.look),
            None,
        )?;
        let child1 = spawn_holding(&[&old_db, &old_pbtxt, &old_log])?;
        let pid1 = child1.0.as_ref().unwrap().id();
        {
            let mut st = lock(&reg.state);
            st.roots.insert(id, pid1).unwrap();
            st.sessions.insert(
                id,
                Entry {
                    session,
                    pid: pid1,
                    cwd: dir.clone(),
                    wrapped: false,
                },
            );
        }
        reg.agents.remember_session(
            id,
            &control::AgentLaunch {
                profile: "agy".into(),
                permission: krill_proto::agent::Permission::Yolo,
            },
            None,
            dir.as_os_str().as_encoded_bytes().to_vec(),
            Some(crate::agent::control::LaunchAccount {
                id: String::new(),
                env_key: "ANTIGRAVITY_CLI_HOME".into(),
                dir: agy_home.display().to_string(),
            }),
        )?;

        // Phase 1: startup reconciliation opens old conversation files without a presence lock.
        // Neither status polling nor chat polling may bind this new session to `old_conv`.
        let status = reg.mobile_session_status(id).expect("session status");
        let checked_at = Instant::now();
        assert_eq!(status.state, control::AgentState::Unknown);
        assert_eq!(status.title, None);

        let poll = reg.chat_poll(id, 0);
        assert!(!poll.bound);
        assert!(poll.session_id.is_empty());
        assert!(poll.items.is_empty());

        let stored = reg.agents.stored(id)?.expect("stored launch");
        assert_eq!(stored.session_id, None);
        assert_eq!(stored.transcript_path, None);

        // Phase 2: once the process holds `presence/<new_conv>.lock`, both status and chat bind it.
        let new_lock = agy_home.join("presence").join(format!("{new_conv}.lock"));
        let new_pbtxt = agy_home
            .join("annotations")
            .join(format!("{new_conv}.pbtxt"));
        let new_log = agy_home
            .join("brain")
            .join(new_conv)
            .join(".system_generated/logs/transcript.jsonl");
        std::fs::create_dir_all(new_lock.parent().unwrap())?;
        std::fs::create_dir_all(new_log.parent().unwrap())?;
        std::fs::write(&new_lock, b"")?;
        std::fs::write(&new_pbtxt, b"title: \"New Session Title\"\n")?;
        std::fs::write(
            &new_log,
            "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nNew user prompt\\n</USER_REQUEST>\"}\n",
        )?;

        drop(child1);
        let child2 = spawn_holding(&[&new_lock, &old_db, &old_pbtxt])?;
        let pid2 = child2.0.as_ref().unwrap().id();
        {
            let mut st = lock(&reg.state);
            st.roots.insert(id, pid2).unwrap();
            st.sessions.get_mut(&id).unwrap().pid = pid2;
        }

        // Wait past resolve_live_agy_title's 1s negative cooldown from Phase 1.
        if let Some(remaining) = Duration::from_millis(1100).checked_sub(checked_at.elapsed()) {
            std::thread::sleep(remaining);
        }

        let bound_poll = reg.chat_poll(id, 0);
        assert!(bound_poll.bound);
        assert_eq!(bound_poll.session_id, new_conv);
        assert!(!bound_poll.items.is_empty());

        let bound_status = reg.mobile_session_status(id).expect("bound status");
        assert_eq!(bound_status.state, control::AgentState::Done);
        assert_eq!(bound_status.title.as_deref(), Some("New user prompt"));

        let stored = reg.agents.stored(id)?.expect("stored launch");
        assert_eq!(stored.session_id.as_deref(), Some(new_conv));
        let expected_log = std::fs::canonicalize(&new_log)?;
        let stored_log =
            std::fs::canonicalize(stored.transcript_path.as_deref().expect("transcript path"))?;
        assert_eq!(stored_log, expected_log);

        drop(child2);
        drop(reg);
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn observer_cannot_call_check_messages_and_acked_delivery_returns_immediately() -> io::Result<()>
    {
        use crate::agent::control::cache::stamp;
        use krill_proto::orchestration::{ORCH_FILE, OrchResponse};
        use std::io::Write as _;

        // DC-18: CheckMessages / CheckMessagesLegacy persist deliveries so they are not read_only.
        let check_v2 = OrchAction::CheckMessages {
            recipient: Some("worker-1".into()),
            types: vec![],
            unread_only: true,
            ack: false,
            wait_ms: 0,
            run_id: None,
            delivery_id: None,
        };
        let check_legacy = OrchAction::CheckMessagesLegacy {
            recipient: Some("worker-1".into()),
            types: vec![],
            unread_only: true,
            ack: false,
            wait_ms: 0,
        };
        let check_filtered = OrchAction::CheckMessagesFiltered {
            recipient: Some("worker-1".into()),
            run_id: None,
            types: vec![],
            unread_only: true,
            ack: false,
            wait_ms: 0,
        };
        assert!(!read_only(&Request::Orchestration(check_v2.clone())));
        assert!(!read_only(&Request::Orchestration(check_legacy)));
        assert!(read_only(&Request::Orchestration(check_filtered)));

        let dir = std::env::temp_dir().join(format!("krill-dc18-dc19-orch-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let run = OrchestrationStore::mutate(&dir, |s| s.create_run("dc18-19", None))?;
        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        reg.orchestration(
            &peer,
            OrchAction::SendMessage(SendParams {
                from: "coordinator".into(),
                to: "worker-1".into(),
                msg_type: MessageType::Status,
                summary: "hi".into(),
                body: String::new(),
                run_id: Some(run.id.clone()),
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            }),
        )
        .unwrap();
        let before = stamp(&dir.join(ORCH_FILE));

        // Wire Role::Observer is denied CheckMessages { ack: false } without mutating orchestration.json.
        let (mut client, server) = UnixStream::pair()?;
        client.set_read_timeout(Some(Duration::from_secs(2)))?;
        let reg_srv = Arc::clone(&reg);
        let srv = std::thread::spawn(move || {
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            };
            reg_srv.serve(server, &hello).unwrap();
        });
        ipc::read_frame(&mut client)?.unwrap();
        let hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "observer-test");
        client.write_all(&hello.encode().unwrap())?;
        client.write_all(&roles::Request::Set(Role::Observer).encode().unwrap())?;
        let (k, body) = ipc::read_frame(&mut client)?.unwrap();
        assert_eq!(Ack::decode(k, &body).unwrap(), Ack::Applied);
        client.write_all(&Request::Orchestration(check_v2.clone()).encode().unwrap())?;
        let (k, body) = ipc::read_frame(&mut client)?.unwrap();
        assert_eq!(
            Reply::decode(k, &body).unwrap(),
            Reply::Failed(Failure::Denied)
        );
        drop(client);
        srv.join().unwrap();
        assert_eq!(stamp(&dir.join(ORCH_FILE)), before);
        assert!(OrchestrationStore::load(&dir)?.deliveries.is_empty());

        // DC-19: mutating CheckMessages bumps orch_signal, and an already-acked delivery with
        // wait_ms > 0 returns immediately instead of spinning until deadline.
        let gen_before = *lock(&reg.orch_signal.0);
        let Reply::Orchestration(OrchResponse::Delivery {
            delivery: Some(batch),
            messages,
        }) = reg.orchestration(&peer, check_v2).unwrap()
        else {
            panic!("expected delivery batch");
        };
        assert_eq!(messages.len(), 1);
        assert!(*lock(&reg.orch_signal.0) > gen_before);
        // Ack the batch once, then re-ack with wait_ms = 5000.
        let ack_action = OrchAction::CheckMessages {
            recipient: Some("worker-1".into()),
            types: vec![],
            unread_only: true,
            ack: true,
            wait_ms: 5_000,
            run_id: Some(run.id.clone()),
            delivery_id: Some(batch.id.clone()),
        };
        reg.orchestration(&peer, ack_action.clone()).unwrap();
        // Force the acked batch to have empty messages (as when messages are pruned or already acked)
        // by clearing its message_ids so check_delivery returns Ok((Some(delivery), [])).
        OrchestrationStore::mutate(&dir, |s| {
            for d in &mut s.deliveries {
                if d.id == batch.id {
                    d.message_ids.clear();
                }
            }
            Ok(())
        })?;
        let started = Instant::now();
        let reply = reg.orchestration(&peer, ack_action).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "delivery_id check must return immediately instead of waiting for wait_ms, took {:?}",
            started.elapsed()
        );
        assert!(matches!(
            reply,
            Reply::Orchestration(OrchResponse::Delivery {
                delivery: Some(_),
                messages
            }) if messages.is_empty()
        ));
        drop(reg);
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn deliver_input_brackets_multiline_esc_and_sanitizes_orchestration_mail_controls()
    -> io::Result<()> {
        use std::io::Write as _;

        // DC-22a: unit assertions on is_terminal_control_seq
        assert!(is_terminal_control_seq(b"\x03"));
        assert!(is_terminal_control_seq(b"\x1b"));
        assert!(is_terminal_control_seq(b"\x1b[A"));
        assert!(is_terminal_control_seq(b"\x1b[1;5C"));
        assert!(is_terminal_control_seq(b"\x1bOP"));
        assert!(!is_terminal_control_seq(b""));
        assert!(!is_terminal_control_seq(b"\x1b[200~"));
        assert!(!is_terminal_control_seq(b"\x1b[201~"));
        assert!(!is_terminal_control_seq(b"line1\n\x1b[31mred\x1b[0m"));
        assert!(!is_terminal_control_seq(b"hello \x1b[201~ world"));

        // Bracketed paste wraps multiline payload containing \x1b[201~ and strips the escape.
        let look = Arc::new(Appearance::load(PathBuf::new()));
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_secs(2)))?;
        let session = Session::start(
            SessionId(419),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            look,
            None,
        )?;
        let tap = session.tap();
        terminal.write_all(b"\x1b[?2004h")?;
        tap.wait_timeout(Duration::from_secs(1), |p| p.end >= 8);
        let tui = std::thread::spawn(move || -> io::Result<(Vec<u8>, Vec<u8>)> {
            let mut buf = [0_u8; 512];
            let n1 = terminal.read(&mut buf)?;
            let first = buf[..n1].to_vec();
            terminal.write_all(b"> pasted")?;
            let n2 = terminal.read(&mut buf)?;
            Ok((first, buf[..n2].to_vec()))
        });
        Registry::deliver_input_to_tap(&tap, b"line1\n\x1b[201~injected\nline2", true)?;
        let (first, second) = tui.join().unwrap()?;
        assert_eq!(first, b"\x1b[200~line1\ninjected\nline2\x1b[201~");
        assert_eq!(second, b"\r");
        drop(session);

        // DC-22b: SendMessage rejects control chars in from/to/run_id, and
        // notify_idle_orchestration_mail_for strips control chars from on-disk records.
        let dir =
            std::env::temp_dir().join(format!("krill-dc22-mail-controls-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let bad = reg
            .orchestration(
                &peer,
                OrchAction::SendMessage(SendParams {
                    from: "evil\x03\nworker".into(),
                    to: "coordinator".into(),
                    msg_type: MessageType::Status,
                    summary: "hi".into(),
                    body: String::new(),
                    run_id: None,
                    task_id: None,
                    dispatch_id: None,
                    outcome: None,
                    reply_to: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            bad,
            Reply::Orchestration(krill_proto::orchestration::OrchResponse::Error(_))
        ));

        // Legacy on-disk message with control chars in sender/run_id must be stripped before PTY write.
        let id = SessionId(420);
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_secs(2)))?;
        let session = Session::start(
            id,
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&reg.look),
            None,
        )?;
        let root_pid = std::process::id();
        {
            let mut st = lock(&reg.state);
            st.roots.insert(id, root_pid).unwrap();
            st.sessions.insert(
                id,
                Entry {
                    session,
                    pid: root_pid,
                    cwd: PathBuf::from("/tmp"),
                    wrapped: false,
                },
            );
        }
        let done_ev = HookEvent {
            id: hook::EventId {
                pid: root_pid,
                unix_ns: status::now_ns(),
            },
            agent: "claude".into(),
            ppid: root_pid,
            input: hook::Input::Complete,
            payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
        };
        reg.status.hook(&[(id, &done_ev)])?;
        OrchestrationStore::mutate(&dir, |s| {
            let run = s.create_run("dc22", Some(id.0))?;
            s.send_message(&SendParams {
                from: "w\x03\x1b\n1".into(),
                to: "coordinator".into(),
                msg_type: MessageType::Status,
                summary: "hi".into(),
                body: String::new(),
                run_id: Some(run.id),
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })?;
            Ok(())
        })?;
        let reader = std::thread::spawn(move || -> io::Result<Vec<u8>> {
            let mut buf = [0_u8; 512];
            let n = terminal.read(&mut buf)?;
            Ok(buf[..n].to_vec())
        });
        reg.notify_idle_orchestration_mail_for(&dir, Some(id))?;
        let sent = reader.join().unwrap()?;
        assert!(
            !sent.contains(&0x03) && !sent.contains(&0x1b) && !sent.contains(&b'\n'),
            "control characters leaked into PTY mail pointer: {sent:?}"
        );
        assert!(
            String::from_utf8_lossy(&sent).contains("'w1'"),
            "expected sanitized sender in pointer: {}",
            String::from_utf8_lossy(&sent)
        );
        drop(reg);
        std::fs::remove_dir_all(dir)
    }
}

#[cfg(test)]
mod spawn_fallback_tests {
    use super::*;

    #[test]
    fn fallback_spawn_on_enoent_uses_userprofile_and_recovers_missing_shell() {
        use krill_proto::os::ffi::OsStrExt as _;
        let home = std::env::temp_dir().join(format!("krilld-userprofile-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        std::fs::create_dir_all(&home).unwrap();
        let mut argv = vec![b"/nonexistent/custom-shell".to_vec(), b"-l".to_vec()];
        let mut cwd = b"nonexistent-relative-root/deleted-child".to_vec();
        let mut env = vec![
            (
                b"UserProfile".to_vec(),
                home.as_os_str().as_bytes().to_vec(),
            ),
            (b"SHELL".to_vec(), b"/nonexistent/custom-shell".to_vec()),
        ];
        assert!(fallback_spawn_on_enoent(&mut argv, &mut cwd, &mut env));
        assert_eq!(cwd, home.as_os_str().as_bytes());
        assert_ne!(argv[0], b"/nonexistent/custom-shell");
        std::fs::remove_dir_all(home).ok();
    }
}

#[cfg(all(test, windows))]
mod windows_spawn_tests {
    /// 지워진 cwd로 `CreateProcessW`가 내는 실제 코드가 폴백 대상이어야 한다(holder는 이 코드를 그대로 보낸다).
    #[test]
    fn deleted_cwd_spawn_errors_fall_back() {
        let root = std::env::temp_dir().join(format!("krilld-gone-cwd-{}", std::process::id()));
        for cwd in [root.clone(), root.join("a").join("b")] {
            let error = std::process::Command::new("cmd.exe")
                .args(["/c", "exit"])
                .current_dir(&cwd)
                .spawn()
                .unwrap_err();
            let code = error.raw_os_error().unwrap();
            assert!(super::missing_path_errno(code), "{cwd:?}: {error} ({code})");
        }
        assert!(!super::missing_path_errno(5));
    }
}

#[cfg(test)]
mod spawn_observability_tests {
    use super::*;

    #[test]
    fn session_start_failed_detail_is_valid_non_pii_and_records_hresult() {
        use krill_proto::error_reporting::{Code, Event, Process};

        for (phase, errno, wrapped, expected_kind, expected_hex) in [
            ("holder_spawn", libc::ENOENT, false, "NotFound", None),
            (
                "holder_spawn",
                0x8007_0002_u32.cast_signed(),
                true,
                if cfg!(windows) { "NotFound" } else { "Other" },
                Some("hex=0x80070002"),
            ),
            (
                "holder_spawn",
                krill_proto::holder::SESSION_LIMIT_ERRNO,
                false,
                "SessionLimit",
                None,
            ),
        ] {
            let kind = session_start_failure_kind(errno);
            if errno == krill_proto::holder::SESSION_LIMIT_ERRNO || errno == libc::ENOENT {
                assert_eq!(kind, expected_kind);
            }
            let detail = format_session_start_detail(phase, kind, Some(errno), wrapped);
            if let Some(hex) = expected_hex {
                assert!(detail.contains(hex), "{detail}");
            }
            let mut event = Event::new(
                Process::Daemon,
                Code::SessionStartFailed,
                krill_proto::error_reporting::source_location(file!(), line!()),
            )
            .unwrap();
            event.detail = Some(detail);
            event.validate().unwrap();
        }
    }

    #[test]
    fn missing_path_errno_recognizes_win32_hresult_wrapper() {
        assert!(missing_path_errno(libc::ENOENT));
        if cfg!(windows) {
            assert!(missing_path_errno(0x8007_0002_u32.cast_signed())); // ERROR_FILE_NOT_FOUND
            assert!(missing_path_errno(0x8007_0003_u32.cast_signed())); // ERROR_PATH_NOT_FOUND
            assert!(missing_path_errno(0x8007_010b_u32.cast_signed())); // ERROR_DIRECTORY
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_fallback_updates_shell_env_when_replacing_missing_login_shell() {
        let mut argv = vec![b"/nonexistent/custom-shell".to_vec(), b"-l".to_vec()];
        let mut cwd = Vec::new();
        let mut env = vec![(b"SHELL".to_vec(), b"/nonexistent/custom-shell".to_vec())];
        assert!(fallback_spawn_on_enoent(&mut argv, &mut cwd, &mut env));
        assert_ne!(argv[0], b"/nonexistent/custom-shell");
        let shell_env = env
            .iter()
            .find(|(k, _)| k.as_slice() == b"SHELL")
            .map(|(_, v)| v.as_slice());
        assert_eq!(shell_env, Some(argv[0].as_slice()));
    }

    #[cfg(unix)]
    #[test]
    fn mobile_dashboard_inputs_uses_cached_agy_state_without_blocking() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("krill-agy-nonblock-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir)?;
        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let id = SessionId(912);
        let (master, _terminal) = UnixStream::pair()?;
        let session = Session::start(
            id,
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&reg.look),
            None,
        )?;
        {
            let mut st = lock(&reg.state);
            st.roots.insert(id, std::process::id()).ok();
            st.sessions.insert(
                id,
                Entry {
                    session,
                    pid: std::process::id(),
                    cwd: dir.clone(),
                    wrapped: false,
                },
            );
        }
        reg.agents.remember_session(
            id,
            &control::AgentLaunch {
                profile: "agy".into(),
                permission: krill_proto::agent::Permission::Yolo,
            },
            None,
            dir.as_os_str().as_encoded_bytes().to_vec(),
            None,
        )?;
        let inputs = reg.mobile_dashboard_inputs()?;
        assert_eq!(inputs.sessions.len(), 1);
        assert_eq!(inputs.sessions[0].status.agent, "agy");
        drop(reg);
        std::fs::remove_dir_all(dir)
    }

    #[cfg(unix)]
    #[test]
    fn resolve_bot_cli_executable_enforces_unix_exec_bit_and_searches_profile_env_path() {
        use std::os::unix::fs::PermissionsExt as _;

        let root =
            std::env::temp_dir().join(format!("krill-bot-cli-resolve-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let bin_dir = root.join("custom-bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let non_exec = bin_dir.join("not-exec");
        std::fs::write(&non_exec, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            resolve_bot_cli_executable(non_exec.to_str().unwrap(), &BTreeMap::new()),
            None,
            "path-qualified non-executable file must be rejected on Unix"
        );

        let exec = bin_dir.join("my-custom-agent");
        std::fs::write(&exec, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_bot_cli_executable(exec.to_str().unwrap(), &BTreeMap::new()),
            Some(exec.clone())
        );

        let mut env = BTreeMap::new();
        env.insert("PATH".to_owned(), bin_dir.display().to_string());
        assert_eq!(
            resolve_bot_cli_executable("my-custom-agent", &env),
            Some(exec)
        );
        assert_eq!(
            resolve_bot_cli_executable("my-custom-agent", &BTreeMap::new()),
            None
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_agent_profile_with_overrides_decodes_query_id_for_resume_and_create() {
        let dir =
            std::env::temp_dir().join(format!("krill-resolve-overrides-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let reg = Registry::new(None, Some(&dir));
        let encoded = krill_proto::agent::encode_agent_program_id(
            "claude",
            &krill_proto::agent::LaunchOverrides {
                model: Some("claude-opus-4-6".to_owned()),
                effort: Some("high".to_owned()),
            },
        )
        .unwrap();
        let (canonical, profile, _perm) =
            reg.resolve_agent_profile_with_overrides(&encoded).unwrap();
        assert_eq!(canonical, "claude");
        assert!(
            profile
                .launch
                .windows(2)
                .any(|w| w == ["--model", "claude-opus-4-6"]),
            "launch args missing decoded --model override: {:?}",
            profile.launch
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
