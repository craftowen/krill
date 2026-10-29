//! Linux 판: `/proc`·`SO_PEERCRED`·pidfd+epoll. 원격 호스트(클라우드 VM)용이라 macOS 전용 기능은 이렇게 대신한다.
//! - 코드서명 검증은 없다: [`peer_token`]이 실패하므로 서명 정책을 켜면 전체 피어는 거부된다(닫힌 쪽으로 실패).
//! - Time Machine 제외·APFS clone은 없다: 앞은 아무것도 안 하고, 뒤는 호출자가 일반 복사로 넘어간다.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use rustix::event::epoll;
use rustix::process::{Pid, PidfdFlags};

use super::{AUDIT_TOKEN_LEN, CodeSign};

/// 자기 프로세스의 상주 메모리(`VmRSS`)와 누적 디스크 쓰기(`/proc/self/io`의 `write_bytes`, 없으면 0).
/// ponytail: macOS `phys_footprint` 대신 `VmRSS`(공유 파일 페이지 포함이라 조금 크게 나온다). 예산 판정이 Linux에서 필요해지면 `smaps_rollup`의 `Pss`로.
pub(crate) fn resources() -> io::Result<(u64, u64)> {
    let rss = footprint(std::process::id())?;
    let writes = std::fs::read_to_string("/proc/self/io")
        .ok()
        .and_then(|t| kv(&t, "write_bytes:"))
        .unwrap_or(0);
    Ok((rss, writes))
}

/// `pid`의 상주 메모리 바이트(`/proc/<pid>/status`의 `VmRSS`).
pub(crate) fn footprint(pid: u32) -> io::Result<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    kv(&status, "VmRSS:")
        .map(|kb| kb.saturating_mul(1024))
        .ok_or_else(|| io::ErrorKind::NotFound.into())
}

/// `key: 값 [kB]` 줄에서 첫 숫자.
fn kv(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix(key))?
        .split_ascii_whitespace()
        .next()?
        .parse()
        .ok()
}

/// 지금 있는 모든 프로세스의 PID(`/proc`의 숫자 디렉터리).
pub(crate) fn all_pids() -> io::Result<Vec<u32>> {
    Ok(std::fs::read_dir("/proc")?
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect())
}

/// `pid`의 직계 자식 PID(`/proc/<pid>/task/<pid>/children`).
pub(crate) fn child_pids(pid: u32) -> io::Result<Vec<u32>> {
    Ok(
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect(),
    )
}

/// UDS 피어의 유효 UID(`SO_PEERCRED`).
pub fn peer_uid(sock: BorrowedFd<'_>) -> io::Result<u32> {
    Ok(rustix::net::sockopt::socket_peercred(sock)?.uid.as_raw())
}

/// UDS 피어가 connect할 때의 PID(`SO_PEERCRED`).
pub fn peer_pid(sock: BorrowedFd<'_>) -> io::Result<u32> {
    let pid = rustix::net::sockopt::socket_peercred(sock)?.pid;
    Ok(pid.as_raw_nonzero().get().unsigned_abs())
}

/// Linux에는 감사 토큰이 없다.
pub fn peer_token(_sock: BorrowedFd<'_>) -> io::Result<[u8; AUDIT_TOKEN_LEN]> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Linux에는 코드서명이 없다. 항상 [`CodeSign::invalid`].
pub fn verify_audit_token(_token: &[u8]) -> CodeSign {
    CodeSign::invalid()
}

/// 부모 PID(`/proc/<pid>/stat` 4번째 칸).
pub fn parent_pid(pid: u32) -> io::Result<u32> {
    stat_field(pid, 4)?
        .try_into()
        .map_err(|_| io::ErrorKind::InvalidData.into())
}

/// 현재 디렉터리(`/proc/<pid>/cwd`). 조회 중 PID 재사용은 거부한다.
pub(crate) fn current_directory(pid: u32) -> io::Result<PathBuf> {
    let start = start_us(pid)?;
    let dir = std::fs::read_link(format!("/proc/{pid}/cwd"))?;
    if start_us(pid)? != start {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(dir)
}

/// `pid`가 열어 둔 파일 경로 목록(`/proc/<pid>/fd/*`).
pub(crate) fn open_vnode_paths(pid: u32) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    entries
        .take(512)
        .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .collect()
}

/// 프로세스 argv(`/proc/<pid>/cmdline`).
pub(crate) fn process_args(pid: u32) -> io::Result<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    Ok(raw
        .strip_suffix(&[0])
        .unwrap_or(&raw)
        .split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// 프로세스 이름. 명령줄을 읽지 않고 커널의 짧은 실행 이름(`/proc/<pid>/comm`)만 사용한다.
pub(crate) fn process_name(pid: u32) -> io::Result<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm"))?;
    let name = name.trim_end_matches('\n').to_owned();
    Ok(super::versioned_agent_name(name, || {
        std::fs::read_link(format!("/proc/{pid}/exe"))
    }))
}

/// 프로세스 시작 시각(부팅 뒤 µs, `/proc/<pid>/stat` 22번째 칸). PID 재사용 구분용.
pub fn start_us(pid: u32) -> io::Result<u64> {
    let ticks = stat_field(pid, 22)?;
    // SAFETY: sysconf는 인자만 읽는다.
    let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
        .ok()
        .filter(|&hz| hz > 0)
        .unwrap_or(100);
    Ok(ticks.saturating_mul(1_000_000) / hz)
}

/// `/proc/<pid>/stat`의 `n`번째 칸(1부터, man proc). comm(2번째)에 공백·괄호가 올 수 있어 마지막 `)` 뒤부터 센다.
fn stat_field(pid: u32, n: usize) -> io::Result<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    text.rfind(')')
        .and_then(|i| text.get(i.checked_add(1)?..))
        .and_then(|rest| rest.split_ascii_whitespace().nth(n.checked_sub(3)?))
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| io::ErrorKind::InvalidData.into())
}

/// 자식이 아닌 프로세스의 종료 감시(pidfd를 epoll 하나에 건다). holder를 잃은 krilld가 세션 종료를 안다.
#[derive(Debug)]
pub struct ExitWatch {
    epoll: OwnedFd,
    pidfds: Mutex<Vec<(u32, OwnedFd)>>,
}

impl ExitWatch {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            epoll: epoll::create(epoll::CreateFlags::CLOEXEC)?,
            pidfds: Mutex::new(Vec::new()),
        })
    }

    /// `pid`를 감시에 더한다. 이미 끝난(없는) pid면 `ESRCH`.
    pub fn add(&self, pid: u32) -> io::Result<()> {
        let raw = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let fd = rustix::process::pidfd_open(raw, PidfdFlags::empty())?;
        epoll::add(
            &self.epoll,
            fd.as_fd(),
            epoll::EventData::new_u64(u64::from(pid)),
            epoll::EventFlags::IN,
        )?;
        self.pidfds
            .lock()
            .map_err(|_| io::Error::other("exit watch poisoned"))?
            .push((pid, fd));
        Ok(())
    }

    /// 감시하던 프로세스 하나가 끝날 때까지 기다리고 그 pid를 돌려준다.
    pub fn next(&self) -> io::Result<u32> {
        loop {
            if let Some(pid) = self.wait(None)?.into_iter().next() {
                return Ok(pid);
            }
        }
    }

    /// 지금 큐에 쌓인 종료를 꺼낸다. 기다리지 않는다.
    pub(crate) fn drain(&self) -> io::Result<Vec<u32>> {
        self.wait(Some(Duration::ZERO))
    }

    fn wait(&self, timeout: Option<Duration>) -> io::Result<Vec<u32>> {
        let ts = timeout.map(|d| rustix::time::Timespec {
            tv_sec: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(d.subsec_nanos()),
        });
        let mut events = Vec::with_capacity(64);
        match epoll::wait(
            &self.epoll,
            rustix::buffer::spare_capacity(&mut events),
            ts.as_ref(),
        ) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        let done: Vec<u32> = events
            .iter()
            .filter_map(|e| u32::try_from(e.data.u64()).ok())
            .collect();
        // kqueue NOTE_EXIT처럼 한 번만 알린다: pidfd를 닫으면 epoll에서도 빠진다.
        self.pidfds
            .lock()
            .map_err(|_| io::Error::other("exit watch poisoned"))?
            .retain(|(p, _)| !done.contains(p));
        Ok(done)
    }
}

/// Linux에는 Time Machine이 없다.
#[expect(clippy::unnecessary_wraps, reason = "macOS 판과 같은 시그니처")]
pub(crate) fn exclude_from_backup(_dir: BorrowedFd<'_>) -> io::Result<()> {
    Ok(())
}

/// APFS clone 없음: 호출자가 일반 복사로 넘어간다.
pub(crate) fn clone_file(_src: &std::path::Path, _dst: &std::path::Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn release_malloc_cache() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_queries_describe_this_process() {
        let me = std::process::id();
        assert_eq!(parent_pid(me).unwrap(), std::os::unix::process::parent_id());
        assert!(start_us(me).unwrap() > 0);
        assert_eq!(start_us(me).unwrap(), start_us(me).unwrap());
        assert_eq!(
            current_directory(me).unwrap(),
            std::env::current_dir().unwrap()
        );
        assert!(footprint(me).unwrap() > 0);
        assert!(all_pids().unwrap().contains(&me));
        assert!(!process_name(me).unwrap().is_empty());
    }

    #[test]
    fn exit_watch_reports_a_finished_child_once() {
        let mut child = std::process::Command::new("sleep")
            .arg("0.1")
            .spawn()
            .unwrap();
        let watch = ExitWatch::new().unwrap();
        watch.add(child.id()).unwrap();
        assert_eq!(watch.next().unwrap(), child.id());
        child.wait().unwrap();
        assert!(watch.drain().unwrap().is_empty());
    }
}
