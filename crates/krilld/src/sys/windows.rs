//! Windows 판(OWE-833): Toolhelp·psapi 프로세스 조회, `AF_UNIX` 피어 PID(`SIO_AF_UNIX_GETPEERPID`)와 토큰 SID 비교,
//! DPAPI 키 봉인, 프로세스 핸들 폴링 종료 감시.
//! - UID가 없다: [`peer_uid`]는 피어 프로세스 토큰의 사용자 SID가 우리 것과 같을 때만 [`SAME_USER`]를 준다.
//! - 코드서명·감사 토큰은 없다: [`peer_token`]이 실패하고 [`verify_audit_token`]은 [`CodeSign::invalid`](Linux와 같다).
//! - 다른 프로세스의 cwd는 읽지 않는다(PEB 읽기는 비공개 구조). 세션 cwd는 셸의 OSC 7·OSC 9;9 보고로 안다.
//! - 부모가 이미 끝났거나(PID가 재사용돼) 부모보다 먼저 시작한 것으로 보이면 [`parent_pid`]는 0이다.
//!   macOS·Linux의 고아가 launchd·init(1)에 입양되는 것과 같아, 범위 계산([`crate::scope`])이 같은 답을 낸다.

use std::io;
use std::os::windows::io::{
    AsRawHandle as _, AsRawSocket as _, BorrowedHandle, BorrowedSocket, FromRawHandle as _,
    OwnedHandle,
};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use windows_sys::Win32::Foundation::{
    ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree, WAIT_OBJECT_0,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, FIONREAD, SIO_AF_UNIX_GETPEERPID, SOCKADDR_IN, SOCKET, SOCKET_ERROR, WSAGetLastError,
    WSAIoctl, ioctlsocket,
};
#[cfg(not(test))]
use windows_sys::Win32::Security::Credentials::{
    CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessIoCounters, GetProcessTimes, IO_COUNTERS, OpenProcess,
    OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use zeroize::Zeroizing;

use super::{AUDIT_TOKEN_LEN, CodeSign, User};

mod child;
mod file_users;
mod foreground;
pub(crate) use child::{CommandJob, pipe_available};
pub(crate) use file_users::FileUsers;
pub use foreground::run_probe as run_console_probe;
pub(crate) use foreground::session_agent_process;

/// Windows에는 UID가 없어 "이 프로세스와 같은 사용자 SID"를 이 값 하나로 나타낸다([`crate::os::euid`]).
pub(crate) const SAME_USER: u32 = 0;
/// Toolhelp 스냅숏을 다시 쓰는 시간. [`crate::resources`]가 pid마다 [`parent_pid`]를 불러 스냅숏을 pid 수만큼
/// 찍지 않게 한다. 캐시에 없는 pid는 새로 찍는다(방금 뜬 프로세스를 놓치지 않는다).
const SNAPSHOT_TTL: Duration = Duration::from_millis(500);
/// 종료 감시 폴링 간격.
const EXIT_POLL: Duration = Duration::from_millis(100);

/// Keep the waiting agent launcher alive on Ctrl+C/Break; the agent handles its own console events.
/// Use a callback rather than the inheritable NULL-handler ignore flag, so children still receive Ctrl+C.
pub(crate) fn protect_agent_launcher() -> io::Result<()> {
    use windows_sys::Win32::System::Console::{GetConsoleCP, SetConsoleCtrlHandler};
    // SAFETY: no pointers; a zero codepage means this background invocation has no console.
    if unsafe { GetConsoleCP() } == 0 {
        return Ok(());
    }
    // SAFETY: static nonpanicking callback, registered for this helper process's lifetime only.
    if unsafe { SetConsoleCtrlHandler(Some(agent_launcher_control), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The trampoline is already attached to its session's `ConPTY`. Codepages are
/// console state, so initialization here affects the child without another console.
/// Best effort: a failure is shown in the session and the child still starts.
pub(crate) fn console_utf8() {
    use std::io::Write as _;
    use windows_sys::Win32::System::Console::{GetConsoleCP, SetConsoleCP, SetConsoleOutputCP};
    // SAFETY: a zero codepage means this invocation has no attached console.
    if unsafe { GetConsoleCP() } == 0 {
        return;
    }
    // Output first: it decides the bytes children such as `powershell -NoProfile` write to pipes.
    // SAFETY: UTF-8 is a supported codepage for the current attached session console.
    let switched = unsafe { SetConsoleOutputCP(65001) } != 0
        // SAFETY: same current session console; no shared daemon console is modified.
        && unsafe { SetConsoleCP(65001) } != 0;
    if !switched {
        let error = io::Error::last_os_error();
        writeln!(
            io::stderr().lock(),
            "krill: UTF-8 console setup failed: {error}"
        )
        .ok();
    }
}

extern "system" fn agent_launcher_control(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

fn unsupported<T>() -> io::Result<T> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn open_vnode_paths(_pid: u32) -> Vec<PathBuf> {
    Vec::new()
}

/// 실패하면 null을 돌려주는 핸들 생성 함수의 결과를 소유한다.
fn owned(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 방금 만든 유효한 핸들이고 소유권은 여기만 있다.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn open_process(pid: u32, access: u32) -> io::Result<OwnedHandle> {
    // SAFETY: 인자만 읽는다. 실패하면 null.
    owned(unsafe { OpenProcess(access, 0, pid) })
}

/// 사적 커밋 바이트(`PrivateUsage`). macOS `phys_footprint`처럼 공유 이미지 페이지를 빼고 페이지 파일로 나간
/// 사적 페이지는 센다(작업 관리자 "커밋 크기"와 같은 값).
fn private_bytes(process: HANDLE) -> io::Result<u64> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    let cb = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).unwrap_or(u32::MAX);
    counters.cb = cb;
    // SAFETY: counters는 cb 바이트짜리 쓰기 가능한 구조체다(`_EX`는 `PROCESS_MEMORY_COUNTERS`의 확장이라 같은 함수가 채운다).
    if unsafe { GetProcessMemoryInfo(process, (&raw mut counters).cast(), cb) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::try_from(counters.PrivateUsage).unwrap_or(u64::MAX))
}

/// 자기 프로세스의 사적 커밋 바이트와 누적 쓰기 바이트(`WriteTransferCount`, 실패하면 0).
/// ponytail: Windows의 쓰기 수는 파일만이 아니라 파이프·소켓 쓰기도 센다(디스크 쓰기보다 크게 나온다). 디스크만
/// 필요하면 ETW·`FileIoInfo`로.
pub(crate) fn resources() -> io::Result<(u64, u64)> {
    // SAFETY: 닫을 필요 없는 의사 핸들을 돌려줄 뿐이다.
    let me = unsafe { GetCurrentProcess() };
    let footprint = private_bytes(me)?;
    let mut io_counters = IO_COUNTERS::default();
    // SAFETY: me는 자기 프로세스 의사 핸들이고 io_counters는 쓰기 가능한 구조체다.
    let writes = if unsafe { GetProcessIoCounters(me, &raw mut io_counters) } == 0 {
        0
    } else {
        io_counters.WriteTransferCount
    };
    Ok((footprint, writes))
}

/// `pid`의 사적 커밋 바이트([`private_bytes`]).
pub(crate) fn footprint(pid: u32) -> io::Result<u64> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    private_bytes(process.as_raw_handle())
}

/// Toolhelp 스냅숏의 한 항목.
#[derive(Debug, Clone)]
struct Process {
    pid: u32,
    parent: u32,
    /// 실행 파일 이름(경로 없이, 예: `node.exe`).
    exe: String,
}

/// 지금 있는 모든 프로세스.
fn snapshot() -> io::Result<Vec<Process>> {
    // SAFETY: 인자만 읽는다. 실패하면 INVALID_HANDLE_VALUE.
    let snap = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    // SAFETY: PROCESSENTRY32W는 정수와 u16 배열뿐이라 0으로 채운 값이 유효하다.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX);
    let mut out = Vec::new();
    // SAFETY: snap은 유효한 스냅숏이고 entry는 dwSize를 채운 쓰기 가능한 구조체다.
    let mut more = unsafe { Process32FirstW(snap.as_raw_handle(), &raw mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        out.push(Process {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            exe: String::from_utf16_lossy(entry.szExeFile.get(..len).unwrap_or_default()),
        });
        // SAFETY: 위와 같다.
        more = unsafe { Process32NextW(snap.as_raw_handle(), &raw mut entry) } != 0;
    }
    Ok(out)
}

/// `pid` 항목. 캐시가 [`SNAPSHOT_TTL`]보다 오래됐거나 `pid`가 없으면 새로 찍는다. `pid`가 `None`이면 전체.
fn processes(pid: Option<u32>) -> io::Result<Vec<Process>> {
    static CACHE: Mutex<Option<(Instant, Vec<Process>)>> = Mutex::new(None);
    let mut cache = CACHE
        .lock()
        .map_err(|_| io::Error::other("process cache poisoned"))?;
    let fresh = cache.as_ref().is_some_and(|(at, list)| {
        at.elapsed() < SNAPSHOT_TTL && pid.is_none_or(|p| list.iter().any(|e| e.pid == p))
    });
    if !fresh {
        *cache = Some((Instant::now(), snapshot()?));
    }
    let list = cache
        .as_ref()
        .map(|(_, list)| list.as_slice())
        .unwrap_or_default();
    Ok(match pid {
        Some(p) => list.iter().filter(|e| e.pid == p).cloned().collect(),
        None => list.to_vec(),
    })
}

fn process(pid: u32) -> io::Result<Process> {
    processes(Some(pid))?
        .pop()
        .ok_or_else(|| io::ErrorKind::NotFound.into())
}

pub(crate) fn all_pids() -> io::Result<Vec<u32>> {
    Ok(processes(None)?.into_iter().map(|e| e.pid).collect())
}

/// 같은 사용자의 피어면 [`SAME_USER`]. SID가 다르면 `PermissionDenied`. PID를 묻고 곧바로 그 프로세스의 토큰을
/// 읽는다(PID를 캐시하지 않는다, windows-spike-OWE-827.md "인증과 ACL 해석").
/// ponytail: 피어가 connect 뒤 바로 끝나고 PID가 다른 사용자 프로세스로 재사용되면 그 사용자로 판정한다(그쪽은
/// 거부라 닫힌 쪽). 반대로 같은 사용자로 잘못 볼 수는 없다 — 재사용된 프로세스가 같은 사용자일 때뿐이다.
pub fn peer_uid(sock: BorrowedSocket<'_>) -> io::Result<u32> {
    let pid = peer_pid(sock)?;
    let peer = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    // SAFETY: 닫을 필요 없는 의사 핸들을 돌려줄 뿐이다.
    let me = token_user(unsafe { GetCurrentProcess() })?;
    let other = token_user(peer.as_raw_handle())?;
    // SAFETY: 두 SID 모두 GetTokenInformation(TokenUser)가 채운 버퍼 안의 유효한 SID이고 버퍼는 아직 살아 있다.
    if unsafe { EqualSid(sid(&me), sid(&other)) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer pid {pid} runs as another user"),
        ));
    }
    Ok(SAME_USER)
}

/// `TokenUser` 정보 버퍼(앞이 [`TOKEN_USER`], SID 본문은 뒤에 이어진다). `u64`로 잡아 정렬을 맞춘다.
fn token_user(process: HANDLE) -> io::Result<Vec<u64>> {
    let mut raw = ptr::null_mut();
    // SAFETY: process는 유효한 프로세스 핸들이고 raw는 쓰기 위치다.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(raw)?;
    let mut len = 0_u32;
    // SAFETY: 크기만 묻는다(버퍼 null, 길이 0). 실패(ERROR_INSUFFICIENT_BUFFER)가 정상이다.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &raw mut len,
        )
    };
    if usize::try_from(len).unwrap_or(0) < size_of::<TOKEN_USER>() {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0_u64; usize::try_from(len).unwrap_or(0).div_ceil(8)];
    // SAFETY: buf는 len 바이트 이상이고 8바이트 정렬이다.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            len,
            &raw mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

fn sid(token_user: &[u64]) -> windows_sys::Win32::Security::PSID {
    // SAFETY: [`token_user`]가 채운 버퍼라 앞이 정렬된 TOKEN_USER다.
    unsafe { (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
}

/// `AF_UNIX` 피어가 connect할 때의 PID(`SIO_AF_UNIX_GETPEERPID`, Windows 10 1803+).
pub fn peer_pid(sock: BorrowedSocket<'_>) -> io::Result<u32> {
    let mut pid = 0_u32;
    let mut returned = 0_u32;
    let socket = SOCKET::try_from(sock.as_raw_socket()).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: socket은 빌린 유효한 소켓이고 출력 버퍼는 4바이트 pid다. 입력·overlapped·완료 루틴은 없다.
    let r = unsafe {
        WSAIoctl(
            socket,
            SIO_AF_UNIX_GETPEERPID,
            ptr::null(),
            0,
            (&raw mut pid).cast(),
            4,
            &raw mut returned,
            ptr::null_mut(),
            None,
        )
    };
    if r == SOCKET_ERROR {
        // SAFETY: 인자 없이 이 스레드의 마지막 Winsock 오류를 읽는다.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    // 반환 바이트 수는 믿지 않는다: pid를 채우고도 0을 돌려준 경우가 있다(VM 실측, 같은 테스트 안 두 번째 호출).
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SIO_AF_UNIX_GETPEERPID returned no pid",
        ));
    }
    Ok(pid)
}

/// Windows에는 감사 토큰이 없다.
pub fn peer_token(_sock: BorrowedSocket<'_>) -> io::Result<[u8; AUDIT_TOKEN_LEN]> {
    unsupported()
}

/// Windows 코드서명 검증은 없다. 항상 [`CodeSign::invalid`].
pub fn verify_audit_token(_token: &[u8]) -> CodeSign {
    CodeSign::invalid()
}

/// 부모 PID. 부모가 없거나 자식보다 늦게 시작했으면(PID 재사용) 0이다(모듈 설명).
pub fn parent_pid(pid: u32) -> io::Result<u32> {
    let parent = process(pid)?.parent;
    if parent == 0 || parent == pid || process(parent).is_err() {
        return Ok(0);
    }
    // 시작 시각을 못 읽으면(다른 사용자·보호 프로세스) 부모를 그대로 믿는다.
    match (start_us(parent), start_us(pid)) {
        (Ok(p), Ok(c)) if p > c => Ok(0),
        _ => Ok(parent),
    }
}

/// 자기 프로세스만 안다. 다른 프로세스의 cwd는 셸 보고(OSC 7·9;9)로 안다(모듈 설명).
pub(crate) fn current_directory(pid: u32) -> io::Result<PathBuf> {
    if pid == std::process::id() {
        return std::env::current_dir();
    }
    unsupported()
}

/// 프로세스 argv. Toolhelp 스냅샷에는 명령줄이 없어 지원하지 않는다.
// ponytail: 계정 전환이 Windows 셸 직접 실행 에이전트를 Manual로 이어 띄운다. 필요해지면 PEB 명령줄을 읽는다.
pub(crate) fn process_args(_pid: u32) -> io::Result<Vec<String>> {
    unsupported()
}

/// 실행 파일 이름에서 `.exe`를 뺀 것(`node.exe` → `node`). macOS·Linux의 짧은 실행 이름과 같은 꼴이다.
pub(crate) fn process_name(pid: u32) -> io::Result<String> {
    let exe = process(pid)?.exe;
    let stem = exe
        .len()
        .checked_sub(4)
        .filter(|&i| {
            exe.get(i..)
                .is_some_and(|ext| ext.eq_ignore_ascii_case(".exe"))
        })
        .and_then(|i| exe.get(..i));
    Ok(stem.unwrap_or(&exe).to_owned())
}

/// 프로세스 생성 시각(1601년 기준 µs). PID 재사용 구분용.
pub fn start_us(pid: u32) -> io::Result<u64> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: process는 유효한 핸들이고 네 FILETIME은 쓰기 가능한 위치다.
    if unsafe { GetProcessTimes(process.as_raw_handle(), created, exited, kernel, user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let ticks = u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime);
    Ok(ticks / 10)
}

/// passwd가 없다: 탐색기가 주는 `USERNAME`·`USERPROFILE`. 셸은 비워 두고 `shell.rs`가 Windows 후보에서 고른다.
pub fn user() -> io::Result<User> {
    let var = |key| {
        std::env::var_os(key)
            .map(std::ffi::OsString::into_encoded_bytes)
            .filter(|v| !v.is_empty())
            .ok_or(io::ErrorKind::NotFound)
    };
    Ok(User {
        name: var("USERNAME")?,
        home: var("USERPROFILE")?,
        shell: Vec::new(),
    })
}

/// IPv4 주소·한글을 포함한 표시 이름·기본 게이트웨이 유무를 같은 네이티브 스냅샷에서 읽는다.
pub(crate) fn ipv4_interfaces() -> io::Result<Vec<(String, std::net::Ipv4Addr, bool)>> {
    let mut size = 15_000_u32;
    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    for _ in 0..3 {
        let bytes = usize::try_from(size).map_err(io::Error::other)?;
        // IP_ADAPTER_ADDRESSES의 u64 정렬을 보장한다. Windows가 크기를 바꾸면 새 버퍼로 다시 조회한다.
        let mut buffer = vec![0_u64; bytes.div_ceil(size_of::<u64>())];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: head는 size 바이트 이상·u64 정렬의 쓰기 가능한 버퍼이며 size도 쓰기 가능하다. Reserved는 null이다.
        let code = unsafe {
            GetAdaptersAddresses(u32::from(AF_INET), flags, ptr::null(), head, &raw mut size)
        };
        if code == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if code == ERROR_NO_DATA {
            return Ok(Vec::new());
        }
        if code != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(code).unwrap_or(i32::MAX),
            ));
        }
        let mut interfaces = Vec::new();
        let mut current = head;
        while !current.is_null() {
            // SAFETY: 성공한 API가 이 버퍼 안에 초기화한 연결 목록이다. buffer는 순회 내내 살아 있고 변경하지 않는다.
            let adapter = unsafe { &*current };
            current = adapter.Next;
            if adapter.OperStatus != IfOperStatusUp || adapter.FriendlyName.is_null() {
                continue;
            }
            let offset = adapter
                .FriendlyName
                .addr()
                .checked_sub(buffer.as_ptr().addr())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "adapter name outside buffer")
                })?;
            let available = size_of_val(buffer.as_slice())
                .checked_sub(offset)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "adapter name outside buffer")
                })?
                / size_of::<u16>();
            // SAFETY: FriendlyName은 API가 버퍼 안에 쓴 UTF-16 포인터다. available은 할당 끝까지만 세며 u16은 모든 비트 패턴이 유효하다.
            let wide = unsafe { std::slice::from_raw_parts(adapter.FriendlyName, available) };
            let name = interface_name(wide);
            let default_route = !adapter.FirstGatewayAddress.is_null();
            let mut address = adapter.FirstUnicastAddress;
            while !address.is_null() {
                // SAFETY: FirstUnicastAddress·Next도 같은 성공한 API가 버퍼 안에 초기화한 목록이다.
                let unicast = unsafe { &*address };
                address = unicast.Next;
                let socket = &unicast.Address;
                if socket.lpSockaddr.is_null()
                    || usize::try_from(socket.iSockaddrLength).unwrap_or(0)
                        < size_of::<SOCKADDR_IN>()
                {
                    continue;
                }
                // SAFETY: API의 AF_INET 조회 결과이고 위에서 null·구조체 크기를 검사했다. 구조체를 복사해 더 강한 정렬을 요구하지 않는다.
                let ipv4: SOCKADDR_IN = unsafe { ptr::read_unaligned(socket.lpSockaddr.cast()) };
                if ipv4.sin_family != AF_INET {
                    continue;
                }
                // SAFETY: AF_INET 주소의 S_addr는 초기화된 IPv4 네트워크 바이트다.
                let raw = unsafe { ipv4.sin_addr.S_un.S_addr };
                interfaces.push((
                    name.clone(),
                    std::net::Ipv4Addr::from(raw.to_ne_bytes()),
                    default_route,
                ));
            }
        }
        return Ok(interfaces);
    }
    Err(io::Error::other("network adapters changed during query"))
}

fn interface_name(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(wide.get(..end).unwrap_or_default())
}

/// 모바일 snapshot마다 자식 프로세스를 띄우지 않고 실제 DNS 호스트 이름을 읽는다.
pub(crate) fn hostname() -> io::Result<String> {
    let mut size = 0;
    // SAFETY: null·크기 0으로 필요한 UTF-16 버퍼 길이만 묻는다. size는 쓰기 가능하다.
    unsafe { GetComputerNameExW(ComputerNameDnsHostname, ptr::null_mut(), &raw mut size) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut name = vec![0_u16; usize::try_from(size).map_err(io::Error::other)?];
    // SAFETY: name은 직전 API가 요구한 size개 요소의 쓰기 가능한 UTF-16 버퍼다.
    if unsafe { GetComputerNameExW(ComputerNameDnsHostname, name.as_mut_ptr(), &raw mut size) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(interface_name(&name))
}

/// 자격 증명 관리자의 일반 자격 증명 blob. go-keyring(agy 등)은 `service:account` 이름으로 저장한다.
/// 읽기만 하고, 없거나 읽을 수 없으면 `None`이다. 복사 뒤 원본 버퍼를 0으로 지우고 푼다.
#[cfg(not(test))]
pub(crate) fn generic_credential(target: &str) -> Option<Zeroizing<Vec<u8>>> {
    let name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    let mut credential: *mut CREDENTIALW = ptr::null_mut();
    // SAFETY: name은 NUL로 끝나는 살아 있는 버퍼이고 credential은 쓰기 위치다. 성공하면 아래에서 CredFree로 푼다.
    let ok = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut credential) };
    if ok == 0 || credential.is_null() {
        return None;
    }
    // SAFETY: CredReadW가 성공했으므로 credential은 CredFree 전까지 유효한 CREDENTIALW다.
    let cred = unsafe { &*credential };
    let (data, size) = (cred.CredentialBlob, cred.CredentialBlobSize);
    let len = usize::try_from(size).unwrap_or(0);
    let bytes = (!data.is_null() && len > 0).then(|| {
        // SAFETY: blob은 credential 수명 동안 len 바이트로 유효하고, 복사한 뒤 이 함수만 지운다.
        let copy = Zeroizing::new(unsafe { std::slice::from_raw_parts(data, len) }.to_vec());
        // SAFETY: 같은 blob을 len 바이트만큼 0으로 덮는다(CredReadW가 이 호출자에게 준 버퍼).
        unsafe { ptr::write_bytes(data, 0, len) };
        copy
    });
    // SAFETY: CredReadW가 할당한 버퍼를 정확히 한 번 푼다.
    unsafe { CredFree(credential.cast()) };
    bytes
}

/// DPAPI로 봉인한다(현재 사용자 범위, UI 없음). 같은 사용자 로그온에서만 풀 수 있다.
pub(crate) fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data)?;
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input은 살아 있는 data를 가리키고(읽기만 한다) out은 쓰기 위치다. 설명·엔트로피·프롬프트는 없다.
    let ok = unsafe {
        CryptProtectData(
            &raw const input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut out,
        )
    };
    take_blob(ok, out).map(|b| b.to_vec())
}

/// [`protect`]의 반대. 다른 사용자·다른 기기의 blob이나 변조된 blob은 실패한다.
pub(crate) fn unprotect(data: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let input = blob(data)?;
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: protect와 같다. 설명 출력은 받지 않는다(null).
    let ok = unsafe {
        CryptUnprotectData(
            &raw const input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut out,
        )
    };
    take_blob(ok, out)
}

fn blob(data: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(|_| io::ErrorKind::InvalidInput)?,
        pbData: data.as_ptr().cast_mut(),
    })
}

/// DPAPI 출력 버퍼를 복사하고, 0으로 지운 뒤 푼다.
fn take_blob(
    ok: windows_sys::core::BOOL,
    out: CRYPT_INTEGER_BLOB,
) -> io::Result<Zeroizing<Vec<u8>>> {
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let len = usize::try_from(out.cbData).unwrap_or(0);
    let bytes = if out.pbData.is_null() {
        Zeroizing::new(Vec::new())
    } else {
        // SAFETY: 성공한 DPAPI 출력은 cbData 바이트짜리 LocalAlloc 버퍼다.
        let bytes = Zeroizing::new(unsafe { std::slice::from_raw_parts(out.pbData, len) }.to_vec());
        // SAFETY: 위 버퍼 전체에 쓴다(평문 키가 해제된 힙에 남지 않게).
        unsafe { ptr::write_bytes(out.pbData, 0, len) };
        bytes
    };
    // SAFETY: DPAPI가 LocalAlloc한 버퍼이고 한 번만 푼다(null이면 아무것도 안 한다).
    unsafe { LocalFree(out.pbData.cast()) };
    Ok(bytes)
}

/// 자식이 아닌 프로세스의 종료 감시. `SYNCHRONIZE` 핸들을 쥐고 [`EXIT_POLL`]마다 0ms 대기로 확인한다.
/// 핸들을 쥐고 있어 그사이 PID가 재사용돼도 다른 프로세스를 보지 않는다.
/// ponytail: 폴링 O(감시 수). 세션이 많아 비용이 보이면 `RegisterWaitForSingleObject`로.
#[derive(Debug)]
pub struct ExitWatch(Mutex<Vec<(u32, OwnedHandle)>>);

impl ExitWatch {
    #[expect(clippy::unnecessary_wraps, reason = "macOS·Linux 판과 같은 시그니처")]
    pub fn new() -> io::Result<Self> {
        Ok(Self(Mutex::new(Vec::new())))
    }

    /// `pid`를 감시에 더한다. 이미 없는 pid면 `OpenProcess` 오류(`ERROR_INVALID_PARAMETER`).
    pub fn add(&self, pid: u32) -> io::Result<()> {
        let handle = open_process(pid, PROCESS_SYNCHRONIZE)?;
        self.handles()?.push((pid, handle));
        Ok(())
    }

    /// 감시하던 프로세스 하나가 끝날 때까지 기다리고 그 pid를 돌려준다.
    pub fn next(&self) -> io::Result<u32> {
        loop {
            if let Some(pid) = self.take(1)?.pop() {
                return Ok(pid);
            }
            std::thread::sleep(EXIT_POLL);
        }
    }

    /// 지금 끝나 있는 것을 꺼낸다. 기다리지 않는다.
    pub(crate) fn drain(&self) -> io::Result<Vec<u32>> {
        self.take(usize::MAX)
    }

    /// 끝난 것을 `max`개까지 꺼내고 감시에서 뺀다(한 번만 알린다).
    fn take(&self, max: usize) -> io::Result<Vec<u32>> {
        let mut done = Vec::new();
        self.handles()?.retain(|(pid, handle)| {
            if done.len() >= max {
                return true;
            }
            // SAFETY: SYNCHRONIZE로 연 유효한 프로세스 핸들이다. 기다리지 않는다(0ms).
            let exited = unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } == WAIT_OBJECT_0;
            if exited {
                done.push(*pid);
            }
            !exited
        });
        Ok(done)
    }

    fn handles(&self) -> io::Result<std::sync::MutexGuard<'_, Vec<(u32, OwnedHandle)>>> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("exit watch poisoned"))
    }
}

/// Windows에는 Time Machine이 없다.
#[expect(clippy::unnecessary_wraps, reason = "macOS 판과 같은 시그니처")]
pub(crate) fn exclude_from_backup(_dir: BorrowedHandle<'_>) -> io::Result<()> {
    Ok(())
}

/// clone 없음: 호출자가 일반 복사로 넘어간다.
pub(crate) fn clone_file(_src: &Path, _dst: &Path) -> io::Result<()> {
    unsupported()
}

/// Adopt one exclusively owned `Winsock` listener restored by the holder.
pub(crate) fn adopt_listener(
    socket: std::os::windows::io::OwnedSocket,
) -> krill_proto::os::net::UnixListener {
    use std::os::windows::io::{FromRawSocket as _, IntoRawSocket as _};
    let raw = socket.into_raw_socket();
    // SAFETY: ownership came from a restored listening socket and is transferred exactly once.
    unsafe { krill_proto::os::net::UnixListener::from_raw_socket(raw) }
}

/// `uds_windows` exposes `AsRawSocket` for listeners but not `AsSocket` (v1.2.1).
pub(crate) fn listener_socket(listener: &krill_proto::os::net::UnixListener) -> BorrowedSocket<'_> {
    // SAFETY: listener owns this socket for the entire returned borrow.
    unsafe { BorrowedSocket::borrow_raw(listener.as_raw_socket()) }
}

/// Cancel the synchronous pipe IO (`ReadFile`/`WriteFile`) of one daemon thread, never the PTY itself.
pub(crate) fn cancel_sync_io(thread: BorrowedHandle<'_>) {
    // SAFETY: borrowed live thread handle; cancellation does not close any handle.
    unsafe {
        windows_sys::Win32::System::IO::CancelSynchronousIo(thread.as_raw_handle());
    }
}

pub(crate) fn thread_finished(thread: BorrowedHandle<'_>) -> bool {
    // SAFETY: live borrowed thread handle, zero timeout cannot block.
    unsafe { WaitForSingleObject(thread.as_raw_handle(), 0) == WAIT_OBJECT_0 }
}

/// Peer-disconnect probe for a blocked PTY input write (daemon-02): polled from the thread that holds
/// the input turn, never read, never blocking. `WSAPoll` rejects `POLLHUP` in `events` (`WSAEINVAL`),
/// so readability is requested and the hang-up, error and invalid-socket bits are taken from `revents`.
/// A graceful FIN may surface only as "readable with nothing queued" (a zero-byte read) rather than
/// `POLLHUP`; that state is sticky, whereas a queue that a concurrent reader drained between the two
/// calls refills or goes idle, so the pair must agree three times before it counts. Queued frames are
/// never a disconnect: the client's unread input is left for its own reader, so a FIN behind unread frames
/// is seen only through `POLLHUP`. `recv(MSG_PEEK)` would be the one atomic probe, but it blocks on these
/// blocking client sockets; a reader that refills the queue between every sample is therefore
/// indistinguishable from EOF here.
pub(crate) fn socket_disconnected(sock: BorrowedSocket<'_>) -> io::Result<bool> {
    for _ in 0..3 {
        let events = socket_events(sock)?;
        if events.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) {
            return Ok(true);
        }
        if !events.contains(PollFlags::RDNORM) || socket_pending(sock)? > 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `revents` of a zero-timeout readability poll (`WSAPoll`).
fn socket_events(sock: BorrowedSocket<'_>) -> io::Result<PollFlags> {
    let mut fds = [PollFd::from_borrowed_fd(sock, PollFlags::RDNORM)];
    poll(
        &mut fds,
        Some(&Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }),
    )?;
    let [fd] = &fds;
    Ok(fd.revents())
}

/// Bytes queued for `recv` (`FIONREAD`); zero once a graceful FIN is all that is left to read.
fn socket_pending(sock: BorrowedSocket<'_>) -> io::Result<u32> {
    let socket = SOCKET::try_from(sock.as_raw_socket()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut pending = 0_u32;
    // SAFETY: borrowed live socket; FIONREAD writes only the queued byte count and consumes nothing.
    if unsafe { ioctlsocket(socket, FIONREAD, &raw mut pending) } == SOCKET_ERROR {
        // SAFETY: reads this thread's last Winsock error; no arguments.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    Ok(pending)
}

pub(crate) fn release_malloc_cache() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "OWE-915 VM network query measurement"]
    fn measure_ipv4_interfaces() {
        for _ in 0..10 {
            let start = Instant::now();
            let interfaces = ipv4_interfaces().unwrap();
            eprintln!(
                "detect_us={} interfaces={interfaces:?}",
                start.elapsed().as_micros()
            );
        }
    }

    #[test]
    fn adapter_name_preserves_korean_and_stops_at_nul() {
        let wide = "이더넷 OWE-915\0discard".encode_utf16().collect::<Vec<_>>();
        assert_eq!(interface_name(&wide), "이더넷 OWE-915");
    }

    #[test]
    fn process_queries_describe_this_process() {
        let me = std::process::id();
        assert!(start_us(me).unwrap() > 0);
        assert_eq!(start_us(me).unwrap(), start_us(me).unwrap());
        assert_eq!(
            current_directory(me).unwrap(),
            std::env::current_dir().unwrap()
        );
        assert!(footprint(me).unwrap() > 0);
        assert!(resources().unwrap().0 > 0);
        assert!(all_pids().unwrap().contains(&me));
        assert!(
            !process_name(me)
                .unwrap()
                .to_ascii_lowercase()
                .ends_with(".exe")
        );
        eprintln!(
            "footprint {} B, resources {:?}, processes {}, name {}",
            footprint(me).unwrap(),
            resources().unwrap(),
            all_pids().unwrap().len(),
            process_name(me).unwrap()
        );
        let parent = parent_pid(me).unwrap();
        assert_ne!(parent, me);
        assert!(parent == 0 || start_us(parent).unwrap() <= start_us(me).unwrap());
    }

    #[test]
    fn child_tree_and_exit_watch() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "ping -n 2 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        assert_eq!(parent_pid(child.id()).unwrap(), std::process::id());
        assert_eq!(
            process_name(child.id()).unwrap().to_ascii_lowercase(),
            "cmd"
        );
        let watch = ExitWatch::new().unwrap();
        watch.add(child.id()).unwrap();
        assert!(watch.drain().unwrap().is_empty());
        assert_eq!(watch.next().unwrap(), child.id());
        child.wait().unwrap();
        assert!(watch.drain().unwrap().is_empty());
    }

    #[test]
    fn dpapi_round_trip_and_tamper() {
        let key = [7_u8; 32];
        let sealed = protect(&key).unwrap();
        assert_ne!(sealed.as_slice(), key.as_slice());
        assert_eq!(unprotect(&sealed).unwrap().as_slice(), key.as_slice());
        let mut bad = sealed.clone();
        if let Some(last) = bad.last_mut() {
            *last ^= 1;
        }
        assert!(unprotect(&bad).is_err());
    }

    #[test]
    fn af_unix_peer_is_this_user() {
        use std::os::windows::io::AsSocket as _;
        let dir = std::env::temp_dir().join(format!("krilld-peer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        std::fs::remove_file(&path).ok();
        let listener = krill_proto::os::net::UnixListener::bind(&path).unwrap();
        let client = krill_proto::os::net::UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        assert_eq!(peer_pid(server.as_socket()).unwrap(), std::process::id());
        assert_eq!(peer_uid(server.as_socket()).unwrap(), SAME_USER);
        drop((client, server, listener));
        std::fs::remove_dir_all(dir).ok();
    }

    /// daemon-02 input ledger: a live idle peer and queued frames are not a disconnect, the probe
    /// consumes nothing, and a graceful close is reported once the queue is drained, on TCP and `AF_UNIX`.
    #[test]
    fn socket_disconnected_sees_eof_without_consuming_on_tcp_and_af_unix() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        disconnect_sequence("tcp", client, server);

        let dir = std::env::temp_dir().join(format!("krilld-disconnect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        std::fs::remove_file(&path).ok();
        let listener = krill_proto::os::net::UnixListener::bind(&path).unwrap();
        let client = krill_proto::os::net::UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        disconnect_sequence("af_unix", client, server);
        drop(listener);
        std::fs::remove_dir_all(dir).ok();
    }

    fn disconnect_sequence<S>(transport: &str, mut client: S, mut server: S)
    where
        S: std::os::windows::io::AsSocket + io::Read + io::Write,
    {
        let probe = |server: &S| socket_disconnected(server.as_socket()).unwrap();
        let queued = |server: &S| socket_pending(server.as_socket()).unwrap();
        // Live, idle peer.
        assert!(!probe(&server), "{transport}: idle live peer");
        // Queued frames stay queued and are not a disconnect.
        client.write_all(b"queued frame").unwrap();
        until(|| queued(&server) >= 12, "queued bytes");
        assert!(!probe(&server), "{transport}: queued frames");
        assert_eq!(queued(&server), 12, "{transport}: probe must not consume");
        let mut buf = [0u8; 12];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"queued frame");
        assert!(!probe(&server), "{transport}: drained live peer");
        // Graceful close behind one more frame: the frame is still delivered, then EOF is sticky.
        client.write_all(b"last").unwrap();
        drop(client);
        until(|| queued(&server) >= 4, "last frame");
        eprintln!(
            "{transport}: verdict with a queued frame and FIN = {}",
            probe(&server)
        );
        if transport == "af_unix" {
            until(|| probe(&server), "AF_UNIX close behind a queued frame");
            assert_eq!(
                queued(&server),
                4,
                "disconnect probe must leave the frame queued"
            );
            eprintln!("AF_UNIX queued FIN cancels before draining the frame");
        }
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"last");
        until(|| probe(&server), "EOF");
        assert!(probe(&server), "{transport}: EOF must stay reported");
        assert_eq!(server.read(&mut buf).unwrap(), 0, "{transport}: EOF read");
    }

    /// Spins until `ready`, failing after 5s so a wrong verdict cannot hang the suite.
    fn until(mut ready: impl FnMut() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
