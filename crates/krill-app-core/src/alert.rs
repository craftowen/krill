//! 알림·Dock 배지·잠자기 방지(MK-044).
//!
//! MK-016 스파이크 잠정 결정(비공개 MK-016 스파이크 기록)에 따라 Developer ID 없이 ad-hoc 서명된
//! `krill-app` 본 앱을 단일 발신자로 삼아:
//! - `gpui::App`의 `UNUserNotificationCenter` 경로(`show_system_notification`,
//!   `dismiss_system_notification`, `on_system_notification_response`)로 `"pane:<id>"` 식별자 알림을
//!   보내고 클릭 시 해당 pane으로 이동하며,
//! - `Needs You`(`Waiting` | `Blocked`) 세션 개수를 `NSDockTile` 배지에 반영하고,
//! - `IOPMAssertion`(`PreventUserIdleSystemSleep`)을 `On` / `Agent` / `Off` 모드에 따라 유지·해제한다.
//!
//! 잠정 결정 의존(MK-016): 이 모듈(`crates/krill-app/src/alert.rs`) 하나에 격리돼 있다.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;

use gpui::{App, Global, SharedString, SystemNotification, WeakEntity};
use krill_proto::SessionId;
use krill_proto::control::AgentState;

use crate::platform::{self, SleepAssertion};
use crate::settings::{self, KeepAwake};
use crate::sidebar::{Selected, Session, Sidebar, Snapshot};

const PANE_TAG_PREFIX: &str = "pane:";
const SLEEP_REASON: &str = "Krill agent working";

enum AwakeCommand {
    Set(bool),
    #[cfg(all(test, target_os = "macos"))]
    Barrier(mpsc::Sender<()>),
}

/// Windows 실행 상태는 스레드에 귀속되므로 취득·해제를 같은 전용 스레드에서 수행한다.
struct AwakeWorker {
    send: mpsc::Sender<AwakeCommand>,
    task: std::thread::JoinHandle<()>,
}

impl AwakeWorker {
    fn spawn() -> std::io::Result<Self> {
        let (send, recv) = mpsc::channel();
        let task = std::thread::Builder::new()
            .name("sleep-assertion".into())
            .spawn(move || {
                let mut assertion = None;
                while let Ok(command) = recv.recv() {
                    match command {
                        AwakeCommand::Set(true) if assertion.is_none() => {
                            assertion = SleepAssertion::acquire(SLEEP_REASON);
                        }
                        AwakeCommand::Set(false) => assertion = None,
                        AwakeCommand::Set(true) => {}
                        #[cfg(all(test, target_os = "macos"))]
                        AwakeCommand::Barrier(done) => {
                            done.send(()).ok();
                        }
                    }
                }
                drop(assertion);
            })?;
        Ok(Self { send, task })
    }
}

use crate::i18n::catalog::alert as text;

/// 세션 ID를 `"pane:<id>"` 알림 식별자로 바꾼다(MK-016).
pub fn pane_tag(id: SessionId) -> SharedString {
    format!("{PANE_TAG_PREFIX}{}", id.0).into()
}

/// `"pane:<id>"` 알림 식별자에서 세션 ID를 꺼낸다.
pub fn parse_pane_tag(tag: &str) -> Option<SessionId> {
    tag.strip_prefix(PANE_TAG_PREFIX)?
        .parse::<u64>()
        .ok()
        .map(SessionId)
}

/// 알림음을 낼 조건(순수 판정, OWE-655). 알림이 꺼져 있거나 사운드가 꺼져 있거나
/// 새로 보낼 알림이 없으면 조용하다.
pub fn should_play_sound(cfg: &settings::Settings, new_notifications: usize) -> bool {
    cfg.notifications && cfg.notification_sound && new_notifications > 0
}

/// 알림 이벤트 종류(OWE-659: 입력 대기·작업 완료·터미널 벨).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertEvent {
    /// 에이전트 입력·승인 대기(`Waiting` | `Blocked`).
    InputWait,
    /// 에이전트 작업 완료(`Done`).
    Done,
    /// 터미널 벨(`BEL`).
    Bell,
}

/// 이벤트별 발송 판정(순수 함수, OWE-659). 마스터 `notifications`가 꺼지면
/// 이벤트 토글과 관계없이 전부 끈다.
#[must_use]
pub const fn should_notify(cfg: &settings::Settings, event: AlertEvent) -> bool {
    if !cfg.notifications {
        return false;
    }
    match event {
        AlertEvent::InputWait => cfg.notify_on_input,
        AlertEvent::Done => cfg.notify_on_done,
        AlertEvent::Bell => cfg.notify_on_bell,
    }
}

/// 잠자기 방지 모드와 현재 `Working` 세션 수로 assertion 유지 여부를 판정한다.
pub const fn should_keep_awake(mode: KeepAwake, working_count: usize) -> bool {
    match mode {
        KeepAwake::On => true,
        KeepAwake::Agent => working_count > 0,
        KeepAwake::Off => false,
    }
}

#[derive(Default)]
struct AlertState {
    /// 마지막으로 관측한 세션별 상태.
    seen: HashMap<SessionId, AgentState>,
    initialized: bool,
    focused: Option<SessionId>,
    window_active: bool,
    /// 이미 알림을 보낸 `(AgentState, since_unix_ns)` 기록(중복 발송 방지).
    notified: HashMap<SessionId, (AgentState, u64)>,
    /// 현재 알림 센터에 게시돼 있는 세션 집합.
    active_tags: HashSet<SessionId>,
    /// 세션별 알림 음소거 집합(#28312, #28289).
    muted_sessions: HashSet<SessionId>,
    /// 워크트리(폴더)별 알림 음소거 집합(#28312, #28289).
    muted_worktrees: HashSet<std::path::PathBuf>,
    /// 오케스트레이션 워커로 실행 중인 세션 집합(#30058: 일반 작업 완료 알림 억제).
    orchestration_workers: HashSet<SessionId>,
    /// 마지막 스냅샷의 `Needs You`(`Waiting` | `Blocked`) 세션 수.
    needs_you_count: usize,
    /// 마지막 스냅샷의 `Working` 세션 수.
    working_count: usize,
    /// 현재 Dock 배지 숫자(알림이 꺼지면 0).
    badge_count: usize,
    /// 알림음을 낸 횟수(테스트·상태 확인용, 실제 발음은 `cfg(test)`가 아닐 때만).
    beep_count: u64,
    /// 메인 스레드는 요청 상태만 보관하고 OS 핸들은 전용 작업자가 소유한다.
    awake_requested: bool,
    awake: Option<AwakeWorker>,
    account_switch_sequence: Option<u64>,
}

impl Global for AlertState {}

// Saved notification/power preferences must be loaded before producing side
// effects. Early snapshots still establish a baseline for session transitions.
fn effective_settings(cx: &App) -> settings::Settings {
    let mut cfg = settings::current(cx);
    if !settings::is_loaded(cx) {
        cfg.notifications = false;
        cfg.keep_awake = settings::KeepAwake::Off;
    }
    cfg
}

fn is_session_or_worktree_muted(state: &AlertState, session: &Session) -> bool {
    state.muted_sessions.contains(&session.id)
        || session
            .worktree
            .as_deref()
            .or(session.cwd.as_deref())
            .is_some_and(|p| state.muted_worktrees.contains(p))
}

fn ensure_state(cx: &mut App) {
    if cx.try_global::<AlertState>().is_none() {
        cx.set_app_identity(krill_proto::daemon::BUNDLE_ID, "Krill");
        cx.set_global(AlertState::default());
        cx.on_app_quit(|cx| {
            let worker = cx.global_mut::<AlertState>().awake.take();
            cx.background_executor().spawn(async move {
                if let Some(AwakeWorker { send, task }) = worker {
                    drop(send);
                    task.join().ok();
                }
            })
        })
        .detach();
    }
}

/// 특정 세션의 알림·배지 음소거 상태를 설정한다(#28312, #28289).
pub fn set_session_muted(id: SessionId, muted: bool, cx: &mut App) {
    ensure_state(cx);
    let state = cx.global_mut::<AlertState>();
    if muted {
        state.muted_sessions.insert(id);
    } else {
        state.muted_sessions.remove(&id);
    }
    if muted {
        dismiss(id, cx);
    }
}

/// 특정 세션이 음소거 상태인지 확인한다(#28312).
#[must_use]
pub fn is_session_muted(id: SessionId, cx: &App) -> bool {
    cx.try_global::<AlertState>()
        .is_some_and(|state| state.muted_sessions.contains(&id))
}

/// 특정 워크트리(폴더)의 알림·배지 음소거 상태를 설정한다(#28312, #28289).
pub fn set_worktree_muted(worktree: impl Into<std::path::PathBuf>, muted: bool, cx: &mut App) {
    ensure_state(cx);
    let path = worktree.into();
    let state = cx.global_mut::<AlertState>();
    if muted {
        state.muted_worktrees.insert(path);
    } else {
        state.muted_worktrees.remove(&path);
    }
}

/// 특정 워크트리(폴더)가 음소거 상태인지 확인한다(#28312).
#[must_use]
pub fn is_worktree_muted(worktree: &Path, cx: &App) -> bool {
    cx.try_global::<AlertState>()
        .is_some_and(|state| state.muted_worktrees.contains(worktree))
}

/// 오케스트레이션 워커 세션 목록을 갱신한다(#30058: 워커 세션의 일반 `Done` 알림을 억제한다).
pub fn set_orchestration_worker_sessions(
    sessions: impl IntoIterator<Item = SessionId>,
    cx: &mut App,
) {
    ensure_state(cx);
    cx.global_mut::<AlertState>().orchestration_workers = sessions.into_iter().collect();
}

/// 알림 클릭 시 해당 세션의 pane으로 포커스를 옮기도록 `Sidebar`에 리스너를 등록한다.
pub fn listen(sidebar: &WeakEntity<Sidebar>, cx: &mut App) {
    ensure_state(cx);
    sync_settings(cx);
    if crate::menu_bar::is_monitoring(cx) {
        return;
    }
    let sidebar = sidebar.clone();
    cx.on_system_notification_response(move |response, cx| {
        let Some(id) = parse_pane_tag(response.tag.as_ref()) else {
            return;
        };
        dismiss(id, cx);
        cx.activate(true);
        sidebar.update(cx, |_, cx| cx.emit(Selected(id))).ok();
    });
}

/// 사이드바가 폴링한 최신 세션 스냅샷을 반영해 알림·Dock 배지·`IOPMAssertion`을 갱신한다.
/// 활성 창이 있으면 모든 알림을 생략한다. 창이 없거나 비활성이면 알림을 보낸다(메뉴바 상주).
pub fn sync_snapshot(snapshot: &Snapshot, cx: &mut App) {
    ensure_state(cx);
    let cfg = effective_settings(cx);
    // 창이 닫힌 뒤에도 `window_active`·`focused`가 stale하게 남을 수 있어 메뉴바 상주 알림까지
    // 막는다. 억제는 창이 있을 때만 적용한다.
    let has_window = !cx.windows().is_empty();
    let mut to_show: Vec<(SessionId, SystemNotification)> = Vec::new();
    let mut to_dismiss: Vec<SessionId> = Vec::new();

    let star_nag_observation = {
        let state = cx.global_mut::<AlertState>();
        let initialized = state.initialized;
        let mut present = HashSet::with_capacity(snapshot.sessions.len());
        let mut needs_you = 0usize;
        let mut working = 0usize;
        let mut transitioned_to_done = false;
        let mut newly_started = 0usize;

        for session in &snapshot.sessions {
            present.insert(session.id);
            let prev = state.seen.insert(session.id, session.status.state);
            let muted = is_session_or_worktree_muted(state, session);
            match session.status.state {
                AgentState::Working => {
                    working += 1;
                    if initialized
                        && prev.is_some_and(|old| {
                            matches!(
                                old,
                                AgentState::Done | AgentState::Waiting | AgentState::Blocked
                            )
                        })
                    {
                        newly_started += 1;
                    }
                    state.notified.remove(&session.id);
                    if state.active_tags.remove(&session.id) {
                        to_dismiss.push(session.id);
                    }
                }
                AgentState::Waiting | AgentState::Blocked => {
                    if !muted {
                        needs_you += 1;
                    }
                    let key = (session.status.state, session.status.since_unix_ns);
                    if state.notified.get(&session.id) != Some(&key) {
                        state.notified.insert(session.id, key);
                        if !muted
                            && should_notify(&cfg, AlertEvent::InputWait)
                            && initialized
                            && !(state.window_active && has_window)
                        {
                            state.active_tags.insert(session.id);
                            to_show.push((session.id, build_notification(session)));
                        }
                    }
                }
                AgentState::Done => {
                    let key = (session.status.state, session.status.since_unix_ns);
                    let is_worker = state.orchestration_workers.contains(&session.id);
                    if initialized
                        && prev.is_some_and(|old| {
                            matches!(
                                old,
                                AgentState::Working | AgentState::Waiting | AgentState::Blocked
                            )
                        })
                    {
                        transitioned_to_done = true;
                    }
                    // Working→Done뿐 아니라 Waiting·Blocked→Done(폴링 사이 전이)도 알린다.
                    // 같은 Done 스냅샷 반복은 notified 기록으로 걸러진다.
                    if prev.is_some_and(|old| old != AgentState::Done)
                        && state.notified.get(&session.id) != Some(&key)
                    {
                        state.notified.insert(session.id, key);
                        if !muted
                            && !is_worker
                            && should_notify(&cfg, AlertEvent::Done)
                            && initialized
                            && !(state.window_active && has_window)
                        {
                            state.active_tags.insert(session.id);
                            to_show.push((session.id, build_notification(session)));
                        }
                    }
                }
                AgentState::Exited | AgentState::Unknown => {
                    state.notified.remove(&session.id);
                    if state.active_tags.remove(&session.id) {
                        to_dismiss.push(session.id);
                    }
                }
            }
        }

        let removed: Vec<SessionId> = state
            .seen
            .keys()
            .copied()
            .filter(|id| !present.contains(id))
            .collect();
        for id in removed {
            state.seen.remove(&id);
            state.notified.remove(&id);
            if state.active_tags.remove(&id) {
                to_dismiss.push(id);
            }
        }

        state.initialized = true;
        state.needs_you_count = needs_you;
        state.working_count = working;
        (
            working > 0 || needs_you > 0,
            transitioned_to_done,
            newly_started,
        )
    };
    let (any_active, transitioned_to_done, newly_started) = star_nag_observation;
    for _ in 0..newly_started {
        crate::star_nag::record_agent_start(cx);
    }
    crate::star_nag::observe_agent_activity(any_active, transitioned_to_done, cx);

    for id in to_dismiss {
        crate::onboarding::actions::dismiss_notification(&pane_tag(id), cx);
    }
    let new_count = to_show.len();
    for (_, notification) in to_show {
        crate::onboarding::actions::show_notification(notification, cx);
    }
    if should_play_sound(&cfg, new_count) {
        play_notification_sound(cx);
    }
    apply_outputs(&cfg, cx);
}

/// 데몬 상태를 모르면 이전 작업 수·배지·알림을 해제하고 재접속 스냅샷을 기준점으로 삼는다.
/// 명시적인 On 모드는 세션 수와 관계없이 사용자의 설정을 따른다.
pub fn disconnected(cx: &mut App) {
    ensure_state(cx);
    crate::star_nag::cancel_agent_value_moment(cx);
    let tags = {
        let state = cx.global_mut::<AlertState>();
        state.initialized = false;
        state.seen.clear();
        state.notified.clear();
        state.needs_you_count = 0;
        state.working_count = 0;
        state.active_tags.drain().collect::<Vec<_>>()
    };
    for id in tags {
        crate::onboarding::actions::dismiss_notification(&pane_tag(id), cx);
    }
    apply_outputs(&effective_settings(cx), cx);
}

/// 재접속 뒤 같은 스냅샷이어도 작업 수·배지를 다시 반영해야 한다.
pub fn needs_snapshot(cx: &App) -> bool {
    cx.try_global::<AlertState>()
        .is_none_or(|state| !state.initialized)
}

/// 활성 창에서 보고 있는 세션은 시스템 알림을 띄우지 않는다.
pub fn focus(session: Option<SessionId>, cx: &mut App) {
    ensure_state(cx);
    cx.global_mut::<AlertState>().focused = session;
    if cx.global::<AlertState>().window_active
        && let Some(id) = session
    {
        dismiss(id, cx);
    }
}

pub fn set_active(active: bool, cx: &mut App) {
    ensure_state(cx);
    cx.global_mut::<AlertState>().window_active = active;
    if active && let Some(id) = cx.global::<AlertState>().focused {
        dismiss(id, cx);
    }
}

/// `settings.toml` 변경(`notifications`, `keep_awake`)을 즉시 배지·알림·`IOPMAssertion`에 반영한다.
pub fn sync_settings(cx: &mut App) {
    ensure_state(cx);
    let cfg = effective_settings(cx);
    apply_outputs(&cfg, cx);
}

/// 특정 세션이 포커스되거나 닫힐 때 남아 있는 알림을 지운다.
pub fn dismiss(id: SessionId, cx: &mut App) {
    ensure_state(cx);
    let removed = cx.global_mut::<AlertState>().active_tags.remove(&id);
    if removed {
        crate::onboarding::actions::dismiss_notification(&pane_tag(id), cx);
    }
}

/// 현재 Dock 배지 문자열(`None`이면 배지 없음).
pub fn dock_badge(cx: &App) -> Option<String> {
    let count = cx.try_global::<AlertState>()?.badge_count;
    (count > 0).then(|| count.to_string())
}

/// 지금까지 낸 알림음 횟수(테스트·상태 확인용).
pub fn beep_count(cx: &App) -> u64 {
    cx.try_global::<AlertState>()
        .map_or(0, |state| state.beep_count)
}

/// 알림음을 한 번 낸다. 횟수는 항상 세고, 실제 발음(macOS Krill 차임, 없으면 `NSBeep`)은 단위 테스트가 아닐 때만 한다.
fn play_notification_sound(cx: &mut App) {
    ensure_state(cx);
    let state = cx.global_mut::<AlertState>();
    state.beep_count = state.beep_count.saturating_add(1);
    #[cfg(not(test))]
    crate::platform::play_notification_chime();
}

/// 터미널 벨 알림(OWE-659). 이벤트 토글이 꺼져 있으면 조용하고, 이미 게시된
/// 알림이 이미 있거나 활성 창이 있으면 생략한다.
pub fn notify_bell(id: SessionId, label: &str, cx: &mut App) {
    ensure_state(cx);
    let cfg = effective_settings(cx);
    if !should_notify(&cfg, AlertEvent::Bell) {
        return;
    }
    let has_window = !cx.windows().is_empty();
    let suppressed = {
        let state = cx.global::<AlertState>();
        state.active_tags.contains(&id)
            || state.muted_sessions.contains(&id)
            || (state.window_active && has_window)
    };
    if suppressed {
        return;
    }
    cx.global_mut::<AlertState>().active_tags.insert(id);
    let title = if label.is_empty() {
        format!("Session #{}", id.0)
    } else {
        label.to_owned()
    };
    crate::onboarding::actions::show_notification(
        SystemNotification {
            tag: pane_tag(id),
            title: format!("{title} — {}", text::BELL.get()).into(),
            body: text::BELL_BODY.get().into(),
            actions: Vec::new(),
        },
        cx,
    );
}

/// 유휴 잠자기 방지를 요청 중인지 돌려준다. OS 적용·해제는 전용 작업자가 순서대로 처리한다.
pub fn is_preventing_sleep(cx: &App) -> bool {
    cx.try_global::<AlertState>()
        .is_some_and(|s| s.awake_requested)
}

fn apply_outputs(cfg: &settings::Settings, cx: &mut App) {
    let mut to_dismiss = Vec::new();
    let badge_label = {
        let state = cx.global_mut::<AlertState>();
        if !cfg.notifications && !state.active_tags.is_empty() {
            to_dismiss.extend(state.active_tags.drain());
        }
        let next_badge = if cfg.notifications {
            state.needs_you_count
        } else {
            0
        };
        state.badge_count = next_badge;

        let want_awake = should_keep_awake(cfg.keep_awake, state.working_count);
        if want_awake && state.awake.is_none() {
            match AwakeWorker::spawn() {
                Ok(worker) => state.awake = Some(worker),
                Err(error) => tracing::warn!(kind = ?error.kind(), "sleep assertion worker failed"),
            }
        }
        if want_awake != state.awake_requested {
            state.awake_requested = state.awake.as_ref().is_some_and(|worker| {
                worker.send.send(AwakeCommand::Set(want_awake)).is_ok() && want_awake
            });
        }

        (next_badge > 0).then(|| next_badge.to_string())
    };

    for id in to_dismiss {
        crate::onboarding::actions::dismiss_notification(&pane_tag(id), cx);
    }
    platform::set_dock_badge(badge_label.as_deref());
    #[cfg(windows)]
    platform::sync_windows(cx);
}

fn build_notification(session: &Session) -> SystemNotification {
    let (headline, default_body) = match session.status.state {
        AgentState::Blocked => (text::BLOCKED.get(), text::BLOCKED_BODY.get()),
        AgentState::Done => (text::DONE.get(), text::DONE_BODY.get()),
        AgentState::Waiting | AgentState::Working | AgentState::Exited | AgentState::Unknown => {
            (text::WAITING.get(), text::WAITING_BODY.get())
        }
    };
    let label = session_label(session);
    let title = format!("{label} — {headline}");
    let body = if session.status.agent.is_empty() {
        default_body.to_owned()
    } else {
        format!("{}: {default_body}", session.status.agent)
    };
    SystemNotification {
        tag: pane_tag(session.id),
        title: title.into(),
        body: body.into(),
        actions: Vec::new(),
    }
}

fn session_label(session: &Session) -> String {
    if let Some(folder) = session.worktree.as_deref().or(session.cwd.as_deref())
        && let Some(name) = basename(folder)
    {
        return name;
    }
    if !session.status.agent.is_empty() {
        return format!("{} #{}", session.status.agent, session.id.0);
    }
    format!("Session #{}", session.id.0)
}

fn basename(path: &Path) -> Option<String> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    (!name.is_empty()).then(|| name.into_owned())
}

#[cfg(test)]
mod tests;

/// 자동 계정 전환은 진단 로그에만 남긴다. 사용자 조치가 필요하지 않아 알림을 보내지 않는다.
/// 최초 스냅샷은 기준점으로 삼아 앱 재시작 때 과거 이력을 다시 기록하지 않는다.
/// 정책 꼬리가 없는 스냅샷(로컬 폴백·구 데몬)은 이력을 모르는 것이라 기준점을 건드리지 않는다.
pub fn sync_account_switches(advanced: &krill_proto::usage::AccountPolicyState, cx: &mut App) {
    if !advanced.supported {
        return;
    }
    let history = &advanced.history;
    ensure_state(cx);
    let latest = history.last().map_or(0, |r| r.sequence);
    let previous = cx
        .global_mut::<AlertState>()
        .account_switch_sequence
        .replace(latest);
    let Some(previous) = previous else { return };
    for record in history.iter().filter(|r| r.sequence > previous) {
        tracing::debug!(
            sequence = record.sequence,
            provider = record.event.provider.display_name(),
            "account switched automatically"
        );
    }
}
