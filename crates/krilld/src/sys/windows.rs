//! Windows 판(M11 Phase 1, OWE-819) — 컴파일 전용 스텁. linux.rs와 같은 함수를 닫힌 쪽으로 실패하게 둔다.
//! - 조회(메모리·PID·cwd·passwd·인터페이스·cron 달력)는 [`io::ErrorKind::Unsupported`]. 호출자는 이미 실패를 견딘다.
//! - 코드서명·감사 토큰은 없다: [`peer_token`]이 실패하고 [`verify_audit_token`]은 [`CodeSign::invalid`](Linux와 같다).
//! - 실구현(Toolhelp·`GetProcessMemoryInfo`·피어 PID·SID 검사·`WaitForMultipleObjects`)은 Phase 3(OWE-833).

use std::io;
use std::os::windows::io::{BorrowedHandle, BorrowedSocket};
use std::path::{Path, PathBuf};

use super::{AUDIT_TOKEN_LEN, CodeSign, User};

fn unsupported<T>() -> io::Result<T> {
    Err(io::ErrorKind::Unsupported.into())
}

/// 자기 프로세스의 메모리·누적 디스크 쓰기. 측정이 없어 0으로 보고하지 않고 실패한다.
pub(crate) fn resources() -> io::Result<(u64, u64)> {
    unsupported()
}

pub(crate) fn footprint(_pid: u32) -> io::Result<u64> {
    unsupported()
}

pub(crate) fn all_pids() -> io::Result<Vec<u32>> {
    unsupported()
}

/// Windows에는 UID가 없다(SID 검사는 OWE-833). 실패하므로 피어 검사는 거부 쪽이다.
pub fn peer_uid(_sock: BorrowedSocket<'_>) -> io::Result<u32> {
    unsupported()
}

pub fn peer_pid(_sock: BorrowedSocket<'_>) -> io::Result<u32> {
    unsupported()
}

/// Windows에는 감사 토큰이 없다.
pub fn peer_token(_sock: BorrowedSocket<'_>) -> io::Result<[u8; AUDIT_TOKEN_LEN]> {
    unsupported()
}

/// Windows 코드서명 검증은 없다. 항상 [`CodeSign::invalid`].
pub fn verify_audit_token(_token: &[u8]) -> CodeSign {
    CodeSign::invalid()
}

pub fn parent_pid(_pid: u32) -> io::Result<u32> {
    unsupported()
}

pub(crate) fn current_directory(_pid: u32) -> io::Result<PathBuf> {
    unsupported()
}

pub(crate) fn process_name(_pid: u32) -> io::Result<String> {
    unsupported()
}

pub fn start_us(_pid: u32) -> io::Result<u64> {
    unsupported()
}

/// passwd가 없다. 셸은 호출자가 Windows 기본 셸 후보로 고른다(`shell.rs`).
pub fn user() -> io::Result<User> {
    unsupported()
}

pub(crate) fn cron_calendar(_seconds: u64) -> io::Result<[u32; 5]> {
    unsupported()
}

pub(crate) fn ipv4_interfaces() -> io::Result<Vec<(String, std::net::Ipv4Addr)>> {
    unsupported()
}

/// 자식이 아닌 프로세스의 종료 감시. 아직 없어 [`ExitWatch::new`]가 실패한다.
#[derive(Debug)]
pub struct ExitWatch(());

#[expect(clippy::unused_self, reason = "macOS·Linux 판과 같은 시그니처")]
impl ExitWatch {
    pub fn new() -> io::Result<Self> {
        unsupported()
    }

    pub fn add(&self, _pid: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn next(&self) -> io::Result<u32> {
        unsupported()
    }

    pub(crate) fn drain(&self) -> io::Result<Vec<u32>> {
        unsupported()
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
