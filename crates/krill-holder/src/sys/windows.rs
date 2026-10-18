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
        .map(|v| owned(usize::try_from(v).map_err(|_| io::ErrorKind::InvalidData)? as HANDLE))
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
