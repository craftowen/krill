//! Read-only Restart Manager queries for processes holding one known file.
//! The registration lives across polls; no shutdown/restart API is called.

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources,
    RmStartSession,
};

pub(crate) struct FileUsers(u32);

impl Drop for FileUsers {
    fn drop(&mut self) {
        // SAFETY: this object exclusively owns a successful RmStartSession registration.
        unsafe { RmEndSession(self.0) };
    }
}

fn checked(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(i32::MAX),
        ))
    }
}

impl FileUsers {
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        let path = krill_proto::os::shell_path(path);
        if !path.is_absolute() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        wide.push(0);
        let mut id = 0;
        let mut key = vec![0u16; usize::try_from(CCH_RM_SESSION_KEY).unwrap_or(32) + 1];
        // SAFETY: both outputs are writable; key has the documented capacity including NUL.
        checked(unsafe { RmStartSession(&raw mut id, 0, key.as_mut_ptr()) })?;
        let session = Self(id);
        let files = [wide.as_ptr()];
        // SAFETY: one live NUL-terminated path; the other resource arrays have zero length.
        checked(unsafe {
            RmRegisterResources(id, 1, files.as_ptr(), 0, ptr::null(), 0, ptr::null())
        })?;
        Ok(session)
    }

    /// PID and creation time in the same microsecond epoch as `sys::start_us`.
    pub(crate) fn processes(&self) -> io::Result<Vec<(u32, u64)>> {
        let mut needed = 0;
        let mut reasons = 0;
        let mut rows = Vec::<RM_PROCESS_INFO>::new();
        for _ in 0..3 {
            let mut count = u32::try_from(rows.len()).map_err(|_| io::ErrorKind::InvalidData)?;
            let output = if rows.is_empty() {
                ptr::null_mut()
            } else {
                rows.as_mut_ptr()
            };
            // SAFETY: rows has count initialized entries (or null/count=0); scalar outputs are writable.
            let result = unsafe {
                RmGetList(
                    self.0,
                    &raw mut needed,
                    &raw mut count,
                    output,
                    &raw mut reasons,
                )
            };
            if result == ERROR_MORE_DATA {
                let length = usize::try_from(needed).map_err(|_| io::ErrorKind::InvalidData)?;
                if length > 4096 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                rows.resize(length, RM_PROCESS_INFO::default());
                continue;
            }
            checked(result)?;
            let count = usize::try_from(count).map_err(|_| io::ErrorKind::InvalidData)?;
            let rows = rows.get(..count).ok_or(io::ErrorKind::InvalidData)?;
            return Ok(rows
                .iter()
                .map(|row| {
                    let started = row.Process.ProcessStartTime;
                    let ticks =
                        u64::from(started.dwHighDateTime) << 32 | u64::from(started.dwLowDateTime);
                    (row.Process.dwProcessId, ticks / 10)
                })
                .collect());
        }
        Err(io::ErrorKind::WouldBlock.into())
    }
}
