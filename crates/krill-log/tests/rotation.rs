//! Real files and independent processes exercise the shared 20 MB invariant.
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use krill_log::{Component, FILE_BYTES, RECORD_BYTES, RollingLog, TOTAL_BYTES};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);
impl Directory {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "mk069-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn total(dir: &Directory) -> io::Result<u64> {
    fs::read_dir(&dir.0)?.try_fold(0, |sum, entry| match entry?.metadata() {
        Ok(metadata) => Ok(sum + metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(sum),
        Err(error) => Err(error),
    })
}

#[test]
fn rotates_before_limit_and_retains_latest_after_restart() -> io::Result<()> {
    let dir = Directory::new()?;
    let log = RollingLog::open(&dir.0, Component::Daemon)?;
    let record = "한글🙂".repeat(RECORD_BYTES);
    for _ in 0..1_000 {
        log.append(&record)?;
    }
    let size = total(&dir)?;
    assert!(size > FILE_BYTES * 4);
    assert!(size <= FILE_BYTES * 5);
    drop(log);
    let log = RollingLog::open(&dir.0, Component::Daemon)?;
    log.append("latest after restart")?;
    let active = fs::read_to_string(dir.0.join("krilld.log"))?;
    assert!(active.ends_with("latest after restart\n"));
    for entry in fs::read_dir(&dir.0)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        assert!(metadata.len() <= FILE_BYTES);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert!(fs::read_to_string(entry.path()).is_ok());
    }
    assert_eq!(fs::metadata(&dir.0)?.permissions().mode() & 0o777, 0o700);
    Ok(())
}

#[test]
fn lock_contention_drops_event_and_recovers() -> io::Result<()> {
    let dir = Directory::new()?;
    let log = RollingLog::open(&dir.0, Component::Cli)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .open(dir.0.join("krill.lock"))?;
    lock.lock()?;
    log.append("dropped")?;
    assert_eq!(total(&dir)?, 0);
    lock.unlock()?;
    log.append("recovered")?;
    assert_eq!(fs::read_to_string(dir.0.join("krill.log"))?, "recovered\n");
    Ok(())
}

#[test]
fn startup_repairs_oversize_and_refuses_file_symlinks() -> io::Result<()> {
    let dir = Directory::new()?;
    File::create(dir.0.join("krill-app.log.3"))?.set_len(FILE_BYTES + 10)?;
    let log = RollingLog::open(&dir.0, Component::App)?;
    assert_eq!(total(&dir)?, FILE_BYTES);
    let victim = dir.0.join("unrelated");
    fs::write(&victim, "preserve")?;
    fs::remove_file(dir.0.join("krill-app.log"))?;
    symlink(&victim, dir.0.join("krill-app.log"))?;
    assert!(log.append("do not follow").is_err());
    assert_eq!(fs::read_to_string(victim)?, "preserve");
    Ok(())
}

#[test]
fn tracing_filters_external_events_and_initialization_is_fail_open() -> io::Result<()> {
    let dir = Directory::new()?;
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", "subprocess_writer", "--nocapture"])
        .env("KRILL_LOG_DIR", &dir.0)
        .env("MK069_TRACE", "1")
        .stdout(Stdio::null())
        .status()?;
    assert!(output.success());
    let active = fs::read_to_string(dir.0.join("krill.log"))?;
    assert!(active.contains("tracing works"));
    assert!(active.contains("count=7"));
    assert!(!active.contains("external private text"));
    assert!(!active.contains("debug disabled"));
    let blocker = dir.0.join("not-a-directory");
    fs::write(&blocker, "file")?;
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", "subprocess_writer", "--nocapture"])
        .env("KRILL_LOG_DIR", blocker)
        .env("MK069_TRACE", "1")
        .stdout(Stdio::null())
        .status()?;
    assert!(status.success());
    Ok(())
}

#[test]
fn subprocess_writer() -> io::Result<()> {
    if std::env::var_os("MK069_TRACE").is_some() {
        krill_log::init(Component::Cli).ok();
        tracing::info!(target: "krill", count = 7, "tracing works");
        tracing::debug!(target: "krill", "debug disabled");
        tracing::error!(target: "other", "external private text");
    } else if let Some(name) = std::env::var_os("MK069_COMPONENT") {
        let Some(component) = Component::ALL.into_iter().find(|c| c.name() == name) else {
            return Err(io::Error::other("unknown component"));
        };
        let dir =
            std::env::var_os("KRILL_LOG_DIR").ok_or_else(|| io::Error::other("missing log dir"))?;
        let log = RollingLog::open(PathBuf::from(dir), component)?;
        let record = "load".repeat(64);
        for _ in 0..21_600 {
            log.append(&record)?;
        }
    }
    Ok(())
}

#[test]
fn accelerated_day_with_concurrent_processes_and_restart() -> io::Result<()> {
    let dir = Directory::new()?;
    // Each phase launches two competing OS processes per family. Two phases simulate
    // 86,400 event attempts/family (one per second for 24 h), with a process restart halfway.
    // This verifies byte/event pressure, not elapsed wall-clock time.
    for _ in 0..2 {
        let mut children = Vec::new();
        for component in Component::ALL {
            for _ in 0..2 {
                children.push(
                    Command::new(std::env::current_exe()?)
                        .args(["--exact", "subprocess_writer", "--nocapture"])
                        .env("KRILL_LOG_DIR", &dir.0)
                        .env("MK069_COMPONENT", component.name())
                        .stdout(Stdio::null())
                        .spawn()?,
                );
            }
        }
        // Inspect while writers are active: rotation must not temporarily exceed the budget.
        loop {
            assert!(total(&dir)? <= TOTAL_BYTES);
            let mut finished = true;
            for child in &mut children {
                if let Some(status) = child.try_wait()? {
                    assert!(status.success());
                } else {
                    finished = false;
                }
            }
            if finished {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    let bytes = total(&dir)?;
    assert!(bytes > FILE_BYTES * 4);
    assert!(bytes <= TOTAL_BYTES);
    for entry in fs::read_dir(&dir.0)? {
        assert!(entry?.metadata()?.len() <= FILE_BYTES);
    }
    eprintln!(
        "MK-069 accelerated day: 345600 attempts, 16 child processes, {bytes} retained bytes (limit {TOTAL_BYTES})"
    );
    Ok(())
}
