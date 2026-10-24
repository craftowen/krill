//! 세션 레지스트리(OWE-486): krilld가 들고 있는 세션과 holder 링크. 연결 하나를 끝까지 처리하는 [`Registry::serve`]가
//! 첫 요청으로 화면 스트림([`crate::attach`], MK-023)과 제어 요청([`krill_proto::control`])을 가른다.
//!
//! - 생성: holder `Spawn` → 받은 master fd로 [`Session`]을 띄우고 세그먼트 기록을 붙인다(MK-024). 프로그램은 로그인
//!   셸·지정 실행 파일(MK-034) 또는 에이전트 프로필(MK-037).
//! - 닫기: 돌고 있으면 holder v3 `Terminate`(v2 이하는 프로세스 그룹에 SIGHUP `Kill`), `Exited`가 오면 `Release`하고
//!   지운다. 이미 끝났으면 바로 `Release`.
//! - 권한: 연결마다 [`Peer`]로 세션 범위를 판정한다(MK-036). 목록은 다룰 수 있는 세션만, 닫기·붙기는 범위 밖이면 거부하고
//!   감사 로그에 남긴다. 생성은 기존 세션을 다루지 않으므로 막지 않는다.
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
mod hibernation;
mod orch_cache;
mod schedule;
mod sidebar;
pub mod usage_tick;

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
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<SessionId, Entry>,
    /// 끝난 세션의 상태. `Spawned` 답보다 `Exited`가 먼저 처리될 수 있어 세션과 따로 둔다.
    exited: HashMap<SessionId, i32>,
    /// 닫기를 요청받은 세션. `Exited`가 오면 바로 지운다.
    closing: HashSet<SessionId>,
    roots: Roots,
    /// 다음 세션 id 후보.
    next: u64,
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
    sidebar: Arc<Mutex<sidebar::Cache>>,
    hibernation: Mutex<hibernation::Clock>,
    schedule_minute: Mutex<Option<u64>>,
    git_watch: Mutex<crate::git::watch::Hub>,
    /// 업그레이드를 시작했다(MK-062). 잠금이 동시 요청을 줄 세운다.
    upgrading: Mutex<bool>,
    /// 진행 중인 `Resume`의 `old` id. 같은 세션의 동시 재개는 먼저 잡은 쪽만 진행한다(K-25).
    resuming: Mutex<HashSet<SessionId>>,
    account_work: Arc<crate::usage::accounts::switch_work::SwitchWork>,
    account_handoffs: Mutex<BTreeMap<SessionId, krill_proto::usage::SessionHandoff>>,
    account_recovered: std::sync::atomic::AtomicBool,
    account_replacements: Mutex<std::collections::BTreeSet<SessionId>>,
    /// main에 프로세스 종료를 알린다(업그레이드 답을 보낸 뒤).
    exit: OnceLock<Sender<()>>,
    /// 디스크 사용량을 잴 상태 디렉터리(MK-071).
    dir: Option<PathBuf>,
    last_gc: Mutex<Instant>,
    app_opened: OnceLock<()>,
    mobile: OnceLock<Arc<crate::mobile::gateway::Gateway>>,
    webhooks: Mutex<crate::webhook::Dispatcher>,
    orch_signal: (Mutex<u64>, std::sync::Condvar),
    orch_cache: Mutex<orch_cache::Cache>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("holder", &self.holder)
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// `dir`(상태 디렉터리)이 있으면 세그먼트 저장소·테마·감사 로그·에이전트 프로필을 그 아래에 둔다. `holder`가
    /// 다시 넘긴 세션은 바로 다시 띄운다. 기록을 열지 못해도 세션은 띄운다(그 세션만 디스크 기록이 없다).
    pub fn new(holder: Option<(Holder, Vec<Kept>)>, dir: Option<&Path>) -> Self {
        let started = Instant::now();
        if !cfg!(test)
            && let Err(error) = crate::usage::accounts::codex_home::migrate(
                &crate::usage::accounts::default_home(),
                dir,
            )
        {
            tracing::warn!(kind = ?error.kind(), "Codex account home migration failed");
        }
        let at = |name: &str| dir.map_or_else(PathBuf::new, |d| d.join(name));
        // 기록 저장소를 열지 못해도 세션은 띄운다. 조용히 넘기지 않고 경고를 남겨 `krill doctor`가 보인다.
        let store = dir.and_then(|d| match Store::open(d, Limits::default()) {
            Ok(store) => {
                clear_record_warnings(d);
                Some(store)
            }
            Err(e) => {
                tracing::warn!("segment store unavailable in {}: {e}", d.display());
                note_record_warning(d, &format!("store open failed: {e}"));
                None
            }
        });
        let (holder, kept) = holder.map_or((None, Vec::new()), |(h, k)| (Some(Arc::new(h)), k));
        let kept_count = kept.len();
        let reg = Self {
            app_opened: OnceLock::new(),
            last_gc: Mutex::new(Instant::now()),
            sidebar: Arc::default(),
            hibernation: Mutex::default(),
            schedule_minute: Mutex::default(),
            git_watch: Mutex::default(),
            upgrading: Mutex::new(false),
            resuming: Mutex::default(),
            account_work: crate::usage::accounts::switch_work::subscribe(
                &crate::usage::accounts::default_home(),
                dir,
            ),
            account_handoffs: Mutex::new(account_switch::load(dir)),
            account_recovered: std::sync::atomic::AtomicBool::new(false),
            account_replacements: Mutex::default(),
            exit: OnceLock::new(),
            dir: dir.map(Path::to_path_buf),
            mobile: OnceLock::new(),
            webhooks: Mutex::new(crate::webhook::Dispatcher::new(dir)),
            orch_signal: (Mutex::new(0), std::sync::Condvar::new()),
            orch_cache: Mutex::default(),
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
            state: Mutex::new(State {
                next: 1,
                roots: Roots::open(dir.map(|d| d.join("session-creators"))),
                ..State::default()
            }),
        };
        let mut st = lock(&reg.state);
        for k in kept {
            // 재기동 공백의 provider roster는 알 수 없다. 이 프로세스는 자동 종료에서 제외한다.
            reg.agents.block_hibernation(k.id);
            if k.status.is_none() {
                // 종료 요청 전후에 krilld가 죽었으면 살아 있는 프로세스를 우선 보존한다.
                reg.clear_sleeping(k.id);
            }
            // 보관 세션의 PTY 크기를 그대로 쓰면 세션 안 `stty`로 키운 거대 winsize가
            // 재기동 때 Term에 들어가 수백 MB를 할당한다(K-01). 엔진·PTY 모두 자른다.
            #[cfg(unix)]
            let size = crate::term::clamp_size(
                crate::session::pty::size(&k.master).unwrap_or(FALLBACK_SIZE),
            );
            #[cfg(windows)]
            let size = FALLBACK_SIZE;
            st.next = st.next.max(k.id.0.saturating_add(1));
            let Ok(session) = reg.start(k.id, k.master, size) else {
                // master는 holder가 계속 쥔다. 다음 krilld 기동 때 다시 온다.
                // 조용히 넘기면 그 krilld 수명 동안 세션이 목록에서 사라진다(K-26).
                tracing::warn!(session = k.id.0, "kept session failed to start");
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
                    cwd: PathBuf::new(),
                },
            );
        }
        let live: Vec<_> = st.sessions.keys().copied().collect();
        st.roots.prune_dead(&live);
        if let Some(store) = &reg.store {
            store.gc(&live, SystemTime::now()).ok();
        }
        if let Some(d) = dir {
            crate::agent::claude::sweep(d, &live);
        }
        drop(st);
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
        {
            let mut last = lock(&self.last_gc);
            if now.saturating_duration_since(*last) < GC_INTERVAL {
                return;
            }
            *last = now;
        }
        if let Some(store) = &self.store {
            let live: Vec<_> = lock(&self.state).sessions.keys().copied().collect();
            if let Err(e) = store.gc(&live, wall) {
                tracing::warn!(error = %e, "history gc failed");
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
                self.apply_agent_hook(id, e, Some(&cwd), Some(&tap))?;
                self.auto_switch_from_hook(id, e);
                hooked.push((id, e.payload.clone()));
            }
            Ok(())
        })
        .ok();
        drop(commit);
        for (id, payload) in &hooked {
            self.maybe_rename_from_hook(*id, payload);
        }
    }

    /// holder 리더 스레드 본체. 링크가 끊길 때까지 돌아오지 않는다.
    pub fn run_holder(&self, link: Link) {
        if let Some(h) = &self.holder {
            h.run(link, |id, status| self.on_exited(id, status));
        }
    }

    /// 감시 스냅샷 뒤 세션이 지워졌으면 그 상태를 다시 만들지 않는다.
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

    /// 프로세스·제목·화면 보조 감시. PTY 읽기와 별도 스레드에서 호출한다.
    pub(crate) fn observe_webhook(&self, id: SessionId) {
        let state = self.status.get(id).state;
        let link = self
            .mobile_gateway()
            .and_then(|gateway| gateway.deep_link(id));
        lock(&self.webhooks).observe(id, state, link);
    }

    pub(crate) fn retain_webhooks(&self, ids: &[SessionId]) {
        lock(&self.webhooks).retain(ids);
    }

    pub fn monitor_status(&self) {
        status::monitor(self);
    }

    /// 현재 화면 규칙. 서명 규칙이 유효하면 그것, 아니면 내장(MK-091).
    pub(crate) fn screen_rules(&self) -> krill_proto::agent::rules::ScreenRules {
        self.agents.screen_rules()
    }

    pub fn holder(&self) -> Option<&Holder> {
        self.holder.as_deref()
    }

    /// 업그레이드 답을 보낸 뒤 `done`으로 main에 종료를 알린다(MK-062). 한 번만 설정된다.
    pub fn exit_with(&self, done: Sender<()>) {
        self.exit.set(done).ok();
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

    pub fn start_mobile(self: &Arc<Self>, hello: ServerHello) -> io::Result<()> {
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

    fn serve_as(
        &self,
        mut s: UnixStream,
        hello: &ServerHello,
        mobile: bool,
        admission: Option<admission::Permit>,
    ) -> io::Result<()> {
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
        // 서명 정책이 강제되면 전체 피어의 코드서명을 핸드셰이크 전에 검증한다(MK-096). 닫힌 피어의 토큰
        // 조회도 실패하므로 먼저 읽은 토큰으로 검증하고, 못 넘기면 접속 자체를 거부한다.
        if !mobile && peer.scope == Scope::Full {
            let policy = scope::SigningPolicy::load(self.dir.as_deref());
            if policy.enforced() {
                let sign = crate::sys::peer_token(s.as_fd())
                    .ok()
                    .map(|token| crate::sys::verify_audit_token(&token));
                if peer.gate_signing(&policy, sign.as_ref(), &self.audit) {
                    return Ok(());
                }
            }
        }
        let (client, n) = crate::ipc::handshake(&mut s, hello)?;
        if peer.scope == Scope::Full
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
                | krill_proto::mobile::FEAT_MOBILE)
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
                let allowed = peer.scope == Scope::Full
                    && (role != Role::Observer
                        || matches!(
                            request,
                            MobileRequest::Status
                                | MobileRequest::ListDevices
                                | MobileRequest::ConnectedDevices
                        ));
                let reply = if !allowed {
                    MobileReply::Failed("permission denied".into())
                } else if let Some(gateway) = self.mobile.get() {
                    gateway.request(request)
                } else {
                    MobileReply::Failed("mobile gateway unavailable".into())
                };
                s.write_all(&reply.encode()?)?;
                frame = ipc::read_frame(&mut s)?;
            }
            return Ok(());
        }
        if k == krill_proto::theme::THEME && n.features & krill_proto::theme::FEAT_THEME != 0 {
            if peer.scope != Scope::Full
                || role == Role::Observer
                || client.role != ipc::ClientRole::App
            {
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
                self.maybe_rename_from_hook(id, &e.payload);
            }
            return ack;
        }
        if k == krill_proto::history::kind::HISTORY_REQ
            && n.features & krill_proto::history::FEAT_HISTORY != 0
        {
            use krill_proto::history::{HistoryReply, HistoryRequest};
            let request = HistoryRequest::decode(k, &body)?;
            let reply = if peer.scope == Scope::Full && role != Role::Observer {
                match &self.store {
                    Some(store) => match request {
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
        if k != control::kind::REQUEST || n.features & FEAT_CONTROL == 0 {
            return Ok(());
        }
        s.set_read_timeout(None).ok();
        let mut body = body;
        let mut clients = HashMap::new();
        let mut control_slot = None;
        loop {
            let decoded = Request::decode(control::kind::REQUEST, &body);
            if !mobile {
                if decoded.as_ref().is_ok_and(admission::protected) {
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
                Ok(req) if role == Role::Observer && !read_only(&req) => {
                    Reply::Failed(Failure::Denied)
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
            let sent = s.write_all(&reply.encode()?);
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
                if peer.scope == Scope::Full {
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
                    if !peer.authorize("agent-launch", session, &st.roots, &self.audit) {
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
                    .filter(|id| peer.scope.allows(**id, &st.roots))
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
                    if peer.scope == Scope::Full {
                        sidebar::include_projects(&mut data);
                    }
                    Reply::Sidebar(data)
                })
            }
            Request::PrepareWorktree(w) => {
                if self.authorize_cwd(peer, "worktree-prepare", &w.repo) {
                    Ok(crate::git::worktree::prepare::serve(&w))
                } else {
                    Err(Failure::Denied)
                }
            }
            Request::NewWorktree(w) => {
                if self.authorize_cwd(peer, "worktree-new", &w.repo) {
                    let size = self.request_size(peer);
                    Ok(crate::git::worktree::serve(&w, size, |c| {
                        self.create(peer, c)
                    }))
                } else {
                    Err(Failure::Denied)
                }
            }
            Request::RemoveWorktree(w) => {
                if self.authorize_cwd(peer, "worktree-rm", &w.path) {
                    Ok(crate::git::worktree::serve_remove(&w))
                } else {
                    Err(Failure::Denied)
                }
            }
            Request::Upgrade(u) => match (&self.holder, peer.scope) {
                (Some(holder), Scope::Full) => {
                    let mut started = lock(&self.upgrading);
                    if *started {
                        Ok(Reply::Upgrade(control::Upgraded::Started))
                    } else {
                        let r = crate::upgrade::prepare(self, holder, u);
                        *started = r == Ok(control::Upgraded::Started);
                        r.map(Reply::Upgrade)
                    }
                }
                (None, Scope::Full) => Err(Failure::NoHolder),
                _ => {
                    peer.deny("upgrade", &self.audit);
                    Err(Failure::Denied)
                }
            },
            Request::AgentHistory(action) => self.agent_history(peer, action),
            Request::Resources(action) => self.resources(peer, action),
            Request::GitChanges { repo } => {
                if peer.scope == Scope::Full {
                    Ok(crate::git::changes::serve(&repo))
                } else {
                    peer.deny("git-changes", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::GitWatch(req) => {
                if peer.scope == Scope::Full {
                    Ok(Reply::GitWatch(
                        lock(&self.git_watch).handle_for(client_name, &req),
                    ))
                } else {
                    peer.deny("git-watch", &self.audit);
                    Err(Failure::Denied)
                }
            }
            Request::GitEdit { repo, edit } => {
                if peer.scope == Scope::Full {
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
        };
        r.unwrap_or_else(Reply::Failed)
    }

    fn resources(&self, peer: &Peer, action: Action) -> Result<Reply, Failure> {
        if action == Action::RestartDaemon {
            if peer.scope != Scope::Full {
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
            .filter(|(id, _)| peer.scope.allows(**id, &st.roots))
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

    /// `Full`이거나 세션 피어의 현재 디렉터리가 `target`과 같을 때만 허용한다. 거부는 감사 로그에 남긴다.
    fn authorize_cwd(&self, peer: &Peer, op: &'static str, target: &[u8]) -> bool {
        let allowed = match peer.scope {
            Scope::Full => true,
            Scope::Session(id) => self.same_cwd(id, target),
            Scope::Restricted => false,
        };
        if !allowed {
            peer.deny(op, &self.audit);
        }
        allowed
    }

    fn same_cwd(&self, id: SessionId, target: &[u8]) -> bool {
        use krill_proto::os::ffi::OsStrExt as _;
        if target.is_empty() {
            return false;
        }
        let Some(cwd) = self.session_cwd(id) else {
            return false;
        };
        let given = Path::new(std::ffi::OsStr::from_bytes(target));
        // `session_cwd`와 같은 규칙(Windows `\\?\` 접두 제거)으로 맞춰야 같은 폴더가 같다고 나온다.
        let given = krill_proto::os::canonicalize(given).unwrap_or_else(|_| given.to_path_buf());
        cwd == given
    }

    /// setup 세션에 쓸 요청 세션의 현재 크기. 세션 범위가 아니거나 크기를 읽지 못하면 `None`(80x24 기본값).
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
        if !peer.authorize("attach", id, &st.roots, &self.audit) {
            return None;
        }
        st.sessions.get(&id).map(|e| e.session.tap())
    }

    /// 세션 `id`의 핫·콜드 스크롤백을 비운다(OWE-873b). 세션·PTY·보이는 화면은 건드리지 않는다.
    fn clear_scrollback(&self, peer: &Peer, id: SessionId) -> Result<Reply, Failure> {
        let tap = {
            let st = lock(&self.state);
            if !peer.authorize("clear-scrollback", id, &st.roots, &self.audit) {
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
        if !peer.authorize("cold", id, &lock(&self.state).roots, &self.audit) {
            return Err(Failure::Denied);
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
                    tracing::warn!("session {} recorder unavailable: {e}", id.0);
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

    fn create(&self, peer: &Peer, c: Create) -> Result<Reply, Failure> {
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
        let session = self.next_id();
        let state = self.state_dir();
        let Launch { argv, env } = match c.program {
            Program::Login => crate::agent::login_in(state.as_deref()),
            Program::Custom { program, args } => Shell::Custom { program, args }.launch(),
            Program::Agent { id } => {
                let (base_id, overrides) = krill_proto::agent::decode_agent_program_id(&id)
                    .map_err(|_| Failure::BadRequest)?;
                let agents = self.agents.agents().map_err(|_| Failure::UnknownAgent)?;
                let (canonical, base_profile) =
                    agents.resolve(base_id).ok_or(Failure::UnknownAgent)?;
                let mut profile = base_profile.clone();
                if !overrides.is_empty() {
                    krill_proto::agent::apply_launch_overrides(canonical, &mut profile, &overrides)
                        .map_err(|_| Failure::BadRequest)?;
                }
                let (launch, session_id, account) = crate::agent::launch_selected_in(
                    &profile,
                    agents.permission,
                    session,
                    state.as_deref(),
                    c.account.as_deref(),
                )
                .map_err(|e| os(&e))?;
                crate::agent::pre_trust_workspace(&profile, &c.cwd, account.as_ref());
                let metadata = control::AgentLaunch {
                    profile: canonical.to_owned(),
                    permission: agents.permission,
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
            self.spawn_holder(peer, holder, session, argv, env, c.cwd, c.cols, c.rows)?;
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
        if !peer.authorize("fork", old, &lock(&self.state).roots, &self.audit) {
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
        let session = self.next_id();
        crate::agent::pre_trust_workspace(profile, &c.cwd, account.as_ref());
        let (launch, session_id, account) = crate::agent::fork_in(
            profile,
            permission,
            session,
            state.as_deref(),
            source_sid.as_deref(),
            account.as_ref(),
        )
        .map_err(|e| os(&e))?;
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
            peer,
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
        if !peer.authorize("resume", old, &lock(&self.state).roots, &self.audit) {
            return Err(Failure::Denied);
        }
        {
            let st = lock(&self.state);
            if st.sessions.contains_key(&old) && !st.exited.contains_key(&old) {
                return Err(Failure::BadRequest);
            }
        }
        // 같은 세션의 동시 재개는 먼저 잡은 쪽만 진행한다. 두 창이 동시에 눌러도
        // `claude --resume`이 두 번 뜨지 않는다(K-25).
        let _claim = self.claim_resume(old).ok_or(Failure::BadRequest)?;
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
        let session = self.next_id();
        let Launch { argv, env } = match stored {
            Some(s) => {
                let agents = self.agents.agents().map_err(|_| Failure::UnknownAgent)?;
                let (canonical, profile) =
                    agents.resolve(&s.profile).ok_or(Failure::UnknownAgent)?;
                let resume_id = s.session_id.or(fallback_sid);
                // 실행 당시 계정으로 이어 간다(OWE-698). 휴면 깨우기도 이 경로다.
                // 요청이 계정을 지정하면 그 계정으로 이어 간다(OWE-699, 다른 계정으로 이어가기).
                let account = crate::agent::resume_target(
                    profile,
                    state.as_deref(),
                    c.account.as_deref(),
                    s.account.as_ref(),
                    resume_id.as_deref(),
                )
                .map_err(|e| os(&e))?;
                crate::agent::pre_trust_workspace(profile, &c.cwd, account.as_ref());
                let (launch, session_id, account) = crate::agent::resume_in(
                    profile,
                    s.permission,
                    session,
                    state.as_deref(),
                    resume_id.as_deref(),
                    account.as_ref(),
                )
                .map_err(|e| os(&e))?;
                let metadata = control::AgentLaunch {
                    profile: canonical.to_owned(),
                    permission: s.permission,
                };
                self.agents
                    .remember_session(session, &metadata, session_id, c.cwd.clone(), account)
                    .map_err(|e| {
                        self.discard_settings(session);
                        os(&e)
                    })?;
                launch
            }
            None => match c.program {
                Program::Login => crate::agent::login_in(state.as_deref()),
                Program::Custom { program, args } => Shell::Custom { program, args }.launch(),
                Program::Agent { id } => {
                    let agents = self.agents.agents().map_err(|_| Failure::UnknownAgent)?;
                    let (canonical, profile) = agents.resolve(&id).ok_or(Failure::UnknownAgent)?;
                    let account = crate::agent::resume_target(
                        profile,
                        state.as_deref(),
                        c.account.as_deref(),
                        None,
                        fallback_sid.as_deref(),
                    )
                    .map_err(|e| os(&e))?;
                    crate::agent::pre_trust_workspace(profile, &c.cwd, account.as_ref());
                    let (launch, session_id, account) = crate::agent::resume_in(
                        profile,
                        agents.permission,
                        session,
                        state.as_deref(),
                        fallback_sid.as_deref(),
                        account.as_ref(),
                    )
                    .map_err(|e| os(&e))?;
                    let metadata = control::AgentLaunch {
                        profile: canonical.to_owned(),
                        permission: agents.permission,
                    };
                    self.agents
                        .remember_session(session, &metadata, session_id, c.cwd.clone(), account)
                        .map_err(|e| {
                            self.discard_settings(session);
                            os(&e)
                        })?;
                    launch
                }
            },
        };
        if let Some(store) = &self.store {
            // holder Spawn 전에 연결을 남겨 데몬이 응답 전에 죽어도 이력을 잇는다.
            store.inherit_history(session, old).map_err(|e| os(&e))?;
        }
        let (session, pid) =
            self.spawn_holder(peer, holder, session, argv, env, c.cwd, c.cols, c.rows)?;
        let (has_old, exited_old) = {
            let st = lock(&self.state);
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
        peer: &Peer,
        holder: &Holder,
        id: SessionId,
        argv: Vec<Vec<u8>>,
        mut env: Vec<(Vec<u8>, Vec<u8>)>,
        cwd: Vec<u8>,
        cols: u16,
        rows: u16,
    ) -> Result<(SessionId, u32), Failure> {
        use krill_proto::os::ffi::OsStrExt as _;
        env.extend(crate::agent::pane_env_in(id, self.state_dir().as_deref()));
        // daemon `Create`의 거대 cols·rows도 여기서 자른다(K-01, CLI만 막는다).
        let size = crate::term::clamp_size(WinSize { cols, rows });
        let initial_cwd = PathBuf::from(std::ffi::OsStr::from_bytes(&cwd));
        env.extend(crate::git::worktree::environment(&initial_cwd));
        let spawn = |req| ToHolder::Spawn {
            req,
            id,
            argv,
            env,
            cwd,
            size,
        };
        // holder가 spawn한 직후 krilld가 죽어도 생성 계보가 남도록 먼저 저장한다.
        lock(&self.state)
            .roots
            .record_creator(id, peer.scope)
            .map_err(|e| os(&e))?;
        // 자식은 Spawned 응답보다 먼저 hook을 보낼 수 있다. 루트·세션을 모두 등록하기 전에는
        // socket hook 적용과 spool 소거를 막아 최초 이벤트가 미귀속 상태로 ACK/폐기되지 않게 한다.
        // holder 응답 리더는 on_exited 워커와 분리돼 있어 이 잠금을 기다리지 않는다.
        let _commit = lock(&self.hook_commit);
        match holder.request(spawn).map_err(|_| Failure::NoHolder)? {
            (FromHolder::Spawned { pid, .. }, Some(master)) => {
                let session = self.start(id, master, size).map_err(|e| {
                    // krilld가 master를 버렸으니 holder 것까지 놓으면 세션은 SIGHUP으로 끝난다.
                    holder.notify(|req| ToHolder::Release { req, id }).ok();
                    self.discard_settings(id);
                    self.agents.forget(id);
                    os(&e)
                })?;
                let mut st = lock(&self.state);
                st.roots.insert(id, pid).ok();
                st.sessions.insert(
                    id,
                    Entry {
                        session,
                        pid,
                        cwd: initial_cwd,
                    },
                );
                Ok((id, pid))
            }
            (FromHolder::Failed { errno, .. }, _) => {
                // spawn 실패: 세션이 없으니 주입 settings만 지우고 만들다 만 계보 기록도 걷는다.
                lock(&self.state).roots.remove(id);
                self.discard_settings(id);
                self.agents.forget(id);
                Err(Failure::from_holder_errno(errno))
            }
            _ => {
                lock(&self.state).roots.remove(id);
                Err(Failure::Os(libc::EIO))
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

    /// 쓰지 않은 id. 닫힌 세션의 디스크 기록(보존 7일)·에이전트 기동 기록과도 겹치지 않는다.
    fn next_id(&self) -> SessionId {
        let mut st = lock(&self.state);
        loop {
            let id = SessionId(st.next);
            st.next = st.next.saturating_add(1);
            let on_disk = self
                .store
                .as_ref()
                .is_some_and(|s| s.session_dir(id).exists())
                || self.agents.has(id);
            if !st.sessions.contains_key(&id) && !on_disk {
                return id;
            }
        }
    }

    fn list(&self, peer: &Peer) -> Reply {
        let st = lock(&self.state);
        Reply::Sessions(
            st.sessions
                .iter()
                .filter(|(id, _)| peer.scope.allows(**id, &st.roots))
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
                .filter(|&id| peer.scope.allows(id, &st.roots))
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
                .filter(|(id, _)| peer.scope.allows(**id, &st.roots))
                .map(|(&session, e)| {
                    let exited = st.exited.contains_key(&session) || e.session.is_closed();
                    (session, e.pid, exited, e.session.tap())
                })
                .collect()
        };
        Reply::States(
            sessions
                .into_iter()
                .map(|(session, root, exited, tap)| {
                    let mut status = self.status.get(session);
                    // Pi has no hook identity. Its executable may be reported as `node`,
                    // and its terminal title can be renamed. Keep the explicit launch identity.
                    if status.agent.is_empty()
                        && let Ok(Some(launch)) = self.agents.launch(session)
                        && launch.profile == "pi"
                    {
                        status.agent = launch.profile;
                    }
                    status.foreground =
                        status::foreground_process(root, tap.foreground(), exited, |pid| {
                            crate::sys::process_name(pid).ok()
                        });
                    status.title = tap.title();
                    let is_agy_fg = status.agent.eq_ignore_ascii_case("agy")
                        || status.agent.eq_ignore_ascii_case("antigravity")
                        || status.foreground.as_deref().is_some_and(|fg| {
                            fg.eq_ignore_ascii_case("agy") || fg.eq_ignore_ascii_case("antigravity")
                        });
                    let stored = if is_agy_fg || (status.title.is_none() && status.agent.is_empty())
                    {
                        self.agents.stored(session).ok().flatten()
                    } else {
                        None
                    };
                    let is_agy_stored = stored.as_ref().is_some_and(|s| {
                        s.profile.eq_ignore_ascii_case("agy")
                            || s.profile.eq_ignore_ascii_case("antigravity")
                    });
                    if is_agy_fg || is_agy_stored {
                        let (title, discovered) =
                            crate::agent_history::vault::resolve_live_agy_title(
                                session,
                                root,
                                tap.foreground(),
                                exited,
                                stored.as_ref(),
                            );
                        if discovered.is_some()
                            && let Ok(Some(found)) = account_switch::identity::discover(
                                krill_proto::usage::ProviderKind::Antigravity,
                                root,
                                tap.foreground(),
                            )
                        {
                            self.agents.bind_discovered_session(
                                session,
                                &found.id,
                                Some(&found.path),
                            );
                        }
                        if title.is_some() || status.title.is_none() {
                            status.title = title;
                        }
                    }
                    SessionStatus { session, status }
                })
                .collect(),
        )
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
            self.apply_agent_hook(id, e, Some(&cwd), Some(&tap))?;
            self.auto_switch_from_hook(id, e);
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    /// OWE-737: 첫 프롬프트 hook(`UserPromptSubmit` 등)이 오면 세션 자동 제목을 기록하고,
    /// 현재 디렉터리가 기본 이름(`worktree-1`, `claude-1` 등)의 git 브랜치이면 슬러그로 개명한다.
    pub(crate) fn maybe_rename_from_hook(&self, id: SessionId, payload: &[u8]) {
        use krill_proto::os::ffi::OsStrExt as _;
        if self.agents.is_subagent_or_foreign_hook(id, payload) {
            return;
        }
        let state_dir = self.state_dir();
        if !krill_proto::git::is_auto_rename_enabled(state_dir.as_deref()) {
            return;
        }
        let Some(prompt) = krill_proto::git::extract_prompt_from_hook_payload(payload) else {
            return;
        };
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
    ) -> io::Result<()> {
        use krill_proto::os::ffi::OsStrExt as _;
        let cwd_hint = cwd.map(|cwd| cwd.as_os_str().as_bytes());
        let is_subagent_or_foreign =
            self.agents
                .record_hook_session_with_context(id, &e.payload, Some(&e.agent), cwd_hint);
        self.status.subagent_hook(id, e, !is_subagent_or_foreign);
        self.observe_webhook(id);
        if !is_subagent_or_foreign && self.status.hook(&[(id, e)])? > 0 {
            self.observe_webhook(id);
            let status = self.status.get(id);
            if status.state == control::AgentState::Working {
                self.agents.clear_rate_limited(id);
            }
            if status.state == control::AgentState::Done
                && let Some(tap) = tap
            {
                tap.agent_done(status.since_unix_ns);
            }
        }
        // OWE-710: Claude StopFailure 한도 도달이면 자동전환을 즉시 평가한다.
        // OWE-901: agy Stop·PostInvocation 한도 봉투도 같이 본다.
        // OWE-703 2단계 지시: 한도 도달이면 실행 당시 계정(OWE-698 기록)을 RateLimited로
        // 먼저 기록한다. 캐시 사용량이 미상·낮아도 후보에서 빠지고 평가가 일어난다.
        if status::stop_failure_rate_limited(&e.payload) || status::agy_rate_limited(&e.payload) {
            if !is_subagent_or_foreign {
                self.agents.mark_rate_limited(id);
            }
            if let Ok(Some(stored)) = self.agents.stored(id)
                && let Some(launch) = stored.account
                && let Some(provider) =
                    krill_proto::usage::ProviderKind::from_env_var(&launch.env_key)
            {
                let account_id = if launch.id.is_empty() {
                    crate::usage::accounts::host_default_id(provider)
                } else {
                    launch.id
                };
                crate::usage::accounts::record_rate_limited(provider, &account_id);
            }
        }
        Ok(())
    }

    // Account evaluation wakes the session worker. Release `state` first: the worker's
    // final handoff holds its queue lock while retiring the old session.
    fn auto_switch_from_hook(&self, id: SessionId, e: &HookEvent) {
        if let Some(switched) = status::auto_switch_on_rate_limit(
            &crate::usage::accounts::default_home(),
            self.state_dir().as_deref(),
            &e.agent,
            &e.payload,
        ) && !switched.is_empty()
        {
            for s in &switched {
                tracing::info!(
                    session = id.0,
                    provider = s.provider.as_str(),
                    from = %s.from_id,
                    to = %s.to_id,
                    percent = s.reason_percent,
                    "stop failure rate limit auto-switched account"
                );
            }
        }
    }

    fn close(&self, peer: &Peer, id: SessionId) -> Result<Reply, Failure> {
        if !peer.authorize("close", id, &lock(&self.state).roots, &self.audit) {
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
                st.closing.insert(id);
                drop(st);
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
                return match holder.request(end) {
                    // ESRCH: 그사이 끝났다. Exited가 먼저 와서 on_exited가 지운다.
                    Ok((FromHolder::Done { .. }, _)) => Ok(Reply::Closed),
                    Ok((FromHolder::Failed { errno, .. }, _)) if errno == libc::ESRCH => {
                        Ok(Reply::Closed)
                    }
                    r => {
                        lock(&self.state).closing.remove(&id);
                        Err(match r {
                            Ok((FromHolder::Failed { errno, .. }, _)) => Failure::Os(errno),
                            Ok(_) => Failure::Os(libc::EIO),
                            Err(_) => Failure::NoHolder,
                        })
                    }
                };
            }
        }
        // 이미 끝난 세션: holder가 보관하던 master·상태를 놓게 한다. holder가 없거나 이미 놓았어도 지운다.
        holder.request(|req| ToHolder::Release { req, id }).ok();
        self.remove(id);
        Ok(Reply::Closed)
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
        // `Release`를 링크에 쓴 뒤에 지운다. 목록에서 사라진 뒤 krilld가 죽어도 holder는 소켓에 남은 `Release`를
        // 먼저 읽고 끝난 세션을 다음 krilld에 다시 넘기지 않는다(OWE-502).
        if let Some(h) = &self.holder {
            h.notify(|req| ToHolder::Release { req, id }).ok();
        }
        self.remove(id);
    }

    fn state_dir(&self) -> Option<PathBuf> {
        self.dir.clone().or_else(krill_proto::daemon::state_dir)
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
        self.discard_settings(id);
        self.agents.forget(id);
        let mut st = lock(&self.state);
        st.sessions.remove(&id);
        st.exited.remove(&id);
        st.closing.remove(&id);
        st.roots.remove(id);
        drop(st);
        self.status.remove(id);
        self.clear_sleeping(id);
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
        use crate::usage::accounts;
        use krill_proto::usage::{UsageReply, UsageRequest};

        if peer.scope != Scope::Full {
            peer.deny("usage", &self.audit);
            return UsageReply::Failed(Failure::Denied);
        }
        if req.contains_removed_provider() {
            return UsageReply::Failed(Failure::BadRequest);
        }
        if role == Role::Observer && !req.is_read() {
            return UsageReply::Failed(Failure::Denied);
        }
        let home = accounts::default_home();
        let state = self.state_dir();
        let state_ref = state.as_deref();
        let mut reply = match req {
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
            UsageRequest::SetDirectUsage(enabled) => {
                match accounts::set_direct_usage(&home, state_ref, enabled) {
                    Ok(snap) => UsageReply::Accounts(snap),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
            UsageRequest::SetAdvancedPolicy(policy) => {
                match accounts::set_advanced_policy(&home, state_ref, &policy) {
                    Ok(snap) => UsageReply::Accounts(snap),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
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
                        Ok(()) => UsageReply::Accounts(accounts::refresh_accounts_with(
                            &home, state_ref, false,
                        )),
                        Err(error) => UsageReply::Failed(error),
                    }
                }
            }
            UsageRequest::RefreshUsage => {
                crate::usage::direct::refresh_manual(&home, state_ref);
                UsageReply::Accounts(accounts::refresh_accounts(&home, state_ref))
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
            UsageRequest::UpsertAutoSwitchRule(rule) => {
                match accounts::upsert_auto_switch_rule(&home, state_ref, rule) {
                    Ok(snap) => UsageReply::Accounts(snap),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
            UsageRequest::RemoveAutoSwitchRule {
                provider,
                from_account_id,
            } => {
                match accounts::remove_auto_switch_rule(
                    &home,
                    state_ref,
                    provider,
                    &from_account_id,
                ) {
                    Ok(snap) => UsageReply::Accounts(snap),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
            UsageRequest::ConsumeResetCredit {
                provider,
                account_id,
            } => match accounts::consume_reset_credit(&home, state_ref, provider, &account_id) {
                Ok((snap, outcome, windows_reset)) => UsageReply::ResetConsumed {
                    snapshot: snap,
                    outcome,
                    windows_reset,
                },
                Err(e) => UsageReply::Failed(os(&e)),
            },
            UsageRequest::SetAutoSwitch(policy) => {
                match accounts::set_auto_switch(&home, state_ref, &policy) {
                    Ok(snap) => UsageReply::Accounts(snap),
                    Err(e) => UsageReply::Failed(os(&e)),
                }
            }
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

    /// 세션의 에이전트 트랜스크립트를 `offset` 바이트부터 증분 정규화해 읽는다(OWE-945 / MK-242).
    pub(crate) fn chat_poll(
        &self,
        id: SessionId,
        offset: u64,
    ) -> krill_proto::agent_history::ChatPollSnapshot {
        use krill_proto::os::ffi::OsStrExt as _;
        let (root_pid, fg_pid, exited) = {
            let st = lock(&self.state);
            if let Some(e) = st.sessions.get(&id) {
                let ex = st.exited.contains_key(&id) || e.session.is_closed();
                (e.pid, e.session.tap().foreground(), ex)
            } else {
                (0, None, true)
            }
        };
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
            if let Ok(Some(found)) = account_switch::identity::discover(provider, root_pid, fg_pid)
            {
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
                        reset: offset > 0,
                        ..Default::default()
                    };
                }
                self.agents
                    .bind_discovered_session(id, &found.id, Some(&found.path));
                if let Some(s) = &mut stored {
                    if s.session_id.is_none() {
                        s.session_id = Some(found.id);
                    }
                    if s.transcript_path.is_none()
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
                    reset: offset > 0,
                    bound: false,
                    items: Vec::new(),
                };
            }
        }

        let launched_at = self.agents.launched_at(id);
        let claimed = self.agents.claimed_transcripts(id);
        let (snapshot, resolved) = crate::agent_history::chat::poll_session_bounded(
            self.state_dir().as_deref(),
            stored.as_ref(),
            &cwd,
            offset,
            launched_at,
            &claimed,
        );
        if let Some((path, sid)) = resolved
            && stored.as_ref().is_some_and(|s| {
                s.session_id.as_deref() == Some(&sid) && s.transcript_path.is_none()
            })
        {
            // Cwd/recency fallback is for displaying history only. It must never become
            // authority to replace a live conversation during an account switch.
            self.agents
                .bind_discovered_session(id, &sid, Some(path.as_path()));
        }
        snapshot
    }

    pub(crate) fn deliver_input_to_tap(tap: &Tap, bytes: &[u8], submit: bool) -> io::Result<()> {
        let (_, _, _, modes) = tap.capture_with_modes();
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
        let is_control_seq = payload.starts_with(b"\x1b")
            || payload
                .iter()
                .any(|&b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t');
        let use_bracketed = bracketed
            && !payload.is_empty()
            && !is_control_seq
            && (submit || payload.contains(&b'\n') || payload.contains(&b'\r'));
        if use_bracketed {
            let mut framed = Vec::with_capacity(payload.len() + 12);
            framed.extend_from_slice(b"\x1b[200~");
            let mut rest = payload;
            while let Some(pos) = rest.windows(6).position(|w| w == b"\x1b[201~") {
                framed.extend_from_slice(rest.get(..pos).unwrap_or_default());
                rest = rest.get(pos + 6..).unwrap_or_default();
            }
            framed.extend_from_slice(rest);
            framed.extend_from_slice(b"\x1b[201~");
            tap.write_input(&framed, None)?;
            if submit {
                std::thread::sleep(Duration::from_millis(25));
                tap.write_input(b"\r", None)?;
            }
        } else {
            if !payload.is_empty() {
                tap.write_input(payload, None)?;
            }
            if submit {
                tap.write_input(b"\r", None)?;
            }
        }
        Ok(())
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
        let was_working = self.status.get(session).state == control::AgentState::Working;
        Self::deliver_input_to_tap(&tap, bytes, submit).map_err(|e| os(&e))?;
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
    }

    fn notify_orch(&self) {
        let (lock_gen, cvar) = &self.orch_signal;
        let mut g = lock(lock_gen);
        *g = g.wrapping_add(1);
        drop(g);
        cvar.notify_all();
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
        let res: io::Result<OrchResponse> = match action {
            OrchAction::Snapshot => self.orchestration_snapshot().map(OrchResponse::Snapshot),
            OrchAction::CreateRun {
                title,
                coordinator_session_id,
            } => OrchestrationStore::mutate(&dir, |s| s.create_run(&title, coordinator_session_id))
                .map(|r| {
                    self.notify_orch();
                    OrchResponse::Run(r)
                }),
            OrchAction::SetCurrentRun { run_id } => OrchestrationStore::mutate(&dir, |s| {
                s.set_current_run(run_id.as_deref())?;
                Ok(OrchResponse::CurrentRun(s.current_run_id.clone()))
            }),
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
                OrchResponse::Dispatch(d)
            }),
            OrchAction::SendMessage(params) => {
                OrchestrationStore::mutate(&dir, |s| s.send_message(&params)).map(|msgs| {
                    self.notify_orch();
                    OrchResponse::Messages(msgs)
                })
            }
            OrchAction::CheckMessages {
                recipient,
                types,
                unread_only,
                ack,
                wait_ms,
            } => self.check_messages_wait(
                &dir,
                (recipient.as_deref(), None),
                &types,
                unread_only,
                ack,
                wait_ms,
            ),
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
                    let found = OrchestrationStore::load(&dir).ok().and_then(|s| {
                        s.messages
                            .iter()
                            .find(|m| m.reply_to.as_deref() == Some(question_id.as_str()))
                            .cloned()
                    });
                    if let Some(ans) = found {
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
            } => OrchestrationStore::mutate(&dir, |s| s.reply_message(&question_id, &from, &body))
                .map(|m| {
                    self.notify_orch();
                    OrchResponse::Message(m)
                }),
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
                OrchResponse::Dispatch(d)
            }),
            OrchAction::AbandonWorker { worker_or_dispatch } => {
                OrchestrationStore::mutate(&dir, |s| s.abandon_worker(&worker_or_dispatch)).map(
                    |d| {
                        self.notify_orch();
                        OrchResponse::Dispatch(d)
                    },
                )
            }
            OrchAction::RetainWorker { worker_or_dispatch } => {
                OrchestrationStore::mutate(&dir, |s| s.retain_worker(&worker_or_dispatch)).map(
                    |d| {
                        self.notify_orch();
                        OrchResponse::Dispatch(d)
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

fn read_only(req: &Request) -> bool {
    matches!(
        req,
        Request::List
            | Request::States
            | Request::SessionAccounts
            | Request::CaptureSince { .. }
            | Request::Capture { .. }
            | Request::GitChanges { .. }
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
                    | krill_proto::orchestration::OrchAction::CheckMessages { ack: false, .. }
                    | krill_proto::orchestration::OrchAction::CheckMessagesFiltered {
                        ack: false,
                        ..
                    }
            )
    )
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
