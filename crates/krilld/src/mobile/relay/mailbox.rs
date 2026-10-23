//! Bounded, nonblocking producers and fair per-connection delivery. Closing a
//! client wakes its bridge without placing a close marker behind a full queue.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::{Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::session::lock;

struct State<T> {
    queues: BTreeMap<u32, VecDeque<(T, usize)>>,
    ready: VecDeque<u32>,
    bytes: usize,
    items: usize,
    closed: bool,
}

pub(super) struct Mailbox<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
    byte_limit: usize,
    peer_limit: usize,
    item_limit: usize,
}

pub(super) struct CloseGuard<'a, T>(&'a Mailbox<T>);

impl<T> Drop for CloseGuard<'_, T> {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl<T> Mailbox<T> {
    pub(super) fn close_guard(&self) -> CloseGuard<'_, T> {
        CloseGuard(self)
    }

    pub(super) fn new(byte_limit: usize, peer_limit: usize, item_limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                queues: BTreeMap::new(),
                ready: VecDeque::new(),
                bytes: 0,
                items: 0,
                closed: false,
            }),
            changed: Condvar::new(),
            byte_limit,
            peer_limit,
            item_limit,
        }
    }

    pub(super) fn send(&self, peer: u32, value: T, bytes: usize) -> io::Result<()> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let peer_bytes = state
            .queues
            .get(&peer)
            .map_or(0, |q| q.iter().map(|(_, size)| size).sum::<usize>());
        if state.items >= self.item_limit
            || bytes > self.byte_limit.saturating_sub(state.bytes)
            || bytes > self.peer_limit.saturating_sub(peer_bytes)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let queue = state.queues.entry(peer).or_default();
        let first = queue.is_empty();
        queue.push_back((value, bytes));
        if first {
            state.ready.push_back(peer);
        }
        state.items += 1;
        state.bytes += bytes;
        self.changed.notify_one();
        Ok(())
    }

    fn take(state: &mut State<T>) -> Option<T> {
        let peer = state.ready.pop_front()?;
        let queue = state.queues.get_mut(&peer)?;
        let (value, bytes) = queue.pop_front()?;
        if queue.is_empty() {
            state.queues.remove(&peer);
        } else {
            state.ready.push_back(peer);
        }
        state.items -= 1;
        state.bytes -= bytes;
        Some(value)
    }

    pub(super) fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let mut state = lock(&self.state);
        Self::take(&mut state).ok_or(if state.closed {
            mpsc::TryRecvError::Disconnected
        } else {
            mpsc::TryRecvError::Empty
        })
    }

    pub(super) fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        let started = Instant::now();
        let mut state = lock(&self.state);
        loop {
            if let Some(value) = Self::take(&mut state) {
                return Ok(value);
            }
            if state.closed {
                return Err(mpsc::RecvTimeoutError::Disconnected);
            }
            let left = timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(mpsc::RecvTimeoutError::Timeout);
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
    }

    pub(super) fn recv(&self) -> Result<T, mpsc::RecvError> {
        let mut state = lock(&self.state);
        loop {
            if let Some(value) = Self::take(&mut state) {
                return Ok(value);
            }
            if state.closed {
                return Err(mpsc::RecvError);
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        lock(&self.state).items == 0
    }
    pub(super) fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }

    pub(super) fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.queues.clear();
        state.ready.clear();
        state.items = 0;
        state.bytes = 0;
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn early_worker_exit_closes_mailbox_even_when_a_session_retains_it() {
        let queue = std::sync::Arc::new(Mailbox::new(8, 8, 2));
        let session = queue.clone();
        queue.send(0, vec![1], 1).unwrap();
        let worker = std::thread::spawn(move || {
            let _close = queue.close_guard();
            // Authentication can fail before creating the normal output reader.
            Err::<(), _>(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert!(worker.join().unwrap().is_err());
        assert!(session.is_closed());
        assert!(session.is_empty());
        assert!(matches!(
            session.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
    #[test]
    fn budgets_are_nonblocking_and_connections_are_fair() {
        let queue = Mailbox::new(8, 6, 4);
        queue.send(1, "a", 3).unwrap();
        queue.send(1, "b", 3).unwrap();
        assert_eq!(
            queue.send(1, "overflow", 1).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        queue.send(2, "c", 2).unwrap();
        assert!(queue.send(3, "overflow", 1).is_err());
        assert_eq!(queue.try_recv().unwrap(), "a");
        assert_eq!(queue.try_recv().unwrap(), "c");
        assert_eq!(queue.try_recv().unwrap(), "b");
        queue.send(2, "reclaimed", 6).unwrap();
    }

    #[test]
    fn close_wakes_a_waiter_and_releases_queued_payloads() {
        let queue = std::sync::Arc::new(Mailbox::<Vec<u8>>::new(8, 8, 2));
        let waiting = queue.clone();
        let reader = std::thread::spawn(move || waiting.recv());
        queue.close();
        assert!(reader.join().unwrap().is_err());
        assert!(queue.send(1, vec![1], 1).is_err());
        assert!(queue.is_empty());
    }
}
