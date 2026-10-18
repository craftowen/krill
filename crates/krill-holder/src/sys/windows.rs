//! Windows 핸들 소유권·프로세스·ConPTY FFI 경계.
#![expect(unsafe_code, reason = "Win32 핸들 소유권과 STARTUPINFOEX 호출 경계")]
use crate::link::Link;
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

pub const LINK_ENV: &str = "KRILL_HOLDER_LINK";
static TAKEN: AtomicBool = AtomicBool::new(false);
fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 呼出元は新規作成・複製された有効ハンドルの所有権を渡す。
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}
pub fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut r, mut w) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: 出力は有効な HANDLE ポインター、既定で非継承。
    cvt(unsafe { CreatePipe(&raw mut r, &raw mut w, std::ptr::null(), 0) })?;
    Ok((owned(r)?, owned(w)?))
}
fn cvt(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub fn available(pipe: BorrowedHandle<'_>) -> io::Result<u32> {
    let mut n = 0;
    // SAFETY: 借用したパイプと有効な出力ポインター、データは読み取らない。
    cvt(unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &raw mut n,
            std::ptr::null_mut(),
        )
    })?;
    Ok(n)
}
pub fn received_handles(values: Vec<u64>) -> io::Result<Vec<OwnedHandle>> {
    let mut seen = std::collections::HashSet::new();
    if values
        .iter()
        .any(|v| *v == 0 || usize::try_from(*v).is_err() || *v == u64::MAX || !seen.insert(*v))
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // 値は認証された親/子がこのプロセスに DuplicateHandle したもののみ。
    values
        .into_iter()
        .map(|v| {
            let h = owned(usize::try_from(v).map_err(|_| io::ErrorKind::InvalidData)? as HANDLE)?;
            // SAFETY: owned handle; removes inheritance before any descendant can be launched.
            cvt(unsafe {
                windows_sys::Win32::Foundation::SetHandleInformation(h.as_raw_handle(), 1, 0)
            })?;
            Ok(h)
        })
        .collect()
}
pub fn inherited_link() -> io::Result<Link> {
    if TAKEN.swap(true, Ordering::AcqRel) {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let value = std::env::var(LINK_ENV).map_err(|_| io::ErrorKind::NotFound)?;
    let vals = value
        .split(',')
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| io::ErrorKind::InvalidData.into())
        })
        .collect::<io::Result<Vec<_>>>()?;
    if vals.len() != 2 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut it = received_handles(vals)?.into_iter();
    Ok(Link::from_handles(
        it.next().ok_or(io::ErrorKind::InvalidData)?,
        it.next().ok_or(io::ErrorKind::InvalidData)?,
    ))
}
/// 送信失敗では受信先の複製を回収する。target は holder 自身が作った子だけ。
pub struct Remote<'a> {
    target: BorrowedHandle<'a>,
    values: Vec<u64>,
    committed: bool,
}
impl<'a> Remote<'a> {
    pub fn duplicate(
        target: BorrowedHandle<'a>,
        handles: &[BorrowedHandle<'_>],
    ) -> io::Result<Self> {
        let mut out = Self {
            target,
            values: Vec::new(),
            committed: false,
        };
        // SAFETY: 疑似ハンドルは現在のプロセスを表す。閉じない。
        let current = unsafe { GetCurrentProcess() };
        for h in handles {
            let mut remote = std::ptr::null_mut();
            // SAFETY: 両プロセスとローカルの借用ハンドルが呼出中有効。
            cvt(unsafe {
                DuplicateHandle(
                    current,
                    h.as_raw_handle(),
                    target.as_raw_handle(),
                    &raw mut remote,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            })?;
            out.values.push(remote as usize as u64);
        }
        Ok(out)
    }
    pub fn values(&self) -> &[u64] {
        &self.values
    }
    pub fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for Remote<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for v in &self.values {
            // SAFETY: 未送信の複製だけを元プロセス内で閉じる。ローカル複製は作らない。
            unsafe {
                DuplicateHandle(
                    self.target.as_raw_handle(),
                    usize::try_from(*v).unwrap_or_default() as HANDLE,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    DUPLICATE_CLOSE_SOURCE,
                );
            }
        }
    }
}

use crate::link::Pty;
use krill_proto::holder::WinSize;
use std::ffi::{OsStr, OsString, c_void};
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsHandle;
use std::ptr::{null, null_mut};
use windows_sys::Win32::System::Console::{self as winconsole, COORD, HPCON};
use windows_sys::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JobObjectAssociateCompletionPortInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_DUP_HANDLE, PROCESS_INFORMATION, ResumeThread,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> i32;
type Close = unsafe extern "system" fn(HPCON);
type Pack = unsafe extern "system" fn(HANDLE, HANDLE, HANDLE, *mut HPCON) -> i32;
#[derive(Clone, Copy, Debug)]
pub struct Api {
    create: Create,
    resize: Resize,
    close: Close,
    pack: Option<Pack>,
}
impl Api {
    pub fn load() -> Self {
        static API: std::sync::OnceLock<Api> = std::sync::OnceLock::new();
        *API.get_or_init(|| {
            Self::bundle().unwrap_or(Self {
                create: winconsole::CreatePseudoConsole,
                resize: winconsole::ResizePseudoConsole,
                close: winconsole::ClosePseudoConsole,
                pack: None,
            })
        })
    }
    fn bundle() -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().ok_or(io::ErrorKind::NotFound)?;
        if !dir.join("x64/OpenConsole.exe").is_file() {
            return Err(io::ErrorKind::NotFound.into());
        }
        let path = wide(dir.join("conpty.dll").as_os_str())?;
        // SAFETY: absolute NUL-terminated path; the module remains loaded for the process lifetime.
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        let get = |name: &[u8]| {
            // SAFETY: static NUL-terminated ASCII export name and a loaded module.
            unsafe { GetProcAddress(module, name.as_ptr()) }.ok_or(io::ErrorKind::NotFound)
        };
        // SAFETY: export signatures match conpty.h. LoadLibrary reference stays live.
        let create = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, Create>(get(
                b"ConptyCreatePseudoConsole\0",
            )?)
        };
        // SAFETY: export signature matches conpty.h.
        let resize = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, Resize>(get(
                b"ConptyResizePseudoConsole\0",
            )?)
        };
        // SAFETY: export signature matches conpty.h.
        let close = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, Close>(get(
                b"ConptyClosePseudoConsole\0",
            )?)
        };
        // SAFETY: export signature matches conpty.h.
        let pack = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, Pack>(get(
                b"ConptyPackPseudoConsole\0",
            )?)
        };
        Ok(Self {
            create,
            resize,
            close,
            pack: Some(pack),
        })
    }
    pub fn inbox(self) -> bool {
        self.pack.is_none()
    }
}
fn hr(code: i32) -> io::Result<()> {
    if code < 0 {
        Err(io::Error::other(format!("ConPTY HRESULT {code:#x}")))
    } else {
        Ok(())
    }
}
fn coord(size: WinSize) -> io::Result<COORD> {
    let c = COORD {
        X: i16::try_from(size.cols).map_err(|_| io::ErrorKind::InvalidInput)?,
        Y: i16::try_from(size.rows).map_err(|_| io::ErrorKind::InvalidInput)?,
    };
    if c.X <= 0 || c.Y <= 0 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(c)
}
#[derive(Debug)]
pub struct Console {
    handle: Option<HPCON>,
    api: Api,
    pub pty: Pty,
}
impl Console {
    pub fn create(api: Api, size: WinSize) -> io::Result<Self> {
        let (input, writer) = pipe()?;
        let (reader, output) = pipe()?;
        let mut h = 0;
        // SAFETY: pipe ends and output are live. Bundled flag 0x8 enables passthrough (OWE-826).
        hr(unsafe {
            (api.create)(
                coord(size)?,
                input.as_raw_handle(),
                output.as_raw_handle(),
                if api.inbox() { 0 } else { 8 },
                &raw mut h,
            )
        })?;
        // SAFETY: winconpty.h defines the ABI as signal/reference/conhost process handles.
        // Inbox layout remains an OS compatibility dependency (OWE-826); never use it for pack.
        let fields = unsafe { *(h as *const [HANDLE; 3]) };
        let copies = fields
            .into_iter()
            .map(|v| duplicate_raw(v, false))
            .collect::<io::Result<Vec<_>>>();
        let mut fields = match copies {
            Ok(v) => v.into_iter(),
            Err(e) => {
                // SAFETY: creation succeeded and ownership has not escaped.
                unsafe {
                    (api.close)(h);
                }
                return Err(e);
            }
        };
        let pty = Pty {
            input: writer,
            output: reader,
            signal: fields.next().ok_or(io::ErrorKind::InvalidData)?,
            reference: fields.next(),
            process: fields.next(),
        };
        Ok(Self {
            handle: Some(h),
            api,
            pty,
        })
    }
    pub fn resize(&self, size: WinSize) -> io::Result<()> {
        let h = self.handle.ok_or(io::ErrorKind::NotConnected)?;
        // SAFETY: this Console owns the live HPCON.
        hr(unsafe { (self.api.resize)(h, coord(size)?) })
    }
    pub fn stop_host(&self) -> io::Result<()> {
        let p = self.pty.process.as_ref().ok_or(io::ErrorKind::NotFound)?;
        // SAFETY: process is the conhost created for this Console, never an unrelated PID.
        cvt(unsafe { TerminateProcess(p.as_raw_handle(), 1) })
    }
    pub fn close(&mut self) {
        if let Some(h) = self.handle.take() {
            // SAFETY: exclusive owner; conhost has exited before this call to avoid output deadlock.
            unsafe {
                (self.api.close)(h);
            }
        }
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.stop_host().ok();
            self.close();
        }
    }
}

fn duplicate_raw(h: HANDLE, inherit: bool) -> io::Result<OwnedHandle> {
    let mut out = null_mut();
    // SAFETY: pseudo handle, no ownership is acquired.
    let current = unsafe { GetCurrentProcess() };
    // SAFETY: only locally owned live handles reach this private FFI helper.
    cvt(unsafe {
        DuplicateHandle(
            current,
            h,
            current,
            &raw mut out,
            0,
            i32::from(inherit),
            DUPLICATE_SAME_ACCESS,
        )
    })?;
    owned(out)
}
pub fn inheritable(h: BorrowedHandle<'_>) -> io::Result<OwnedHandle> {
    duplicate_raw(h.as_raw_handle(), true)
}
pub fn wide(s: &OsStr) -> io::Result<Vec<u16>> {
    let mut v: Vec<_> = s.encode_wide().collect();
    if v.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    v.push(0);
    Ok(v)
}
fn quote(s: &OsStr) -> Vec<u16> {
    let mut out = vec![34];
    let mut slash = 0;
    for ch in s.encode_wide() {
        if ch == 92 {
            slash += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            92,
            if ch == 34 { slash * 2 + 1 } else { slash },
        ));
        out.push(ch);
        slash = 0;
    }
    out.extend(std::iter::repeat_n(92, slash * 2));
    out.push(34);
    out
}
#[derive(Debug)]
pub struct Process {
    pub handle: OwnedHandle,
    pub pid: u32,
}
impl Process {
    pub fn status(&self) -> io::Result<Option<i32>> {
        // SAFETY: borrowed process handle, nonblocking wait.
        if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } != 0 {
            return Ok(None);
        }
        let mut code = 0;
        // SAFETY: live handle and writable DWORD output.
        cvt(unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &raw mut code) })?;
        Ok(Some(i32::from_ne_bytes(code.to_ne_bytes())))
    }
}
struct Attributes {
    buf: Vec<usize>,
}
impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: documented sizing query with null list.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &raw mut bytes);
        }
        let mut buf = vec![0; bytes.div_ceil(size_of::<usize>())];
        // SAFETY: pointer-aligned buffer of the requested size; only constructed on success.
        cvt(unsafe {
            InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), count, 0, &raw mut bytes)
        })?;
        let out = Self { buf };
        Ok(out)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: buffer holds a successfully initialized attribute list.
        unsafe {
            DeleteProcThreadAttributeList(self.buf.as_mut_ptr().cast());
        }
    }
}
pub fn spawn(
    path: &OsStr,
    args: &[OsString],
    env: &[(OsString, OsString)],
    cwd: Option<&OsStr>,
    console: Option<&Console>,
    inherit: &[BorrowedHandle<'_>],
    job: Option<&Job>,
) -> io::Result<Process> {
    let app = wide(path)?;
    let mut cmd = quote(path);
    for arg in args {
        cmd.push(32);
        cmd.extend(quote(arg));
    }
    if cmd.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    cmd.push(0);
    let mut env = env.to_vec();
    env.sort_by_cached_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (k, v) in env {
        let mut entry = k;
        entry.push("=");
        entry.push(v);
        block.extend(wide(&entry)?);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    let dir = cwd.map(wide).transpose()?;
    let raw: Vec<_> = inherit.iter().map(AsRawHandle::as_raw_handle).collect();
    let n = u32::from(console.is_some()) + u32::from(!raw.is_empty());
    let mut attr = (n != 0).then(|| Attributes::new(n)).transpose()?;
    let list = attr
        .as_mut()
        .map_or(null_mut(), |a| a.buf.as_mut_ptr().cast());
    if let Some(c) = console {
        // SAFETY: list is initialized, HPCON is live, attribute uses the value itself.
        cvt(unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                c.handle.ok_or(io::ErrorKind::NotConnected)? as *const c_void,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        })?;
    }
    if !raw.is_empty() {
        // SAFETY: exact list of inheritable handles, all borrowed for the creation call.
        cvt(unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                raw.as_ptr().cast(),
                size_of_val(raw.as_slice()),
                null_mut(),
                null(),
            )
        })?;
    }
    // SAFETY: STARTUPINFOEXW and PROCESS_INFORMATION have valid all-zero initial states.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb =
        u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(|_| io::ErrorKind::InvalidInput)?;
    si.lpAttributeList = list;
    // SAFETY: PROCESS_INFORMATION is an output-only C struct.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | if n != 0 {
            EXTENDED_STARTUPINFO_PRESENT
        } else {
            0
        }
        | if console.is_none() {
            DETACHED_PROCESS
        } else {
            0
        };
    // SAFETY: all buffers and attribute storage remain live, inheritance is limited by HANDLE_LIST.
    cvt(unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmd.as_mut_ptr(),
            null(),
            null(),
            i32::from(!raw.is_empty()),
            flags,
            block.as_ptr().cast(),
            dir.as_ref().map_or(null(), Vec::as_ptr),
            &raw const si.StartupInfo,
            &raw mut pi,
        )
    })?;
    let process = Process {
        handle: owned(pi.hProcess)?,
        pid: pi.dwProcessId,
    };
    let thread = owned(pi.hThread)?;
    let start = || {
        if let Some(job) = job {
            job.assign(&process)?;
        }
        // SAFETY: own initial thread, suspended until optional Job assignment completes.
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    };
    if let Err(e) = start() {
        terminate(&process).ok();
        return Err(e);
    }
    Ok(process)
}
pub fn child_link(path: &OsStr, args: &[OsString]) -> io::Result<(Process, Link)> {
    let (parent, child) = Link::pair()?;
    let input = inheritable(child.input.as_handle())?;
    let output = inheritable(child.output.as_handle())?;
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(k, _)| k != LINK_ENV && k != "KRILL_HOLDER_STATE")
        .collect();
    env.push((
        LINK_ENV.into(),
        format!(
            "{},{}",
            input.as_raw_handle() as usize,
            output.as_raw_handle() as usize
        )
        .into(),
    ));
    let p = spawn(
        path,
        args,
        &env,
        None,
        None,
        &[input.as_handle(), output.as_handle()],
        None,
    )?;
    Ok((p, parent))
}
pub fn duplicate_target(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: caller supplies only the pid from its own newly created child Process.
    owned(unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) })
}
#[derive(Debug)]
pub struct Job {
    pub handle: OwnedHandle,
    pub port: OwnedHandle,
}
impl Job {
    pub fn new() -> io::Result<Self> {
        // SAFETY: unnamed, noninheritable job; no KILL_ON_JOB_CLOSE or breakaway flags.
        let handle = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        // SAFETY: INVALID_HANDLE_VALUE creates a new IOCP.
        let port =
            owned(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1) })?;
        let info = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: null_mut(),
            CompletionPort: port.as_raw_handle(),
        };
        // SAFETY: correctly sized association structure and live job/port handles.
        cvt(unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectAssociateCompletionPortInformation,
                (&raw const info).cast(),
                u32::try_from(size_of_val(&info)).map_err(|_| io::ErrorKind::InvalidInput)?,
            )
        })?;
        Ok(Self { handle, port })
    }
    pub fn assign(&self, process: &Process) -> io::Result<()> {
        // SAFETY: own suspended child only, assigned before any code runs.
        cvt(unsafe {
            AssignProcessToJobObject(self.handle.as_raw_handle(), process.handle.as_raw_handle())
        })
    }
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: job contains only shells/descendants created for this session.
        cvt(unsafe { TerminateJobObject(self.handle.as_raw_handle(), 1) })
    }
    pub fn empty(&self) -> io::Result<bool> {
        let (mut code, mut key, mut overlapped) = (0, 0, null_mut());
        loop {
            // SAFETY: live IOCP, output pointers are valid, zero timeout.
            if unsafe {
                GetQueuedCompletionStatus(
                    self.port.as_raw_handle(),
                    &raw mut code,
                    &raw mut key,
                    &raw mut overlapped,
                    0,
                )
            } == 0
            {
                let e = io::Error::last_os_error();
                return if e.raw_os_error() == Some(258) {
                    Ok(false)
                } else {
                    Err(e)
                };
            }
            if code == 4
            /* JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO (winnt.h) */
            {
                return Ok(true);
            }
        }
    }
}
pub fn terminate(process: &Process) -> io::Result<()> {
    // SAFETY: newly created suspended child, used only to unwind failed creation.
    cvt(unsafe { TerminateProcess(process.handle.as_raw_handle(), 1) })
}
