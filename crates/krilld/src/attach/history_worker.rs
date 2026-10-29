//! 이력 재생을 입력 요청 루프에서 분리한다. 여러 소비자의 요청은 FIFO로 보존하고 대기열은 제한한다.

use std::collections::VecDeque;
use std::io;
use std::net::Shutdown;
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(test)]
use std::sync::PoisonError;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;

#[cfg(test)]
use krill_proto::SessionId;
#[cfg(test)]
use krill_proto::screen::HistoryRows;
use krill_proto::screen::{HistoryRequest, HistoryViewRequest, Msg};

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
    fn pop_now(&self) -> Option<(u64, Request)> {
        let mut state = lock(&self.state);
        if state.closed {
            return None;
        }
        state.pending.pop_front().map(|req| (state.generation, req))
    }

    #[cfg(test)]
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

struct SharedTask {
    queue: Arc<Queue>,
    conn: Arc<Conn>,
    tap: Tap,
    features: u64,
    views: Arc<Mutex<super::history_view::Service>>,
}

#[derive(Default)]
struct SharedPoolState {
    tasks: VecDeque<SharedTask>,
    running: bool,
}

#[derive(Default)]
struct SharedPool {
    state: Mutex<SharedPoolState>,
}

fn enqueue_shared(task: SharedTask) -> io::Result<()> {
    static POOL: OnceLock<Arc<SharedPool>> = OnceLock::new();
    let pool = Arc::clone(POOL.get_or_init(Arc::default));
    let mut state = lock(&pool.state);
    state.tasks.push_back(task);
    if !state.running {
        state.running = true;
        let worker_pool = Arc::clone(&pool);
        if let Err(err) = thread::Builder::new()
            .name("history".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                loop {
                    let task = {
                        let mut state = lock(&worker_pool.state);
                        let Some(task) = state.tasks.pop_front() else {
                            state.running = false;
                            break;
                        };
                        task
                    };
                    let result = catch_unwind(AssertUnwindSafe(|| -> io::Result<()> {
                        while let Some((generation, req)) = task.queue.pop_now() {
                            let mut stable = match &req {
                                Request::View(req) => Some(lock(&task.views).load(
                                    &task.tap,
                                    req,
                                    &|| !task.queue.current(generation),
                                )),
                                Request::Legacy(_) => None,
                            };
                            let reply = match &req {
                                Request::Legacy(req) => {
                                    super::history(&task.tap, req, task.features).encode()?
                                }
                                Request::View(_) => Vec::new(),
                            };
                            crate::alloc::purge();
                            let mut out = lock(&task.conn.out);
                            if task.queue.current(generation) && !task.conn.gone() {
                                if let Some(reply) = stable.as_mut() {
                                    task.tap.publish_history(
                                        reply,
                                        &|| !task.queue.current(generation),
                                        |bytes| std::io::Write::write_all(&mut *out, bytes),
                                    )?;
                                } else {
                                    std::io::Write::write_all(&mut *out, &reply)?;
                                }
                            }
                            drop(out);
                            drop(reply);
                            drop(stable);
                            crate::alloc::purge();
                        }
                        Ok(())
                    }));
                    crate::alloc::purge();
                    if !matches!(result, Ok(Ok(()))) {
                        lock(&task.conn.out).shutdown(Shutdown::Both).ok();
                        task.queue.close();
                    }
                }
                crate::alloc::purge();
                crate::session::purge_soon();
            })
        {
            state.running = false;
            state.tasks.pop_back();
            return Err(err);
        }
    }
    Ok(())
}

pub(super) struct Worker {
    queue: Arc<Queue>,
    conn: Arc<Conn>,
    shared: Option<(Tap, u64, Arc<Mutex<super::history_view::Service>>)>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    #[expect(clippy::unnecessary_wraps, reason = "호출부 IO 결과 시그니처 유지")]
    pub(super) fn start(tap: Tap, conn: Arc<Conn>, features: u64) -> io::Result<Self> {
        Ok(Self {
            queue: Arc::new(Queue::default()),
            conn,
            shared: Some((tap, features, Arc::default())),
            thread: None,
        })
    }

    #[cfg(test)]
    fn spawn(
        session: SessionId,
        conn: Arc<Conn>,
        mut load: impl FnMut(&HistoryRequest) -> HistoryRows + Send + 'static,
    ) -> io::Result<Self> {
        let queue = Arc::new(Queue::default());
        let (pending, output) = (Arc::clone(&queue), Arc::clone(&conn));
        let worker = thread::Builder::new()
            .name(format!("history-{}", session.0))
            .stack_size(128 * 1024)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                let result = catch_unwind(AssertUnwindSafe(|| -> io::Result<()> {
                    while let Some((generation, req)) = pending.next() {
                        let reply = match &req {
                            Request::Legacy(req) => load(req).encode()?,
                            Request::View(_) => Vec::new(),
                        };
                        let mut out = lock(&output.out);
                        if pending.current(generation) && !output.gone() {
                            std::io::Write::write_all(&mut *out, &reply)?;
                        }
                        drop(out);
                        drop(reply);
                        crate::alloc::purge();
                    }
                    Ok(())
                }));
                crate::alloc::purge();
                if !matches!(result, Ok(Ok(()))) {
                    lock(&output.out).shutdown(Shutdown::Both).ok();
                }
                pending.close();
            })?;
        Ok(Self {
            queue,
            conn,
            shared: None,
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
        {
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
        }
        if let Some((tap, features, views)) = &self.shared {
            enqueue_shared(SharedTask {
                queue: Arc::clone(&self.queue),
                conn: Arc::clone(&self.conn),
                tap: tap.clone(),
                features: *features,
                views: Arc::clone(views),
            })?;
        }
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
