//! One active cold replay pair across all connections, independent of PTY/live locks.
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use crate::session::lock;

const CANCEL_POLL: Duration = Duration::from_millis(10);

#[derive(Default)]
struct Gate {
    busy: Mutex<bool>,
    available: Condvar,
}

impl Gate {
    fn enter(&self, cancelled: &dyn Fn() -> bool) -> Option<Permit<'_>> {
        let mut busy = lock(&self.busy);
        loop {
            if cancelled() {
                return None;
            }
            if !*busy {
                *busy = true;
                return Some(Permit(self));
            }
            // Queue replacement / connection close need not know this independent
            // gate. Check their atomic generation even without a permit notification.
            busy = self
                .available
                .wait_timeout(busy, CANCEL_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

pub(crate) struct Permit<'a>(&'a Gate);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) = false;
        self.0.available.notify_one();
    }
}

pub(crate) fn acquire(cancelled: &dyn Fn() -> bool) -> Option<Permit<'static>> {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(Gate::default).enter(cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    #[test]
    fn waiting_cold_query_cancels_without_releasing_an_active_query() {
        let gate = Arc::new(Gate::default());
        let active = gate.enter(&|| false).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let waiting = Arc::clone(&gate);
        let cancelled = Arc::clone(&cancel);
        let (ready, began) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            ready.send(()).unwrap();
            let permit = waiting.enter(&|| cancelled.load(Ordering::Acquire));
            done.send(permit.is_none()).unwrap();
        });
        began.recv().unwrap();
        cancel.store(true, Ordering::Release);
        // The active replay remains held; the waiter must observe cancellation anyway.
        assert!(finished.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(*lock(&gate.busy));
        worker.join().unwrap();
        drop(active);
        assert!(gate.enter(&|| false).is_some());
    }

    #[test]
    fn cold_query_permit_releases_on_unwind_and_does_not_admit_a_cancelled_query() {
        let gate = Gate::default();
        assert!(gate.enter(&|| true).is_none());
        let result = std::panic::catch_unwind(|| {
            let _active = gate.enter(&|| false).unwrap();
            panic!("replay failure");
        });
        assert!(result.is_err());
        assert!(gate.enter(&|| false).is_some());
    }
}
