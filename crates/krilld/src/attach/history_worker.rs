//! 이력 재생을 입력 요청 루프에서 분리한다. 여러 소비자의 요청은 FIFO로 보존하고 대기열은 제한한다.

use std::collections::VecDeque;
use std::io;
use std::net::Shutdown;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

use krill_proto::SessionId;
use krill_proto::screen::{HistoryRequest, HistoryRows, HistoryViewRequest, Msg};

use super::{Conn, Tap};
use crate::session::lock;

// 설계 상한: 실행 하나 + 작은 FIFO. 과부하는 연결 오류로 드러내고 무상한 요청을 쌓지 않는다.
const MAX_PENDING: usize = 8;

enum Request {
    Legacy(HistoryRequest),
    View(HistoryViewRequest),
}

#[derive(Default)]
struct State {
    pending: VecDeque<Request>,
    generation: u64,
    closed: bool,
}

#[derive(Default)]
struct Queue {
    state: Mutex<State>,
    changed: Condvar,
}

impl Queue {
    fn next(&self) -> Option<(u64, Request)> {
        let mut state = lock(&self.state);
        loop {
            if state.closed {
                return None;
            }
            if let Some(req) = state.pending.pop_front() {
                return Some((state.generation, req));
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn current(&self, generation: u64) -> bool {
        let state = lock(&self.state);
        !state.closed && state.generation == generation
    }

    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.pending.clear();
        self.changed.notify_one();
    }
}

pub(super) struct Worker {
    queue: Arc<Queue>,
    conn: Arc<Conn>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub(super) fn start(tap: Tap, conn: Arc<Conn>, features: u64) -> io::Result<Self> {
        let legacy = tap.clone();
        Self::spawn_inner(
            tap.id(),
            conn,
            move |req| super::history(&legacy, req, features),
            Some(tap),
        )
    }

    #[cfg(test)]
    fn spawn(
        session: SessionId,
        conn: Arc<Conn>,
        load: impl FnMut(&HistoryRequest) -> HistoryRows + Send + 'static,
    ) -> io::Result<Self> {
        Self::spawn_inner(session, conn, load, None)
    }

    fn spawn_inner(
        session: SessionId,
        conn: Arc<Conn>,
        mut load: impl FnMut(&HistoryRequest) -> HistoryRows + Send + 'static,
        tap: Option<Tap>,
    ) -> io::Result<Self> {
        let queue = Arc::new(Queue::default());
        let (pending, output) = (Arc::clone(&queue), Arc::clone(&conn));
        let worker = thread::Builder::new()
            .name(format!("history-{}", session.0))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| -> io::Result<()> {
                    let mut views = super::history_view::Service::default();
                    while let Some((generation, req)) = pending.next() {
                        let mut stable = match &req {
                            Request::View(req) => tap
                                .as_ref()
                                .map(|tap| views.load(tap, req, &|| !pending.current(generation))),
                            Request::Legacy(_) => None,
                        };
                        let reply = match &req {
                            Request::Legacy(req) => load(req).encode()?,
                            Request::View(_) => Vec::new(),
                        };
                        // invalidate와 같은 순서(out → state)로 잠근다. clear ACK 뒤에
                        // 이전 세대의 이력 응답이 도착하지 않도록 전송과 무효화를 직렬화한다.
                        let mut out = lock(&output.out);
                        if pending.current(generation) && !output.gone() {
                            if let (Some(tap), Some(reply)) = (tap.as_ref(), stable.as_mut()) {
                                tap.publish_history(
                                    reply,
                                    &|| !pending.current(generation),
                                    |bytes| std::io::Write::write_all(&mut *out, bytes),
                                )?;
                            } else {
                                std::io::Write::write_all(&mut *out, &reply)?;
                            }
                        }
                    }
                    Ok(())
                }));
                if !matches!(result, Ok(Ok(()))) {
                    lock(&output.out).shutdown(Shutdown::Both).ok();
                }
                pending.close();
            })?;
        Ok(Self {
            queue,
            conn,
            thread: Some(worker),
        })
    }

    pub(super) fn submit(&self, req: HistoryRequest) -> io::Result<()> {
        self.enqueue(Request::Legacy(req))
    }
    pub(super) fn submit_view(&self, req: HistoryViewRequest) -> io::Result<()> {
        self.enqueue(Request::View(req))
    }
    fn enqueue(&self, req: Request) -> io::Result<()> {
        let mut state = lock(&self.queue.state);
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if state.pending.len() >= MAX_PENDING {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "history request queue full",
            ));
        }
        state.pending.push_back(req);
        self.queue.changed.notify_one();
        Ok(())
    }

    pub(super) fn invalidate(&self) {
        let _out = lock(&self.conn.out);
        let mut state = lock(&self.queue.state);
        state.generation = state.generation.wrapping_add(1);
        state.pending.clear();
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

#[cfg(test)]
mod tests;
