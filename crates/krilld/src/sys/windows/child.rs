//! Bounded command IO and lifetime; these jobs never contain holder/PTY sessions.

use std::io;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _, BorrowedHandle, OwnedHandle};
use std::process::Child;
use std::ptr;

use windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

/// None means EOF, zero means no data yet. Only the owning reader consumes this pipe.
pub(crate) fn pipe_available(pipe: BorrowedHandle<'_>) -> io::Result<Option<u32>> {
    let mut available = 0;
    // SAFETY: live borrowed pipe, writable count; no data is read or removed.
    if unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &raw mut available,
            ptr::null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_BROKEN_PIPE.cast_signed()) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    Ok(Some(available))
}

pub(crate) struct CommandJob(OwnedHandle);

impl CommandJob {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: creates an unnamed noninheritable job; only this object owns its handle.
        let handle = super::owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: valid job and correctly sized readonly limit information.
        if unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                u32::try_from(size_of_val(&info)).map_err(|_| io::ErrorKind::InvalidInput)?,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    /// Caller created this child suspended. Assign before resuming so helpers cannot escape.
    pub(crate) fn start(&self, child: &Child) -> io::Result<()> {
        // SAFETY: borrowed owned child; job contains only this command and its future descendants.
        if unsafe {
            AssignProcessToJobObject(self.0.as_raw_handle(), child.as_handle().as_raw_handle())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // std closes the initial thread handle. Locate that thread while the child is suspended.
        // SAFETY: snapshot request has no pointer arguments; the returned handle is newly owned.
        let snapshot = super::owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
        let mut entry = THREADENTRY32 {
            dwSize: u32::try_from(size_of::<THREADENTRY32>())
                .map_err(|_| io::ErrorKind::InvalidInput)?,
            ..THREADENTRY32::default()
        };
        // SAFETY: live snapshot and correctly sized writable entry.
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == child.id() {
                // SAFETY: thread belongs to our suspended, handle-owned child; no inheritance.
                let thread = super::owned(unsafe {
                    OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID)
                })?;
                // SAFETY: only the child initial thread is suspended and has never run user code.
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            // SAFETY: same live snapshot and writable entry.
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) };
        }
        Err(io::Error::other("cannot find suspended command thread"))
    }

    pub(crate) fn terminate(&self) {
        // SAFETY: this job owns only the command tree; session/holder jobs are never passed here.
        unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
    }
}
