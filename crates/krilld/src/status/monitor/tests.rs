use super::*;

fn tracked(pid: u32, start: u64) -> Tracked {
    Tracked {
        process: Some((pid, start)),
        ..Tracked::default()
    }
}

#[test]
fn missed_exit_and_reused_pid_are_detected_without_kernel_events() {
    let now = Instant::now();
    for result in [
        Err(io::Error::from_raw_os_error(libc::ESRCH)),
        Err(io::ErrorKind::NotFound.into()),
        Ok(101),
    ] {
        let mut agent = tracked(42, 100);
        assert!(agent.ended(&[], now, |pid| {
            assert_eq!(pid, 42);
            result
        }));
        assert!(agent.process.is_none());
        assert!(!agent.ended(&[42], now, |_| panic!("already ended")));
    }
}

#[test]
fn live_background_agent_and_transient_lookup_errors_are_preserved() {
    let now = Instant::now();
    for result in [
        Ok(100),
        Err(io::ErrorKind::PermissionDenied.into()),
        Err(io::ErrorKind::Interrupted.into()),
    ] {
        let mut agent = tracked(42, 100);
        assert!(!agent.ended(&[], now, |_| result));
        assert_eq!(agent.process, Some((42, 100)));
        assert!(!agent.ended(&[], now, |_| panic!("polling is rate limited")));
        assert!(agent.ended(&[], now + PROCESS_POLL, |_| {
            Err(io::ErrorKind::NotFound.into())
        }));
    }
}

#[test]
fn kernel_exit_bypasses_polling_interval() {
    let now = Instant::now();
    let mut agent = tracked(42, 100);
    assert!(!agent.ended(&[], now, |_| Ok(100)));
    assert!(agent.ended(&[42], now, |_| Ok(100)));
    assert!(agent.process.is_none());
}

#[cfg(unix)]
#[test]
fn reaped_process_is_detected_when_no_exit_watch_is_available() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let identity = crate::sys::start_us(child.id());
    child.kill().unwrap();
    child.wait().unwrap();
    let mut agent = tracked(child.id(), identity.unwrap());
    assert!(agent.ended(&[], Instant::now(), crate::sys::start_us));
}
