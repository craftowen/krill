//! Admission for new local handshakes and ordinary control connections only.
//! Existing attach/watch/long-poll connections never compete for ordinary slots.
use std::sync::atomic::{AtomicUsize, Ordering};

static HANDSHAKES: AtomicUsize = AtomicUsize::new(0);
static CONTROLS: AtomicUsize = AtomicUsize::new(0);
const MAX_HANDSHAKES: usize = 128;
const MAX_CONTROLS: usize = 256;

#[derive(Debug)]
pub struct Permit(&'static AtomicUsize);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

fn reserve(counter: &'static AtomicUsize, limit: usize) -> Option<Permit> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < limit).then_some(n + 1)
        })
        .ok()?;
    Some(Permit(counter))
}

pub fn handshake() -> Option<Permit> {
    reserve(&HANDSHAKES, MAX_HANDSHAKES)
}
pub(super) fn control() -> Option<Permit> {
    reserve(&CONTROLS, MAX_CONTROLS)
}

pub(super) fn protected(request: &krill_proto::control::Request) -> bool {
    use krill_proto::control::Request;
    use krill_proto::orchestration::OrchAction;
    matches!(
        request,
        Request::WaitStatus { .. }
            | Request::GitWatch(_)
            | Request::Orchestration(OrchAction::CheckMessages { wait_ms: 1.., .. })
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_reservations_release_without_leaking_on_rejection() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let first = reserve(&COUNTER, 1).unwrap();
        assert!(reserve(&COUNTER, 1).is_none());
        assert_eq!(COUNTER.load(Ordering::Acquire), 1);
        drop(first);
        assert!(reserve(&COUNTER, 1).is_some());
    }
}
