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
    // SAFETY: loads a shared system icon, requiring no DestroyIcon.
    data.hIcon = unsafe { LoadIconW(ptr::null_mut(), IDI_APPLICATION) };
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
