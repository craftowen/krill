//! macOS 밖의 구현(M11 Windows 포팅, OWE-834). 시그니처는 `macos.rs`와 같다. Win32 호출은 `cfg(windows)`의 [`win`]에만
//! 있고, 그 밖(Linux)은 no-op이다.

use std::ffi::OsStr;
use std::io;

use gpui::{AnyWindowHandle, App, Window};

#[cfg(windows)]
mod shell;

/// Apply native title-bar appearance and the current attention overlay.
pub fn sync_window(window: &Window, dark: bool) {
    #[cfg(windows)]
    if let Some(hwnd) = win::hwnd(window) {
        shell::sync_window(hwnd, window.window_handle().window_id(), dark);
    }
    #[cfg(not(windows))]
    let _ = (window, dark);
}

/// macOS는 디스플레이 링크 틱을 건너뛰고 바로 그린다. 여기서는 gpui 다음 프레임에 맡긴다.
pub fn draw_now(_handle: AnyWindowHandle, _cx: &App) {}

/// 플랫폼 IME 쪽 조합을 버린다(Windows `ImmNotifyIME(NI_COMPOSITIONSTR, CPS_CANCEL)`). 위젯이 조합을 끊고 확정했을 때
/// 부른다 — 안 버리면 IME가 같은 글자를 나중에 다시 보낸다(MK-002). Win32 창이 아니면(테스트 플랫폼) 아무것도 안 한다.
pub fn discard_marked_text(window: &Window) {
    #[cfg(windows)]
    if let Some(hwnd) = win::hwnd(window) {
        win::cancel_composition(hwnd);
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// 시스템 경고음(터미널 벨, OWE-490). Windows는 `MessageBeep(MB_OK)`.
pub fn beep() {
    #[cfg(windows)]
    win::beep();
}

/// 알림음. Windows는 지금처럼 시스템 경고음을 쓴다.
pub fn play_notification_chime() {
    beep();
}

/// Windows uses a taskbar attention overlay; the full unread count stays in the app.
pub fn set_dock_badge(label: Option<&str>) {
    #[cfg(windows)]
    shell::set_badge(label);
    #[cfg(not(windows))]
    let _ = label;
}

pub fn dock_badge() -> Option<String> {
    #[cfg(windows)]
    return shell::badge();
    #[cfg(not(windows))]
    None
}

pub fn sync_dock_tile(label: Option<&str>) -> Option<String> {
    set_dock_badge(label);
    dock_badge()
}

/// 유휴 잠자기 방지 핸들(MK-044). Windows는 `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`로 잡고 Drop 시
/// `ES_CONTINUOUS`로 푼다. 이 상태는 부른 스레드에 묶이므로 잡고 놓는 쪽은 같은 스레드(`alert`의 전용 sleep assertion 작업 스레드)다.
/// 그 밖의 OS는 잡지 못한다(`acquire`가 `None`).
#[derive(Debug)]
pub struct SleepAssertion(u32);

impl SleepAssertion {
    pub fn acquire(_reason: &str) -> Option<Self> {
        #[cfg(windows)]
        return win::keep_awake(true).then_some(Self(1));
        #[cfg(not(windows))]
        None
    }

    pub const fn id(&self) -> u32 {
        self.0
    }
}

impl Drop for SleepAssertion {
    fn drop(&mut self) {
        #[cfg(windows)]
        win::keep_awake(false);
    }
}

/// 경로나 URL을 기본 앱으로 연다(Windows `ShellExecuteW` "open"). 폴더는 파일 탐색기로, `https:`·`vscode:` 같은 URL은
/// 등록된 처리기로 열린다. 그 밖의 OS는 `Unsupported`.
pub fn shell_open(target: &OsStr) -> io::Result<()> {
    #[cfg(windows)]
    return win::shell_open(target);
    #[cfg(not(windows))]
    {
        let _ = target;
        Err(io::ErrorKind::Unsupported.into())
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

/// TIFF→PNG `AppKit` 변환은 macOS 전용이다.
#[must_use]
pub fn tiff_to_png(_tiff: &[u8]) -> Option<Vec<u8>> {
    None
}

/// Win32 호출(CONVENTIONS U1: mk-app의 platform FFI 구역).
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Win32 IMM32·User32·Kernel32·Shell32 C FFI(CONVENTIONS U1)"
)]
mod win {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::ptr;

    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    use windows_sys::Win32::UI::Input::Ime::{
        CPS_CANCEL, ImmGetContext, ImmNotifyIME, ImmReleaseContext, NI_COMPOSITIONSTR,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_OK, SW_SHOWNORMAL};

    pub(super) fn preferred_locale() -> Option<String> {
        use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

        let mut count = 0;
        let mut length = 0;
        // SAFETY: count and length are writable u32 values; a null buffer with zero
        // length requests the required UTF-16 buffer size without writing language data.
        let ok = unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &raw mut count,
                std::ptr::null_mut(),
                &raw mut length,
            )
        };
        if ok == 0 || count == 0 || length == 0 {
            return None;
        }
        let mut buffer = vec![0; usize::try_from(length).ok()?];
        // SAFETY: buffer has length writable UTF-16 units and lives through the call;
        // count and length are valid output pointers, and length initially holds its capacity.
        let ok = unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &raw mut count,
                buffer.as_mut_ptr(),
                &raw mut length,
            )
        };
        if ok == 0 || count == 0 {
            return None;
        }
        super::super::locale::first_preferred_locale(buffer.get(..usize::try_from(length).ok()?)?)
    }

    pub(super) fn hwnd(window: &Window) -> Option<HWND> {
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
            _ => None,
        }
    }

    pub(super) fn cancel_composition(hwnd: HWND) {
        // SAFETY: hwnd는 지금 그리는 중인 gpui 창의 HWND다(`discard_marked_text`). 창이 없으면 NULL HIMC가 온다.
        let himc = unsafe { ImmGetContext(hwnd) };
        if himc.is_null() {
            return;
        }
        // SAFETY: himc는 위에서 받은 이 창의 입력 컨텍스트(널 아님). CPS_CANCEL은 조합 문자열만 비운다.
        unsafe { ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0) };
        // SAFETY: `ImmGetContext`로 받은 컨텍스트는 같은 HWND로 한 번 돌려줘야 한다.
        unsafe { ImmReleaseContext(hwnd, himc) };
    }

    pub(super) fn beep() {
        // SAFETY: 인자 하나짜리 User32 함수다. 실패해도(소리 장치 없음) 부작용이 없다.
        unsafe { MessageBeep(MB_OK) };
    }

    /// 켜면 이 스레드가 시스템 유휴 잠자기를 막고, 끄면 그 요청을 거둔다. 성공하면 `true`.
    pub(super) fn keep_awake(on: bool) -> bool {
        let flags = if on {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: 플래그 하나만 받는 Kernel32 함수다. 실패하면 0(이전 상태 없음)을 돌려준다.
        unsafe { SetThreadExecutionState(flags) != 0 }
    }

    pub(super) fn shell_open(target: &OsStr) -> io::Result<()> {
        if target.encode_wide().any(|c| c == 0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let file: Vec<u16> = target.encode_wide().chain(Some(0)).collect();
        let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        // SAFETY: verb·file은 이 호출 동안 살아 있는 NUL 종료 UTF-16 버퍼, 나머지 포인터는 NULL(소유 창·인자·작업 폴더
        // 없음)이다. 반환값은 HINSTANCE지만 32 이하면 오류 코드다(Win32 문서).
        let code = unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if code > 32 {
            Ok(())
        } else {
            Err(io::Error::other(format!("ShellExecuteW failed ({code})")))
        }
    }
}

#[cfg(windows)]
pub use shell::menu_bar;

#[cfg(not(windows))]
/// Linux has no status item.
pub mod menu_bar {
    use gpui::App;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Command {
        Open,
        Quit,
    }

    #[derive(Debug)]
    pub struct StatusItem(());

    #[expect(clippy::unused_self, reason = "macOS StatusItem과 같은 시그니처")]
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

pub(crate) fn preferred_locale() -> Option<String> {
    win::preferred_locale()
}
