//! Deadline-aware child pipes. Killing the direct child does not close a pipe
//! inherited by its descendants, so never join a blocking pipe reader/writer.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const MAX_PIPE_WORKERS: usize = 16;
static PIPE_WORKERS: AtomicUsize = AtomicUsize::new(0);

struct Slot;
impl Drop for Slot {
    fn drop(&mut self) {
        PIPE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(crate) struct PipeTask<T>(mpsc::Receiver<io::Result<T>>, Arc<AtomicBool>);

impl<T> Drop for PipeTask<T> {
    fn drop(&mut self) {
        self.1.store(true, Ordering::Release);
    }
}

impl<T: Send + 'static> PipeTask<T> {
    pub(crate) fn spawn(work: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<Self> {
        Self::spawn_cancellable(move |_| work())
    }

    fn spawn_cancellable(
        work: impl FnOnce(Arc<AtomicBool>) -> io::Result<T> + Send + 'static,
    ) -> io::Result<Self> {
        PIPE_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_PIPE_WORKERS).then_some(count + 1)
            })
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "child pipe workers busy"))?;
        let slot = Slot;
        let (send, recv) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        std::thread::Builder::new()
            .name("child-pipe".into())
            .spawn(move || {
                let _slot = slot;
                send.send(work(worker_cancelled)).ok();
            })?;
        Ok(Self(recv, cancelled))
    }

    pub(crate) fn try_finish(&self) -> Option<io::Result<T>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(io::Error::other("child pipe worker stopped")))
            }
        }
    }

    pub(crate) fn finish(self, deadline: Instant) -> io::Result<T> {
        self.0
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    io::Error::new(io::ErrorKind::TimedOut, "child pipe deadline exceeded")
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::other("child pipe worker stopped")
                }
            })?
    }
}

#[derive(Debug)]
pub(crate) struct PipeRead {
    pub bytes: Vec<u8>,
    pub timed_out: bool,
}

#[cfg(unix)]
pub(crate) trait PipeHandle: std::os::fd::AsFd + Send {}
#[cfg(unix)]
impl<T: std::os::fd::AsFd + Send> PipeHandle for T {}
#[cfg(not(unix))]
pub(crate) trait PipeHandle: Send {}
#[cfg(not(unix))]
impl<T: Send> PipeHandle for T {}

#[cfg(unix)]
fn nonblocking(pipe: &impl PipeHandle) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(pipe)?;
    rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Capture {
    Head(usize),
    Tail(usize),
}

pub(crate) fn read(
    pipe: impl Read + PipeHandle + 'static,
    limit: u64,
    deadline: Instant,
) -> io::Result<PipeTask<PipeRead>> {
    read_captured(
        pipe,
        Capture::Head(usize::try_from(limit).unwrap_or(usize::MAX)),
        deadline,
    )
}

/// 파이프를 EOF까지 소비하되 메모리에는 마지막 limit 바이트만 남긴다.
pub(crate) fn read_tail(
    pipe: impl Read + PipeHandle + 'static,
    limit: usize,
    deadline: Instant,
) -> io::Result<PipeTask<PipeRead>> {
    read_captured(pipe, Capture::Tail(limit), deadline)
}

fn read_captured(
    mut pipe: impl Read + PipeHandle + 'static,
    capture: Capture,
    deadline: Instant,
) -> io::Result<PipeTask<PipeRead>> {
    #[cfg(unix)]
    nonblocking(&pipe)?;
    PipeTask::spawn_cancellable(move |cancelled| {
        let limit = match capture {
            Capture::Head(limit) | Capture::Tail(limit) => limit,
        };
        let mut bytes = Vec::new();
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if Instant::now() >= deadline {
                return Ok(PipeRead {
                    bytes,
                    timed_out: true,
                });
            }
            let mut buffer = [0; 4096];
            let count = match capture {
                Capture::Head(_) => buffer.len().min(limit.saturating_sub(bytes.len())),
                Capture::Tail(_) => buffer.len(),
            };
            if count == 0 {
                return Ok(PipeRead {
                    bytes,
                    timed_out: false,
                });
            }
            match pipe.read(&mut buffer[..count]) {
                Ok(0) => {
                    return Ok(PipeRead {
                        bytes,
                        timed_out: false,
                    });
                }
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    if matches!(capture, Capture::Tail(_)) && bytes.len() > limit {
                        bytes.drain(..bytes.len() - limit);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error),
            }
        }
    })
}

pub(crate) fn write(
    mut pipe: impl Write + PipeHandle + 'static,
    bytes: Vec<u8>,
    deadline: Instant,
) -> io::Result<PipeTask<()>> {
    #[cfg(unix)]
    nonblocking(&pipe)?;
    PipeTask::spawn_cancellable(move |cancelled| {
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            if cancelled.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child stdin deadline exceeded",
                ));
            }
            match pipe.write(remaining) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })
}

// Windows std child pipes can block inside read/write. The waiting caller still
// has a deadline, and admission is held until the native IO worker actually exits:
// repeated timed-out jobs cannot create unbounded detached threads.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_io_returns_at_deadline_without_joining() {
        let (release, held) = mpsc::channel();
        let task = PipeTask::spawn(move || {
            held.recv().ok();
            Ok(())
        })
        .unwrap();
        let result = task.finish(Instant::now());
        release.send(()).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn descendant_retaining_stdout_does_not_extend_deadline() {
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 2>/dev/null & echo $! >&2; printf ready"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        let reader = read(child.stdout.take().unwrap(), 128, deadline).unwrap();
        let mut descendant = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut descendant)
            .unwrap();
        child.wait().unwrap();
        let result = reader.finish(deadline + Duration::from_millis(100));
        // Always release the isolated test descendant before checking the result.
        Command::new("/bin/kill")
            .arg(descendant.trim())
            .status()
            .unwrap();
        let output = result.unwrap();
        assert!(output.timed_out);
        assert_eq!(output.bytes, b"ready");
    }
}
