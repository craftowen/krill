//! Full-disk resilient background state persistence writer (Herdr `a8ed0c3` / `3447942`).
//!
//! - Coalesces rapid mutations so only the newest snapshot is written to disk.
//! - On write failure (e.g. `ENOSPC` / full disk), the writer thread NEVER stops or drops
//!   the latest snapshot; it records the error for [`StateWriter::flush_saved`], retries
//!   with exponential backoff (`100ms -> 5s`), and automatically persists once space returns.
//! - Callers that must return observed session results (`send`, `wait`, `keys`, `list`,
//!   shutdown) use [`StateWriter::flush_observed`] so a full disk never breaks live sessions.
//! - All failure logging uses panic-safe `let _ = writeln!(io::stderr(), ...)`.

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FIRST_RETRY: Duration = Duration::from_millis(100);
const MAX_RETRY: Duration = Duration::from_secs(5);

struct WriterState {
    submitted: u64,
    latest: Option<(u64, Vec<u8>)>,
    saved: u64,
    last_error: Option<(u64, io::ErrorKind, String)>,
    closed: bool,
}

impl WriterState {
    fn new() -> Self {
        Self {
            submitted: 0,
            latest: None,
            saved: 0,
            last_error: None,
            closed: false,
        }
    }
}

/// Resilient single-file atomic persistence worker.
pub(crate) struct StateWriter {
    label: &'static str,
    queue: Arc<(Mutex<WriterState>, Condvar)>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for StateWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateWriter")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl StateWriter {
    /// Starts a writer persisting snapshots atomically to `path` (`0600`).
    pub(crate) fn new(label: &'static str, path: PathBuf) -> io::Result<Self> {
        Self::start_with_retry(label, FIRST_RETRY, MAX_RETRY, move |bytes| {
            krill_proto::os::write_atomic_0600(&path, bytes)
        })
    }

    pub(crate) fn start_with_retry(
        label: &'static str,
        first_retry: Duration,
        max_retry: Duration,
        mut write: impl FnMut(&[u8]) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let queue = Arc::new((Mutex::new(WriterState::new()), Condvar::new()));
        let work = Arc::clone(&queue);
        let worker = thread::Builder::new()
            .name(format!("state-writer-{label}"))
            .stack_size(256 * 1024)
            .spawn(move || {
                let (mutex, changed) = &*work;
                let mut state = mutex.lock().unwrap_or_else(PoisonError::into_inner);
                let mut retry = first_retry;
                loop {
                    if let Some((version, bytes)) = state.latest.take() {
                        state.last_error = None;
                        drop(state);
                        let result = write(&bytes);
                        if let Err(err) = &result {
                            tracing::warn!(
                                target = label,
                                kind = ?err.kind(),
                                "state persistence failed; retrying"
                            );
                            writeln!(io::stderr(), "krilld: cannot save {label}: {err}").ok();
                        }
                        state = mutex.lock().unwrap_or_else(PoisonError::into_inner);
                        match result {
                            Ok(()) => {
                                state.saved = state.saved.max(version);
                                state.last_error = None;
                                retry = first_retry;
                                changed.notify_all();
                            }
                            Err(err) => {
                                state.last_error = Some((version, err.kind(), err.to_string()));
                                changed.notify_all();
                                if !state.closed {
                                    // Preserve this snapshot unless a newer save superseded it.
                                    state.latest.get_or_insert((version, bytes));
                                    let wait_result = changed
                                        .wait_timeout_while(state, retry, |s| !s.closed)
                                        .unwrap_or_else(PoisonError::into_inner);
                                    state = wait_result.0;
                                    retry = retry.saturating_mul(2).min(max_retry);
                                }
                            }
                        }
                    } else if state.closed {
                        break;
                    } else {
                        state = changed.wait(state).unwrap_or_else(PoisonError::into_inner);
                    }
                }
            })?;
        Ok(Self {
            label,
            queue,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Enqueues the newest complete snapshot for background persistence.
    pub(crate) fn save(&self, bytes: Vec<u8>) -> u64 {
        let (mutex, changed) = &*self.queue;
        let mut state = mutex.lock().unwrap_or_else(PoisonError::into_inner);
        state.submitted = state.submitted.saturating_add(1);
        let version = state.submitted;
        state.latest = Some((version, bytes));
        changed.notify_all();
        version
    }

    /// Cancels a pending retry for `failed_version` if no newer snapshot superseded it.
    /// Used when a caller rolls back an uncommitted mutation after `flush_saved` returned `Err`.
    pub(crate) fn discard_if_current(&self, failed_version: u64) {
        let (mutex, changed) = &*self.queue;
        let mut state = mutex.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .latest
            .as_ref()
            .is_some_and(|(v, _)| *v == failed_version)
        {
            state.latest = None;
            changed.notify_all();
        }
    }

    /// Waits until `target` (or latest submitted version) is durably saved or fails its attempt.
    /// Returns an honest I/O error if the write failed, while keeping the writer alive to retry.
    pub(crate) fn flush_saved(&self, target: Option<u64>, timeout: Duration) -> io::Result<()> {
        let (mutex, changed) = &*self.queue;
        let mut state = mutex.lock().unwrap_or_else(PoisonError::into_inner);
        let version = target.unwrap_or(state.submitted);
        if version == 0 {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        loop {
            if state.saved >= version {
                return Ok(());
            }
            if let Some((failed_ver, kind, ref msg)) = state.last_error
                && failed_ver >= version
            {
                return Err(io::Error::new(kind, msg.clone()));
            }
            if state.closed {
                return Err(io::Error::other(format!(
                    "state writer '{}' stopped before saving",
                    self.label
                )));
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("state persistence '{}' timed out", self.label),
                ));
            };
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("state persistence '{}' timed out", self.label),
                ));
            }
            let (guard, wait_res) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            if wait_res.timed_out() && state.saved < version {
                if let Some((failed_ver, kind, ref msg)) = state.last_error
                    && failed_ver >= version
                {
                    return Err(io::Error::new(kind, msg.clone()));
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("state persistence '{}' timed out", self.label),
                ));
            }
        }
    }

    /// Waits for the latest persistence attempt to settle without failing the caller when the disk is full.
    pub(crate) fn flush_observed(&self, timeout: Duration) -> io::Result<()> {
        match self.flush_saved(None, timeout) {
            Err(err) if err.kind() == io::ErrorKind::TimedOut => Err(err),
            Ok(()) | Err(_) => Ok(()),
        }
    }

    /// Attempts a bounded final flush and cleanly stops the worker thread.
    pub(crate) fn shutdown(&self, timeout: Duration) {
        if let Err(err) = self.flush_saved(None, timeout) {
            writeln!(
                io::stderr(),
                "krilld: final flush of {} did not persist: {err}",
                self.label
            )
            .ok();
        }
        let (mutex, changed) = &*self.queue;
        {
            let mut state = mutex.lock().unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            changed.notify_all();
        }
        if let Some(handle) = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            handle.join().ok();
        }
    }
}

impl Drop for StateWriter {
    fn drop(&mut self) {
        let (mutex, changed) = &*self.queue;
        {
            let mut state = mutex.lock().unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            changed.notify_all();
        }
        if let Some(handle) = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            handle.join().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn failed_write_reports_honest_error_keeps_writer_alive_and_recovers_newest_snapshot() {
        let (started_tx, started_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel::<io::Result<()>>();
        let writer = StateWriter::start_with_retry(
            "test-state",
            Duration::from_millis(15),
            Duration::from_millis(40),
            move |bytes| {
                started_tx.send(bytes.to_vec()).unwrap();
                reply_rx.recv_timeout(Duration::from_secs(5)).unwrap()
            },
        )
        .unwrap();

        let v1 = writer.save(vec![1]);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            [1]
        );

        // Coalesce newer snapshots while v1 is blocked on disk I/O.
        let _v2 = writer.save(vec![2]);
        let v3 = writer.save(vec![3]);

        // Simulate ENOSPC on v1: flush_saved returns honest StorageFull error.
        reply_tx
            .send(Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "No space left on device",
            )))
            .unwrap();
        let err = writer
            .flush_saved(Some(v1), Duration::from_secs(2))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);

        // Writer stays alive and automatically retries the newest coalesced snapshot [3].
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            [3]
        );
        // A second ENOSPC on [3] must still let flush_observed succeed without failing callers.
        reply_tx
            .send(Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "No space left on device",
            )))
            .unwrap();
        assert!(writer.flush_observed(Duration::from_secs(2)).is_ok());

        // Writer retries [3] again; once space returns, flush_saved confirms durability.
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            [3]
        );
        reply_tx.send(Ok(())).unwrap();
        writer
            .flush_saved(Some(v3), Duration::from_secs(2))
            .unwrap();
        writer.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn repeated_full_disk_retries_same_snapshot_until_space_returns() {
        let (attempts_tx, attempts_rx) = mpsc::channel();
        let fail_remaining = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let counter = Arc::clone(&fail_remaining);
        let writer = StateWriter::start_with_retry(
            "retry-same",
            Duration::from_millis(15),
            Duration::from_millis(30),
            move |bytes| {
                attempts_tx.send(bytes.to_vec()).unwrap();
                if counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                    Err(io::Error::new(
                        io::ErrorKind::StorageFull,
                        "simulated ENOSPC",
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();

        let v1 = writer.save(b"important-state".to_vec());
        // Wait for all 3 attempts (2 ENOSPC failures + 1 recovery success)
        for _ in 0..3 {
            assert_eq!(
                attempts_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                b"important-state"
            );
        }
        // Once space returns, flush_saved succeeds cleanly.
        writer
            .flush_saved(Some(v1), Duration::from_secs(2))
            .unwrap();
        writer.shutdown(Duration::from_secs(1));
    }
}
