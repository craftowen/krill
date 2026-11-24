//! Win32 desktop integration. All HWND/COM work stays on gpui's UI thread.
#![expect(unsafe_code, reason = "Win32/COM FFI in the platform boundary (U1)")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, HICON, IDI_APPLICATION, IDI_INFORMATION, IsWindow, LoadIconW, MF_STRING,
    PostMessageW, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};
use windows_sys::core::GUID;

type WindowState = (gpui::WindowId, bool, Option<String>);
thread_local! {
    static BADGE: RefCell<Option<String>> = const { RefCell::new(None) };
    static WINDOWS: RefCell<HashMap<usize, WindowState>> = RefCell::new(HashMap::new());
    static TRAY: RefCell<TrayState> = RefCell::new(TrayState::default());
}

pub(super) fn badge() -> Option<String> {
    BADGE.with_borrow(Clone::clone)
}

pub(super) fn set_badge(label: Option<&str>) {
    BADGE.with_borrow_mut(|old| *old = label.map(str::to_owned));
}

pub(super) fn sync_window(hwnd: HWND, id: gpui::WindowId, dark: bool) {
    let state = (id, dark, badge());
    let changed = WINDOWS.with_borrow_mut(|windows| {
        // SAFETY: IsWindow only validates an opaque handle; stale entries are discarded.
        windows.retain(|hwnd, _| unsafe { IsWindow(*hwnd as HWND) != 0 });
        windows.insert(hwnd as usize, state.clone()).as_ref() != Some(&state)
    });
    if !changed {
        return;
    }
    let value = i32::from(dark);
    // SAFETY: hwnd belongs to the current gpui window; value is a live BOOL-sized buffer.
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            ptr::from_ref(&value).cast(),
            4,
        )
    };
    if result < 0 {
        tracing::debug!(result, "native title bar appearance unavailable");
    }
    if !overlay(hwnd, state.2.as_deref()) {
        WINDOWS.with_borrow_mut(|windows| {
            windows.remove(&(hwnd as usize));
        });
    }
}

// ITaskbarList3 ABI from the Windows SDK shobjidl.h. windows-sys exposes the COM
// entry points but does not generate this interface. Unused vtable slots remain
// opaque; only HrInit, SetOverlayIcon and IUnknown::Release are called.
#[repr(C)]
struct TaskbarVtable {
    unknown: [*const c_void; 2],
    release: unsafe extern "system" fn(*mut Taskbar) -> u32,
    init: unsafe extern "system" fn(*mut Taskbar) -> i32,
    unused: [*const c_void; 14],
    overlay: unsafe extern "system" fn(*mut Taskbar, HWND, HICON, *const u16) -> i32,
}
#[repr(C)]
struct Taskbar {
    vtable: *const TaskbarVtable,
}
static TASKBAR_CLASS: GUID = GUID::from_u128(0x56fdf344_fd6d_11d0_958a_006097c9a090);
static TASKBAR_INTERFACE: GUID = GUID::from_u128(0xea1afb91_9e28_4b86_90e9_9e9f8a5eefaf);

fn overlay(hwnd: HWND, label: Option<&str>) -> bool {
    let mut applied = false;
    // SAFETY: initializes COM for this thread; successful calls are balanced below.
    let initialized = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    let mut object = ptr::null_mut();
    // SAFETY: CLSID/IID are SDK constants, object is a writable output pointer.
    let result = unsafe {
        CoCreateInstance(
            &raw const TASKBAR_CLASS,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &raw const TASKBAR_INTERFACE,
            &raw mut object,
        )
    };
    if result >= 0 && !object.is_null() {
        let object = object.cast::<Taskbar>();
        let description = wide(label.unwrap_or(""));
        // SAFETY: CoCreateInstance returned a live ITaskbarList3 pointer.
        let table = unsafe { (*object).vtable };
        // SAFETY: COM guarantees its vtable lives until the final Release.
        let vtable = unsafe { &*table };
        // SAFETY: object is the requested interface and init has the SDK signature.
        if unsafe { (vtable.init)(object) } >= 0 {
            let icon = if label.is_some() {
                // SAFETY: system resource identifier; returned icon is shared.
                unsafe { LoadIconW(ptr::null_mut(), IDI_INFORMATION) }
            } else {
                ptr::null_mut()
            };
            // SAFETY: hwnd is live and description is a NUL-terminated buffer.
            // SetOverlayIcon copies the icon, so no icon ownership passes to COM.
            let result = unsafe { (vtable.overlay)(object, hwnd, icon, description.as_ptr()) };
            applied = result >= 0;
            if result < 0 {
                tracing::debug!(result, "taskbar overlay unavailable");
            }
        }
        // SAFETY: releases exactly the reference returned by CoCreateInstance.
        unsafe { (vtable.release)(object) };
    }
    if initialized >= 0 {
        // SAFETY: balances exactly this function's successful CoInitializeEx call.
        unsafe { CoUninitialize() };
    }
    applied
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

const CALLBACK: u32 = WM_APP + 17;
#[derive(Default)]
struct TrayState {
    open: String,
    quit: String,
    title: String,
    command: Option<menu_bar::Command>,
    taskbar_created: u32,
}

fn notify(hwnd: HWND, title: &str, operation: u32) -> bool {
    let Ok(size) = u32::try_from(size_of::<NOTIFYICONDATAW>()) else {
        return false;
    };
    let mut data = NOTIFYICONDATAW {
        cbSize: size,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: CALLBACK,
        ..Default::default()
    };
    // Krill 아이콘은 exe 리소스 ID 1(packaging/windows/krill.rc). 없으면 시스템 기본 아이콘.
    // SAFETY: a null name returns this exe's module handle without taking a reference.
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    // SAFETY: LoadIconW with an integer resource id (MAKEINTRESOURCE(1)) returns a shared icon
    // needing no DestroyIcon, or null when the resource is missing.
    let krill = unsafe { LoadIconW(module, ptr::without_provenance(1)) };
    data.hIcon = if krill.is_null() {
        // SAFETY: loads a shared system icon, requiring no DestroyIcon.
        unsafe { LoadIconW(ptr::null_mut(), IDI_APPLICATION) }
    } else {
        krill
    };
    let mut length = 0;
    for character in title.chars() {
        if length + character.len_utf16() >= data.szTip.len() {
            break;
        }
        length += character.encode_utf16(&mut data.szTip[length..]).len();
    }
    // SAFETY: data is initialized, cbSize matches its ABI, HWND lives until Drop.
    unsafe { Shell_NotifyIconW(operation, &raw const data) != 0 }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // Never unwind through a Win32 callback, including RefCell borrow failures.
    std::panic::catch_unwind(|| process_message(hwnd, msg, w, l)).unwrap_or(0)
}

fn process_message(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let restart =
        TRAY.with_borrow(|state| state.taskbar_created != 0 && msg == state.taskbar_created);
    if restart {
        TRAY.with_borrow(|state| {
            notify(hwnd, &state.title, NIM_ADD);
        });
        WINDOWS.with_borrow_mut(HashMap::clear);
        return 0;
    }
    if msg == CALLBACK {
        match u32::try_from(l).unwrap_or(0) {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                TRAY.with_borrow_mut(|s| s.command = Some(menu_bar::Command::Open));
            }
            WM_RBUTTONUP | WM_CONTEXTMENU => {
                let (open, quit) = TRAY.with_borrow(|s| (wide(&s.open), wide(&s.quit)));
                // SAFETY: creates an owned popup menu on this UI thread.
                let menu = unsafe { CreatePopupMenu() };
                if menu.is_null() {
                    return 0;
                }
                // SAFETY: menu is live, label buffer is NUL-terminated.
                unsafe { AppendMenuW(menu, MF_STRING, 1, open.as_ptr()) };
                // SAFETY: same owned menu; quit buffer lives through the popup.
                unsafe { AppendMenuW(menu, MF_STRING, 2, quit.as_ptr()) };
                let mut point = POINT::default();
                // SAFETY: point is a valid writable POINT buffer.
                unsafe { GetCursorPos(&raw mut point) };
                // SAFETY: hwnd is this callback's hidden status window.
                unsafe { SetForegroundWindow(hwnd) };
                // SAFETY: owned menu, valid HWND and screen coordinates. TPM_RETURNCMD
                // returns the selected id instead of posting a command to an app view.
                let command = unsafe {
                    TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        point.x,
                        point.y,
                        0,
                        hwnd,
                        ptr::null(),
                    )
                };
                // SAFETY: this function owns the menu; TrackPopupMenu has returned.
                unsafe { DestroyMenu(menu) };
                // SAFETY: benign message to this callback's HWND, no borrowed pointers.
                unsafe { PostMessageW(hwnd, WM_NULL, 0, 0) };
                TRAY.with_borrow_mut(|s| {
                    s.command = match command {
                        1 => Some(menu_bar::Command::Open),
                        2 => Some(menu_bar::Command::Quit),
                        _ => None,
                    }
                });
            }
            _ => {}
        }
        return 0;
    }
    // SAFETY: forwards untouched callback arguments to Windows' default procedure.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

pub mod menu_bar {
    use super::{
        CreateWindowExW, DestroyWindow, GetModuleHandleW, HWND, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        RegisterClassW, RegisterWindowMessageW, TRAY, TrayState, WNDCLASSW, notify, ptr, wide,
        window_proc,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Command {
        Open,
        Quit,
    }

    #[derive(Debug)]
    pub struct StatusItem(HWND);

    impl StatusItem {
        pub fn new(open: &str, quit: &str, cx: &gpui::App) -> Option<Self> {
            if cx.is_test() {
                return None;
            }
            let class = wide("KrillStatusItem");
            let restart = wide("TaskbarCreated");
            // SAFETY: obtains a borrowed process-lifetime module handle.
            let instance = unsafe { GetModuleHandleW(ptr::null()) };
            let class_info = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            // SAFETY: class_info and its string live through registration; the callback
            // is static and panic-contained. An already registered class is reusable.
            unsafe { RegisterClassW(&raw const class_info) };
            // SAFETY: registered class and live module; the resulting hidden HWND is
            // owned by StatusItem and destroyed on this same UI thread.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    instance,
                    ptr::null(),
                )
            };
            if hwnd.is_null() {
                return None;
            }
            // SAFETY: restart is a NUL-terminated live UTF-16 buffer.
            let taskbar_created = unsafe { RegisterWindowMessageW(restart.as_ptr()) };
            TRAY.with_borrow_mut(|state| {
                *state = TrayState {
                    open: open.to_owned(),
                    quit: quit.to_owned(),
                    title: "Krill".into(),
                    taskbar_created,
                    command: None,
                }
            });
            if !notify(hwnd, "Krill", NIM_ADD) {
                // SAFETY: this is the newly created window owned by this function.
                unsafe { DestroyWindow(hwnd) };
                TRAY.with_borrow_mut(|state| *state = TrayState::default());
                return None;
            }
            Some(Self(hwnd))
        }
        #[expect(clippy::unused_self, reason = "same StatusItem API as macOS")]
        pub fn set_menu_labels(&self, open: &str, quit: &str) {
            TRAY.with_borrow_mut(|s| {
                open.clone_into(&mut s.open);
                quit.clone_into(&mut s.quit);
            });
        }
        pub fn set_title(&self, title: &str) {
            TRAY.with_borrow_mut(|s| s.title = format!("Krill · {title}"));
            TRAY.with_borrow(|s| {
                notify(self.0, &s.title, NIM_MODIFY);
            });
        }
        pub fn take_command() -> Option<Command> {
            TRAY.with_borrow_mut(|s| s.command.take())
        }
    }
    impl Drop for StatusItem {
        fn drop(&mut self) {
            notify(self.0, "", NIM_DELETE);
            // SAFETY: StatusItem owns this hidden HWND and drops on its UI thread.
            unsafe { DestroyWindow(self.0) };
            TRAY.with_borrow_mut(|state| *state = TrayState::default());
        }
    }
}

/// State-scoped Windows app reopening; native handles remain owned.
pub mod instance {
    use std::cell::RefCell;
    use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
    use std::time::Duration;

    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
    use windows_sys::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowW,
        GetWindowThreadProcessId, PostMessageW, RegisterClassW, WM_APP, WNDCLASSW,
    };

    use std::ptr;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    const CLASS: &str = "KrillInstance";
    const OPEN: u32 = WM_APP + 18;
    /// 기존 앱이 아직 창을 만들기 전일 수 있어 잠시 다시 찾는다.
    const FIND_TRIES: u32 = 50;
    const FIND_GAP: Duration = Duration::from_millis(100);

    thread_local! {
        static OPENS: RefCell<Option<UnboundedSender<()>>> = const { RefCell::new(None) };
    }

    /// 이 프로세스가 가진 실행 표지. 프로세스가 끝날 때까지 살려 둔다.
    #[derive(Debug)]
    pub struct Instance {
        _mutex: Option<OwnedHandle>,
        hwnd: HWND,
        opens: Option<UnboundedReceiver<()>>,
    }

    impl Instance {
        /// 다른 실행이 보낸 열기 요청. 한 번만 꺼낸다.
        pub fn take_opens(&mut self) -> Option<UnboundedReceiver<()>> {
            self.opens.take()
        }
    }

    impl Drop for Instance {
        fn drop(&mut self) {
            if !self.hwnd.is_null() {
                // SAFETY: this hidden HWND was created by `claim` on this same UI thread.
                unsafe { DestroyWindow(self.hwnd) };
                OPENS.with_borrow_mut(|opens| *opens = None);
            }
        }
    }

    /// UI 스레드(main)에서 gpui 시작 전에 부른다. `key`가 같은 앱이 이미 있고 `forward`면 그 앱에
    /// 열기를 넘기고 `None`(호출자는 종료)이다. 넘기지 못하면(응답 없는 앱) 그냥 실행한다.
    pub fn claim(key: &str, forward: bool) -> Option<Instance> {
        let name = wide(&format!("Local\\{key}"));
        // SAFETY: null security attributes; `name` is a live NUL-terminated buffer.
        let mutex = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        // SAFETY: reads this thread's last error immediately after CreateMutexW.
        let existed = !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = if mutex.is_null() {
            tracing::warn!("instance mutex unavailable");
            None
        } else {
            // SAFETY: CreateMutexW returned a unique owned non-null handle.
            Some(unsafe { OwnedHandle::from_raw_handle(mutex) })
        };
        if existed && forward {
            if forward_open(key) {
                return None;
            }
            tracing::warn!("running Krill did not accept open; starting another instance");
        }
        let (tx, rx) = unbounded_channel();
        // 표지만 열린 두 번째 실행은 듣지 않는다: 열기는 기존 앱 하나만 받는다.
        let hwnd = if existed {
            ptr::null_mut()
        } else {
            OPENS.with_borrow_mut(|opens| *opens = Some(tx));
            let hwnd = listen(key);
            if hwnd.is_null() {
                // 표지는 가졌지만 받을 창이 없다: 다음 실행은 5초 찾다가 따로 뜬다(중복 가능).
                tracing::warn!("instance window unavailable; relaunch cannot reopen this app");
            }
            hwnd
        };
        Some(Instance {
            _mutex: mutex,
            hwnd,
            opens: Some(rx),
        })
    }

    fn listen(key: &str) -> HWND {
        let class = wide(CLASS);
        let title = wide(key);
        // SAFETY: obtains a borrowed process-lifetime module handle.
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let info = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        // SAFETY: `info` and its class name live through registration; the callback is
        // static and panic-contained.
        unsafe { RegisterClassW(&raw const info) };
        // SAFETY: registered class and live buffers; the hidden top-level HWND is owned by
        // `Instance` and destroyed on this thread. Top-level (not message-only) so FindWindowW sees it.
        unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                module,
                ptr::null(),
            )
        }
    }

    fn forward_open(key: &str) -> bool {
        let class = wide(CLASS);
        let title = wide(key);
        for _ in 0..FIND_TRIES {
            // SAFETY: both arguments are live NUL-terminated buffers.
            let hwnd = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
            if !hwnd.is_null() {
                let mut pid = 0;
                // SAFETY: `pid` is a writable u32; a stale hwnd only returns 0.
                unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
                // 사용자가 방금 실행한 이 프로세스가 앞으로 가져오기 권한을 기존 앱에 넘긴다.
                // SAFETY: takes a plain process id, no pointers.
                unsafe { AllowSetForegroundWindow(pid) };
                // SAFETY: no pointers in the message; a stale hwnd makes the post fail.
                if unsafe { PostMessageW(hwnd, OPEN, 0, 0) } != 0 {
                    return true;
                }
            }
            std::thread::sleep(FIND_GAP);
        }
        false
    }

    unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // Never unwind through a Win32 callback, including RefCell borrow failures.
        std::panic::catch_unwind(|| {
            if msg == OPEN {
                OPENS.with_borrow(|opens| {
                    if let Some(opens) = opens {
                        opens.send(()).ok();
                    }
                });
                return 0;
            }
            // SAFETY: forwards untouched callback arguments to Windows' default procedure.
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        })
        .unwrap_or(0)
    }

    /// 실제 창 열기 수신(gpui 메시지 루프)은 Windows 실기·CI에서만 확인한다. 여기서는 표지 획득·해제와
    /// 같은 키 두 번째 실행의 전달, 다른 상태 폴더 키의 독립만 본다.
    #[cfg(test)]
    mod tests {
        use super::claim;

        #[test]
        fn second_launch_forwards_only_for_the_same_state_dir_and_owner_release_frees_it() {
            let key = format!("Krill-App-test-{}", std::process::id());
            let owner = claim(&key, true).expect("first launch owns the key");
            assert!(!owner.hwnd.is_null(), "owner listens for reopen");
            // 같은 상태 폴더: 기존 앱에 열기를 넘기고 끝난다.
            assert!(claim(&key, true).is_none());
            // 업데이트 health 확인 실행은 넘기지 않고 듣지도 않는다.
            let update = claim(&key, false).expect("update launch is never forwarded");
            assert!(update.hwnd.is_null());
            drop(update);
            // 다른 상태 폴더는 막지 않는다.
            let other = claim(&format!("{key}-other"), true).expect("other state dir");
            drop(other);
            drop(owner);
            // 기존 앱이 끝나면(업데이트 교체 포함) 다음 실행이 다시 주인이 된다.
            let next = claim(&key, true).expect("released key is claimable");
            assert!(!next.hwnd.is_null());
        }
    }
}
