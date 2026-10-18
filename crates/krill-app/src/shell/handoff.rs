//! 팝아웃 핸드오프의 안전한 브리프 전달(OWE-633 추가 과제).
//!
//! 새 에이전트 탭에 브리프를 바로 치면 TUI 부팅 전이라 시작 메뉴(trust 등)에 들어가거나
//! 여러 줄이 줄마다 실행될 수 있다. `krill worker-start`와 같은 순서로 안전하게 보낸다.
//! 먼저 칸에 세션이 붙을 때까지 기다리고(탭을 닫으면 조용히 그만둔다), 다음으로 제어 연결
//! (`WaitStatus`+캡처)로 부팅 판정을 한다. 시작 중 종료·trust/권한 메뉴면 브리프를 치지 않고
//! 오류를 알린다. 준비됐으면 [`krill_app::security::plan_paste`]로 bracketed paste로 감싸고
//! Enter(`\r`)를 붙여 보낸다. 프로그램이 bracketed paste를 안 쓰면 여러 줄은 보내지 않고
//! 오류를 알린다.
//! 실패·시간 초과는 알림으로 알린다. 브리프는 클립보드에 남아 있어 직접 붙여넣을 수 있다.

use krill_proto::os::net::UnixStream;
use std::io::{self, Write as _};
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, AsyncApp, ClipboardItem, Context, WeakEntity};
use krill_app::hosts::HostTarget;
use krill_proto::agent::rules::{MenuHit, ScreenRules};
use krill_proto::control::{
    AgentState, CaptureFormat, FEAT_CLI, FEAT_CONTROL, FEAT_WAIT_STATUS, Reply, Request,
};
use krill_proto::daemon;
#[cfg(test)]
use krill_proto::daemon::Paths;
use krill_proto::input::Now;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use krill_proto::term_modes::Modes;

use krill_app::i18n::catalog::popout as text;
use krill_app::layout::PaneId;
use krill_app::link::notice_text;
use krill_app::security::{self, PastePlan};
use krill_proto::SessionId;

use super::Shell;

/// 에이전트 부팅 대기 상한(`crates/krill/src/session.rs`의 `BOOT_TIMEOUT`과 같은 값).
pub(super) const BOOT_TIMEOUT: Duration = Duration::from_secs(12);
/// 칸 세션 할당 폴링 간격.
const SESSION_POLL: Duration = Duration::from_millis(100);
/// 부팅 판정 폴링 간격(CLI와 같은 값).
const READY_POLL: Duration = Duration::from_millis(150);
/// 제어 연결 읽기·쓰기 상한.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// 전달 결과. `Dropped`는 탭을 닫은 경우라 조용히 끝낸다.
enum Outcome {
    Delivered,
    Dropped,
    Failed(FailKind),
}

/// 전달 실패 분류. UI에서 카탈로그 문구로 바꾼다.
#[derive(Debug, PartialEq, Eq)]
enum FailKind {
    Timeout,
    Exited,
    Blocked,
    Transport(String),
}

/// 부팅 판정. `krill session::boot_verdict`와 같은 규칙이다(CLI와 어긋나면 둘 중 하나를 고친다).
/// `None`이면 계속 대기한다.
fn boot_verdict(
    state: AgentState,
    screen: &str,
    quiet_ms: u64,
    elapsed: Duration,
    rules: &ScreenRules,
) -> Option<Result<(), FailKind>> {
    const BLOCKED: FailKind = FailKind::Blocked;
    match state {
        AgentState::Exited => return Some(Err(FailKind::Exited)),
        AgentState::Blocked => return Some(Err(BLOCKED)),
        AgentState::Done | AgentState::Waiting => return Some(Ok(())),
        AgentState::Working | AgentState::Unknown => {}
    }
    if rules.menu_hit(screen) == Some(MenuHit::Blocked) {
        return Some(Err(BLOCKED));
    }
    let non_empty: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let ready = non_empty.iter().rev().take(4).any(|l| rules.is_prompt(l))
        || (!non_empty.is_empty() && quiet_ms >= 1200 && elapsed >= Duration::from_millis(1500));
    ready.then_some(Ok(()))
}

/// bracketed paste 감싸기 + Enter(`\r`). 프로그램이 bracketed paste를 안 쓰면
/// 여러 줄은 보내지 않고 `None`이다.
fn handoff_payload(pane: PaneId, brief: &[u8], modes: Modes) -> Option<Vec<u8>> {
    match security::plan_paste(pane, brief, modes) {
        PastePlan::Immediate(mut bytes) => {
            bytes.push(b'\r');
            Some(bytes)
        }
        PastePlan::Warn(_) => None,
    }
}

fn roundtrip(stream: &mut UnixStream, req: &Request) -> io::Result<Reply> {
    stream.write_all(&req.encode().map_err(io::Error::other)?)?;
    let (kind, bytes) = ipc::read_frame(stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    Reply::decode(kind, &bytes).map_err(io::Error::other)
}

/// 새 에이전트 세션이 프롬프트 대기 상태가 될 때까지 기다린다(백그라운드 스레드에서만 호출).
/// 시작 중 종료·trust/권한 메뉴·시간 초과는 오류다.
fn wait_agent_ready(
    target: &HostTarget,
    sid: SessionId,
    timeout: Duration,
) -> Result<(), FailKind> {
    let refused = |detail: String| FailKind::Transport(detail);
    let hello = daemon::client_hello(ClientRole::App, "krill-app-handoff");
    let conn = target
        .connect(&hello)
        .map_err(|e| refused(format!("daemon connection failed: {e}")))?;
    if conn.negotiated.features & (FEAT_CONTROL | FEAT_CLI | FEAT_WAIT_STATUS)
        != (FEAT_CONTROL | FEAT_CLI | FEAT_WAIT_STATUS)
    {
        return Err(refused("daemon does not support handoff wait".to_owned()));
    }
    let mut sock = conn.stream;
    sock.set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|e| refused(format!("timer setup failed: {e}")))?;
    sock.set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|e| refused(format!("timer setup failed: {e}")))?;
    let rules = ScreenRules::default();
    let start = Instant::now();
    loop {
        if start.elapsed() >= timeout {
            return Err(FailKind::Timeout);
        }
        let Reply::WaitStatus { state, quiet_ms } =
            roundtrip(&mut sock, &Request::WaitStatus { session: sid })
                .map_err(|e| refused(format!("status wait failed: {e}")))?
        else {
            return Err(refused("unexpected status reply".to_owned()));
        };
        // 캡처에 실패하면 빈 화면으로 판정한다(CLI와 같음).
        let screen = match roundtrip(
            &mut sock,
            &Request::Capture {
                session: sid,
                format: CaptureFormat::Plain,
            },
        ) {
            Ok(Reply::Captured { bytes, .. }) => String::from_utf8_lossy(&bytes).into_owned(),
            _ => String::new(),
        };
        if let Some(verdict) = boot_verdict(state, &screen, quiet_ms, start.elapsed(), &rules) {
            std::thread::sleep(READY_POLL);
            return verdict;
        }
        std::thread::sleep(READY_POLL);
    }
}

/// 브리프 전송(한 번, UI 스레드에서만 호출).
enum SendOutcome {
    Sent,
    /// 탭을 닫았으면 조용히 그만둔다.
    Gone,
    NoLive,
    Refused,
    InputFailed(String),
}

fn send_brief(
    shell: &mut Shell,
    pane: PaneId,
    brief: &[u8],
    cx: &mut Context<Shell>,
) -> SendOutcome {
    let Some(live) = shell.live.get_mut(&pane) else {
        return if shell.layout.launch(pane).is_none() {
            SendOutcome::Gone
        } else {
            SendOutcome::NoLive
        };
    };
    let Some(payload) = handoff_payload(pane, brief, live.pane.modes()) else {
        return SendOutcome::Refused;
    };
    match live.pane.input(&payload, Now::get()) {
        None => {
            shell.show(None, cx);
            SendOutcome::Sent
        }
        Some(notice) => SendOutcome::InputFailed(notice_text(&notice)),
    }
}

fn fail_text(kind: &FailKind, agent: &str) -> String {
    match kind {
        FailKind::Timeout => text::HANDOFF_ERR_TIMEOUT.format(&[("agent", agent)]),
        FailKind::Exited => text::HANDOFF_ERR_EXITED.format(&[("agent", agent)]),
        FailKind::Blocked => text::HANDOFF_ERR_BLOCKED.format(&[("agent", agent)]),
        FailKind::Transport(reason) => {
            text::HANDOFF_ERR_SEND.format(&[("agent", agent), ("reason", reason)])
        }
    }
}

impl Shell {
    /// 칸의 에이전트 식별자(데몬 스냅샷 우선, 없으면 실행 설정 `Program::Agent`에서 추출).
    pub(super) fn pane_agent_id(&self, pane: PaneId, cx: &App) -> Option<String> {
        let host = self.layout.host(pane);
        if let Some(session) = self
            .layout
            .session(pane)
            .and_then(|id| self.sidebar.read(cx).session_on(host, id))
            && !session.status.agent.is_empty()
        {
            return Some(session.status.agent.clone());
        }
        match self.layout.launch(pane).map(|l| &l.program) {
            Some(krill_proto::control::Program::Agent { id }) => {
                let agent = super::tabs::launch_agent_id(id);
                (!agent.is_empty()).then(|| agent.to_owned())
            }
            _ => None,
        }
    }

    /// 연결된 데몬이 [`krill_proto::control::FEAT_SESSION_FORK`]를 협상했는지 확인한다.
    pub(super) fn peer_supports_fork(&self, pane: PaneId) -> bool {
        self.live
            .get(&pane)
            .filter(|l| l.pane.is_linked())
            .is_none_or(|l| l.pane.features() & krill_proto::control::FEAT_SESSION_FORK != 0)
    }

    /// 에이전트 세션 포크(`claude` / `codex` 활성 세션)가 가능한지 판정한다.
    pub(super) fn can_fork_pane(&self, pane: PaneId, cx: &App) -> bool {
        self.peer_supports_fork(pane)
            && self.layout.file(pane).is_none()
            && self.layout.session(pane).is_some()
            && matches!(
                self.pane_agent_id(pane, cx).as_deref(),
                Some("claude" | "codex")
            )
    }

    /// 새 세션에 요약 이어하기가 가능한지 판정한다(터미널 세션이 연결된 칸).
    pub(super) fn can_continue_pane(&self, pane: PaneId) -> bool {
        self.peer_supports_fork(pane)
            && self.layout.file(pane).is_none()
            && self.layout.session(pane).is_some()
    }

    /// 현재 탭 바로 옆에 포크된 에이전트 탭을 연다(MK-182). 원 세션 PTY는 건드리지 않는다.
    pub(super) fn fork_pane_session(
        &mut self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        if !self.peer_supports_fork(pane) {
            self.notice = Some(
                krill_app::i18n::catalog::resources::OUTDATED
                    .get()
                    .to_owned(),
            );
            cx.notify();
            return None;
        }
        if !self.can_fork_pane(pane, cx) {
            return None;
        }
        let sid = self.layout.session(pane)?;
        let agent = self.pane_agent_id(pane, cx)?;
        let host = self.layout.host(pane).clone();
        let cwd = self.pane_cwd_bytes(pane, cx);
        let tab_idx = self
            .layout
            .tabs()
            .iter()
            .position(|t| t.root.contains(pane))
            .unwrap_or_else(|| self.layout.active());
        let create = krill_proto::control::Create {
            account: None,
            program: krill_proto::control::Program::Agent {
                id: format!("fork:{agent}:{}", sid.0),
            },
            cwd,
            cols: krill_app::pane::SIZE.0,
            rows: krill_app::pane::SIZE.1,
        };
        let mut forked_pane = None;
        self.change(true, cx, |l| {
            l.new_tab_after(tab_idx);
            let p = l.active_tab().focused;
            l.set_host(p, host);
            l.set_launch(p, create);
            forked_pane = Some(p);
        });
        let agent_name = self
            .agents
            .as_ref()
            .and_then(|s| s.profiles.iter().find(|p| p.id == agent))
            .map_or(agent.as_str(), |p| p.name.as_str())
            .to_owned();
        self.notice =
            Some(krill_app::i18n::catalog::shell::FORKED_SESSION.format(&[("agent", &agent_name)]));
        cx.notify();
        forked_pane
    }

    /// 새 세션에 이어하기 모달을 연다(MK-182).
    pub(super) fn open_continue_session(
        &mut self,
        pane: PaneId,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if !self.peer_supports_fork(pane) {
            self.notice = Some(
                krill_app::i18n::catalog::resources::OUTDATED
                    .get()
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        let Some(sid) = self.layout.session(pane) else {
            return;
        };
        let default_agent = self.pane_agent_id(pane, cx);
        let agents = self
            .agents
            .as_ref()
            .map(|s| {
                let has_agy = s.profiles.iter().any(|p| p.id == "agy");
                s.profiles
                    .iter()
                    .filter(|p| !(has_agy && p.id == "gemini"))
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let dialog = cx.new(|cx| {
            krill_app::continue_session::Dialog::new(
                sid,
                agents,
                default_agent.as_deref(),
                window,
                cx,
            )
        });
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &krill_app::continue_session::Event, window, cx| match ev {
                krill_app::continue_session::Event::Close => {
                    this.continue_session = None;
                    window.focus(&this.focus, cx);
                    cx.notify();
                }
                krill_app::continue_session::Event::Submit {
                    source,
                    agent,
                    prompt,
                } => {
                    let (source, agent, prompt) = (*source, agent.clone(), prompt.clone());
                    this.continue_session = None;
                    window.focus(&this.focus, cx);
                    this.continue_in_new_session(pane, source, agent, &prompt, cx);
                }
            },
        );
        self.continue_session = Some((dialog, sub));
        cx.notify();
    }

    /// 원 세션 맥락 + 추가 지시문으로 바로 옆 탭에 새 에이전트 세션을 시작한다(MK-182).
    pub(super) fn continue_in_new_session(
        &mut self,
        source_pane: PaneId,
        source: SessionId,
        agent_id: String,
        prompt: &str,
        cx: &mut Context<Self>,
    ) -> PaneId {
        let host = self.layout.host(source_pane).clone();
        let cwd = self.pane_cwd_bytes(source_pane, cx);
        let tail = self
            .live
            .get(&source_pane)
            .map(|l| super::popout::screen_plain_text(&l.pane.shown()))
            .unwrap_or_default();
        let brief =
            krill_app::continue_session::format_handoff_brief(source, &agent_id, prompt, &tail);
        let bytes = brief.as_bytes().to_vec();
        cx.write_to_clipboard(ClipboardItem::new_string(brief));
        let tab_idx = self
            .layout
            .tabs()
            .iter()
            .position(|t| t.root.contains(source_pane))
            .unwrap_or_else(|| self.layout.active());
        let create = krill_proto::control::Create {
            account: None,
            program: krill_proto::control::Program::Agent {
                id: agent_id.clone(),
            },
            cwd,
            cols: krill_app::pane::SIZE.0,
            rows: krill_app::pane::SIZE.1,
        };
        let mut target_pane = source_pane;
        self.change(true, cx, |l| {
            l.new_tab_after(tab_idx);
            let p = l.active_tab().focused;
            l.set_host(p, host);
            l.set_launch(p, create);
            target_pane = p;
        });
        self.notice = Some(text::HANDOFF_WAITING.format(&[("agent", &agent_id)]));
        cx.notify();
        self.start_handoff_delivery(target_pane, agent_id, bytes, BOOT_TIMEOUT, cx);
        target_pane
    }

    fn pane_cwd_bytes(&self, pane: PaneId, cx: &App) -> Vec<u8> {
        use krill_proto::os::ffi::OsStrExt as _;
        let host = self.layout.host(pane);
        if let Some(cwd) = self
            .layout
            .session(pane)
            .and_then(|id| self.sidebar.read(cx).session_on(host, id))
            .and_then(|s| s.cwd.as_ref())
        {
            return cwd.as_os_str().as_bytes().to_vec();
        }
        self.launch_for(pane).cwd
    }

    /// 팝아웃 핸드오프 클릭: 클립보드 복사 + 새 에이전트 탭 + 안전 전달 시작.
    /// 브리프를 바로 치지 않고 에이전트가 준비될 때까지 기다렸다가 보낸다.
    pub(super) fn spawn_handoff_agent(
        &mut self,
        agent_id: &str,
        brief: String,
        cx: &mut Context<Self>,
    ) {
        let bytes = brief.as_bytes().to_vec();
        cx.write_to_clipboard(ClipboardItem::new_string(brief));
        self.new_agent(agent_id.to_owned(), cx);
        let pane = self.layout.active_tab().focused;
        self.notice = Some(text::HANDOFF_WAITING.format(&[("agent", agent_id)]));
        cx.notify();
        self.start_handoff_delivery(pane, agent_id.to_owned(), bytes, BOOT_TIMEOUT, cx);
    }

    /// 브리프 전달을 시작한다. `timeout`을 인자로 받아 테스트에서 짧게 검증한다.
    pub(super) fn start_handoff_delivery(
        &mut self,
        pane: PaneId,
        agent: String,
        brief: Vec<u8>,
        timeout: Duration,
        cx: &mut Context<Self>,
    ) {
        let weak: WeakEntity<Self> = cx.weak_entity();
        let target = self.target_for_pane(pane);
        cx.spawn(async move |_, cx| {
            let outcome = deliver_handoff(&weak, target, pane, &brief, timeout, cx).await;
            weak.update(cx, |this, cx| {
                match outcome {
                    Outcome::Delivered => {
                        this.notice = Some(text::HANDOFF_SPAWNED.format(&[("agent", &agent)]));
                    }
                    Outcome::Dropped => {}
                    Outcome::Failed(kind) => {
                        this.notice = Some(fail_text(&kind, &agent));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// 칸에 세션이 붙고 에이전트가 준비되면 브리프를 보낸다. UI를 막지 않는다.
async fn deliver_handoff(
    weak: &WeakEntity<Shell>,
    target: Option<HostTarget>,
    pane: PaneId,
    brief: &[u8],
    timeout: Duration,
    cx: &mut AsyncApp,
) -> Outcome {
    // 1. 칸에 세션이 붙을 때까지 대기한다. 먼저 확인하고 재운다(시간 초과 0이면 바로 판정).
    let start = Instant::now();
    let sid = loop {
        match weak.read_with(cx, |s, _| {
            (s.layout.launch(pane).is_some(), s.layout.session(pane))
        }) {
            Err(_) | Ok((false, _)) => return Outcome::Dropped,
            Ok((true, Some(sid))) => break sid,
            Ok((true, None)) => {}
        }
        if start.elapsed() >= timeout {
            return Outcome::Failed(FailKind::Timeout);
        }
        cx.background_executor().timer(SESSION_POLL).await;
    };
    // 2. 에이전트 부팅 판정.
    let Some(target) = target else {
        return Outcome::Failed(FailKind::Transport("daemon not connected".to_owned()));
    };
    let ready = cx
        .background_executor()
        .spawn(async move { wait_agent_ready(&target, sid, timeout) })
        .await;
    if let Err(kind) = ready {
        return Outcome::Failed(kind);
    }
    // 3. bracketed paste + Enter 전송.
    match weak.update(cx, |s, cx| send_brief(s, pane, brief, cx)) {
        Err(_) | Ok(SendOutcome::Gone) => Outcome::Dropped,
        Ok(SendOutcome::Sent) => Outcome::Delivered,
        Ok(SendOutcome::NoLive) => {
            Outcome::Failed(FailKind::Transport("pane is not attached".to_owned()))
        }
        Ok(SendOutcome::Refused) => Outcome::Failed(FailKind::Transport(
            "paste refused: the program does not use bracketed paste".to_owned(),
        )),
        Ok(SendOutcome::InputFailed(detail)) => Outcome::Failed(FailKind::Transport(detail)),
    }
}

#[cfg(test)]
mod e2e_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// CLI `boot_verdict`와 같은 벡터(`crates/krill/src/session.rs` 테스트와 대조).
    #[test]
    fn boot_verdict_matches_cli_rules() {
        let rules = ScreenRules::default();
        let early = Duration::from_millis(100);
        let verdict = |state, screen, quiet_ms, elapsed| {
            boot_verdict(state, screen, quiet_ms, elapsed, &rules).map(|r| r.is_ok())
        };
        assert_eq!(verdict(AgentState::Unknown, "", 0, early), None);
        assert_eq!(
            verdict(AgentState::Working, "Loading agy...", 0, early),
            None
        );
        assert_eq!(
            verdict(AgentState::Unknown, "Antigravity CLI\n>\n", 0, early),
            Some(true)
        );
        assert_eq!(verdict(AgentState::Done, "", 0, early), Some(true));
        assert_eq!(verdict(AgentState::Exited, "", 0, early), Some(false));
        assert_eq!(verdict(AgentState::Blocked, "", 0, early), Some(false));
        assert_eq!(
            verdict(
                AgentState::Unknown,
                "Do you trust the contents of this project?\n> Yes, I trust this folder",
                5000,
                early
            ),
            Some(false)
        );
        assert_eq!(
            verdict(
                AgentState::Working,
                "banner",
                1200,
                Duration::from_millis(1500)
            ),
            Some(true)
        );
    }

    #[test]
    fn boot_verdict_rejects_exited_and_blocked_before_prompt() {
        let rules = ScreenRules::default();
        let early = Duration::from_millis(100);
        assert_eq!(
            boot_verdict(AgentState::Exited, ">", 0, early, &rules),
            Some(Err(FailKind::Exited))
        );
        assert_eq!(
            boot_verdict(AgentState::Blocked, ">", 0, early, &rules),
            Some(Err(FailKind::Blocked))
        );
    }

    #[test]
    fn handoff_payload_wraps_bracketed_and_refuses_unbracketed_multiline() {
        use krill_app::layout::Layout;
        let pane = Layout::default().active_tab().focused;
        let bracketed = Modes {
            kitty: 0,
            flags: Modes::BRACKETED_PASTE,
        };
        let plain = Modes::default();
        let brief = b"line1\nline2\n";
        let payload = handoff_payload(pane, brief, bracketed).unwrap();
        assert!(payload.starts_with(b"\x1b[200~"), "{payload:?}");
        assert!(payload.ends_with(b"\x1b[201~\r"), "{payload:?}");
        assert!(handoff_payload(pane, brief, plain).is_none());
        assert!(handoff_payload(pane, b"single", plain).is_some());
    }

    #[gpui::test]
    fn fork_and_continue_session_insert_adjacent_tab_and_preserve_source(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::shell::tests::{open, state_path};
        use krill_proto::control::{Create, Program};

        let path = state_path("mk-182-fork-continue");
        let (view, cx) = open(cx, path);
        view.update(cx, |shell, cx| {
            let p0 = shell.layout.active_tab().focused;
            shell.layout.new_tab();
            let p_end = shell.layout.active_tab().focused;
            shell.layout.activate(0);
            shell.layout.set_session(p0, SessionId(42));
            shell.layout.set_launch(
                p0,
                Create {
                    account: None,
                    program: Program::Agent {
                        id: "claude".into(),
                    },
                    cwd: b"/tmp/repo".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            assert!(shell.can_fork_pane(p0, cx));
            assert!(shell.can_continue_pane(p0));

            let forked = shell.fork_pane_session(p0, cx).expect("fork pane");
            assert_eq!(shell.layout.tabs().len(), 3);
            assert_eq!(shell.layout.active(), 1);
            assert_eq!(shell.layout.tabs()[0].focused, p0);
            assert_eq!(shell.layout.tabs()[1].focused, forked);
            assert_eq!(shell.layout.tabs()[2].focused, p_end);
            assert_eq!(shell.layout.session(p0), Some(SessionId(42)));
            let launch = shell.layout.launch(forked).expect("fork launch");
            assert_eq!(
                launch.program,
                Program::Agent {
                    id: "fork:claude:42".into()
                }
            );
            assert_eq!(launch.cwd, b"/tmp/repo");

            let continued =
                shell.continue_in_new_session(p0, SessionId(42), "codex".into(), "다음 단계", cx);
            assert_eq!(shell.layout.tabs().len(), 4);
            assert_eq!(shell.layout.active(), 1);
            assert_eq!(shell.layout.tabs()[1].focused, continued);
            assert_eq!(shell.layout.session(p0), Some(SessionId(42)));
            let cont_launch = shell.layout.launch(continued).expect("continue launch");
            assert_eq!(cont_launch.program, Program::Agent { id: "codex".into() });

            // 구 데몬(FEAT_SESSION_FORK 미협상)에 붙은 칸은 메뉴·액션이 숨겨지거나 데몬 업데이트 필요 안내로 끝난다.
            let (ours, _theirs) = UnixStream::pair().unwrap();
            let mut live = crate::shell::Live::new(gpui::Task::ready(()));
            live.pane.link(
                SessionId(42),
                1,
                krill_app::pane::Writer::spawn(ours).unwrap(),
                krill_proto::screen::FEAT_SCREEN | krill_proto::input::FEAT_INPUT,
                Now::get(),
            );
            shell.live.insert(p0, live);
            assert!(!shell.peer_supports_fork(p0));
            assert!(!shell.can_fork_pane(p0, cx));
            assert!(!shell.can_continue_pane(p0));
            assert!(shell.fork_pane_session(p0, cx).is_none());
            assert_eq!(shell.layout.tabs().len(), 4);
            assert_eq!(
                shell.notice.as_deref(),
                Some(krill_app::i18n::catalog::resources::OUTDATED.get())
            );
        });
    }
}
