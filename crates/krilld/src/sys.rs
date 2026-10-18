//! krilld의 unsafe 전부: libc FFI와 커널 이벤트 대기. OS별 구현은 `macos`·`linux` 하위 모듈이다.
#![expect(
    unsafe_code,
    reason = "getpeereid·LOCAL_PEERPID·proc_pidinfo·proc_listallpids·getpwuid_r은 libc에만 있다(rustix의 피어 자격 조회는 Linux 전용, T2-C2. passwd·libproc 조회는 rustix에 없음). rustix kevent는 unsafe API다"
)]

use std::ffi::{CStr, c_char};
use std::io;
use std::mem::MaybeUninit;
use std::ptr;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;

/// UTC cron calendar fields, independent of process timezone and daylight saving.
pub(crate) fn cron_calendar(seconds: u64) -> io::Result<[u32; 5]> {
    let timestamp = libc::time_t::try_from(seconds).map_err(io::Error::other)?;
    let mut calendar = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: timestamp is initialized and calendar points to writable tm storage.
    if unsafe { libc::gmtime_r(&raw const timestamp, calendar.as_mut_ptr()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful gmtime_r initialized every tm field.
    let calendar = unsafe { calendar.assume_init() };
    [
        calendar.tm_min,
        calendar.tm_hour,
        calendar.tm_mday,
        calendar.tm_mon.saturating_add(1),
        calendar.tm_wday,
    ]
    .map(u32::try_from)
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .map_err(io::Error::other)?
    .try_into()
    .map_err(|_| io::Error::other("invalid UTC calendar"))
}

/// `LOCAL_PEERTOKEN`이 돌려주는 감사 토큰 바이트 수(`audit_token_t` 8×u32).
pub const AUDIT_TOKEN_LEN: usize = 32;

/// UDS 피어의 코드서명 검증 결과(MK-096). 서명(ad hoc 포함)이 있고 식별자가 읽히면 유효하다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSign {
    pub valid: bool,
    pub identifier: Option<String>,
    pub team: Option<String>,
}

impl CodeSign {
    /// 검증 실패·미서명. 전체 권한을 주지 않는다.
    pub fn invalid() -> Self {
        Self {
            valid: false,
            identifier: None,
            team: None,
        }
    }
}

/// 현재 사용자의 passwd 항목. login(1)이 HOME·USER·LOGNAME·SHELL을 채우는 출처와 같다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: Vec<u8>,
    pub home: Vec<u8>,
    pub shell: Vec<u8>,
}

/// `getpwuid_r(getuid())`. 버퍼가 모자라면(ERANGE) 1MiB까지 늘려 다시 묻는다.
pub fn user() -> io::Result<User> {
    let uid = rustix::process::getuid().as_raw();
    let mut buf = vec![0 as c_char; 1_024];
    let mut pw = MaybeUninit::<libc::passwd>::uninit();
    loop {
        let mut found: *mut libc::passwd = ptr::null_mut();
        // SAFETY: `pw`·`buf`·`found`는 이 함수가 가진 유효한 쓰기 위치이고 `buf.len()`이 `buf`의 실제 길이다.
        let r = unsafe {
            libc::getpwuid_r(
                uid,
                pw.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut found,
            )
        };
        match r {
            0 if found.is_null() => return Err(io::ErrorKind::NotFound.into()),
            0 => break,
            libc::ERANGE if buf.len() < 1 << 20 => buf.resize(buf.len().saturating_mul(2), 0),
            e => return Err(io::Error::from_raw_os_error(e)),
        }
    }
    // SAFETY: getpwuid_r이 0을 돌려주고 `found`가 null이 아니면 `pw`를 채운 것이다.
    let pw = unsafe { pw.assume_init_ref() };
    // SAFETY: 세 필드 모두 getpwuid_r이 채운 값이고 `buf`는 아직 살아 있다.
    let name = unsafe { field(pw.pw_name) };
    // SAFETY: 위와 같다.
    let home = unsafe { field(pw.pw_dir) };
    // SAFETY: 위와 같다.
    let shell = unsafe { field(pw.pw_shell) };
    Ok(User { name, home, shell })
}

/// passwd 문자열 필드를 복사한다(빈 값은 null일 수 있다).
///
/// # Safety
///
/// `p`는 null이거나, 읽는 동안 살아 있는 NUL 종료 문자열을 가리켜야 한다.
unsafe fn field(p: *const c_char) -> Vec<u8> {
    if p.is_null() {
        return Vec::new();
    }
    // SAFETY: 호출자 계약대로 null이 아닌 `p`는 살아 있는 NUL 종료 문자열이다.
    unsafe { CStr::from_ptr(p) }.to_bytes().to_vec()
}

/// 활성 비루프백 IPv4 인터페이스. getifaddrs 목록은 이 함수 안에서만 빌리고 반드시 반환한다.
pub(crate) fn ipv4_interfaces() -> io::Result<Vec<(String, std::net::Ipv4Addr)>> {
    let mut head = ptr::null_mut();
    // SAFETY: head는 getifaddrs가 목록 포인터를 쓸 유효한 위치다.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut cursor = head;
    let mut out = Vec::new();
    while !cursor.is_null() {
        // SAFETY: 성공한 getifaddrs 목록이며 freeifaddrs 전까지 유효하다.
        let entry = unsafe { &*cursor };
        if !entry.ifa_addr.is_null()
            && !entry.ifa_name.is_null()
            && entry.ifa_flags & u32::try_from(libc::IFF_UP).unwrap_or(0) != 0
        {
            // SAFETY: ifa_addr는 해당 목록 노드에 속한 유효한 sockaddr다.
            let address = unsafe { &*entry.ifa_addr };
            if i32::from(address.sa_family) == libc::AF_INET {
                // SAFETY: AF_INET 주소이므로 sockaddr_in 크기·정렬을 갖는다.
                let ipv4 = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
                let ip = std::net::Ipv4Addr::from(ipv4.sin_addr.s_addr.to_ne_bytes());
                if !ip.is_loopback() && !ip.is_unspecified() {
                    // SAFETY: ifa_name은 freeifaddrs 전까지 유효한 NUL 종료 문자열이다.
                    let name = unsafe { CStr::from_ptr(entry.ifa_name) }
                        .to_string_lossy()
                        .into_owned();
                    out.push((name, ip));
                }
            }
        }
        cursor = entry.ifa_next;
    }
    // SAFETY: head는 이번 getifaddrs 호출이 반환했고 아직 해제하지 않았다.
    unsafe { libc::freeifaddrs(head) };
    Ok(out)
}

/// PTY 마스터의 전경 프로세스 그룹 ID(`tcgetpgrp`). 그룹이 없거나(0) 실패하면 `None`.
pub(crate) fn tcgetpgrp(fd: std::os::fd::BorrowedFd<'_>) -> Option<u32> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `fd`는 호출 동안 살아 있는 유효한 파일 디스크립터다.
    let pgrp = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
    (pgrp > 0).then(|| u32::try_from(pgrp).ok()).flatten()
}
