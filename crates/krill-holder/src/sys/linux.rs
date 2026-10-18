//! Linux 대응: holder가 쓰는 kqueue 부분집합(읽기·SIGCHLD·`NOTE_EXIT`)을 epoll·signalfd·pidfd로 흉내 내고,
//! libproc 조회는 `/proc`로 대신한다. 이벤트 루프(`holder.rs`)는 macOS와 같은 코드를 탄다.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::Mutex;
use std::time::Duration;

use rustix::event::epoll;
use rustix::process::{Pid, PidfdFlags, Signal};

/// kqueue API 모양을 그대로 따른 epoll 래퍼(holder가 쓰는 만큼만).
pub mod kq {
    use super::{OwnedFd, Pid, Signal, epoll, io};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EventFilter {
        /// fd 번호(rustix kqueue의 `Read(RawFd)`와 같은 값).
        Read(i32),
        Signal {
            signal: Signal,
            times: usize,
        },
        Proc {
            pid: Pid,
            flags: ProcessEvents,
        },
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EventFlags(u8);

    impl EventFlags {
        pub const ADD: Self = Self(1);
        pub const RECEIPT: Self = Self(2);

        pub(super) fn contains(self, other: Self) -> bool {
            self.0 & other.0 == other.0
        }
    }

    impl std::ops::BitOr for EventFlags {
        type Output = Self;
        fn bitor(self, rhs: Self) -> Self {
            Self(self.0 | rhs.0)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProcessEvents(u8);

    impl ProcessEvents {
        pub const EXIT: Self = Self(1);
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Event {
        pub(super) filter: EventFilter,
        pub(super) flags: EventFlags,
        pub(super) data: isize,
    }

    impl Event {
        pub fn new(filter: EventFilter, flags: EventFlags, _udata: *mut std::ffi::c_void) -> Self {
            Self {
                filter,
                flags,
                data: 0,
            }
        }

        pub fn filter(&self) -> EventFilter {
            self.filter
        }

        /// `EV_RECEIPT` 결과: 0 또는 errno.
        pub fn data(&self) -> isize {
            self.data
        }
    }

    pub fn kqueue() -> io::Result<OwnedFd> {
        Ok(epoll::create(epoll::CreateFlags::CLOEXEC)?)
    }
}

use kq::{Event, EventFilter, EventFlags};

const TAG_READ: u64 = 1 << 32;
const TAG_SIGNAL: u64 = 2 << 32;
const TAG_PROC: u64 = 3 << 32;

/// `NOTE_EXIT`를 건 프로세스의 pidfd. 이벤트가 오면 닫는다(kqueue `NOTE_EXIT`처럼 한 번만 알린다).
/// holder는 단일 스레드라 잠금 경합은 없다. exec 핸드오프 뒤 새 이미지에서는 비어 있고 `restore`가 다시 건다.
static PROCS: Mutex<Vec<(Pid, OwnedFd)>> = Mutex::new(Vec::new());
static SIGNALS: Mutex<Option<OwnedFd>> = Mutex::new(None);

fn poisoned<T>(_: T) -> io::Error {
    io::Error::from(rustix::io::Errno::IO)
}

/// kqueue `kevent`와 같은 계약: `changes`를 등록하고 이벤트를 `out`에 채운다. `EV_RECEIPT` 변경이 있으면
/// 기다리지 않고 등록 결과만 돌려준다.
pub fn wait(
    kq: BorrowedFd<'_>,
    changes: &[Event],
    out: &mut Vec<Event>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    out.clear();
    let mut receipts = false;
    for c in changes {
        let result = register(kq, c.filter);
        if c.flags.contains(EventFlags::RECEIPT) {
            receipts = true;
            let errno = match result {
                Ok(()) => 0,
                Err(e) => e.raw_os_error().unwrap_or(libc::EIO),
            };
            out.push(Event {
                data: isize::try_from(errno).unwrap_or(-1),
                ..*c
            });
        } else {
            result?;
        }
    }
    if receipts {
        return Ok(());
    }
    let ts = timeout.map(|d| rustix::time::Timespec {
        tv_sec: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(d.subsec_nanos()),
    });
    let mut ready = Vec::with_capacity(16);
    match epoll::wait(kq, rustix::buffer::spare_capacity(&mut ready), ts.as_ref()) {
        Ok(_) | Err(rustix::io::Errno::INTR) => {}
        Err(e) => return Err(e.into()),
    }
    for e in ready {
        let tag = e.data.u64();
        let value = tag & 0xffff_ffff;
        let filter = match tag & !0xffff_ffff {
            TAG_READ => EventFilter::Read(i32::try_from(value).unwrap_or(-1)),
            TAG_SIGNAL => {
                drain_signals()?;
                EventFilter::Signal {
                    signal: Signal::CHILD,
                    times: 1,
                }
            }
            TAG_PROC => {
                let Some(pid) = i32::try_from(value).ok().and_then(Pid::from_raw) else {
                    continue;
                };
                PROCS.lock().map_err(poisoned)?.retain(|(p, _)| *p != pid);
                EventFilter::Proc {
                    pid,
                    flags: kq::ProcessEvents::EXIT,
                }
            }
            _ => continue,
        };
        out.push(Event::new(filter, EventFlags::ADD, std::ptr::null_mut()));
    }
    Ok(())
}

fn register(kq: BorrowedFd<'_>, filter: EventFilter) -> io::Result<()> {
    let level = epoll::EventFlags::IN;
    match filter {
        EventFilter::Read(fd) => {
            // SAFETY: 호출자(holder)가 지금 열려 있는 링크 소켓 번호만 넘긴다. 등록하는 동안만 빌린다.
            let fd = unsafe { BorrowedFd::borrow_raw(fd) };
            let tag = TAG_READ | u64::try_from(fd.as_raw_fd()).map_err(|_| poisoned(()))?;
            add(kq, fd, tag, level)
        }
        EventFilter::Signal { .. } => {
            let mut slot = SIGNALS.lock().map_err(poisoned)?;
            if slot.is_none() {
                let fd = sigchld_fd()?;
                add(kq, fd.as_fd(), TAG_SIGNAL, level)?;
                *slot = Some(fd);
            }
            Ok(())
        }
        EventFilter::Proc { pid, .. } => {
            let fd = rustix::process::pidfd_open(pid, PidfdFlags::empty())?;
            let tag = TAG_PROC | u64::from(pid.as_raw_nonzero().get().unsigned_abs());
            add(kq, fd.as_fd(), tag, level)?;
            PROCS.lock().map_err(poisoned)?.push((pid, fd));
            Ok(())
        }
    }
}

/// 이미 등록된 fd를 다시 더하는 것은 kqueue `EV_ADD`처럼 문제없다.
fn add(
    kq: BorrowedFd<'_>,
    fd: BorrowedFd<'_>,
    tag: u64,
    flags: epoll::EventFlags,
) -> io::Result<()> {
    match epoll::add(kq, fd, epoll::EventData::new_u64(tag), flags) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// SIGCHLD를 막고 signalfd로 받는다. 막힌 마스크는 자식에게 가지 않는다(std `Command`가 exec 전에 마스크를 비운다).
fn sigchld_fd() -> io::Result<OwnedFd> {
    let mut set = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: `set`은 이 함수 스택의 쓰기 가능한 sigset_t다. sigemptyset이 초기화한다.
    unsafe { libc::sigemptyset(set.as_mut_ptr()) };
    // SAFETY: 위에서 초기화한 sigset_t다.
    unsafe { libc::sigaddset(set.as_mut_ptr(), libc::SIGCHLD) };
    // SAFETY: 초기화한 sigset_t를 읽기만 하고, 이전 마스크는 받지 않는다(null).
    let r = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, set.as_ptr(), std::ptr::null_mut()) };
    if r != 0 {
        return Err(io::Error::from_raw_os_error(r));
    }
    // SAFETY: 새 signalfd를 만든다(-1). 초기화한 sigset_t를 읽기만 한다.
    let fd = unsafe { libc::signalfd(-1, set.as_ptr(), libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: signalfd가 방금 돌려준 새 fd이고 다른 값이 소유하지 않는다.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// signalfd에 쌓인 SIGCHLD를 비운다(내용은 보지 않는다 — `reap`이 `waitpid` 루프로 모두 거둔다).
fn drain_signals() -> io::Result<()> {
    let slot = SIGNALS.lock().map_err(poisoned)?;
    let Some(fd) = slot.as_ref() else {
        return Ok(());
    };
    let mut buf = [0u8; 128 * 4];
    while matches!(rustix::io::read(fd, &mut buf), Ok(n) if n > 0) {}
    Ok(())
}

/// 프로세스 시작 시각(부팅 뒤 클럭 틱, `/proc/<pid>/stat` 22번째 칸). pid 재사용 판별에만 쓴다. 없는 프로세스면 `None`.
pub fn start_time(pid: Pid) -> Option<u64> {
    stat_field(pid.as_raw_pid(), 22)
}

/// `pid`의 직계 자식들(`/proc/*/stat`의 부모 칸). 실패하면 빈 목록.
pub fn children(pid: Pid) -> Vec<Pid> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let want = u64::from(pid.as_raw_nonzero().get().unsigned_abs());
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&p| stat_field(p, 4) == Some(want))
        .filter_map(Pid::from_raw)
        .collect()
}

/// `/proc/<pid>/stat`의 `n`번째 칸(1부터, man proc). 2번째 칸(comm)에 공백·괄호가 올 수 있어 마지막 `)` 뒤부터 센다.
fn stat_field(pid: i32, n: usize) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = text.get(text.rfind(')')?.checked_add(1)?..)?;
    rest.split_ascii_whitespace()
        .nth(n.checked_sub(3)?)?
        .parse()
        .ok()
}

/// 띄운 쪽에게서 물려받은 fd 3 이상을 모두 닫는다(OWE-485, macOS 판과 같은 계약). `close_range`(Linux 5.9+).
pub fn close_inherited() -> io::Result<()> {
    // SAFETY: `main` 첫머리라 이 프로세스의 어떤 값도 3 이상 fd를 소유하지 않는다. 단일 스레드다.
    if unsafe { libc::close_range(3, libc::c_uint::MAX, 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_start_time_is_stable_and_parent_lists_us() {
        let me = rustix::process::getpid();
        let t = start_time(me);
        assert!(t.is_some());
        assert_eq!(start_time(me), t);
        let parent = rustix::process::getppid().expect("parent");
        assert!(children(parent).contains(&me));
    }
}
