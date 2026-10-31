//! 연결의 입력·RESYNC 처리와 콜드 이력 재생을 분리한다. 실행 하나 + 최신 대기 요청 하나만 보관한다.

use std::io::{self, Write as _};
use std::net::Shutdown;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

use krill_proto::screen::Msg;
use krill_proto::search::{Options, SearchOptionsRequest, SearchRequest, SearchResult};

use super::{Conn, Tap};
use crate::session::lock;

#[derive(Default)]
struct State {
    pending: Option<SearchOptionsRequest>,
    closed: bool,
}

#[derive(Default)]
struct Queue {
    state: Mutex<State>,
    changed: Condvar,
    generation: AtomicU64,
}

impl Queue {
    fn submit(&self, req: SearchOptionsRequest) {
        let mut state = lock(&self.state);
        if !state.closed {
            self.generation.fetch_add(1, Ordering::Release);
            state.pending = Some(req);
            self.changed.notify_one();
        }
    }

    fn publish(&self, generation: u64, send: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let state = lock(&self.state);
        if state.closed || self.generation.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        // 느린 전송은 새 검색·취소를 막지 않는다. 이미 전송을 시작한 이전
        // 응답은 클라이언트가 req로 구분하며 프레임 끝까지 전송한다.
        drop(state);
        send()
    }

    fn next(&self) -> Option<(SearchOptionsRequest, u64)> {
        let mut state = lock(&self.state);
        loop {
            if state.closed {
                return None;
            }
            if let Some(req) = state.pending.take() {
                return Some((req, self.generation.load(Ordering::Acquire)));
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
        self.generation.fetch_add(1, Ordering::Release);
        state.pending = None;
        self.changed.notify_one();
    }
}

pub(super) struct Worker {
    queue: Arc<Queue>,
    thread: Option<thread::JoinHandle<()>>,
    tap: Tap,
}

impl Worker {
    pub(super) fn start(tap: Tap, conn: Arc<Conn>) -> io::Result<Self> {
        Self::spawn(tap, conn, |tap, req, cancelled| {
            crate::search::search_cancellable(tap, &req.request, req.options, cancelled)
        })
    }

    fn spawn(
        tap: Tap,
        conn: Arc<Conn>,
        mut load: impl FnMut(&Tap, &SearchOptionsRequest, &dyn Fn() -> bool) -> SearchResult
        + Send
        + 'static,
    ) -> io::Result<Self> {
        let wake = tap.clone();
        let queue = Arc::new(Queue::default());
        let pending = Arc::clone(&queue);
        let worker = thread::Builder::new()
            .name(format!("search-{}", tap.id().0))
            .stack_size(256 * 1024)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                let result = catch_unwind(AssertUnwindSafe(|| -> io::Result<()> {
                    while let Some((req, generation)) = pending.next() {
                        let cancelled = || pending.generation.load(Ordering::Acquire) != generation;
                        // 검색 실패가 입력·화면 연결까지 닫지 않도록 요청 단위로 격리한다.
                        let reply = catch_unwind(AssertUnwindSafe(|| load(&tap, &req, &cancelled)))
                            .unwrap_or_else(|_| {
                                tracing::warn!(session = tap.id().0, "search replay panicked");
                                SearchResult {
                                    req: req.request.req,
                                    snapshot: krill_proto::search::Snapshot::default(),
                                    total: 0,
                                    hit: None,
                                    wrapped: false,
                                    stale: false,
                                    incomplete: true,
                                    approximate: false,
                                }
                            });
                        let frame = reply.encode()?;
                        // 쓰기 순서를 기다리는 동안에도 새 요청을 받는다. 잠금을 얻은
                        // 뒤 세대를 확인해 그 사이 만료된 결과는 전송하지 않는다.
                        let mut out = lock(&conn.out);
                        pending.publish(generation, || out.write_all(&frame))?;
                    }
                    Ok(())
                }));
                if !matches!(result, Ok(Ok(()))) {
                    // 프레임 쓰기 실패나 작업자 자체의 panic은 연결을 닫아 요청 read를 풀어 준다.
                    lock(&conn.out).shutdown(Shutdown::Both).ok();
                }
                pending.close();
            })?;
        Ok(Self {
            queue,
            thread: Some(worker),
            tap: wake,
        })
    }

    pub(super) fn submit(&self, req: SearchRequest) {
        self.submit_options(SearchOptionsRequest {
            request: req,
            options: Options {
                case_sensitive: true,
                regex: false,
            },
        });
    }

    pub(super) fn submit_options(&self, req: SearchOptionsRequest) {
        self.queue.submit(req);
        self.tap.wake();
    }
}

#[cfg(test)]
mod tests;

impl Drop for Worker {
    fn drop(&mut self) {
        self.queue.close();
        self.tap.wake();
        if let Some(worker) = self.thread.take() {
            worker.join().ok();
        }
    }
}
