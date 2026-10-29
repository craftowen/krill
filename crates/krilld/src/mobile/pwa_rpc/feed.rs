//! 모바일 대시보드 변경 피드. 인증된 PWA 연결 하나가 스냅샷 폴링 대신 같은 E2EE 채널로 바뀐 스냅샷만 받는다.
//! 데몬 작업은 폴링과 같다: 클라이언트가 고른 간격마다 같은 `snapshot_since`를 부르고, etag가 같으면 아무것도 보내지 않는다.
//! 응답과 푸시는 연결의 봉인·쓰기 잠금 하나로 직렬화되므로 긴 대기가 명령 응답을 막지 않는다.
use crate::registry::Registry;
use crate::session::lock;
use krill_proto::ipc;
use krill_proto::mobile::MobileReply;
use krill_proto::screen::Msg;
use serde_json::Value;
use std::cell::RefCell;
use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 클라이언트 간격 요청의 허용 범위. 0은 일시정지(숨은 탭)다.
const MIN_INTERVAL: Duration = Duration::from_secs(1);
const MAX_INTERVAL: Duration = Duration::from_secs(60);

type Sink = Box<dyn Fn(&[u8]) -> io::Result<()> + Send + Sync>;

/// 연결 하나의 피드. 스레드는 첫 구독 때만 만들고 연결이 끝나면 `stop`이 멈추고 회수한다.
pub(crate) struct Feed {
    state: Mutex<State>,
    changed: Condvar,
    sink: Sink,
    registry: Weak<Registry>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Default)]
struct State {
    interval: Option<Duration>,
    due: Option<Instant>,
    /// 이 연결에 마지막으로 보낸(응답이든 푸시든) 스냅샷.
    etag: Option<String>,
    rev: u64,
    stopped: bool,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Feed>>> = const { RefCell::new(None) };
}

impl std::fmt::Debug for Feed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feed").finish_non_exhaustive()
    }
}

impl Feed {
    /// `sink`은 IPC 프레임 하나를 연결의 응답과 같은 순서 잠금 아래에서 봉인해 보낸다.
    pub(crate) fn new(
        registry: Weak<Registry>,
        sink: impl Fn(&[u8]) -> io::Result<()> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            changed: Condvar::new(),
            sink: Box::new(sink),
            registry,
            thread: Mutex::default(),
        })
    }

    /// 이 연결의 요청을 처리하는 스레드에 묶는다. 다른 스레드(데스크톱 IPC)의 스냅샷은 피드를 모른다.
    pub(crate) fn install(self: &Arc<Self>) {
        CURRENT.with(|current| *current.borrow_mut() = Some(Arc::clone(self)));
    }

    /// 연결 종료: 대기를 즉시 깨워 끝내고 스레드를 회수한다. 진행 중인 스냅샷 하나는 끝까지 기다린다.
    pub(crate) fn stop(&self) {
        lock(&self.state).stopped = true;
        self.changed.notify_all();
        let thread = lock(&self.thread).take();
        if let Some(thread) = thread {
            thread.join().ok();
        }
    }

    fn observe(self: &Arc<Self>, request: Option<u64>, etag: &str, rev: u64) -> u64 {
        let mut state = lock(&self.state);
        if rev >= state.rev {
            state.rev = rev;
            state.etag = Some(etag.to_owned());
        }
        if let Some(ms) = request {
            state.interval =
                (ms > 0).then(|| Duration::from_millis(ms).clamp(MIN_INTERVAL, MAX_INTERVAL));
            state.due = state.interval.map(|interval| Instant::now() + interval);
            self.changed.notify_all();
        }
        if state.stopped || (state.interval.is_some() && !self.spawn()) {
            state.interval = None;
        }
        state
            .interval
            .map_or(0, |i| u64::try_from(i.as_millis()).unwrap_or(u64::MAX))
    }

    fn spawn(self: &Arc<Self>) -> bool {
        let mut thread = lock(&self.thread);
        if thread.is_some() {
            return true;
        }
        let feed = Arc::clone(self);
        *thread = thread::Builder::new()
            .name("mobile-feed".into())
            .stack_size(super::super::gateway::RPC_STACK)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                feed.run();
            })
            .ok();
        thread.is_some()
    }

    fn wait<'a>(
        &self,
        state: MutexGuard<'a, State>,
        timeout: Option<Duration>,
    ) -> MutexGuard<'a, State> {
        match timeout {
            None => self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
            Some(timeout) => {
                self.changed
                    .wait_timeout(state, timeout)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
        }
    }

    fn run(&self) {
        let mut state = lock(&self.state);
        loop {
            if state.stopped {
                return;
            }
            let (Some(interval), Some(due)) = (state.interval, state.due) else {
                state = self.wait(state, None);
                continue;
            };
            let now = Instant::now();
            if now < due {
                state = self.wait(state, Some(due - now));
                continue;
            }
            state.due = Some(now + interval);
            let since = state.etag.clone();
            drop(state);
            let snapshot = self
                .registry
                .upgrade()
                .and_then(|registry| registry.mobile_gateway().map(Arc::clone))
                .map(|gateway| gateway.backend().snapshot_versioned(since.as_deref()));
            state = lock(&self.state);
            let Some(Ok((value, rev))) = snapshot else {
                continue;
            };
            if state.stopped || state.interval.is_none() || rev <= state.rev {
                continue;
            }
            let Some((frame, etag)) = push(value, rev) else {
                continue;
            };
            // 상태 잠금 안에서 보내 응답 쪽 `observe`와 etag·rev 순서가 섞이지 않게 한다.
            match (self.sink)(&frame) {
                Ok(()) => {
                    state.etag = Some(etag);
                    state.rev = rev;
                }
                // 릴레이 아웃박스가 가득 참: 이번 푸시는 버리고(nonce 공백은 허용) 다음 간격에 최신 상태로 다시 시도한다.
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                // 연결이 깨졌다. sink가 연결을 이미 닫았다.
                Err(_) => {
                    state.stopped = true;
                    state.interval = None;
                    return;
                }
            }
        }
    }
}

/// 이 스레드에 피드가 있으면 스냅샷 응답을 반영하고, 응답에 실을 현재 간격(ms, 0=꺼짐)을 돌려준다.
pub(super) fn observe(request: Option<u64>, etag: &str, rev: u64) -> Option<u64> {
    let feed = CURRENT.with(|current| current.borrow().clone())?;
    Some(feed.observe(request, etag, rev))
}

/// 바뀐 스냅샷 푸시. 기존 응답과 같은 Workflow 프레임이고 JSON이 `{"push":`로 시작해 응답(`{"data"`/`{"ok"`)과 구별된다.
fn push(mut value: Value, rev: u64) -> Option<(Vec<u8>, String)> {
    if value.get("unchanged").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let etag = value.get("etag")?.as_str()?.to_owned();
    value.as_object_mut()?.insert("rev".into(), rev.into());
    let mut text = format!(
        "{{\"push\":\"snapshot\",\"data\":{}}}",
        serde_json::to_string(&value).ok()?
    );
    if text.len() >= ipc::MAX_FRAME.saturating_sub(32) {
        // 너무 크면 알림만 보내 클라이언트가 기존 요청 경로로 받게 한다.
        text = "{\"push\":\"stale\"}".into();
    }
    Some((MobileReply::Workflow(text).encode().ok()?, etag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::ipc::{FEATURES, PROTO_MAX, PROTO_MIN, ServerHello};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn registry() -> Arc<Registry> {
        let registry = Arc::new(Registry::new(None, None));
        registry
            .start_mobile(ServerHello {
                proto_min: PROTO_MIN,
                proto_max: PROTO_MAX,
                features: FEATURES,
                build: "feed-test".into(),
                instance_id: 1,
            })
            .unwrap();
        registry
    }
    fn until(mut done: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < end, "condition not reached");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn relay_backpressure_drops_one_push_and_resends_the_latest_state_once() {
        let registry = registry();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);
        let feed = Feed::new(Arc::downgrade(&registry), move |frame: &[u8]| {
            let (kind, body) =
                ipc::read_frame(&mut &frame[..])?.ok_or(io::ErrorKind::InvalidData)?;
            let MobileReply::Workflow(text) = MobileReply::decode(kind, &body)? else {
                return Err(io::ErrorKind::InvalidData.into());
            };
            assert!(text.starts_with("{\"push\":\"snapshot\",\"data\":{"));
            match counted.fetch_add(1, Ordering::SeqCst) {
                0 => Err(io::ErrorKind::WouldBlock.into()),
                _ => Ok(()),
            }
        });
        assert_eq!(
            feed.observe(Some(1), "never-sent", 0),
            1000,
            "clamped to 1s"
        );
        until(|| attempts.load(Ordering::SeqCst) >= 2);
        let delivered = lock(&feed.state).etag.clone();
        assert!(delivered.is_some_and(|etag| etag != "never-sent"));
        // Delivered state is not pushed again on later checks.
        thread::sleep(Duration::from_millis(2500));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let started = Instant::now();
        feed.stop();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(lock(&feed.thread).is_none());
    }

    #[test]
    fn broken_connection_stops_the_feed_and_later_replies_report_it_off() {
        let registry = registry();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);
        let feed = Feed::new(Arc::downgrade(&registry), move |_: &[u8]| {
            counted.fetch_add(1, Ordering::SeqCst);
            Err(io::ErrorKind::BrokenPipe.into())
        });
        assert_eq!(feed.observe(Some(1000), "never-sent", 0), 1000);
        until(|| lock(&feed.state).stopped);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            feed.observe(Some(1000), "x", 0),
            0,
            "a dead feed is never reported armed"
        );
        feed.stop();
    }

    #[test]
    fn paused_feed_thread_is_released_immediately_on_disconnect() {
        let registry = registry();
        let feed = Feed::new(Arc::downgrade(&registry), |_: &[u8]| Ok(()));
        assert_eq!(feed.observe(Some(60_000), "e", 0), 60_000);
        assert_eq!(feed.observe(Some(0), "e", 0), 0);
        assert!(
            lock(&feed.thread).is_some(),
            "one thread per connection, reused"
        );
        assert_eq!(feed.observe(Some(2500), "e", 0), 2500);
        let started = Instant::now();
        feed.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            feed.observe(Some(2500), "e", 0),
            0,
            "no respawn after disconnect"
        );
        assert!(lock(&feed.thread).is_none());
    }
}
