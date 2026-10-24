//! 접수 장부: seq 예약·payload 대조·상태 전이. 같은 seq는 결과와 무관하게 한 번만 실행된다.
//!
//! 상태 전이는 이 장부의 잠금 안에서만 일어난다. 실행기는 `begin_dispatch`가 `true`일 때만
//! 첫 부작용을 낸다. 취소는 `Queued`에서만 이기므로 취소 응답과 발행이 겹치지 않는다. 발행 중 취소는
//! `too_late`이지만 멈춤 요청을 남겨 `batch`의 남은 단계를 막는다(이미 낸 입력은 되돌리지 않는다).
//! 끝난 항목은 상한 있는 [`Summary`]만 기억한다.

use std::collections::VecDeque;

use super::protocol::{CancelOutcome, MAX_SEQ_GAP, Outcome, RequestState, Summary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    New,
    /// 같은 seq·같은 요청. 다시 실행하지 않는다.
    Duplicate(RequestState),
    PayloadMismatch,
    /// 이미 지나간(또는 퇴출된) seq.
    Expired,
    OutOfRange,
    /// 장부가 진행 중 요청으로 가득 찼다.
    Busy,
}

#[derive(Debug)]
struct Entry {
    seq: u64,
    digest: [u8; 32],
    state: RequestState,
    summary: Option<Summary>,
    stop_requested: bool,
}

#[derive(Debug)]
pub struct Ledger {
    max_seen: u64,
    entries: VecDeque<Entry>,
    capacity: usize,
}

impl Ledger {
    pub fn new(capacity: usize) -> Self {
        Self {
            max_seen: 0,
            entries: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    fn entry_mut(&mut self, seq: u64) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.seq == seq)
    }

    /// 새 요청이면 `Queued`로 예약한다. seq는 1부터, 지금까지 본 최대값보다 커야 한다.
    pub fn admit(&mut self, seq: u64, digest: [u8; 32]) -> Admission {
        if let Some(entry) = self.entries.iter().find(|e| e.seq == seq) {
            return if entry.digest == digest {
                Admission::Duplicate(entry.state)
            } else {
                Admission::PayloadMismatch
            };
        }
        if seq == 0 {
            return Admission::OutOfRange;
        }
        if seq <= self.max_seen {
            return Admission::Expired;
        }
        if seq - self.max_seen > MAX_SEQ_GAP {
            return Admission::OutOfRange;
        }
        if self.entries.len() >= self.capacity {
            // 끝난 항목만 퇴출한다. 진행 중 항목을 잊으면 상태 조회가 거짓이 된다.
            let Some(oldest_done) = self.entries.iter().position(|e| !e.state.is_live()) else {
                return Admission::Busy;
            };
            self.entries.remove(oldest_done);
        }
        self.entries.push_back(Entry {
            seq,
            digest,
            state: RequestState::Queued,
            summary: None,
            stop_requested: false,
        });
        self.max_seen = seq;
        Admission::New
    }

    pub fn state(&self, seq: u64) -> RequestState {
        self.status(seq).0
    }

    /// 상태와, 끝난 요청이면 그 요약. 퇴출된 seq는 요약 없이 `Expired`다.
    pub fn status(&self, seq: u64) -> (RequestState, Option<Summary>) {
        if let Some(entry) = self.entries.iter().find(|e| e.seq == seq) {
            (entry.state, entry.summary.clone())
        } else if seq == 0 || seq > self.max_seen {
            (RequestState::Unseen, None)
        } else {
            (RequestState::Expired, None)
        }
    }

    /// 실행기가 낸 결과로 최종 상태와 요약을 기록한다.
    pub fn settle(&mut self, outcome: &Outcome) {
        if let Some(entry) = self.entry_mut(outcome.seq) {
            entry.state = outcome.state;
            entry.summary = Some(Summary::of(outcome));
        }
    }

    /// 발행 중 취소가 들어왔는지. `batch`는 단계 사이에 본다.
    pub fn stop_requested(&self, seq: u64) -> bool {
        self.entries
            .iter()
            .any(|e| e.seq == seq && e.stop_requested)
    }

    /// 최종 상태를 기록한다. 장부에 없는(퇴출된) seq는 무시한다.
    pub fn finish(&mut self, seq: u64, state: RequestState) {
        if let Some(entry) = self.entry_mut(seq) {
            entry.state = state;
        }
    }

    /// `Queued` → `Dispatching`. 취소·종료가 먼저 이겼으면 `false`이고 발행하면 안 된다.
    pub fn begin_dispatch(&mut self, seq: u64) -> bool {
        match self.entry_mut(seq) {
            Some(entry) if entry.state == RequestState::Queued => {
                entry.state = RequestState::Dispatching;
                true
            }
            _ => false,
        }
    }

    pub fn cancel(&mut self, seq: u64) -> CancelOutcome {
        match self.state(seq) {
            RequestState::Queued => {
                self.finish(seq, RequestState::NotDispatched);
                CancelOutcome::Cancelled
            }
            RequestState::Dispatching => {
                if let Some(entry) = self.entry_mut(seq) {
                    entry.stop_requested = true;
                }
                CancelOutcome::TooLate
            }
            RequestState::Expired => CancelOutcome::Expired,
            RequestState::Unseen => CancelOutcome::Unseen,
            RequestState::Completed | RequestState::NotDispatched | RequestState::Unknown => {
                CancelOutcome::AlreadyFinished
            }
        }
    }

    /// 종료·EOF: 아직 발행 전인 요청을 모두 미실행으로 확정한다.
    pub fn cancel_all_queued(&mut self) {
        for entry in &mut self.entries {
            if entry.state == RequestState::Queued {
                entry.state = RequestState::NotDispatched;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 32] = [1; 32];
    const B: [u8; 32] = [2; 32];

    #[test]
    fn same_seq_never_admitted_twice() {
        let mut ledger = Ledger::new(8);
        assert_eq!(ledger.admit(1, A), Admission::New);
        assert_eq!(
            ledger.admit(1, A),
            Admission::Duplicate(RequestState::Queued)
        );
        assert_eq!(ledger.admit(1, B), Admission::PayloadMismatch);
        assert!(ledger.begin_dispatch(1));
        assert_eq!(
            ledger.admit(1, A),
            Admission::Duplicate(RequestState::Dispatching)
        );
        ledger.finish(1, RequestState::Unknown);
        // Unknown is final: a retransmit reports it instead of re-running.
        assert_eq!(
            ledger.admit(1, A),
            Admission::Duplicate(RequestState::Unknown)
        );
    }

    #[test]
    fn sequence_must_advance_within_gap() {
        let mut ledger = Ledger::new(8);
        assert_eq!(ledger.admit(0, A), Admission::OutOfRange);
        assert_eq!(ledger.admit(MAX_SEQ_GAP + 1, A), Admission::OutOfRange);
        assert_eq!(ledger.admit(5, A), Admission::New);
        // A skipped lower seq is not resurrected later.
        assert_eq!(ledger.admit(3, A), Admission::Expired);
        assert_eq!(ledger.state(3), RequestState::Expired);
        assert_eq!(ledger.state(6), RequestState::Unseen);
        assert_eq!(ledger.admit(5 + MAX_SEQ_GAP, A), Admission::New);
    }

    #[test]
    fn eviction_is_expiry_not_permission_to_rerun() {
        let mut ledger = Ledger::new(2);
        for seq in 1..=2 {
            assert_eq!(ledger.admit(seq, A), Admission::New);
            ledger.finish(seq, RequestState::Completed);
        }
        assert_eq!(ledger.admit(3, A), Admission::New);
        assert_eq!(ledger.state(1), RequestState::Expired);
        assert_eq!(ledger.admit(1, A), Admission::Expired);
        assert_eq!(ledger.cancel(1), CancelOutcome::Expired);
    }

    #[test]
    fn live_entries_are_never_evicted() {
        let mut ledger = Ledger::new(2);
        assert_eq!(ledger.admit(1, A), Admission::New);
        assert_eq!(ledger.admit(2, A), Admission::New);
        assert!(ledger.begin_dispatch(2));
        assert_eq!(ledger.admit(3, A), Admission::Busy);
        assert_eq!(ledger.state(1), RequestState::Queued);
        // Busy did not consume seq 3.
        ledger.finish(1, RequestState::Completed);
        assert_eq!(ledger.admit(3, A), Admission::New);
    }

    #[test]
    fn cancel_only_wins_before_dispatch() {
        let mut ledger = Ledger::new(8);
        ledger.admit(1, A);
        ledger.admit(2, A);
        assert_eq!(ledger.cancel(1), CancelOutcome::Cancelled);
        assert!(
            !ledger.begin_dispatch(1),
            "cancelled request must not dispatch"
        );
        assert_eq!(ledger.state(1), RequestState::NotDispatched);
        assert!(ledger.begin_dispatch(2));
        assert_eq!(ledger.cancel(2), CancelOutcome::TooLate);
        ledger.finish(2, RequestState::Completed);
        assert_eq!(ledger.cancel(2), CancelOutcome::AlreadyFinished);
        assert_eq!(ledger.cancel(99), CancelOutcome::Unseen);
    }

    #[test]
    fn late_cancel_requests_stop_and_settle_keeps_bounded_summary() {
        use crate::platform::computer::protocol::{Delivery, ErrorInfo};
        let mut ledger = Ledger::new(1);
        ledger.admit(1, A);
        assert!(!ledger.stop_requested(1));
        assert!(ledger.begin_dispatch(1));
        assert_eq!(ledger.cancel(1), CancelOutcome::TooLate);
        assert!(ledger.stop_requested(1));
        assert_eq!(ledger.status(1), (RequestState::Dispatching, None));
        let mut outcome = Outcome::not_dispatched(1, "x", "m");
        outcome.state = RequestState::Completed;
        outcome.delivery = Delivery::Partial {
            issued_units: 3,
            reason: "r".repeat(10_000),
        };
        outcome.error = Some(ErrorInfo {
            code: "가".repeat(100),
            message: "m".repeat(10_000),
        });
        outcome.data = Some(serde_json::json!({"secret": "typed text"}));
        ledger.settle(&outcome);
        let (state, summary) = ledger.status(1);
        assert_eq!(state, RequestState::Completed);
        let summary = summary.unwrap();
        assert_eq!(summary.delivery, "partial");
        assert_eq!(summary.issued_units, Some(3));
        assert!(summary.error_code.unwrap().len() <= 64);
        let wire = serde_json::to_string(&ledger.status(1).1).unwrap();
        assert!(wire.len() < 256 && !wire.contains("typed text"), "{wire}");
        // Eviction forgets the summary: expired, never replayed.
        ledger.admit(2, A);
        assert_eq!(ledger.status(1), (RequestState::Expired, None));
        assert_eq!(ledger.admit(1, A), Admission::Expired);
    }

    #[test]
    fn shutdown_settles_queued_only() {
        let mut ledger = Ledger::new(8);
        ledger.admit(1, A);
        ledger.admit(2, A);
        assert!(ledger.begin_dispatch(1));
        ledger.cancel_all_queued();
        assert_eq!(ledger.state(1), RequestState::Dispatching);
        assert_eq!(ledger.state(2), RequestState::NotDispatched);
        assert!(!ledger.begin_dispatch(2));
    }
}
