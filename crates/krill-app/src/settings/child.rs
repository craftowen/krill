//! Bounded management commands. Only the directly spawned child is owned here.
use std::io;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub(crate) fn output(cmd: &mut Command, timeout: Duration, limit: u64) -> io::Result<Output> {
    let deadline = Instant::now() + timeout;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing child stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing child stderr"))?;
        let out = crate::process_io::read(stdout, limit.saturating_add(1), deadline)?;
        let err = crate::process_io::read(stderr, 65537, deadline)?;
        let status = loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "command timed out; result may be unknown",
                ));
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let stdout = out.finish(deadline)?;
        let stderr = err.finish(deadline)?;
        if stdout.timed_out || stderr.timed_out {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command pipe deadline exceeded; result may be unknown",
            ));
        }
        if u64::try_from(stdout.bytes.len()).unwrap_or(u64::MAX) > limit
            || stderr.bytes.len() > 65536
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "command output exceeded limit; result may be unknown",
            ));
        }
        Ok(Output {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    })();
    if result.is_err() {
        child.kill().ok();
        child.wait().ok();
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn output_cap_and_total_deadline() {
        let out = output(
            Command::new("/bin/sh").args(["-c", "printf ready"]),
            Duration::from_secs(2),
            8,
        )
        .unwrap();
        assert_eq!(out.stdout, b"ready");
        assert_eq!(
            output(
                Command::new("/bin/sh").args(["-c", "printf too-large"]),
                Duration::from_secs(2),
                4
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            output(
                Command::new("/bin/sh").args(["-c", "while :; do :; done"]),
                Duration::from_millis(30),
                8
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }
    #[cfg(unix)]
    #[test]
    fn descendant_held_management_pipe_respects_total_deadline() {
        let result = output(
            Command::new("/bin/sh").args(["-c", "sleep 0.2 & printf ready"]),
            Duration::from_millis(30),
            64,
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}
