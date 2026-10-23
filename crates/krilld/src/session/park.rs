//! One daemon deadline worker; deadlines hold no PTY/session ownership.
use std::sync::Weak;

use super::{
    Arc, Condvar, Instant, Mutex, OnceLock, PARK_GRACE, PoisonError, Shared, lock, thread,
};

struct Deadline {
    session: Weak<Shared>,
    epoch: u64,
    at: Instant,
}

#[derive(Default)]
struct Deadlines {
    pending: Mutex<Vec<Deadline>>,
    changed: Condvar,
}

pub(super) fn schedule(session: &Arc<Shared>, epoch: u64) {
    static TIMER: OnceLock<Mutex<Option<Arc<Deadlines>>>> = OnceLock::new();
    let timer = {
        let mut current = lock(TIMER.get_or_init(Mutex::default));
        if current.is_none() {
            let timer = Arc::new(Deadlines::default());
            let worker = Arc::clone(&timer);
            if thread::Builder::new()
                .name("park".into())
                .spawn(move || run(&worker))
                .is_err()
            {
                return;
            }
            *current = Some(timer);
        }
        let Some(timer) = current.as_ref() else {
            return;
        };
        Arc::clone(timer)
    };
    let mut pending = lock(&timer.pending);
    {
        let live = lock(&session.live);
        // A delayed schedule call from an older viewer drop must not replace the
        // newer epoch's deadline after a quick tab round trip.
        if live.epoch != epoch || live.viewers > 0 {
            return;
        }
    }
    // Replace this session's old deadline, and discard dead sessions on every insertion.
    pending.retain(|d| d.session.strong_count() > 0 && !d.session.ptr_eq(&Arc::downgrade(session)));
    pending.push(Deadline {
        session: Arc::downgrade(session),
        epoch,
        at: Instant::now() + PARK_GRACE,
    });
    timer.changed.notify_one();
}

fn run(timer: &Deadlines) {
    loop {
        let mut pending = lock(&timer.pending);
        let Some((index, deadline)) = pending.iter().enumerate().min_by_key(|(_, d)| d.at) else {
            drop(
                timer
                    .changed
                    .wait(pending)
                    .unwrap_or_else(PoisonError::into_inner),
            );
            continue;
        };
        let delay = deadline.at.saturating_duration_since(Instant::now());
        if !delay.is_zero() {
            drop(
                timer
                    .changed
                    .wait_timeout(pending, delay)
                    .unwrap_or_else(PoisonError::into_inner),
            );
            continue;
        }
        let deadline = pending.swap_remove(index);
        drop(pending);
        if let Some(session) = deadline.session.upgrade() {
            // park() rechecks viewers and epoch under the live lock.
            session.park(deadline.epoch);
        }
    }
}
