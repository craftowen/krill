//! macOS 판: libproc·kqueue·Security.framework·APFS 호출.

use std::ffi::{c_int, c_void};
use std::io;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::ptr;

use core_foundation::base::{CFType, TCFType};
use core_foundation::data::CFData;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use security_framework::os::macos::code_signing::{Flags, GuestAttributes, SecCode};

use rustix::event::kqueue::{self, Event, EventFilter, EventFlags, ProcessEvents};
use rustix::process::Pid;

use super::{AUDIT_TOKEN_LEN, CodeSign};

/// 자기 프로세스의 물리 footprint와 누적 디스크 쓰기(자식·coalition 제외).
pub(crate) fn resources() -> io::Result<(u64, u64)> {
    let info = rusage(std::process::id())?;
    Ok((info.ri_phys_footprint, info.ri_diskio_byteswritten))
}

/// `pid`의 물리 footprint(`footprint(1)`의 값). 같은 UID 프로세스만 읽힌다.
pub(crate) fn footprint(pid: u32) -> io::Result<u64> {
    Ok(rusage(pid)?.ri_phys_footprint)
}

fn rusage(pid: u32) -> io::Result<libc::rusage_info_v4> {
    let pid = c_int::try_from(pid).map_err(io::Error::other)?;
    let mut info = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: info는 RUSAGE_INFO_V4에 맞는 크기의 쓰기 가능한 버퍼다.
    let result =
        unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, info.as_mut_ptr().cast()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 호출이 성공해 정수 필드 구조체 전체를 채웠다.
    Ok(unsafe { info.assume_init() })
}

/// 지금 있는 모든 프로세스의 PID(`proc_listallpids`).
pub(crate) fn all_pids() -> io::Result<Vec<u32>> {
    // 그 사이 늘어난 프로세스를 담도록 여유를 둔다. 꽉 차면 한 번 더 키운다.
    let mut cap = 4096_usize;
    loop {
        let mut pids: Vec<libc::pid_t> = vec![0; cap];
        let bytes = c_int::try_from(cap.saturating_mul(size_of::<libc::pid_t>()))
            .map_err(io::Error::other)?;
        // SAFETY: pids는 bytes 바이트의 쓰기 가능한 버퍼다.
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let n = usize::try_from(n).map_err(|_| io::Error::last_os_error())?;
        if n < cap || cap >= 1 << 20 {
            pids.truncate(n);
            return Ok(pids
                .into_iter()
                .filter_map(|p| u32::try_from(p).ok())
                .collect());
        }
        cap = cap.saturating_mul(4);
    }
}

/// `pid`의 직계 자식 PID(`proc_listchildpids`).
pub(crate) fn child_pids(pid: u32) -> io::Result<Vec<u32>> {
    let ppid = libc::pid_t::try_from(pid).map_err(io::Error::other)?;
    let mut pids: Vec<libc::pid_t> = vec![0; 64];
    let bytes = c_int::try_from(pids.len().saturating_mul(size_of::<libc::pid_t>()))
        .map_err(io::Error::other)?;
    // SAFETY: pids는 bytes 바이트의 쓰기 가능한 버퍼다.
    let n = unsafe { libc::proc_listchildpids(ppid, pids.as_mut_ptr().cast(), bytes) };
    let n = usize::try_from(n).map_err(|_| io::Error::last_os_error())?;
    pids.truncate(n);
    Ok(pids
        .into_iter()
        .filter_map(|p| u32::try_from(p).ok())
        .collect())
}

/// UDS 피어의 유효 UID(`getpeereid`).
pub fn peer_uid(sock: BorrowedFd<'_>) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `sock`은 빌린 동안 열려 있는 fd이고, `uid`·`gid`는 이 함수 스택의 유효한 쓰기 위치다.
    let r = unsafe { libc::getpeereid(sock.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if r == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// UDS 피어가 connect할 때의 PID(`getsockopt(SOL_LOCAL, LOCAL_PEERPID)`). 커널이 connect 시점에 기록한 값이다.
pub fn peer_pid(sock: BorrowedFd<'_>) -> io::Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = libc::socklen_t::try_from(size_of::<libc::pid_t>()).map_err(io::Error::other)?;
    // SAFETY: `sock`은 빌린 동안 열려 있는 fd이고, `pid`·`len`은 이 함수 스택의 유효한 쓰기 위치이며 `len`이 `pid`의
    // 크기다.
    let r = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast::<c_void>(),
            &raw mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(pid).map_err(io::Error::other)
}

// `security-framework-sys 2.17`에 없는 `SecCode` 서명 정보 조회와 NULL requirement 무결성 검사(MK-096). 아래는 Apple 공개 API다.
// `SecCodeCheckValidity`·`SecCodeCopySigningInformation`: `code`는 살아 있는 `SecCodeRef`, `info`는 유효한 쓰기 위치여야 한다.
// `kSecCodeInfo*`는 Security.framework의 상수 `CFString`이다.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCheckValidity(code: *const c_void, flags: u32, requirement: *const c_void) -> i32;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        info: *mut CFDictionaryRef,
    ) -> i32;
    static kSecCodeInfoIdentifier: CFStringRef;
    static kSecCodeInfoTeamIdentifier: CFStringRef;
}

/// `SecCodeCopySigningInformation`에 서명 정보 딕셔너리를 달라는 플래그(`kSecCSSigningInformation`, SecCode.h).
const SIGNING_INFORMATION: u32 = 1;

/// UDS 피어가 connect할 때 커널이 기록한 감사 토큰(`getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`).
/// PID와 달리 connect 시점에 고정돼 재사용 경쟁이 없다.
///
/// # Errors
///
/// 조회 실패(닫힌 피어 등).
pub fn peer_token(sock: BorrowedFd<'_>) -> io::Result<[u8; AUDIT_TOKEN_LEN]> {
    let mut token = [0u8; AUDIT_TOKEN_LEN];
    let mut len =
        libc::socklen_t::try_from(std::mem::size_of_val(&token)).map_err(io::Error::other)?;
    // SAFETY: `sock`은 빌린 동안 열려 있는 fd이고, `token`·`len`은 이 함수 스택의 유효한 쓰기 위치이며 `len`이
    // `token`의 크기다.
    let r = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast::<c_void>(),
            &raw mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(token)
}

/// 감사 토큰의 프로세스를 `SecCode` 손님으로 찾아 서명 정보(식별자·팀)를 읽는다.
/// 조회·검증이 조금이라도 실패하면 [`CodeSign::invalid`]다(거부는 이 값으로만 판단한다).
pub fn verify_audit_token(token: &[u8]) -> CodeSign {
    let data = CFData::from_buffer(token);
    let mut attrs = GuestAttributes::new();
    attrs.set_audit_token(data.as_concrete_TypeRef());
    let Ok(guest) = SecCode::copy_guest_with_attribues(None, &attrs, Flags::default()) else {
        return CodeSign::invalid();
    };
    // SAFETY: `guest`는 이 함수 안에서 살아 있는 `SecCodeRef`이고, `requirement`는 NULL(서명 자체의 동적 무결성 확인)이다.
    if unsafe { SecCodeCheckValidity(guest.as_concrete_TypeRef().cast(), 0, ptr::null()) } != 0 {
        return CodeSign::invalid();
    }
    let mut info: CFDictionaryRef = ptr::null_mut();
    // SAFETY: `guest`는 이 함수 안에서 살아 있는 `SecCodeRef`이고, `info`는 이 함수 스택의 유효한 쓰기 위치다.
    let status = unsafe {
        SecCodeCopySigningInformation(
            guest.as_concrete_TypeRef().cast(),
            SIGNING_INFORMATION,
            &raw mut info,
        )
    };
    if status != 0 || info.is_null() {
        return CodeSign::invalid();
    }
    // SAFETY: Copy라 +1 참조를 우리가 들고 있어 `drop`이 해제한다.
    let dict: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(info) };
    // SAFETY: `kSecCodeInfoIdentifier`는 Security.framework의 상수 `CFString`이다.
    let id_key: CFStringRef = unsafe { kSecCodeInfoIdentifier };
    // SAFETY: `id_key`는 살아 있는 상수 `CFString`이고 `dict`가 살아 있는 동안만 쓴다.
    let identifier = unsafe { signing_string(&dict, id_key) };
    // SAFETY: `kSecCodeInfoTeamIdentifier`는 Security.framework의 상수 `CFString`이다.
    let team_key: CFStringRef = unsafe { kSecCodeInfoTeamIdentifier };
    // SAFETY: `team_key`는 살아 있는 상수 `CFString`이고 `dict`가 살아 있는 동안만 쓴다.
    let team = unsafe { signing_string(&dict, team_key) };
    if identifier.is_none() {
        return CodeSign::invalid();
    }
    CodeSign {
        valid: true,
        identifier,
        team,
    }
}

/// 서명 정보 딕셔너리에서 `CFString` 값을 읽는다. 값의 소유자는 딕셔너리라 `drop`하지 않는다.
///
/// # Safety
///
/// `key`는 살아 있는 `CFString`이어야 하고, `dict`가 살아 있는 동안만 쓴다.
unsafe fn signing_string(
    dict: &CFDictionary<CFString, CFType>,
    key: CFStringRef,
) -> Option<String> {
    // SAFETY: 호출자 계약대로 `key`는 살아 있는 `CFString`이다. get 규칙이라 `drop` 대신 소유권을 놓는다.
    let key: CFString = unsafe { CFString::wrap_under_get_rule(key) };
    let key = ManuallyDrop::new(key);
    dict.find(&*key).and_then(|value| {
        let text: CFString = value.downcast()?;
        let out = text.to_string();
        // get 규칙: 소유자는 딕셔너리라 해제하지 않는다.
        let _text = ManuallyDrop::new(text);
        Some(out)
    })
}

/// 부모 PID(`proc_pidinfo(PROC_PIDT_SHORTBSDINFO)`). 다른 UID 프로세스(예: root `login`)도 조회된다.
pub fn parent_pid(pid: u32) -> io::Result<u32> {
    // SAFETY: `proc_bsdshortinfo`는 정수 필드만 있고 PROC_PIDT_SHORTBSDINFO의 출력 구조체다.
    let info = unsafe { pidinfo::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO)? };
    Ok(info.pbsi_ppid)
}

/// Current directory, rejecting PID reuse during the query.
pub(crate) fn current_directory(pid: u32) -> io::Result<PathBuf> {
    let start = start_us(pid)?;
    // SAFETY: proc_vnodepathinfo contains only integers and fixed character arrays,
    // and is the documented output for PROC_PIDVNODEPATHINFO.
    let info = unsafe { pidinfo::<libc::proc_vnodepathinfo>(pid, libc::PROC_PIDVNODEPATHINFO)? };
    if start_us(pid)? != start {
        return Err(io::ErrorKind::NotFound.into());
    }
    let bytes: Vec<_> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|&&c| c != 0)
        .map(|&c| c.to_ne_bytes()[0])
        .collect();
    if bytes.is_empty() {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// 프로세스 argv(`KERN_PROCARGS2`). 같은 UID 프로세스만 조회된다.
pub(crate) fn process_args(pid: u32) -> io::Result<Vec<String>> {
    let pid = c_int::try_from(pid).map_err(io::Error::other)?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: 출력 버퍼가 null이면 커널은 필요한 크기만 `size`에 쓴다.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            ptr::null_mut(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0_u8; size];
    // SAFETY: `buf`는 `size` 바이트짜리 쓰기 가능한 버퍼다.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast::<c_void>(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(size);
    parse_procargs2(&buf)
}

/// `KERN_PROCARGS2` 버퍼: `argc`(i32) · 실행 경로\0 · \0 패딩 · argv\0 × argc · 환경변수.
fn parse_procargs2(buf: &[u8]) -> io::Result<Vec<String>> {
    let (argc, rest) = buf
        .split_first_chunk::<4>()
        .ok_or(io::ErrorKind::InvalidData)?;
    let argc = usize::try_from(i32::from_ne_bytes(*argc)).map_err(io::Error::other)?;
    Ok(rest
        .split(|&b| b == 0)
        .skip(1)
        .skip_while(|s| s.is_empty())
        .take(argc)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// 프로세스 이름. 명령줄을 읽지 않고 커널의 짧은 실행 이름만 사용한다.
pub(crate) fn process_name(pid: u32) -> io::Result<String> {
    // SAFETY: `proc_bsdshortinfo`는 정수 필드만 있고 PROC_PIDT_SHORTBSDINFO의 출력 구조체다.
    let info = unsafe { pidinfo::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO)? };
    let bytes: Vec<u8> = info
        .pbsi_comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.to_ne_bytes()[0])
        .collect();
    let name = String::from_utf8_lossy(&bytes).into_owned();
    Ok(super::versioned_agent_name(name, || executable_path(pid)))
}

/// 실행 파일 경로(`proc_pidpath`). 같은 UID 프로세스만 조회된다.
fn executable_path(pid: u32) -> io::Result<PathBuf> {
    let pid = c_int::try_from(pid).map_err(io::Error::other)?;
    // `PROC_PIDPATHINFO_MAXSIZE`(4096).
    let mut buf = [0_u8; 4096];
    let size = u32::try_from(buf.len()).map_err(io::Error::other)?;
    // SAFETY: `buf`는 `size` 바이트짜리 유효한 쓰기 위치다.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), size) };
    let n = usize::try_from(n)
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(io::Error::last_os_error)?;
    let bytes = buf.get(..n).ok_or(io::ErrorKind::InvalidData)?;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
}

/// 프로세스 시작 시각(µs, PID 재사용 구분용, `PROC_PIDTBSDINFO`). 같은 UID 프로세스만 조회된다.
pub fn start_us(pid: u32) -> io::Result<u64> {
    // SAFETY: `proc_bsdinfo`는 정수 필드만 있고 PROC_PIDTBSDINFO의 출력 구조체다.
    let info = unsafe { pidinfo::<libc::proc_bsdinfo>(pid, libc::PROC_PIDTBSDINFO)? };
    Ok(info
        .pbi_start_tvsec
        .saturating_mul(1_000_000)
        .saturating_add(info.pbi_start_tvusec))
}

/// `sys/proc_info.h`의 `struct proc_fileinfo`와 `struct vnode_fdinfowithpath`(`PROC_PIDFDVNODEPATHINFO = 2`).
#[repr(C)]
struct ProcFileInfo {
    openflags: u32,
    status: u32,
    offset: libc::off_t,
    kind: i32,
    guardflags: u32,
}

#[repr(C)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

const PROC_PIDFDVNODEPATHINFO: c_int = 2;

/// `pid`가 열어 둔 일반 파일·디렉터리(vnode) 경로 목록(`PROC_PIDLISTFDS` + `PROC_PIDFDVNODEPATHINFO`).
pub(crate) fn open_vnode_paths(pid: u32) -> Vec<PathBuf> {
    let Ok(pid_c) = c_int::try_from(pid) else {
        return Vec::new();
    };
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        };
        512
    ];
    let Ok(bytes) = c_int::try_from(fds.len().saturating_mul(size_of::<libc::proc_fdinfo>()))
    else {
        return Vec::new();
    };
    // SAFETY: `fds`는 `bytes` 바이트짜리 쓰기 가능한 버퍼다.
    let n = unsafe {
        libc::proc_pidinfo(
            pid_c,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast::<c_void>(),
            bytes,
        )
    };
    let Ok(n) = usize::try_from(n) else {
        return Vec::new();
    };
    let count = (n / size_of::<libc::proc_fdinfo>()).min(fds.len());
    let Ok(vsize) = c_int::try_from(size_of::<VnodeFdInfoWithPath>()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for fd in fds.into_iter().take(count) {
        if fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
            continue;
        }
        let mut vinfo = MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
        // SAFETY: `vinfo`는 `vsize` 바이트짜리 쓰기 가능한 버퍼다.
        let vn = unsafe {
            libc::proc_pidfdinfo(
                pid_c,
                fd.proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                vinfo.as_mut_ptr().cast::<c_void>(),
                vsize,
            )
        };
        if vn != vsize {
            continue;
        }
        // SAFETY: 0으로 초기화했고 `proc_pidfdinfo`가 구조체 전체를 채웠다.
        let vinfo = unsafe { vinfo.assume_init() };
        let path_bytes: Vec<u8> = vinfo
            .pvip
            .vip_path
            .iter()
            .flatten()
            .take_while(|&&c| c != 0)
            .map(|&c| c.to_ne_bytes()[0])
            .collect();
        if !path_bytes.is_empty() {
            out.push(PathBuf::from(std::ffi::OsString::from_vec(path_bytes)));
        }
    }
    out
}

/// `proc_pidinfo(pid, flavor)`가 `T`를 꽉 채워야 성공. 없는 PID·권한 없음이면 오류.
///
/// # Safety
///
/// `T`는 모든 비트 패턴이 유효한 정수 필드 구조체이고 `flavor`의 출력 형식이어야 한다.
unsafe fn pidinfo<T>(pid: u32, flavor: c_int) -> io::Result<T> {
    let pid = c_int::try_from(pid).map_err(io::Error::other)?;
    let size = c_int::try_from(size_of::<T>()).map_err(io::Error::other)?;
    let mut info = MaybeUninit::<T>::zeroed();
    // SAFETY: `info`는 `size` 바이트짜리 유효한 쓰기 위치다.
    let n = unsafe { libc::proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast::<c_void>(), size) };
    if n != size {
        return Err(if n <= 0 {
            io::Error::last_os_error()
        } else {
            io::ErrorKind::UnexpectedEof.into()
        });
    }
    // SAFETY: 0으로 초기화했고 호출자 계약대로 `T`는 어떤 비트 패턴이든 유효하다.
    Ok(unsafe { info.assume_init() })
}

/// 자식이 아닌 프로세스의 종료 감시(`EVFILT_PROC NOTE_EXIT`, kqueue 하나). holder를 잃은 krilld가 세션 종료를 안다.
#[derive(Debug)]
pub struct ExitWatch(OwnedFd);

impl ExitWatch {
    pub fn new() -> io::Result<Self> {
        Ok(Self(kqueue::kqueue()?))
    }

    /// `pid`를 감시에 더한다. `EV_RECEIPT`라 등록 결과만 받는다. 이미 끝난(없는) pid면 `ESRCH`.
    pub fn add(&self, pid: u32) -> io::Result<()> {
        let pid = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let flags = ProcessEvents::EXIT;
        let ev = Event::new(
            EventFilter::Proc { pid, flags },
            EventFlags::ADD | EventFlags::RECEIPT,
            ptr::null_mut(),
        );
        let mut out = Vec::with_capacity(1);
        self.kevent(&[ev], &mut out, Some(std::time::Duration::ZERO))?;
        match out.first().map(|e| i32::try_from(e.data())) {
            Some(Ok(0)) => Ok(()),
            Some(Ok(errno)) => Err(io::Error::from_raw_os_error(errno)),
            _ => Err(io::ErrorKind::Other.into()),
        }
    }

    /// 감시하던 프로세스 하나가 끝날 때까지 기다리고 그 pid를 돌려준다.
    pub fn next(&self) -> io::Result<u32> {
        let mut out = Vec::with_capacity(1);
        loop {
            self.kevent(&[], &mut out, None)?;
            if let Some(EventFilter::Proc { pid, .. }) = out.first().map(Event::filter) {
                return u32::try_from(pid.as_raw_nonzero().get()).map_err(io::Error::other);
            }
        }
    }

    /// 지금 큐에 쌓인 종료를 꺼낸다. 기다리지 않는다(전경 그룹 확인과 같은 감시 스레드에서 쓴다).
    pub(crate) fn drain(&self) -> io::Result<Vec<u32>> {
        let mut events = Vec::with_capacity(256);
        self.kevent(&[], &mut events, Some(std::time::Duration::ZERO))?;
        Ok(events
            .iter()
            .filter_map(|e| match e.filter() {
                EventFilter::Proc { pid, .. } => u32::try_from(pid.as_raw_nonzero().get()).ok(),
                _ => None,
            })
            .collect())
    }

    fn kevent(
        &self,
        changes: &[Event],
        out: &mut Vec<Event>,
        timeout: Option<std::time::Duration>,
    ) -> io::Result<()> {
        out.clear();
        // SAFETY: 등록하는 필터는 EVFILT_PROC뿐이라 knote가 가리키는 fd가 없다(닫힌 fd를 가리키는 이벤트가 생길 수 없다).
        match unsafe {
            kqueue::kevent(
                &self.0,
                changes,
                rustix::buffer::spare_capacity(out),
                timeout,
            )
        } {
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Time Machine 고정 제외 속성. 값은 tmutil addexclusion이 만든 binary plist와 같다.
pub(crate) fn exclude_from_backup(dir: BorrowedFd<'_>) -> io::Result<()> {
    // bplist00: 문자열 하나 "com.apple.backupd" (tmutil 출력으로 확인).
    const VALUE: &[u8] = b"bplist00\x5f\x10\x11com.apple.backupd\x08\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x1c";
    // SAFETY: dir는 유효한 빌린 fd, 속성 이름은 NUL 종결, VALUE는 호출 동안 유효한 VALUE.len() 바이트다.
    let result = unsafe {
        libc::fsetxattr(
            dir.as_raw_fd(),
            c"com.apple.metadata:com_apple_backup_excludeItem".as_ptr(),
            VALUE.as_ptr().cast(),
            VALUE.len(),
            0,
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// C 라이브러리(libzstd 등)가 시스템 malloc zone에 남긴 빈 캐시 페이지를 OS에 돌려준다.
pub(crate) fn release_malloc_cache() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
    }
    // SAFETY: zone이 NULL이면 모든 기본 malloc zone에서 goal(0 = 가능한 만큼)만큼 빈 캐시 페이지를 비운다.
    unsafe {
        malloc_zone_pressure_relief(ptr::null_mut(), 0);
    }
}

/// APFS copy-on-write 복사. 지원하지 않는 볼륨 등의 오류는 호출자가 일반 복사로 처리한다.
/// 심링크는 따라가지 않고, 이미 있는 목적지는 덮어쓰지 않는다.
pub(crate) fn clone_file(src: &std::path::Path, dst: &std::path::Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    // libc 0.2에는 플래그가 없다. macOS SDK sys/clonefile.h의 CLONE_NOFOLLOW.
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let src = std::ffi::CString::new(src.as_os_str().as_bytes())?;
    let dst = std::ffi::CString::new(dst.as_os_str().as_bytes())?;
    // SAFETY: 두 경로는 호출 동안 유효한 NUL 종료 문자열이다. clonefile은 포인터를 보관하지 않는다.
    let result = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod vnode_path_tests {
    use super::*;

    #[test]
    fn open_vnode_paths_finds_an_open_file_in_this_process() {
        let path = std::env::temp_dir().join(format!("krill-open-vnode-{}", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        assert!(open_vnode_paths(std::process::id()).contains(&canonical));
        drop(file);
        std::fs::remove_file(path).unwrap();
        assert!(open_vnode_paths(u32::MAX).is_empty());
    }
}
