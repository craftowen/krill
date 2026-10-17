//! 연결의 입력·RESYNC 처리와 콜드 이력 재생을 분리한다. 실행 하나 + 최신 대기 요청 하나만 보관한다.

use std::io;
use std::net::Shutdown;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

use krill_proto::screen::Msg;
use krill_proto::search::SearchRequest;

use super::{Conn, Tap};
use crate::session::lock;

#[derive(Default)]
struct State {
    pending: Option<SearchRequest>,
    closed: bool,
}

#[derive(Default)]
struct Queue {
    state: Mutex<State>,
    changed: Condvar,
}

impl Queue {
    fn next(&self) -> Option<SearchRequest> {
        let mut state = lock(&self.state);
        loop {
            if state.closed {
                return None;
            }
            if let Some(req) = state.pending.take() {
                return Some(req);
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.pending = None;
        self.changed.notify_one();
    }
}

pub(super) struct Worker {
    queue: Arc<Queue>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub(super) fn start(tap: Tap, conn: Arc<Conn>) -> io::Result<Self> {
        let queue = Arc::new(Queue::default());
        let pending = Arc::clone(&queue);
        let worker = thread::Builder::new()
            .name(format!("search-{}", tap.id().0))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| -> io::Result<()> {
                    while let Some(req) = pending.next() {
                        let reply = crate::search::search(&tap, &req);
                        if lock(&pending.state).closed {
                            break;
                        }
                        conn.send(&reply.encode()?)?;
                    }
                    Ok(())
                }));
                if !matches!(result, Ok(Ok(()))) {
                    // 검색의 오류·panic은 이 연결만 닫는다. 요청 read도 풀어 작업자를 회수한다.
                    lock(&conn.out).shutdown(Shutdown::Both).ok();
                }
                pending.close();
            })?;
        Ok(Self {
            queue,
            thread: Some(worker),
        })
    }

    pub(super) fn submit(&self, req: SearchRequest) {
        let mut state = lock(&self.queue.state);
        if !state.closed {
            state.pending = Some(req);
            self.queue.changed.notify_one();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.queue.close();
        if let Some(worker) = self.thread.take() {
            worker.join().ok();
        }
    }
}
