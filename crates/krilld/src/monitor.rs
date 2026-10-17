//! MK-068: 1분 간격 자기 자원 감시. 256MiB를 3회 연속 넘으면 holder 승인 후 main에 종료를 요청한다.
//! 60MiB는 정상 운영 목표이지 재시작 상한이 아니다. 재시작 시각은 상태 디렉터리에 남겨 30분 쿨다운을 지킨다.
//! 세션 생존은 holder가 맡고, 출력 보장은 ARCHITECTURE §4.1의 krilld 크래시 범위와 같다.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc::Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::holder::{FromHolder, ToHolder};

use crate::{registry::Registry, sys};

const INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_LIMIT: u64 = 256 << 20;
const COOLDOWN: u64 = 30 * 60;
const LOG_MAX: u64 = 1_000_000;

/// holder에 연결된 데몬만 감시한다. 실패해도 PTY 리더와 세션은 계속 동작한다.
/// `KRILL_FOOTPRINT_LIMIT_BYTES`는 양수 바이트 수이며 생략·잘못된 값은 256MiB다.
/// 지표는 `<state>/monitor.log`(0600, 1MB마다 한 벌 회전)에 기록한다.
pub fn start(registry: Arc<Registry>, done: Sender<()>) -> io::Result<()> {
    if registry.holder().is_none() {
        return Ok(());
    }
    let Some(dir) = krill_proto::daemon::state_dir() else {
        return Ok(());
    };
    let limit = std::env::var("KRILL_FOOTPRINT_LIMIT_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_LIMIT);
    thread::Builder::new().name("resources".to_owned()).spawn(move || {
        let mut monitor = Monitor::new(dir, limit);
        let mut previous = sys::resources().ok().map(|(_, writes)| (Instant::now(), writes));
        loop {
            thread::sleep(INTERVAL);
            let Ok((footprint, writes)) = sys::resources() else {
                monitor.over = 0; // 측정 실패는 연속 초과로 세지 않는다.
                monitor.log("sample_failed").ok();
                continue;
            };
            let now = Instant::now();
            if let Some((at, total)) = previous {
                monitor.log(&format!(
                    "disk_write elapsed_ms={} bytes={} total_bytes={writes} footprint_bytes={footprint}",
                    now.duration_since(at).as_millis(), writes.saturating_sub(total)
                )).ok();
            }
            previous = Some((now, writes));
            if monitor.exceeded(footprint) {
                // 재기동 후에도 제한을 지킨다. 기록 실패 시 세션을 지키며 계속 운영한다.
                if monitor.reserve(unix_seconds()).unwrap_or(false) {
                    if restart(&registry).is_ok() {
                        monitor.log("restart_requested").ok();
                        done.send(()).ok();
                        return;
                    }
                    monitor.log("restart_unavailable").ok();
                } else {
                    monitor.log("restart_suppressed").ok();
                }
            }
        }
    })?;
    Ok(())
}

pub(crate) fn restart(registry: &Registry) -> io::Result<()> {
    let holder = registry.holder().ok_or(io::ErrorKind::NotConnected)?;
    let path = std::env::current_exe()?
        .as_os_str()
        .as_encoded_bytes()
        .to_vec();
    match holder.request(|req| ToHolder::UpgradeKrilld { req, path })? {
        (FromHolder::Done { .. }, _) => Ok(()),
        _ => Err(io::ErrorKind::NotConnected.into()),
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug)]
struct Monitor {
    dir: PathBuf,
    limit: u64,
    over: u8,
}

impl Monitor {
    fn new(dir: PathBuf, limit: u64) -> Self {
        Self {
            dir,
            limit,
            over: 0,
        }
    }

    fn exceeded(&mut self, footprint: u64) -> bool {
        self.over = if footprint > self.limit {
            self.over.saturating_add(1)
        } else {
            0
        };
        self.over >= 3
    }

    fn reserve(&self, now: u64) -> io::Result<bool> {
        let path = self.dir.join("monitor-restart");
        let last = match fs::read_to_string(&path) {
            Ok(text) => Some(text.trim().parse::<u64>().map_err(io::Error::other)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        // 시계가 뒤로 움직여도 재시작 루프를 막는다.
        if last.is_some_and(|last| now.saturating_sub(last) < COOLDOWN) {
            return Ok(false);
        }
        let temp = self.dir.join("monitor-restart.tmp");
        private_file(&temp)?.write_all(now.to_string().as_bytes())?;
        fs::rename(temp, path)?;
        Ok(true)
    }

    fn log(&self, message: &str) -> io::Result<()> {
        let path = self.dir.join("monitor.log");
        if fs::metadata(&path).is_ok_and(|m| m.len() >= LOG_MAX) {
            fs::rename(&path, self.dir.join("monitor.log.1"))?;
        }
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?
            .write_all(format!("{} {message}\n", unix_seconds()).as_bytes())
    }
}

fn private_file(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn consecutive_samples_and_persistent_cooldown() {
        let dir = std::env::temp_dir().join(format!("mk068-policy-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut m = Monitor::new(dir.clone(), 100);
        for footprint in [101, 101, 100, 101, 101] {
            assert!(!m.exceeded(footprint));
        }
        assert!(m.exceeded(101));
        assert!(m.reserve(10_000).unwrap());
        let restarted = Monitor::new(dir.clone(), 100);
        assert!(!restarted.reserve(10_001).unwrap());
        assert!(!restarted.reserve(9_999).unwrap());
        assert!(!restarted.reserve(11_799).unwrap());
        assert!(restarted.reserve(11_800).unwrap());
        assert_eq!(
            fs::metadata(dir.join("monitor-restart"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn kernel_resources_and_bounded_private_log() {
        let (footprint, _) = sys::resources().unwrap();
        assert!(footprint > 0);
        let dir = std::env::temp_dir().join(format!("mk068-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let m = Monitor::new(dir.clone(), DEFAULT_LIMIT);
        m.log("disk_write elapsed_ms=60000 bytes=4096 total_bytes=8192 footprint_bytes=1")
            .unwrap();
        assert!(
            fs::read_to_string(dir.join("monitor.log"))
                .unwrap()
                .contains("elapsed_ms=60000 bytes=4096")
        );
        assert_eq!(
            fs::metadata(dir.join("monitor.log"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        private_file(&dir.join("monitor.log"))
            .unwrap()
            .set_len(LOG_MAX)
            .unwrap();
        m.log("sample_failed").unwrap();
        assert_eq!(
            fs::metadata(dir.join("monitor.log.1")).unwrap().len(),
            LOG_MAX
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
