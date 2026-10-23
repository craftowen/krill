//! 기존 기록 이행은 세션 생성과 UI 요청 경로에서 기다리지 않는다.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static RUNNING: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

pub(super) fn spawn(profile: &Path, job: impl FnOnce() + Send + 'static) {
    let profile = profile.to_path_buf();
    if !crate::session::lock(&RUNNING).insert(profile.clone()) {
        return;
    }
    let key = profile.clone();
    let result = std::thread::Builder::new()
        .name("account-history".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                tracing::warn!("profile history migration panicked; source data retained");
            }
            crate::session::lock(&RUNNING).remove(&key);
        });
    if let Err(error) = result {
        crate::session::lock(&RUNNING).remove(&profile);
        tracing::warn!(kind = ?error.kind(), "profile history worker failed to start");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn background_job_does_not_block_launch_and_coalesces_the_same_profile() {
        let profile = std::env::temp_dir().join(format!("history-job-{}", std::process::id()));
        let (started, ready) = mpsc::channel();
        let (release, hold) = mpsc::channel();
        let (done, complete) = mpsc::channel();
        spawn(&profile, move || {
            started.send(()).unwrap();
            hold.recv_timeout(Duration::from_secs(5)).unwrap();
            done.send(()).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let (duplicate, received) = mpsc::channel();
        spawn(&profile, move || {
            duplicate.send(()).unwrap();
        });
        assert!(received.try_recv().is_err());
        release.send(()).unwrap();
        complete.recv_timeout(Duration::from_secs(2)).unwrap();
    }
}
