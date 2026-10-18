//! macOS 밖의 no-op 구현(M11 Windows 포팅). 시그니처는 `macos.rs`와 같다. Win32 실구현은 OWE-834.

use gpui::{AnyWindowHandle, App, Window};

/// macOS는 디스플레이 링크 틱을 건너뛰고 바로 그린다. 여기서는 gpui 다음 프레임에 맡긴다.
pub fn draw_now(_handle: AnyWindowHandle, _cx: &App) {}

/// 플랫폼 IME 조합 버리기. 아직 없다.
pub fn discard_marked_text(_window: &Window) {}

/// 시스템 경고음. 아직 없다.
pub fn beep() {}

/// Dock 배지는 macOS 전용이다.
pub fn set_dock_badge(_label: Option<&str>) {}

/// Dock 배지는 macOS 전용이다.
pub fn dock_badge() -> Option<String> {
    None
}

/// Dock 배지는 macOS 전용이다.
pub fn sync_dock_tile(_label: Option<&str>) -> Option<String> {
    None
}

/// 유휴 잠자기 방지 핸들. 아직 잡지 못한다(`acquire`가 `None`).
#[derive(Debug)]
pub struct SleepAssertion(u32);

impl SleepAssertion {
    pub fn acquire(_reason: &str) -> Option<Self> {
        None
    }

    pub const fn id(&self) -> u32 {
        self.0
    }
}

/// Secure Keyboard Entry는 macOS 전용이다(설정 UI도 숨긴다). 적용하지 못했으므로 `false`.
pub fn set_secure_event_input(_enabled: bool) -> bool {
    false
}

pub fn is_secure_event_input_engaged() -> bool {
    false
}

pub fn is_system_secure_event_input_enabled() -> bool {
    false
}

/// 메뉴바 상태 항목은 macOS 전용이다. `StatusItem::new`가 `None`이라 모니터는 항목 없이 돈다.
pub mod menu_bar {
    use gpui::App;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Command {
        Open,
        Quit,
    }

    #[derive(Debug)]
    pub struct StatusItem(());

    impl StatusItem {
        pub fn new(_open_label: &str, _quit_label: &str, _cx: &App) -> Option<Self> {
            None
        }

        pub fn set_menu_labels(&self, _open: &str, _quit: &str) {}

        pub fn set_title(&self, _title: &str) {}

        pub fn take_command() -> Option<Command> {
            None
        }
    }
}
