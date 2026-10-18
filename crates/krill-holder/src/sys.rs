//! holder의 unsafe 전부: fork~exec 사이 훅, kevent, exec 핸드오프로 넘어온 fd 되찾기, 물려받은 fd 닫기, libproc 조회.
#![expect(
    unsafe_code,
    reason = "pre_exec·kevent·from_raw_fd는 rustix/std에서, libproc은 libc FFI로 unsafe API다"
)]

use std::io;
#[cfg(target_os = "macos")]
use std::os::fd::BorrowedFd;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
#[cfg(target_os = "macos")]
use std::time::Duration;

use krill_proto::SessionId;
#[cfg(target_os = "macos")]
use rustix::event::kqueue::{Event, kevent};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::Pid;

use crate::holder::Session;

#[cfg(target_os = "macos")]
pub use rustix::event::kqueue as kq;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{children, close_inherited, kq, start_time, wait};

/// 자식이 새 세션을 만들고(setsid) stdin(PTY slave)을 제어 터미널로 잡게 한다(MK-004 결정 3).
pub fn set_controlling_tty(cmd: &mut Command) {
    let hook = || {
        rustix::process::setsid()?;
        // fd 0은 std가 dup2한 PTY slave다.
        rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
        Ok(())
    };
    // SAFETY: 훅은 setsid·ioctl 시스템 호출만 한다(async-signal-safe, 할당·락 없음, CONVENTIONS U7).
    unsafe {
        cmd.pre_exec(hook);
    }
}

#[cfg(target_os = "macos")]
/// kqueue에서 이벤트를 기다린다.
pub fn wait(
    kq: BorrowedFd<'_>,
    changes: &[Event],
    out: &mut Vec<Event>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    out.clear();
    // SAFETY: 등록하는 fd 필터는 krilld 링크 소켓뿐이다. 그 fd가 닫히면 커널이 knote를 지우므로
    // (kqueue(2) "Calling close() … will remove any kevents that reference the descriptor")
    // 닫힌 fd를 가리키는 이벤트가 남지 않는다.
    match unsafe { kevent(kq, changes, rustix::buffer::spare_capacity(out), timeout) } {
        Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// exec 핸드오프 상태를 새 이미지에 넘기는 환경변수. krilld·세션에는 넘기지 않는다.
///
/// 형식(공백으로 나눈 10진수 줄): 첫 줄 `req krilld_pid link_fd listener_fd(-1=없음) streak upgrading(0|1)`,
/// 이어서 세션마다 `id pid master_fd [status]`(status는 끝난 세션만, pid가 음수면 `Adopt`한 세션).
/// 쓰는 쪽은 `Holder::exec_self`. 이름은 영구다: 운영 중인 구 holder가 이 이름으로 새 이미지를 exec한다(개명 감사).
pub const STATE_ENV: &str = "MK_HOLDER_STATE";
/// 개명 도중 잠시 쓴 이름. 그때 빌드된 운영 holder가 이 이름으로 exec하므로 읽기만 한다.
pub const STATE_ENV_ALT: &str = "KRILL_HOLDER_STATE";

/// 앞 이미지에게서 물려받은 holder 상태.
#[derive(Debug)]
pub struct Handoff {
    pub req: u32,
    pub krilld: Pid,
    pub link: OwnedFd,
    pub listener: Option<OwnedFd>,
    pub streak: u32,
    pub upgrading: bool,
    pub sessions: Vec<Session>,
}

/// [`STATE_ENV`]가 있으면 상태를 풀고 거기 적힌 fd를 되찾는다. 프로세스 시작 때 한 번만 부른다.
pub fn take_handoff() -> io::Result<Option<Handoff>> {
    let Some(state) = [STATE_ENV, STATE_ENV_ALT].iter().find_map(std::env::var_os) else {
        return Ok(None);
    };
    let rows = state
        .to_str()
        .ok_or_else(bad)?
        .lines()
        .map(|l| {
            l.split(' ')
                .map(|s| s.parse::<i64>().map_err(|_| bad()))
                .collect()
        })
        .collect::<io::Result<Vec<Vec<i64>>>>()?;
    let Some((&[req, kpid, link, listener, streak, upgrading], rows)) =
        rows.split_first().map(|(h, r)| (h.as_slice(), r))
    else {
        return Err(bad());
    };
    let mut fds: Vec<i64> = rows.iter().filter_map(|r| r.get(2).copied()).collect();
    fds.push(link);
    fds.extend((listener >= 0).then_some(listener));
    fds.sort_unstable();
    if fds.first().is_none_or(|fd| *fd <= 2) || fds.windows(2).any(|w| w.first() == w.last()) {
        return Err(bad()); // 표준 입출력이거나 같은 번호를 두 번 소유하면 이중 close가 된다
    }
    let mut sessions = Vec::new();
    for r in rows {
        let (id, pid, fd, status) = match *r.as_slice() {
            [id, pid, fd] => (id, pid, fd, None),
            [id, pid, fd, st] => (id, pid, fd, Some(i32::try_from(st).map_err(|_| bad())?)),
            _ => return Err(bad()),
        };
        sessions.push(Session {
            id: SessionId(u64::try_from(id).map_err(|_| bad())?),
            pid: pid_of(pid.unsigned_abs())?,
            master: adopt(fd)?,
            status,
            adopted: pid < 0,
        });
    }
    Ok(Some(Handoff {
        req: u32::try_from(req).map_err(|_| bad())?,
        krilld: pid_of(kpid.unsigned_abs())?,
        link: adopt(link)?,
        listener: (listener >= 0).then(|| adopt(listener)).transpose()?,
        streak: u32::try_from(streak).map_err(|_| bad())?,
        upgrading: upgrading == 1,
        sessions,
    }))
}

#[cfg(target_os = "macos")]
/// 프로세스 시작 시각(µs). pid 재사용을 가려내는 정체성으로 쓴다. 없는 프로세스면 `None`.
pub fn start_time(pid: Pid) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: 커널이 `info`에 최대 `size`바이트를 쓴다. 반환값이 구조체 크기와 같을 때만 읽는다.
    let n = unsafe {
        libc::proc_pidinfo(
            pid.as_raw_pid(),
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    // SAFETY: 위에서 커널이 구조체 전체를 채웠음을 확인했다.
    let info = unsafe { info.assume_init() };
    Some(
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
    )
}

#[cfg(target_os = "macos")]
/// `pid`의 직계 자식들. 실패하면 빈 목록.
/// ponytail: 자식 1024개에서 자른다. 그보다 많은 프로세스를 거느린 세션이 나오면 반환값이 버퍼를 채울 때 키워 다시 부른다.
pub fn children(pid: Pid) -> Vec<Pid> {
    let mut buf = vec![0 as libc::pid_t; 1024];
    let Ok(size) = libc::c_int::try_from(buf.len() * size_of::<libc::pid_t>()) else {
        return Vec::new();
    };
    // SAFETY: `buf`는 `size`바이트짜리 pid_t 배열이다. 반환값은 채운 pid 개수다(이 맥에서 실측, OWE-416).
    let n = unsafe { libc::proc_listchildpids(pid.as_raw_pid(), buf.as_mut_ptr().cast(), size) };
    buf.truncate(usize::try_from(n).unwrap_or(0));
    buf.into_iter().filter_map(Pid::from_raw).collect()
}

#[cfg(target_os = "macos")]
/// 띄운 쪽에게서 물려받은 fd 3 이상을 모두 닫는다(OWE-485). `--detach` 새 기동에서 `main` 첫머리에 한 번만 부른다.
///
/// macOS std는 `pipe()` 뒤 CLOEXEC를 따로 걸고 `posix_spawn`에 `CLOEXEC_DEFAULT`를 주지 않는다. 그래서 다중
/// 스레드 기동자(앱)가 holder를 띄우는 순간 다른 스레드가 만든 CLOEXEC 전 fd가 holder로 새고, 닫지 않으면
/// krilld·세션까지 데몬 수명 내내 남의 fd를 쥔다. launchd 경로는 0~2만 주고, 핸드오프 fd는 [`STATE_ENV`]로
/// 넘어오므로(`--detach` 없음) 이 함수와 겹치지 않는다.
pub fn close_inherited() -> io::Result<()> {
    let me = rustix::process::getpid().as_raw_pid();
    // SAFETY: 버퍼 없이 부르면 커널은 아무것도 쓰지 않고 필요한 바이트 수만 돌려준다(libproc 관례).
    let need = unsafe { libc::proc_pidinfo(me, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    let n = usize::try_from(need).map_err(|_| io::Error::last_os_error())?
        / size_of::<libc::proc_fdinfo>();
    let mut buf = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        n
    ];
    let size =
        libc::c_int::try_from(buf.len() * size_of::<libc::proc_fdinfo>()).map_err(|_| bad())?;
    // SAFETY: `buf`는 `size`바이트짜리 proc_fdinfo 배열이다. 커널은 최대 `size`바이트를 쓰고 쓴 바이트 수를 돌려준다.
    let got =
        unsafe { libc::proc_pidinfo(me, libc::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), size) };
    buf.truncate(
        usize::try_from(got).map_err(|_| io::Error::last_os_error())?
            / size_of::<libc::proc_fdinfo>(),
    );
    for fd in buf.iter().map(|f| f.proc_fd).filter(|fd| *fd > 2) {
        // SAFETY: `main` 첫머리라 이 프로세스의 어떤 값도 3 이상 fd를 소유하지 않는다(std 런타임은 fd를 쥐지
        // 않는다). 단일 스레드라 목록을 읽은 뒤 새 fd가 생기지 않는다.
        unsafe { libc::close(fd) };
    }
    Ok(())
}

fn adopt(fd: i64) -> io::Result<OwnedFd> {
    let fd = i32::try_from(fd).map_err(|_| bad())?;
    // SAFETY: 앞 이미지가 CLOEXEC를 풀고 STATE_ENV에 적어 넘긴 fd다. 시작 때 한 번만 불리고
    // (`take_handoff`), 번호가 겹치지 않고 0~2가 아님을 위에서 확인했다. 이 프로세스의 다른 값은
    // 이 번호를 소유하지 않는다.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    fcntl_setfd(&fd, FdFlags::CLOEXEC)?; // 닫힌 번호면 EBADF
    Ok(fd)
}

fn pid_of(v: u64) -> io::Result<Pid> {
    Pid::from_raw(i32::try_from(v).map_err(|_| bad())?).ok_or_else(bad)
}

fn bad() -> io::Error {
    io::Error::from(rustix::io::Errno::INVAL)
}
