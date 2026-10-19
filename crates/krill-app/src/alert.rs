//! 알림·Dock 배지·잠자기 방지(MK-044).
//!
//! MK-016 스파이크 잠정 결정(`docs/spikes/MK-016.md`)에 따라 Developer ID 없이 ad-hoc 서명된
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

use gpui::{App, Global, SharedString, SystemNotification, WeakEntity};
use krill_proto::SessionId;
use krill_proto::control::AgentState;

use crate::platform::{self, SleepAssertion};
use crate::settings::{self, KeepAwake};
use crate::sidebar::{Selected, Session, Sidebar, Snapshot};

const PANE_TAG_PREFIX: &str = "pane:";
const SLEEP_REASON: &str = "Krill agent working";

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

/// 창이 살아 있을 때 알림을 생략할지. `notify_only_inactive`면 활성 창이 하나라도
/// 있으면 모두 생략하고, 아니면 보고 있는 칸만 생략한다. 활성 창이 없으면
/// (메뉴바 상주 포함) 항상 보낸다.
const fn suppress_notification(
    notify_only_inactive: bool,
    focused_current: bool,
    has_active_window: bool,
) -> bool {
    if !has_active_window {
        return false;
    }
    if notify_only_inactive {
        true
    } else {
        focused_current
    }
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
    /// 마지막 스냅샷의 `Needs You`(`Waiting` | `Blocked`) 세션 수.
    needs_you_count: usize,
    /// 마지막 스냅샷의 `Working` 세션 수.
    working_count: usize,
    /// 현재 Dock 배지 숫자(알림이 꺼지면 0).
    badge_count: usize,
    /// 알림음을 낸 횟수(테스트·상태 확인용, 실제 발음은 `cfg(test)`가 아닐 때만).
    beep_count: u64,
    /// 활성 `IOPMAssertion` 핸들.
    sleep_assertion: Option<SleepAssertion>,
    account_switch_sequence: Option<u64>,
}

impl Global for AlertState {}

fn ensure_state(cx: &mut App) {
    if cx.try_global::<AlertState>().is_none() {
        cx.set_app_identity(krill_proto::daemon::BUNDLE_ID, "Krill");
        cx.set_global(AlertState::default());
    }
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
/// 활성 창의 포커스 칸만 알림을 생략한다. 창이 없거나 비활성이면 알림을 보낸다(메뉴바 상주).
pub fn sync_snapshot(snapshot: &Snapshot, cx: &mut App) {
    ensure_state(cx);
    let cfg = settings::current(cx);
    // 창이 닫힌 뒤에도 `window_active`·`focused`가 stale하게 남을 수 있어 메뉴바 상주 알림까지
    // 막는다. 억제는 창이 있을 때만 적용한다.
    let has_window = !cx.windows().is_empty();
    let mut to_show: Vec<(SessionId, SystemNotification)> = Vec::new();
    let mut to_dismiss: Vec<SessionId> = Vec::new();

    {
        let state = cx.global_mut::<AlertState>();
        let initialized = state.initialized;
        let mut present = HashSet::with_capacity(snapshot.sessions.len());
        let mut needs_you = 0usize;
        let mut working = 0usize;

        for session in &snapshot.sessions {
            present.insert(session.id);
            let prev = state.seen.insert(session.id, session.status.state);
            match session.status.state {
                AgentState::Working => {
                    working += 1;
                    state.notified.remove(&session.id);
                    if state.active_tags.remove(&session.id) {
                        to_dismiss.push(session.id);
                    }
                }
                AgentState::Waiting | AgentState::Blocked => {
                    needs_you += 1;
                    let key = (session.status.state, session.status.since_unix_ns);
                    if state.notified.get(&session.id) != Some(&key) {
                        state.notified.insert(session.id, key);
                        if should_notify(&cfg, AlertEvent::InputWait)
                            && initialized
                            && !suppress_notification(
                                cfg.notify_only_inactive,
                                state.focused == Some(session.id),
                                state.window_active && has_window,
                            )
                        {
                            state.active_tags.insert(session.id);
                            to_show.push((session.id, build_notification(session)));
                        }
                    }
                }
                AgentState::Done => {
                    let key = (session.status.state, session.status.since_unix_ns);
                    // Working→Done뿐 아니라 Waiting·Blocked→Done(폴링 사이 전이)도 알린다.
                    // 같은 Done 스냅샷 반복은 notified 기록으로 걸러진다.
                    if prev.is_some_and(|old| old != AgentState::Done)
                        && state.notified.get(&session.id) != Some(&key)
                    {
                        state.notified.insert(session.id, key);
                        if should_notify(&cfg, AlertEvent::Done)
                            && initialized
                            && !suppress_notification(
                                cfg.notify_only_inactive,
                                state.focused == Some(session.id),
                                state.window_active && has_window,
                            )
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
    }

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
    apply_outputs(&settings::current(cx), cx);
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
    let cfg = settings::current(cx);
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

/// 알림음을 한 번 낸다. 횟수는 항상 세고, 실제 발음(`NSBeep`)은 단위 테스트가 아닐 때만 한다.
fn play_notification_sound(cx: &mut App) {
    ensure_state(cx);
    let state = cx.global_mut::<AlertState>();
    state.beep_count = state.beep_count.saturating_add(1);
    #[cfg(not(test))]
    crate::platform::beep();
}

/// 터미널 벨 알림(OWE-659). 이벤트 토글이 꺼져 있으면 조용하고, 이미 게시된
/// 알림이 있거나 보고 있는 칸의 벨이면 생략한다.
pub fn notify_bell(id: SessionId, label: &str, cx: &mut App) {
    ensure_state(cx);
    let cfg = settings::current(cx);
    if !should_notify(&cfg, AlertEvent::Bell) {
        return;
    }
    let has_window = !cx.windows().is_empty();
    let suppressed = {
        let state = cx.global::<AlertState>();
        state.active_tags.contains(&id)
            || suppress_notification(
                cfg.notify_only_inactive,
                state.focused == Some(id),
                state.window_active && has_window,
            )
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

/// 설정 창의 "테스트 알림 보내기"(OWE-659). 기존 발송 경로로 지금 바로 알림을
/// 하나 띄운다. 마스터 `notifications`가 꺼져 있으면 보내지 않는다. 식별자는
/// `"pane:"` 접두를 쓰지 않아 클릭해도 세션으로 이동하지 않는다.
pub fn send_test_notification(cx: &mut App) {
    ensure_state(cx);
    if !settings::current(cx).notifications {
        return;
    }
    crate::onboarding::actions::show_notification(
        SystemNotification {
            tag: "settings-test".into(),
            title: text::TEST_TITLE.get().into(),
            body: text::TEST_BODY.get().into(),
            actions: Vec::new(),
        },
        cx,
    );
}

/// 현재 `IOPMAssertion`으로 유휴 잠자기를 막고 있는지 돌려준다.
pub fn is_preventing_sleep(cx: &App) -> bool {
    cx.try_global::<AlertState>()
        .and_then(|s| s.sleep_assertion.as_ref())
        .is_some()
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
        match (want_awake, state.sleep_assertion.is_some()) {
            (true, false) => {
                state.sleep_assertion = SleepAssertion::acquire(SLEEP_REASON);
            }
            (false, true) => {
                state.sleep_assertion = None;
            }
            _ => {}
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

/// OWE-712: 데몬 이력으로 주기 틱·CLI 전환도 알리고 같은 이력은 한 번만 보낸다.
/// 최초 스냅샷은 기준점으로 삼아 앱 재시작 때 과거 알림을 재생하지 않는다.
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
    let cfg = settings::current(cx);
    if !cfg.notifications {
        return;
    }
    let active = !cx.windows().is_empty() && cx.global::<AlertState>().window_active;
    if cfg.notify_only_inactive && active {
        return;
    }
    for record in history.iter().filter(|r| r.sequence > previous) {
        let event = &record.event;
        let body = crate::i18n::catalog::popout::AUTO_SWITCH_NOTICE.format(&[
            ("provider", event.provider.display_name()),
            ("from", &event.from_label),
            ("pct", &event.reason_percent.to_string()),
            ("to", &event.to_label),
        ]);
        crate::onboarding::actions::show_notification(
            SystemNotification {
                tag: format!("account-switch:{}", record.sequence).into(),
                title: crate::i18n::catalog::shell::ACCOUNT_POLICY.get().into(),
                body: body.into(),
                actions: Vec::new(),
            },
            cx,
        );
    }
}
