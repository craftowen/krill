//! MK-087 네이티브 재현: 메뉴바·아이콘 생성, 마지막 창 닫힘 후 상태 변경, 일반 모드 복귀.
//! 실제 데몬·설정·에이전트 CLI를 사용하지 않는다. 시스템 알림 배너는 서명 번들에서 별도 확인한다.

use std::io;
use std::process::ExitCode;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use gpui::{App, AppContext as _, QuitMode, WindowOptions};
use krill_app::menu_bar::{self, Counts};
use krill_app::sidebar::{Session, Snapshot, Source};
use krill_proto::{
    SessionId,
    control::{AgentState, AgentStatus},
};

struct Memory(Arc<Mutex<AgentState>>);

impl Source for Memory {
    fn fetch(&self) -> io::Result<Snapshot> {
        let state = *self.0.lock().map_err(|_| io::ErrorKind::Other)?;
        Ok(Snapshot {
            sessions: vec![Session {
                id: SessionId(452),
                status: AgentStatus {
                    state,
                    agent: "claude".into(),
                    ..AgentStatus::UNKNOWN
                },
                worktree: None,
                cwd: None,
                account: None,
            }],
            ..Snapshot::default()
        })
    }
}

fn main() -> ExitCode {
    let passed = Arc::new(AtomicBool::new(false));
    let result = passed.clone();
    gpui_platform::application().with_assets(gpui_kit_assets::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        krill_app::settings::init_at(None, cx);
        cx.set_quit_mode(QuitMode::Explicit);
        // 같은 네이티브 생성자를 먼저 검사한다. 아이콘 생성 실패도 None으로 반환된다.
        let Some(probe) = krill_app::platform::menu_bar::StatusItem::new("창 열기", "완전 종료", cx) else {
            eprintln!("MK-087 native status item/icon creation failed");
            cx.quit();
            return;
        };
        probe.set_title("MK-087 재현");
        drop(probe);
        let state = Arc::new(Mutex::new(AgentState::Working));
        menu_bar::init(Arc::new(Memory(state.clone())), |_, _| {}, cx);
        let Ok(window) = cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty)) else {
            cx.quit();
            return;
        };
        cx.spawn(async move |cx| {
            let bg = cx.background_executor().clone();
            bg.timer(Duration::from_millis(700)).await;
            let initial = cx.update(|cx| menu_bar::counts(cx));
            window.update(cx, |_, window, _| window.remove_window()).ok();
            if let Ok(mut state) = state.lock() { *state = AgentState::Waiting; }
            bg.timer(Duration::from_millis(700)).await;
            let (closed, waiting) = cx.update(|cx| (cx.windows().is_empty(), menu_bar::counts(cx)));
            let reopened = cx.update(|cx| {
                let opened = cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty)).is_ok();
                menu_bar::sync_window_mode(cx);
                opened
            });
            let ok = initial == Some(Counts { working: 1, waiting: 0 })
                && closed && waiting == Some(Counts { working: 0, waiting: 1 }) && reopened;
            println!("MK-087 native item/icon=ok initial={initial:?} closed={closed} waiting={waiting:?} reopened={reopened}");
            result.store(ok, Ordering::Relaxed);
            cx.update(|cx| cx.quit());
        }).detach();
    });
    if passed.load(Ordering::Relaxed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
