//! 오케스트레이션 워커 해제(Release)의 검증된 세션 종료(R2).
//!
//! CLI `krill orchestration worker-release`와 같은 순서다. 먼저 터미널 출력을 캡처해
//! `archived_output`에 굳히고(실패하면 저장된 값이 있으면 그것을 쓰고, 없으면 여기서 그만둔다),
//! 다음으로 세션이 목록에 남아 있을 때만 `Close`를 보내고 `Closed` 확인 + 목록에서 사라짐을
//! 확인한다. 검증을 통과해야 `Released`를 저장한다.
//! 앞에서 실패하면 디스패치 상태는 그대로 두고 오류를 알린다. PTY가 살아 있는데 UI에서
//! 사라지는 일을 막는다.

use krill_proto::os::net::UnixStream;
use std::io::{self, Write as _};
use std::time::{Duration, Instant};

use gpui::Context;
use krill_app::hosts::HostTarget;
use krill_proto::control::{CaptureFormat, FEAT_CLI, FEAT_CONTROL, MAX_CAPTURE, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::orchestration::{Dispatch, DispatchStatus, OrchestrationStore};
use krill_proto::screen::Msg as _;

use krill_app::i18n::catalog::popout as text;
use krill_proto::SessionId;

use super::Shell;

/// `Close` 뒤 목록에서 사라지기를 기다리는 상한(CLI `CLOSE_TIMEOUT`과 같은 값).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
/// 제어 연결 읽기·쓰기 상한.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// `Close`·`List` 폴링 간격(CLI와 같은 값).
const CLOSE_POLL: Duration = Duration::from_millis(20);

/// 해제 계획. 소켓 없이 순서만 검증한다.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ReleasePlan {
    pub dispatch_id: String,
    pub session: Option<u64>,
    pub status: DispatchStatus,
    pub archived: Option<String>,
    /// 세션이 목록에 남아 있고 아직 해제 전이라 `Close` 검증이 필요하다.
    pub needs_close: bool,
}

/// 해제 계획을 세운다. 이미 `Released`면 닫을 것이 없다.
pub(super) fn plan_release(
    store: &OrchestrationStore,
    dispatch_id: &str,
    listed: &[u64],
) -> io::Result<ReleasePlan> {
    let found = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == dispatch_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "worker not found"))?;
    found.validate_release()?;
    let owner = found.session_id.and_then(|sid| {
        store
            .dispatches
            .iter()
            .rev()
            .find(|d| {
                d.session_id == Some(sid)
                    && matches!(d.status, DispatchStatus::Active | DispatchStatus::Retained)
            })
            .or_else(|| {
                store
                    .dispatches
                    .iter()
                    .rev()
                    .find(|d| d.session_id == Some(sid))
            })
    });
    let owns_session = owner.is_some_and(|owner| owner.id == found.id);
    Ok(ReleasePlan {
        dispatch_id: found.id.clone(),
        session: found.session_id.filter(|_| owns_session),
        status: if found.is_released() {
            DispatchStatus::Released
        } else {
            found.status
        },
        archived: found.archived_output.clone(),
        needs_close: !found.is_released()
            && owns_session
            && found.session_id.is_some_and(|sid| listed.contains(&sid)),
    })
}

/// 캡처 결과를 확정한다. 세션이 없거나 이미 해제됐으면 캡처가 필요 없다.
/// 캡처에 실패하면 저장된 아카이브로 대신하고, 그것도 없으면 오류다(상태는 그대로).
pub(super) fn resolve_capture(
    plan: &ReleasePlan,
    attempt: Option<io::Result<String>>,
) -> io::Result<Option<String>> {
    if plan.session.is_none() || plan.status == DispatchStatus::Released {
        return Ok(None);
    }
    match attempt {
        None => Ok(None),
        Some(Ok(text)) => Ok(Some(text)),
        Some(Err(e)) => match plan.archived.clone() {
            Some(saved) => Ok(Some(saved)),
            None => Err(e),
        },
    }
}

/// 아카이빙 확정 → `close` 검증 → `Released` 저장. 앞에서 실패하면 상태는 그대로다
/// (CLI `complete_worker_stop`과 같은 순서).
pub(super) fn complete_worker_release(
    state_dir: &std::path::Path,
    plan: &ReleasePlan,
    capture: Option<String>,
    close: impl FnOnce() -> io::Result<()>,
) -> io::Result<Dispatch> {
    // Keep ownership stable through Close. A newly assigned worker cannot borrow
    // this PTY between authority validation and the verified close operation.
    OrchestrationStore::mutate(state_dir, |s| {
        let listed: Vec<_> = plan
            .session
            .filter(|_| plan.needs_close)
            .into_iter()
            .collect();
        let current = plan_release(s, &plan.dispatch_id, &listed)?;
        if current.session != plan.session {
            return Err(io::Error::other("worker session ownership changed"));
        }
        if let Some(text) = capture {
            let stored = s
                .dispatches
                .iter_mut()
                .find(|d| d.id == plan.dispatch_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "dispatch missing"))?;
            stored.archived_output = Some(text);
            // Preserve the captured output even if Close is denied or times out.
            s.save(state_dir)?;
        }
        if current.status != DispatchStatus::Released {
            close()?;
        }
        s.stop_or_release_worker(&plan.dispatch_id, true)
    })
}

fn roundtrip(stream: &mut UnixStream, req: &Request) -> io::Result<Reply> {
    stream.write_all(&req.encode().map_err(io::Error::other)?)?;
    let (kind, bytes) = ipc::read_frame(stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    Reply::decode(kind, &bytes).map_err(io::Error::other)
}

/// 세션 화면을 평문으로 캡처한다. `FEAT_CLI` 협상된 연결에서만 부른다.
fn capture_text(stream: &mut UnixStream, sid: SessionId) -> io::Result<String> {
    match roundtrip(
        stream,
        &Request::Capture {
            session: sid,
            format: CaptureFormat::Plain,
        },
    )? {
        Reply::Captured { bytes, .. } if bytes.len() <= MAX_CAPTURE => {
            String::from_utf8(bytes).map_err(io::Error::other)
        }
        _ => Err(io::Error::other("unexpected capture reply")),
    }
}

fn list_ids(stream: &mut UnixStream) -> io::Result<Vec<u64>> {
    match roundtrip(stream, &Request::List)? {
        Reply::Sessions(sessions) => Ok(sessions.into_iter().map(|s| s.session.0).collect()),
        _ => Err(io::Error::other("unexpected session list reply")),
    }
}

/// `Close`를 보내 `Closed`를 확인하고 목록에서 사라질 때까지 기다린다.
/// 거부(`Failed`)·EOF·중간 종료·시간 초과는 모두 오류다.
fn close_verified(stream: &mut UnixStream, sid: SessionId, timeout: Duration) -> io::Result<()> {
    match roundtrip(stream, &Request::Close { session: sid })? {
        Reply::Closed => {}
        _ => return Err(io::Error::other("close request denied")),
    }
    let start = Instant::now();
    loop {
        if !list_ids(stream)?.contains(&sid.0) {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "session {} did not close within timeout",
                sid.0
            )));
        }
        std::thread::sleep(CLOSE_POLL);
    }
}

/// 해제 흐름 결과.
enum FlowOutcome {
    Done,
    AlreadyReleased,
}

/// 검증된 해제 전체 흐름(백그라운드 스레드에서만 호출). 실패하면 상태는 그대로다.
fn release_worker_blocking(
    target: &HostTarget,
    dir: &std::path::Path,
    dispatch_id: &str,
) -> io::Result<FlowOutcome> {
    let store = OrchestrationStore::load(dir)?;
    let hello = daemon::client_hello(ClientRole::App, "krill-app-release");
    let conn = target
        .connect(&hello)
        .map_err(|e| io::Error::other(format!("daemon connection failed: {e}")))?;
    if conn.negotiated.features & (FEAT_CONTROL | FEAT_CLI) != (FEAT_CONTROL | FEAT_CLI) {
        return Err(io::Error::other("daemon does not support worker release"));
    }
    let mut sock = conn.stream;
    sock.set_read_timeout(Some(IO_TIMEOUT))
        .map_err(io::Error::other)?;
    sock.set_write_timeout(Some(IO_TIMEOUT))
        .map_err(io::Error::other)?;
    let listed = list_ids(&mut sock)?;
    let plan = plan_release(&store, dispatch_id, &listed)?;
    if plan.status == DispatchStatus::Released {
        return Ok(FlowOutcome::AlreadyReleased);
    }
    let attempt = plan
        .session
        .map(|sid| capture_text(&mut sock, SessionId(sid)));
    let capture = resolve_capture(&plan, attempt)?;
    complete_worker_release(dir, &plan, capture, || match plan.session {
        Some(sid) if plan.needs_close => close_verified(&mut sock, SessionId(sid), CLOSE_TIMEOUT),
        _ => Ok(()),
    })?;
    Ok(FlowOutcome::Done)
}

impl Shell {
    /// 오케스트레이션 워커 해제. 아카이빙→Close 검증→Released 순서로 진행하고
    /// 실패하면 상태를 그대로 둔 채 오류 배너를 띄운다. UI 스레드를 막지 않는다.
    pub(super) fn release_worker(
        &mut self,
        dispatch_id: String,
        state_dir: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = state_dir else {
            cx.notify();
            return;
        };
        let Some(target) = self.paths.clone().map(HostTarget::local) else {
            self.notice =
                Some(text::WORKER_RELEASE_FAILED.format(&[("reason", "daemon not connected")]));
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { release_worker_blocking(&target, &dir, &dispatch_id) })
                .await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(FlowOutcome::Done) => {
                        this.refresh_detached(cx);
                    }
                    Ok(FlowOutcome::AlreadyReleased) => {}
                    Err(e) => {
                        this.notice =
                            Some(text::WORKER_RELEASE_FAILED.format(&[("reason", &e.to_string())]));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::orchestration::DispatchStatus;

    fn test_store(dir: &std::path::Path) -> OrchestrationStore {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("release run", Some(1)).unwrap();
        store
            .start_worker(
                &run.id,
                None,
                "backend",
                "claude",
                Some(10),
                "/tmp",
                "Do the thing",
            )
            .unwrap();
        store.dispatches[0].status = DispatchStatus::Stopped;
        store.save(dir).unwrap();
        store
    }

    #[test]
    fn plan_release_marks_close_only_for_listed_live_workers() {
        let dir = std::env::temp_dir().join(format!("mk-release-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = test_store(&dir);
        let listed = [10u64];
        let plan = plan_release(&store, store.dispatches[0].id.clone().as_str(), &listed).unwrap();
        assert!(plan.needs_close);
        assert_eq!(plan.session, Some(10));
        let plan = plan_release(&store, store.dispatches[0].id.clone().as_str(), &[]).unwrap();
        assert!(!plan.needs_close);
        assert!(plan_release(&store, "missing", &listed).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn release_rejects_active_borrowed_and_reassigned_sessions_before_close() {
        use krill_proto::orchestration::DispatchSessionOrigin;
        let dir = std::env::temp_dir().join(format!("mk-release-authority-{}", std::process::id()));
        let mut store = test_store(&dir);
        let id = store.dispatches[0].id.clone();
        let plan = plan_release(&store, &id, &[10]).unwrap();
        let closed = std::cell::Cell::new(false);
        for borrowed in [false, true] {
            store.dispatches[0].status = if borrowed {
                DispatchStatus::Stopped
            } else {
                DispatchStatus::Active
            };
            store.dispatches[0].session_origin = if borrowed {
                DispatchSessionOrigin::Borrowed
            } else {
                DispatchSessionOrigin::Created
            };
            store.save(&dir).unwrap();
            assert!(
                complete_worker_release(&dir, &plan, Some("keep".into()), || {
                    closed.set(true);
                    Ok(())
                })
                .is_err()
            );
            assert!(!closed.get());
            assert_eq!(OrchestrationStore::load(&dir).unwrap(), store);
        }
        store.dispatches[0].status = DispatchStatus::Stopped;
        store.dispatches[0].session_origin = DispatchSessionOrigin::Created;
        let run_id = store.runs[0].id.clone();
        store
            .start_worker(
                &run_id,
                None,
                "new-owner",
                "claude",
                Some(10),
                "/tmp",
                "next",
            )
            .unwrap();
        store.save(&dir).unwrap();
        assert!(
            complete_worker_release(&dir, &plan, None, || {
                closed.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!closed.get());
        assert_eq!(OrchestrationStore::load(&dir).unwrap(), store);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn complete_worker_release_orders_archive_close_then_released() {
        let dir = std::env::temp_dir().join(format!("mk-release-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = test_store(&dir);
        let id = store.dispatches[0].id.clone();
        let plan = plan_release(&store, &id, &[10]).unwrap();

        // 성공: 아카이브 저장 → Released.
        let done =
            complete_worker_release(&dir, &plan, Some("output".to_owned()), || Ok(())).unwrap();
        assert!(done.is_released());
        assert_eq!(done.status, DispatchStatus::Stopped);
        assert_eq!(done.archived_output.as_deref(), Some("output"));

        // 실패(Denied): 상태는 그대로, 아카이브는 굳어 있다.
        let dir = std::env::temp_dir().join(format!("mk-release-deny-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = test_store(&dir);
        let id = store.dispatches[0].id.clone();
        let plan = plan_release(&store, &id, &[10]).unwrap();
        let denied = || Err::<(), _>(io::Error::other("close request denied"));
        assert!(complete_worker_release(&dir, &plan, Some("output".to_owned()), denied).is_err());
        let kept = OrchestrationStore::load(&dir).unwrap();
        assert_eq!(kept.dispatches[0].status, DispatchStatus::Stopped);
        assert_eq!(
            kept.dispatches[0].archived_output.as_deref(),
            Some("output")
        );

        // 캡처도 close도 없으면 상태만 바뀐다.
        let plan = plan_release(&kept, &id, &[]).unwrap();
        assert!(!plan.needs_close);
        let done = complete_worker_release(&dir, &plan, None, || Ok(())).unwrap();
        assert!(done.is_released());

        // 없는 디스패치는 저장소를 건드리지 않는다.
        let bogus = ReleasePlan {
            dispatch_id: "missing".to_owned(),
            session: None,
            status: DispatchStatus::Active,
            archived: None,
            needs_close: false,
        };
        assert!(complete_worker_release(&dir, &bogus, None, || Ok(())).is_err());
    }

    #[test]
    fn resolve_capture_falls_back_to_archived_output() {
        let plan = ReleasePlan {
            dispatch_id: "d".to_owned(),
            session: Some(10),
            status: DispatchStatus::Active,
            archived: Some("saved".to_owned()),
            needs_close: true,
        };
        assert_eq!(
            resolve_capture(&plan, Some(Ok("fresh".to_owned()))).unwrap(),
            Some("fresh".to_owned())
        );
        assert_eq!(
            resolve_capture(&plan, Some(Err(io::Error::other("capture failed")))).unwrap(),
            Some("saved".to_owned())
        );
        let bare = ReleasePlan {
            archived: None,
            ..plan
        };
        assert!(resolve_capture(&bare, Some(Err(io::Error::other("capture failed")))).is_err());
        let released = ReleasePlan {
            session: Some(10),
            status: DispatchStatus::Released,
            archived: None,
            ..bare
        };
        assert_eq!(resolve_capture(&released, None).unwrap(), None);
    }

    /// 스크립트 상대와 소켓 대화를 검증한다.
    mod socket {
        use super::*;
        use krill_proto::control::Info;

        fn peer(handle: impl FnMut(Request) -> Reply + Send + 'static) -> UnixStream {
            let (ours, mut theirs) = UnixStream::pair().unwrap();
            let mut handle = handle;
            std::thread::spawn(move || {
                loop {
                    let Ok(Some((kind, bytes))) = ipc::read_frame(&mut theirs) else {
                        return;
                    };
                    let Ok(req) = Request::decode(kind, &bytes) else {
                        return;
                    };
                    let Ok(frame) = handle(req).encode() else {
                        return;
                    };
                    if theirs.write_all(&frame).is_err() {
                        return;
                    }
                }
            });
            ours
        }

        fn sessions_reply(ids: &[u64]) -> Reply {
            Reply::Sessions(
                ids.iter()
                    .map(|id| Info {
                        session: SessionId(*id),
                        pid: 1,
                        status: None,
                    })
                    .collect(),
            )
        }

        fn close_peer(gone: bool) -> UnixStream {
            peer(move |req| match req {
                Request::Close { .. } => Reply::Closed,
                Request::List if gone => sessions_reply(&[]),
                Request::List => sessions_reply(&[10]),
                _ => Reply::Failed(krill_proto::control::Failure::BadRequest),
            })
        }

        #[test]
        fn close_verified_confirms_closed_and_gone() {
            let mut stream = close_peer(true);
            close_verified(&mut stream, SessionId(10), Duration::from_secs(5)).unwrap();
        }

        #[test]
        fn close_verified_rejects_denied_and_eof() {
            // 거부(Denied): Failed를 보내면 오류다.
            let mut denied = peer(|_| Reply::Failed(krill_proto::control::Failure::BadRequest));
            assert!(close_verified(&mut denied, SessionId(10), Duration::from_secs(5)).is_err());

            // 응답 EOF: 상대가 끊으면 오류다.
            let (mut eof, peer) = UnixStream::pair().unwrap();
            drop(peer);
            assert!(close_verified(&mut eof, SessionId(10), Duration::from_secs(5)).is_err());
        }

        #[test]
        fn close_verified_times_out_when_session_stays_listed() {
            // 세션이 목록에 계속 남아 있으면 시간 초과다.
            let mut stream = close_peer(false);
            assert!(
                close_verified(&mut stream, SessionId(10), Duration::from_millis(120)).is_err()
            );
        }

        #[test]
        fn capture_text_reads_plain_screen() {
            let mut stream = peer(|req| match req {
                Request::Capture { .. } => Reply::Captured {
                    seq: krill_proto::screen::Seq(3),
                    bytes: b"hello".to_vec(),
                },
                _ => Reply::Failed(krill_proto::control::Failure::BadRequest),
            });
            assert_eq!(capture_text(&mut stream, SessionId(10)).unwrap(), "hello");
            let mut denied = peer(|_| Reply::Failed(krill_proto::control::Failure::BadRequest));
            assert!(capture_text(&mut denied, SessionId(10)).is_err());
        }
    }
}
