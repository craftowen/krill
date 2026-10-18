//! 창 수명과 독립된 상태·알림 모니터(MK-087).
//!
//! 잠정 결정 의존: MK-044/MK-016의 본 앱 단일 알림 발신자. 마지막 창을 닫아도 본 앱이
//! accessory 모드로 남는다. 명시적 종료·크래시 뒤의 알림은 보장하지 않는다.

use std::sync::Arc;
use std::time::Duration;

use gpui::{ActivationPolicy, App, Global, Task};
use krill_proto::{SessionId, control::AgentState};

use crate::platform::menu_bar::{Command, StatusItem};
use crate::sidebar::{Snapshot, Source};

use crate::i18n::catalog::menu_bar as text;

/// 메뉴바에 표시하는 전체 세션 수. 대기는 입력·권한 승인 대기를 합친다.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub working: usize,
    pub waiting: usize,
}

impl Counts {
    fn from_snapshot(snapshot: &Snapshot) -> Self {
        let mut counts = Self::default();
        for session in &snapshot.sessions {
            match session.status.state {
                AgentState::Working => counts.working += 1,
                AgentState::Waiting | AgentState::Blocked => counts.waiting += 1,
                _ => {}
            }
        }
        counts
    }

    fn title(self) -> String {
        format!(
            "{} {} · {} {}",
            text::WORKING.get(),
            self.working,
            text::WAITING.get(),
            self.waiting
        )
    }
}

struct Monitor {
    item: Option<StatusItem>,
    counts: Option<Counts>,
    connected: bool,
    _poll: Task<()>,
    _commands: Option<Task<()>>,
}

impl Global for Monitor {}

/// 창에서 별도 알림 폴링·클릭 핸들러를 등록하지 않도록 한다.
pub fn is_monitoring(cx: &App) -> bool {
    cx.has_global::<Monitor>()
}

/// 마지막 창을 닫으면 Dock에서 숨겨 메뉴바에 남는다. 다시 열면 일반 앱으로 돌아온다.
pub fn sync_window_mode(cx: &mut App) {
    let policy = if cx.windows().is_empty()
        && cx.try_global::<Monitor>().is_some_and(|m| m.item.is_some())
    {
        ActivationPolicy::Accessory
    } else {
        ActivationPolicy::Regular
    };
    cx.set_activation_policy(policy);
}

/// 전역 모니터를 한 번 시작한다. `open`은 필요하면 창을 만들고, 세션이 있으면 해당 pane을 연다.
/// 테스트는 외부 IO 없는 `Source`와 같은 진입점을 쓴다.
pub fn init(
    source: Arc<dyn Source>,
    open: impl Fn(Option<SessionId>, &mut App) + 'static,
    cx: &mut App,
) {
    if is_monitoring(cx) {
        return;
    }
    let open = std::rc::Rc::new(open);
    let notification_open = open.clone();
    cx.on_system_notification_response(move |response, cx| {
        if let Some(id) = crate::alert::parse_pane_tag(response.tag.as_ref()) {
            crate::alert::dismiss(id, cx);
            notification_open(Some(id), cx);
        }
    });
    let item = StatusItem::new(text::OPEN.get(), text::QUIT.get(), cx);
    if let Some(item) = &item {
        item.set_title(text::DISCONNECTED.get());
    } else if cfg!(target_os = "macos") && !cx.is_test() {
        tracing::warn!("menu bar unavailable; keeping Dock access when windows close");
    }
    let poll = cx.spawn(async move |cx| {
        let bg = cx.background_executor().clone();
        loop {
            let source = source.clone();
            let snapshot = bg.spawn(async move { source.fetch() }).await;
            cx.update(|cx| apply(snapshot.as_ref().ok(), cx));
            bg.timer(Duration::from_millis(500)).await;
        }
    });
    // 명령은 메뉴바 항목에서만 온다. 항목이 없으면(Windows) 100ms 깨우기를 돌리지 않는다(유휴 CPU, OWE-834).
    let commands = item.is_some().then(|| {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                cx.update(|cx| match StatusItem::take_command() {
                    Some(Command::Open) => open(None, cx),
                    // 셸의 종료 경로가 미저장 파일을 묻는다(OWE-663). 셸 없는 앱(테스트)은 바로 끝낸다.
                    Some(Command::Quit) => match cx.build_action("shell::Quit", None) {
                        Ok(quit) => cx.dispatch_action(quit.as_ref()),
                        Err(_) => cx.quit(),
                    },
                    None => {}
                });
            }
        })
    });
    cx.set_global(Monitor {
        item,
        counts: None,
        connected: false,
        _poll: poll,
        _commands: commands,
    });
    cx.on_window_closed(|cx, _| sync_window_mode(cx)).detach();
}

fn apply(snapshot: Option<&Snapshot>, cx: &mut App) {
    let next = snapshot.map(Counts::from_snapshot);
    let state = cx.global_mut::<Monitor>();
    if state.counts != next || state.connected != snapshot.is_some() {
        if let Some(item) = &state.item {
            let label = next.map_or_else(|| text::DISCONNECTED.get().to_owned(), Counts::title);
            item.set_title(&label);
        }
        state.counts = next;
        state.connected = snapshot.is_some();
    }
    if let Some(snapshot) = snapshot {
        crate::alert::sync_snapshot(snapshot, cx);
    }
}

/// Refresh native menu labels even when session counts have not changed.
pub(crate) fn refresh_language(cx: &App) {
    let Some(state) = cx.try_global::<Monitor>() else {
        return;
    };
    if let Some(item) = &state.item {
        item.set_menu_labels(text::OPEN.get(), text::QUIT.get());
        let title = state
            .counts
            .map_or_else(|| text::DISCONNECTED.get().to_owned(), Counts::title);
        item.set_title(&title);
    }
}

/// 마지막 수신 상태. 연결이 끊기면 숫자를 숨긴다.
pub fn counts(cx: &App) -> Option<Counts> {
    cx.try_global::<Monitor>()?.counts
}

#[cfg(test)]
mod tests;
