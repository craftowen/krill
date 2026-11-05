//! macOS 밖의 구현(M11 Windows 포팅, OWE-834). 시그니처는 `macos.rs`와 같다. Win32 호출은 `cfg(windows)`의 [`win`]에만
//! 있고, 그 밖(Linux)은 no-op이다.

use std::ffi::OsStr;
use std::io;

use gpui::{AnyWindowHandle, App, Window};

/// Wait for the exact process opened before replacement, with a bounded kernel wait.
#[cfg(windows)]
pub fn wait_process_exit(pid: u32, timeout: std::time::Duration) -> io::Result<()> {
    win::wait_process_exit(pid, timeout)
}

/// 실행 중인 Explorer에게 프로그램을 대신 띄우게 한다(`IShellDispatch2::ShellExecute`). 새 프로세스는 Explorer의 자식이라
/// 이 앱이 갇힌 Job(작업 스케줄러 등 breakaway 금지)을 물려받지 않는다.
#[cfg(windows)]
#[expect(unsafe_code, reason = "Shell COM FFI in the platform boundary (U1)")]
mod explorer {
    use std::ffi::{OsStr, OsString, c_void};
    use std::io;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{RPC_E_CHANGED_MODE, SysAllocString, SysFreeString};
    use windows_sys::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{
        CSIDL_DESKTOP, SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
        ShellWindows,
    };
    use windows_sys::core::{BSTR, GUID, HRESULT};

    const IID_SHELL_WINDOWS: GUID = GUID::from_u128(0x85cb6900_4d95_11cf_960c_0080c7f4ee85);
    const IID_SERVICE_PROVIDER: GUID = GUID::from_u128(0x6d5140c1_7436_11ce_8034_00aa006009fa);
    const IID_SHELL_BROWSER: GUID = GUID::from_u128(0x000214e2_0000_0000_c000_000000000046);
    const IID_DISPATCH: GUID = GUID::from_u128(0x00020400_0000_0000_c000_000000000046);
    const IID_FOLDER_VIEW_DUAL: GUID = GUID::from_u128(0xe7a1af80_4d96_11cf_960c_0080c7f4ee85);
    const IID_SHELL_DISPATCH2: GUID = GUID::from_u128(0xa4c6892c_3ba9_11d2_9dea_00c04fb16162);

    // vtable 번호(Windows SDK shobjidl.h·shldisp.h·servprov.h 순서). IUnknown 0-2, IDispatch 3-6, IOleWindow 3-4.
    const QUERY_INTERFACE: usize = 0;
    const RELEASE: usize = 2;
    const SHELL_WINDOWS_FIND_WINDOW_SW: usize = 15;
    const SERVICE_PROVIDER_QUERY_SERVICE: usize = 3;
    const SHELL_BROWSER_QUERY_ACTIVE_SHELL_VIEW: usize = 15;
    const SHELL_VIEW_GET_ITEM_OBJECT: usize = 15;
    const FOLDER_VIEW_DUAL_APPLICATION: usize = 7;
    const SHELL_DISPATCH2_SHELL_EXECUTE: usize = 31;

    // vtable 메서드 서명.
    type Execute =
        unsafe extern "system" fn(*mut c_void, BSTR, Variant, Variant, Variant, Variant) -> HRESULT;
    type Find = unsafe extern "system" fn(
        *mut c_void,
        *const Variant,
        *const Variant,
        i32,
        *mut i32,
        i32,
        *mut *mut c_void,
    ) -> HRESULT;
    type Service = unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT;
    type Item =
        unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut *mut c_void) -> HRESULT;
    type Getter = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
    type Query = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
    type Release = unsafe extern "system" fn(*mut c_void) -> u32;

    const VT_EMPTY: u16 = 0;
    const VT_I4: u16 = 3;
    const VT_BSTR: u16 = 8;
    const SW_HIDE: usize = 0;

    /// `VARIANT` 배치(머리 8바이트 + 포인터 두 개 자리, x64 24바이트). 값으로 넘기는 인수라 크기가 ABI다.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Variant {
        vt: u16,
        reserved: [u16; 3],
        value: [usize; 2],
    }
    const _: () = assert!(size_of::<Variant>() == 8 + 2 * size_of::<usize>());

    impl Variant {
        const EMPTY: Self = Self::new(VT_EMPTY, 0);
        const fn new(vt: u16, value: usize) -> Self {
            Self {
                vt,
                reserved: [0; 3],
                value: [value, 0],
            }
        }
    }

    /// 소유한 COM 인터페이스 포인터(Drop에서 Release).
    struct Com(*mut c_void);

    impl Com {
        /// `slot`번 메서드 주소.
        fn method(&self, slot: usize) -> *const c_void {
            // SAFETY: self.0 is a live COM interface whose first field is the vtable pointer.
            let vtable = unsafe { *self.0.cast::<*const *const c_void>() };
            // SAFETY: callers only use slot numbers inside that interface's SDK-defined vtable.
            unsafe { *vtable.wrapping_add(slot) }
        }

        fn query(&self, iid: &GUID) -> io::Result<Self> {
            // SAFETY: slot 0 of every COM vtable is IUnknown::QueryInterface with this signature.
            let query: Query = unsafe { std::mem::transmute(self.method(QUERY_INTERFACE)) };
            let mut out = ptr::null_mut();
            // SAFETY: iid and out are live for the call.
            out_param(unsafe { query(self.0, iid, &raw mut out) }, out)
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            // SAFETY: slot 2 is IUnknown::Release; this releases the one reference Com owns.
            let release: Release = unsafe { std::mem::transmute(self.method(RELEASE)) };
            // SAFETY: self.0 is live and owned.
            unsafe { release(self.0) };
        }
    }

    fn out_param(hr: HRESULT, out: *mut c_void) -> io::Result<Com> {
        if hr < 0 || out.is_null() {
            return Err(io::Error::from_raw_os_error(hr));
        }
        Ok(Com(out))
    }

    /// 실패한 COM 단계 이름을 오류 앞에 붙인다(로그에서 어느 단계인지 보이게).
    fn step(name: &'static str) -> impl Fn(io::Error) -> io::Error {
        move |e| io::Error::new(e.kind(), format!("{name}: {e}"))
    }

    fn hresult(hr: HRESULT) -> io::Result<()> {
        if hr < 0 {
            return Err(io::Error::from_raw_os_error(hr));
        }
        Ok(())
    }

    /// `SysAllocString` 소유자.
    struct Bstr(BSTR);

    impl Bstr {
        fn new(text: &OsStr) -> io::Result<Self> {
            let wide: Vec<u16> = text.encode_wide().chain(Some(0)).collect();
            if wide.iter().rev().skip(1).any(|&c| c == 0) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            // SAFETY: wide is a NUL-terminated UTF-16 buffer that lives through the call.
            let raw = unsafe { SysAllocString(wide.as_ptr()) };
            if raw.is_null() {
                return Err(io::ErrorKind::OutOfMemory.into());
            }
            Ok(Self(raw))
        }

        fn variant(&self) -> Variant {
            Variant::new(VT_BSTR, self.0 as usize)
        }
    }

    impl Drop for Bstr {
        fn drop(&mut self) {
            // SAFETY: self.0 came from SysAllocString and is freed once.
            unsafe { SysFreeString(self.0) };
        }
    }

    /// `program args`를 `dir`에서 Explorer가 창 없이 띄우게 요청한다. 요청이 받아들여지면 `Ok`(프로세스 결과는 모른다).
    /// Explorer의 환경을 물려받으므로 이 프로세스의 `KRILL_*`(상태·소켓 폴더 등)은 cmd로 감싸 넘긴다.
    pub fn launch(program: &Path, args: &[&OsStr], dir: Option<&Path>) -> io::Result<()> {
        let env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| key.to_string_lossy().starts_with("KRILL_"))
            .collect();
        let line = cmd_line(program, args, &env)?;
        let cmd = Path::new(&std::env::var_os("SystemRoot").ok_or(io::ErrorKind::NotFound)?)
            .join(r"System32\cmd.exe");
        // SAFETY: initializes COM for this thread; a successful call is balanced below.
        let init = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if init < 0 && init != RPC_E_CHANGED_MODE {
            return Err(io::Error::from_raw_os_error(init));
        }
        let result = shell_execute(&cmd, &line, dir);
        if init >= 0 {
            // SAFETY: balances this function's successful CoInitializeEx.
            unsafe { CoUninitialize() };
        }
        result
    }

    fn shell_execute(program: &Path, args: &OsStr, dir: Option<&Path>) -> io::Result<()> {
        let shell = desktop_shell()?;
        let file = Bstr::new(program.as_os_str())?;
        let args = Bstr::new(args)?;
        let dir = dir.map(|d| Bstr::new(d.as_os_str())).transpose()?;
        // SAFETY: shell is IShellDispatch2; slot 31 is ShellExecute(BSTR, VARIANT×4) in shldisp.h.
        let execute: Execute =
            unsafe { std::mem::transmute(shell.method(SHELL_DISPATCH2_SHELL_EXECUTE)) };
        // SAFETY: the BSTRs outlive the call; VARIANTs are passed by value as the ABI requires.
        hresult(unsafe {
            execute(
                shell.0,
                file.0,
                args.variant(),
                dir.as_ref().map_or(Variant::EMPTY, Bstr::variant),
                Variant::EMPTY,
                Variant::new(VT_I4, SW_HIDE),
            )
        })
    }

    /// 바탕화면 창의 `IShellDispatch2`(Raymond Chen, "How can I launch an unelevated process from my elevated process").
    fn desktop_shell() -> io::Result<Com> {
        let mut out = ptr::null_mut();
        // SAFETY: SDK CLSID/IID constants and a writable output pointer.
        let windows = out_param(
            unsafe {
                CoCreateInstance(
                    &ShellWindows,
                    ptr::null_mut(),
                    CLSCTX_LOCAL_SERVER,
                    &IID_SHELL_WINDOWS,
                    &raw mut out,
                )
            },
            out,
        )
        .map_err(step("CoCreateInstance(ShellWindows)"))?;
        // SAFETY: windows is IShellWindows; slot 15 is FindWindowSW.
        let find: Find =
            unsafe { std::mem::transmute(windows.method(SHELL_WINDOWS_FIND_WINDOW_SW)) };
        let location = Variant::new(VT_I4, CSIDL_DESKTOP as usize);
        let (mut hwnd, mut out) = (0, ptr::null_mut());
        // SAFETY: VARIANT inputs and output pointers are live. No desktop window (Explorer not
        // running) returns S_FALSE with a null dispatch, which out_param rejects.
        let desktop = out_param(
            unsafe {
                find(
                    windows.0,
                    &raw const location,
                    &Variant::EMPTY,
                    SWC_DESKTOP,
                    &raw mut hwnd,
                    SWFO_NEEDDISPATCH,
                    &raw mut out,
                )
            },
            out,
        )
        .map_err(step("FindWindowSW"))?;
        let provider = desktop
            .query(&IID_SERVICE_PROVIDER)
            .map_err(step("IServiceProvider"))?;
        // SAFETY: provider is IServiceProvider; slot 3 is QueryService.
        let service: Service =
            unsafe { std::mem::transmute(provider.method(SERVICE_PROVIDER_QUERY_SERVICE)) };
        let mut out = ptr::null_mut();
        // SAFETY: GUID inputs and output pointer are live.
        let browser = out_param(
            unsafe {
                service(
                    provider.0,
                    &SID_STopLevelBrowser,
                    &IID_SHELL_BROWSER,
                    &raw mut out,
                )
            },
            out,
        )
        .map_err(step("QueryService"))?;
        // SAFETY: browser is IShellBrowser; slot 15 is QueryActiveShellView.
        let active: Getter =
            unsafe { std::mem::transmute(browser.method(SHELL_BROWSER_QUERY_ACTIVE_SHELL_VIEW)) };
        let mut out = ptr::null_mut();
        // SAFETY: output pointer is live.
        let view = out_param(unsafe { active(browser.0, &raw mut out) }, out)
            .map_err(step("QueryActiveShellView"))?;
        // SAFETY: view is IShellView; slot 15 is GetItemObject.
        let item: Item = unsafe { std::mem::transmute(view.method(SHELL_VIEW_GET_ITEM_OBJECT)) };
        let mut out = ptr::null_mut();
        // SAFETY: IID input and output pointer are live.
        let folder = out_param(
            unsafe { item(view.0, SVGIO_BACKGROUND as u32, &IID_DISPATCH, &raw mut out) },
            out,
        )
        .map_err(step("GetItemObject"))?
        .query(&IID_FOLDER_VIEW_DUAL)
        .map_err(step("IShellFolderViewDual"))?;
        // SAFETY: folder is IShellFolderViewDual; slot 7 is get_Application.
        let application: Getter =
            unsafe { std::mem::transmute(folder.method(FOLDER_VIEW_DUAL_APPLICATION)) };
        let mut out = ptr::null_mut();
        // SAFETY: output pointer is live.
        let dispatch = out_param(unsafe { application(folder.0, &raw mut out) }, out)
            .map_err(step("get_Application"))?;
        dispatch
            .query(&IID_SHELL_DISPATCH2)
            .map_err(step("IShellDispatch2"))
    }

    /// `cmd /s /c "set "K=V"&…&"program" "arg"…"`의 cmd 인수. 모든 단어를 CRT 규칙으로 따옴표에 넣어 cmd가
    /// `&`·`(` 같은 글자를 해석하지 않게 한다. 따옴표 안에서도 펼쳐지는 `%`와 줄바꿈·따옴표가 있으면 거부한다.
    fn cmd_line(
        program: &Path,
        args: &[&OsStr],
        env: &[(OsString, OsString)],
    ) -> io::Result<OsString> {
        let words = std::iter::once(program.as_os_str()).chain(args.iter().copied());
        let unsafe_char = |c: u16| [b'%', b'"', b'\r', b'\n', 0].map(u16::from).contains(&c);
        if words
            .clone()
            .chain(env.iter().flat_map(|(k, v)| [&**k, &**v]))
            .any(|w| w.encode_wide().any(unsafe_char))
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut line: Vec<u16> = "/d /v:off /s /c \"".encode_utf16().collect();
        for (key, value) in env {
            line.extend("set \"".encode_utf16());
            line.extend(key.encode_wide());
            line.push(u16::from(b'='));
            line.extend(value.encode_wide());
            line.extend("\"&".encode_utf16());
        }
        for (i, word) in words.enumerate() {
            if i > 0 {
                line.push(u16::from(b' '));
            }
            // 따옴표가 없으므로 CRT 규칙에서 남는 것은 닫는 따옴표 앞 역슬래시를 두 배로 하는 것뿐이다.
            let word: Vec<u16> = word.encode_wide().collect();
            let trailing = word
                .iter()
                .rev()
                .take_while(|&&c| c == u16::from(b'\\'))
                .count();
            line.push(u16::from(b'"'));
            line.extend(word);
            line.extend(std::iter::repeat_n(u16::from(b'\\'), trailing));
            line.push(u16::from(b'"'));
        }
        line.push(u16::from(b'"'));
        Ok(OsString::from_wide(&line))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cmd_line_quotes_every_word_and_forwards_env() {
            let env = [(
                OsString::from("KRILL_STATE_DIR"),
                OsString::from(r"C:\e2e\r2 & x\state"),
            )];
            let line = cmd_line(
                Path::new(r"C:\Users\John Smith\krill-holder.exe"),
                &[
                    OsStr::new("--bootstrap"),
                    OsStr::new(r"C:\a(b)\krilld.exe"),
                    OsStr::new(r"C:\s\"),
                ],
                &env,
            )
            .unwrap();
            assert_eq!(
                line.to_str().unwrap(),
                r#"/d /v:off /s /c "set "KRILL_STATE_DIR=C:\e2e\r2 & x\state"&"C:\Users\John Smith\krill-holder.exe" "--bootstrap" "C:\a(b)\krilld.exe" "C:\s\\"""#
            );
            assert!(cmd_line(Path::new(r"C:\%PATH%\x.exe"), &[], &[]).is_err());
            assert!(
                cmd_line(
                    Path::new("x.exe"),
                    &[],
                    &[(OsString::from("KRILL_A"), OsString::from("a\"b"))]
                )
                .is_err()
            );
        }
    }
}

#[cfg(windows)]
mod shell;

/// 이 앱이 갇힌 Job(작업 스케줄러 등) 밖에서 프로그램을 띄우도록 Explorer에 요청한다. 세션 데몬 bootstrap이
/// Job 거부로 끝났을 때 `krill_proto::daemon`이 부른다(`set_job_escape`). Explorer가 없거나 실패하면 오류.
#[cfg(windows)]
pub fn launch_outside_job(
    program: &std::path::Path,
    args: &[&OsStr],
    dir: Option<&std::path::Path>,
) -> io::Result<()> {
    explorer::launch(program, args, dir)
        .inspect_err(|error| tracing::warn!(%error, "Explorer could not start the session daemon"))
}

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

/// `NSPasteboard` 직접 이미지 추출은 macOS 전용이다.
#[must_use]
pub fn read_pasteboard_image(_prefer_over_text: bool) -> Option<(gpui::ImageFormat, Vec<u8>)> {
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

    pub(super) fn wait_process_exit(pid: u32, timeout: std::time::Duration) -> io::Result<()> {
        use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
        use windows_sys::Win32::Foundation::{
            ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        if pid == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        // SAFETY: opens a synchronization-only handle to the supplied PID; no inheritance.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER.cast_signed()) {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: OpenProcess returned a new exclusively owned handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        // Never pass INFINITE even if a caller supplies an enormous duration.
        let millis = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX - 1)
            .min(u32::MAX - 1);
        // SAFETY: handle remains owned throughout the bounded wait.
        match unsafe { WaitForSingleObject(handle.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "old app did not exit; replacement aborted",
            )),
            _ => Err(io::Error::last_os_error()),
        }
    }

    #[cfg(test)]
    #[test]
    fn process_wait_distinguishes_exit_timeout_and_invalid_pid() {
        use std::time::Duration;
        assert_eq!(
            wait_process_exit(0, Duration::ZERO).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            wait_process_exit(std::process::id(), Duration::ZERO)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let command = std::env::var_os("SystemRoot").unwrap();
        let mut child = crate::platform::background_command(
            // cmd.exe는 argv[0]까지 스위치로 훑는다. `/`가 섞인 경로면 "The syntax of the command is incorrect"로 끝난다.
            std::path::Path::new(&command).join(r"System32\cmd.exe"),
        )
        .args(["/D", "/C", "exit /b 0"])
        .spawn()
        .unwrap();
        wait_process_exit(child.id(), Duration::from_secs(3)).unwrap();
        assert!(child.wait().unwrap().success());
    }

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
pub use shell::instance;
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
