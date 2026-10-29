//! A failed upgrade resumes the same engine, recorder and PTY instead of leaving a shutdown latch.
use super::{
    Condvar, HALTED, Mutex, MutexGuard, Ordering, PoisonError, Recorder, Shared, halt, io, lock,
    seal,
};
#[cfg(unix)]
use super::{Errno, HALT, read};

static EXCLUSIVE: Mutex<()> = Mutex::new(());
static CHANGED: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

#[must_use = "commit a successful handoff; dropping preparation resumes the old readers"]
pub(crate) struct Handoff {
    _exclusive: MutexGuard<'static, ()>,
    resume: bool,
}

impl Handoff {
    pub(crate) fn begin() -> io::Result<Self> {
        let mut guard = Self {
            _exclusive: lock(&EXCLUSIVE),
            resume: false,
        };
        // A reader clears its previous pause under this gate. Do not let a new
        // preparation mistake that old acknowledgement for its own pause.
        let _gate = lock(&CHANGED.0);
        // Failed retries cannot undo an earlier committed orphan handoff.
        guard.resume = !HALTED.load(Ordering::SeqCst);
        halt()?;
        Ok(guard)
    }

    /// The successor may consume the PTY after commit; the old readers must stay parked.
    pub(crate) fn commit(mut self) {
        self.resume = false;
    }
}

impl Drop for Handoff {
    fn drop(&mut self) {
        if !self.resume {
            return;
        }
        let _gate = lock(&CHANGED.0);
        #[cfg(unix)]
        if let Some((reader, _)) = HALT.get() {
            let mut bytes = [0; 64];
            loop {
                match read(reader, &mut bytes) {
                    Ok(0) => break,
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(_) => break,
                }
            }
        }
        HALTED.store(false, Ordering::SeqCst);
        CHANGED.1.notify_all();
    }
}

pub(super) fn wait(shared: &Shared, recorder: &mut Option<Recorder>) {
    if !HALTED.load(Ordering::SeqCst) {
        return;
    }
    if let Some(recorder) = recorder {
        seal(shared, recorder);
    }
    let mut gate = lock(&CHANGED.0);
    shared.paused.store(true, Ordering::SeqCst);
    while HALTED.load(Ordering::SeqCst) {
        gate = CHANGED.1.wait(gate).unwrap_or_else(PoisonError::into_inner);
    }
    #[cfg(test)]
    super::pause_tests::before_resume(shared.id);
    shared.paused.store(false, Ordering::SeqCst);
}
