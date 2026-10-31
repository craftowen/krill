use super::*;
use krill_proto::search::Direction;
use std::sync::mpsc;
use std::time::Duration;

fn request(req: u32) -> SearchOptionsRequest {
    SearchOptionsRequest {
        request: SearchRequest {
            req,
            query: format!("query-{req}"),
            direction: Direction::Next,
            after: None,
        },
        options: Options::default(),
    }
}

#[test]
fn blocked_result_delivery_does_not_block_new_queries_or_queue_close() {
    for close in [false, true] {
        let queue = Arc::new(Queue::default());
        queue.submit(request(1));
        let (_, generation) = queue.next().unwrap();
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let publishing = Arc::clone(&queue);
        let writer = thread::spawn(move || {
            publishing.publish(generation, || {
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, finished) = mpsc::channel();
        let pending = Arc::clone(&queue);
        let submitter = thread::spawn(move || {
            pending.submit(request(2));
            pending.submit(request(3));
            if close {
                pending.close();
            }
            done.send(()).unwrap();
        });
        let result = finished.recv_timeout(Duration::from_secs(1));
        // Always release our simulated slow writer before asserting, including on failure.
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        submitter.join().unwrap();
        assert!(
            result.is_ok(),
            "result delivery blocked queue operations: {result:?}"
        );
        if close {
            assert!(queue.next().is_none());
        } else {
            assert_eq!(queue.next().unwrap().0, request(3));
            queue.close();
        }
    }
}

#[test]
fn obsolete_or_closed_search_results_are_not_published() {
    let queue = Queue::default();
    queue.submit(request(1));
    let (_, old) = queue.next().unwrap();
    queue.submit(request(2));
    queue
        .publish(old, || panic!("obsolete result sent"))
        .unwrap();
    let (req, current) = queue.next().unwrap();
    assert_eq!(req, request(2));
    let mut sent = false;
    queue
        .publish(current, || {
            sent = true;
            Ok(())
        })
        .unwrap();
    assert!(sent);
    queue.close();
    queue
        .publish(current, || panic!("closed result sent"))
        .unwrap();
    queue.submit(request(3));
    assert!(queue.next().is_none());
}

#[test]
#[cfg(unix)]
fn search_panic_keeps_connection_and_allows_retry() {
    use krill_proto::ipc;
    use krill_proto::os::net::UnixStream;
    use krill_proto::search::Snapshot;
    use std::io::Read as _;
    use std::sync::atomic::AtomicBool;

    let pty = rustix_openpty::openpty(None, None).unwrap();
    let mut output = std::fs::File::from(pty.user);
    let session = crate::Session::start(
        krill_proto::SessionId(53),
        pty.controller,
        krill_proto::holder::WinSize { cols: 80, rows: 24 },
        Arc::new(crate::Appearance::load(std::path::PathBuf::from(
            "/nonexistent/search-panic-theme",
        ))),
        None,
    )
    .unwrap();
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let conn = Arc::new(Conn {
        out: Mutex::new(server),
        resync: AtomicBool::new(false),
        echo: AtomicBool::new(false),
        gone: AtomicBool::new(false),
    });
    let (started, began) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = Worker::spawn(session.tap(), Arc::clone(&conn), move |_, req, _| {
        if req.request.req == 1 {
            started.send(()).unwrap();
            released.recv().unwrap();
            panic!("injected search replay failure");
        }
        SearchResult {
            req: req.request.req,
            snapshot: Snapshot::default(),
            total: 0,
            hit: None,
            wrapped: false,
            stale: false,
            incomplete: false,
            approximate: false,
        }
    })
    .unwrap();
    worker.submit_options(request(1));
    began.recv_timeout(Duration::from_secs(2)).unwrap();
    release.send(()).unwrap();
    let (kind, body) = ipc::read_frame(&mut client)
        .unwrap()
        .expect("a failed search must not close the terminal connection");
    let failed = SearchResult::decode(kind, &body).unwrap();
    assert_eq!(failed.req, 1);
    assert!(failed.incomplete);
    assert!(failed.hit.is_none());
    worker.submit_options(request(2));
    let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
    let retried = SearchResult::decode(kind, &body).unwrap();
    assert_eq!(retried.req, 2);
    assert!(!retried.incomplete);
    assert!(!session.tap().pulse().closed);
    // Check actual input delivery through the same live PTY after replay has panicked.
    let input = b"still-alive\n";
    session.tap().write_input(input, None).unwrap();
    let mut fds = [rustix::event::PollFd::new(
        &output,
        rustix::event::PollFlags::IN,
    )];
    assert!(
        rustix::event::poll(
            &mut fds,
            Some(&rustix::event::Timespec {
                tv_sec: 2,
                tv_nsec: 0
            }),
        )
        .unwrap()
            > 0
    );
    let mut received = vec![0; input.len()];
    output.read_exact(&mut received).unwrap();
    assert_eq!(received, input);
    let ack = krill_proto::input::InputAck {
        id: krill_proto::input::InputId(7),
        result: krill_proto::input::InputResult::Applied,
    };
    conn.send(&ack.encode().unwrap()).unwrap();
    let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
    assert_eq!(
        krill_proto::input::InputAck::decode(kind, &body).unwrap(),
        ack
    );
    drop(worker);
    drop(output);
    session.wait().unwrap();
}
