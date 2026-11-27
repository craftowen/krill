//! 모바일 대시보드 변경 피드. 인증된 PWA 연결 하나가 스냅샷 폴링 대신 같은 E2EE 채널로 바뀐 스냅샷만 받는다.
//! 데몬 작업은 폴링과 같다: 클라이언트가 고른 간격마다 같은 `snapshot_since`를 부르고, etag가 같으면 아무것도 보내지 않는다.
//! 응답과 푸시는 연결의 봉인·쓰기 잠금 하나로 직렬화되므로 긴 대기가 명령 응답을 막지 않는다.
use crate::registry::Registry;
use crate::session::lock;
use krill_proto::ipc;
use krill_proto::mobile::MobileReply;
use krill_proto::screen::Msg;
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use serde_json::Value;
use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 클라이언트 간격 요청의 허용 범위. 0은 일시정지(숨은 탭)다.
const MIN_INTERVAL: Duration = Duration::from_secs(1);
const MAX_INTERVAL: Duration = Duration::from_secs(60);
const CHAT_DEBOUNCE: Duration = Duration::from_millis(80);
const CHAT_FALLBACK_TICK: Duration = Duration::from_secs(1);

type Sink = Box<dyn Fn(&[u8]) -> io::Result<()> + Send + Sync>;

pub(crate) struct ChatFeedPoll {
    pub(crate) delta: Option<Value>,
    pub(crate) next_offset: u64,
    pub(crate) watch_path: Option<PathBuf>,
}

struct ChatSub {
    session: u64,
    offset: u64,
    watch_path: Option<PathBuf>,
    last_notify: Option<Instant>,
    next_tick: Instant,
    watcher: Option<RecommendedWatcher>,
}

/// 연결 하나의 피드. 스레드는 첫 구독 때만 만들고 연결이 끝나면 `stop`이 멈추고 회수한다.
pub(crate) struct Feed {
    me: Weak<Self>,
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
    chat_sub: Option<ChatSub>,
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

fn watch_chat_target(
    watch_path: Option<&Path>,
    weak_feed: Weak<Feed>,
) -> Option<RecommendedWatcher> {
    let path = watch_path?;
    let target_dir = path.parent()?;
    if !target_dir.exists() {
        return None;
    }
    let file_name: OsString = path.file_name()?.to_os_string();
    let mut wal_name = file_name.clone();
    wal_name.push("-wal");
    let mut shm_name = file_name.clone();
    shm_name.push("-shm");
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(ev) = event else {
            return;
        };
        let relevant = ev.paths.is_empty()
            || ev.paths.iter().any(|p| {
                p.file_name()
                    .is_some_and(|name| name == file_name || name == wal_name || name == shm_name)
            });
        if relevant && let Some(feed) = weak_feed.upgrade() {
            feed.note_chat_fs_event();
        }
    })
    .ok()?;
    watcher
        .watch(target_dir, RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

impl Feed {
    /// `sink`은 IPC 프레임 하나를 연결의 응답과 같은 순서 잠금 아래에서 봉인해 보낸다.
    pub(crate) fn new(
        registry: Weak<Registry>,
        sink: impl Fn(&[u8]) -> io::Result<()> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
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
        let old_sub = {
            let mut state = lock(&self.state);
            state.stopped = true;
            state.chat_sub.take()
        };
        drop(old_sub);
        self.changed.notify_all();
        let thread = lock(&self.thread).take();
        if let Some(thread) = thread {
            thread.join().ok();
        }
    }

    fn note_chat_fs_event(&self) {
        let mut state = lock(&self.state);
        if state.stopped {
            return;
        }
        if let Some(sub) = state.chat_sub.as_mut() {
            sub.last_notify.get_or_insert_with(Instant::now);
            self.changed.notify_all();
        }
    }

    fn subscribe_chat(
        self: &Arc<Self>,
        session: u64,
        offset: u64,
        watch_path: Option<PathBuf>,
    ) -> bool {
        let watcher = watch_chat_target(watch_path.as_deref(), Weak::clone(&self.me));
        let now = Instant::now();
        let mut old_sub = None;
        let armed = {
            let mut state = lock(&self.state);
            if state.stopped {
                false
            } else {
                old_sub = state.chat_sub.replace(ChatSub {
                    session,
                    offset,
                    watch_path,
                    last_notify: Some(now),
                    next_tick: now + CHAT_FALLBACK_TICK,
                    watcher,
                });
                self.changed.notify_all();
                if self.spawn() {
                    true
                } else {
                    old_sub = state.chat_sub.take().or(old_sub);
                    false
                }
            }
        };
        drop(old_sub);
        armed
    }

    fn unsubscribe_chat(&self, session: u64) {
        let old_sub = {
            let mut state = lock(&self.state);
            if state
                .chat_sub
                .as_ref()
                .is_some_and(|sub| sub.session == session)
            {
                let taken = state.chat_sub.take();
                self.changed.notify_all();
                taken
            } else {
                None
            }
        };
        drop(old_sub);
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
        struct FeedThreadGuard {
            feed: Arc<Feed>,
        }
        impl Drop for FeedThreadGuard {
            fn drop(&mut self) {
                let old_sub = {
                    let mut state = lock(&self.feed.state);
                    state.chat_sub.take()
                };
                drop(old_sub);
                *lock(&self.feed.thread) = None;
            }
        }
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
                let _guard = FeedThreadGuard {
                    feed: Arc::clone(&feed),
                };
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    feed.run();
                }))
                .is_err()
                {
                    tracing::error!("mobile-feed thread panicked");
                }
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
                let old_sub = state.chat_sub.take();
                drop(state);
                drop(old_sub);
                return;
            }
            let snapshot_due = state.interval.zip(state.due);
            let chat_due = state.chat_sub.as_ref().map(|sub| {
                sub.last_notify
                    .map_or(sub.next_tick, |at| at + CHAT_DEBOUNCE)
            });

            let next_due = match (snapshot_due.map(|(_, d)| d), chat_due) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            let Some(due) = next_due else {
                state = self.wait(state, None);
                continue;
            };
            let now = Instant::now();
            if now < due {
                state = self.wait(state, Some(due - now));
                continue;
            }

            if chat_due.is_some_and(|cd| cd <= now)
                && let Some(sub) = state.chat_sub.as_mut()
            {
                sub.last_notify = None;
                sub.next_tick = now + CHAT_FALLBACK_TICK;
                let session = sub.session;
                let offset = sub.offset;
                let prev_path = sub.watch_path.clone();
                drop(state);

                let poll = self
                    .registry
                    .upgrade()
                    .and_then(|registry| registry.mobile_gateway().map(Arc::clone))
                    .map(|gateway| gateway.backend().poll_chat_for_feed(session, offset));
                let mut new_watcher = None;
                let mut path_changed = false;
                if let Some(Ok(ref res)) = poll
                    && res.watch_path != prev_path
                {
                    path_changed = true;
                    new_watcher =
                        watch_chat_target(res.watch_path.as_deref(), Weak::clone(&self.me));
                }

                state = lock(&self.state);
                if state.stopped {
                    let old_sub = state.chat_sub.take();
                    drop(state);
                    drop(new_watcher);
                    drop(old_sub);
                    return;
                }
                let still_matching = state
                    .chat_sub
                    .as_ref()
                    .is_some_and(|s| s.session == session && s.offset == offset);
                if !still_matching {
                    drop(state);
                    drop(new_watcher);
                    state = lock(&self.state);
                    continue;
                }
                let mut old_watcher = None;
                if let Some(Ok(res)) = poll {
                    if path_changed && let Some(s) = state.chat_sub.as_mut() {
                        s.watch_path = res.watch_path;
                        old_watcher = s.watcher.take();
                        s.watcher = new_watcher.take();
                    }
                    if let Some(delta) = res.delta
                        && let Some(frame) = chat_push(session, &delta)
                    {
                        match (self.sink)(&frame) {
                            Ok(()) => {
                                if let Some(s) = state.chat_sub.as_mut() {
                                    s.offset = res.next_offset;
                                }
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Err(_) => {
                                state.stopped = true;
                                state.interval = None;
                                let old_sub = state.chat_sub.take();
                                drop(state);
                                drop(old_watcher);
                                drop(new_watcher);
                                drop(old_sub);
                                return;
                            }
                        }
                    }
                }
                if old_watcher.is_some() || new_watcher.is_some() {
                    drop(state);
                    drop(old_watcher);
                    drop(new_watcher);
                    state = lock(&self.state);
                }
                continue;
            }

            if let Some((interval, s_due)) = snapshot_due
                && s_due <= now
            {
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
                        let old_sub = state.chat_sub.take();
                        drop(state);
                        drop(old_sub);
                        return;
                    }
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

pub(super) fn subscribe_chat(
    session: u64,
    offset: u64,
    watch_path: Option<PathBuf>,
) -> Option<bool> {
    let feed = CURRENT.with(|current| current.borrow().clone())?;
    Some(feed.subscribe_chat(session, offset, watch_path))
}

pub(super) fn unsubscribe_chat(session: u64) {
    if let Some(feed) = CURRENT.with(|current| current.borrow().clone()) {
        feed.unsubscribe_chat(session);
    }
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

fn chat_push(session: u64, data: &Value) -> Option<Vec<u8>> {
    let mut text = format!(
        "{{\"push\":\"chat_delta\",\"session\":{session},\"data\":{}}}",
        serde_json::to_string(data).ok()?
    );
    if text.len() >= ipc::MAX_FRAME.saturating_sub(32) {
        text = format!("{{\"push\":\"chat_stale\",\"session\":{session}}}");
    }
    MobileReply::Workflow(text).encode().ok()
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

    #[test]
    fn chat_push_encodes_delta_and_downgrades_oversized_payload_to_chat_stale() {
        let small = chat_push(9, &serde_json::json!({"items": [], "next_offset": 12})).unwrap();
        let (kind, body) = ipc::read_frame(&mut &small[..]).unwrap().unwrap();
        let MobileReply::Workflow(text) = MobileReply::decode(kind, &body).unwrap() else {
            panic!("expected workflow frame");
        };
        assert!(text.starts_with("{\"push\":\"chat_delta\",\"session\":9,"));

        let oversized = serde_json::json!({
            "items": [{"text": "x".repeat(ipc::MAX_FRAME)}],
            "next_offset": 99
        });
        let stale = chat_push(9, &oversized).unwrap();
        let (kind, body) = ipc::read_frame(&mut &stale[..]).unwrap().unwrap();
        let MobileReply::Workflow(text) = MobileReply::decode(kind, &body).unwrap() else {
            panic!("expected workflow frame");
        };
        assert_eq!(text, "{\"push\":\"chat_stale\",\"session\":9}");
    }

    #[test]
    fn subscribe_chat_and_unsubscribe_manage_feed_thread_cleanly() {
        let registry = registry();
        let dir = std::env::temp_dir().join(format!("mk-feed-sub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(&path, "").unwrap();

        let feed = Feed::new(Arc::downgrade(&registry), |_: &[u8]| Ok(()));
        assert!(feed.subscribe_chat(42, 0, Some(path.clone())));
        assert!(lock(&feed.state).chat_sub.is_some());
        assert!(lock(&feed.thread).is_some());

        std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
        until(|| {
            lock(&feed.state)
                .chat_sub
                .as_ref()
                .is_some_and(|s| s.next_tick > Instant::now())
        });

        feed.unsubscribe_chat(42);
        assert!(lock(&feed.state).chat_sub.is_none());

        feed.stop();
        assert!(lock(&feed.thread).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mobile_feed_catches_panic_and_clears_thread_slot_for_recovery() {
        let registry = registry();
        let feed = Feed::new(Arc::downgrade(&registry), |_: &[u8]| {
            panic!("simulated feed sink panic");
        });
        // Trigger a snapshot push immediately after observe; sink panic must be caught and thread slot cleared.
        feed.observe(Some(1_000), "different-etag", 1);
        {
            let mut state = lock(&feed.state);
            state.due = Some(Instant::now());
        }
        feed.changed.notify_all();
        until(|| lock(&feed.thread).is_none());
        feed.stop();
        assert!(lock(&feed.thread).is_none());
    }
}
