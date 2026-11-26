//! Snapshot/publication barriers for negotiated history; never used by PTY output/input.
use super::{Tap, lock};
use krill_proto::screen::{Cell, HistoryScreen, HistoryViewRows, HistoryViewStatus, Msg};

pub(crate) struct HistorySnapshot {
    pub(crate) stamp: [u64; 3],
    pub(crate) seq: u64,
    pub(crate) rows: Vec<Vec<Cell>>,
    pub(crate) screen: HistoryScreen,
}

impl Tap {
    fn access_history(
        &self,
        cancelled: &dyn Fn() -> bool,
    ) -> std::io::Result<std::sync::MutexGuard<'_, ()>> {
        loop {
            if cancelled() {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            match self.0.history_access.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
    pub(crate) fn history_snapshot(
        &self,
        cancelled: &dyn Fn() -> bool,
    ) -> std::io::Result<HistorySnapshot> {
        let (stamp, seq, rows, screen) = {
            let _access = self.access_history(cancelled)?;
            let live = self.0.live();
            let rows = live.engine.snapshot_hot_rows();
            let screen = HistoryScreen::from_screen(&live.engine.screen());
            let (epoch, seq) = (live.history_epoch, live.seq);
            drop(live);
            let stamp = match self.0.store.as_ref() {
                Some(store) => store.with_history_epoch_cancelled(
                    self.id(),
                    cancelled,
                    |generation, cleared| [epoch, generation, cleared],
                )?,
                None => [epoch, 0, 0],
            };
            (stamp, seq, rows, screen)
        };
        // live.seq advances before recorder append/publish. Never freeze a catalog missing that batch.
        let mut out = lock(&self.0.out);
        while out.end < seq && !out.closed {
            if cancelled() {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            out = self
                .0
                .changed
                .wait_timeout(out, std::time::Duration::from_millis(10))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if cancelled() {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if out.end < seq {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        drop(out);
        Ok(HistorySnapshot {
            stamp,
            seq,
            rows,
            screen,
        })
    }
    pub(crate) fn publish_history(
        &self,
        reply: &mut HistoryViewRows,
        cancelled: &dyn Fn() -> bool,
        send: impl FnOnce(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let _access = self.access_history(cancelled)?;
        let current = self.0.live().history_epoch;
        let publish = |generation, cleared| {
            if reply.epoch != [current, generation, cleared] {
                reply.status = HistoryViewStatus::Invalidated;
                reply.rows.clear();
                reply.identity = None;
                reply.screen = None;
            }
            send(&reply.encode()?)
        };
        match self.0.store.as_ref() {
            Some(store) => store.with_history_epoch_cancelled(self.id(), cancelled, publish)?,
            None => publish(0, 0),
        }
    }
}
