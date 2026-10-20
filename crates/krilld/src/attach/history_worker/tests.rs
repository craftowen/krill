use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

use krill_proto::ipc;
use krill_proto::os::net::UnixStream;
use krill_proto::screen::{Source, kind};

use super::*;

fn request(req: u32) -> HistoryRequest {
    HistoryRequest {
        req,
        source: Source::Cold { page: 0 },
        skip: 0,
        count: 1,
    }
}

fn reply(req: &HistoryRequest) -> HistoryRows {
    HistoryRows {
        req: req.req,
        total: 0,
        pages: 0,
        skip: req.skip,
        rows: Vec::new(),
    }
}

fn connection() -> (UnixStream, Arc<Conn>) {
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    (
        client,
        Arc::new(Conn {
            out: Mutex::new(server),
            resync: AtomicBool::new(false),
            gone: AtomicBool::new(false),
        }),
    )
}

#[test]
fn invalidation_discards_running_and_pending_history_before_new_generation() {
    let (mut client, conn) = connection();
    let (started, starts) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = Worker::spawn(SessionId(1), conn, move |req| {
        started.send(req.req).unwrap();
        released.recv().unwrap();
        reply(req)
    })
    .unwrap();
    worker.submit(request(1)).unwrap();
    assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
    worker.submit(request(2)).unwrap();
    worker.submit(request(3)).unwrap();
    worker.invalidate();
    worker.submit(request(4)).unwrap();
    release.send(()).unwrap();
    assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 4);
    release.send(()).unwrap();
    let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
    assert_eq!(kind, kind::HISTORY_ROWS);
    assert_eq!(HistoryRows::decode(kind, &body).unwrap().req, 4);
    drop(worker);
}

#[test]
fn pending_history_is_bounded_and_preserves_request_order() {
    let (_client, conn) = connection();
    let (started, starts) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = Worker::spawn(SessionId(1), conn, move |req| {
        started.send(req.req).unwrap();
        released.recv().unwrap();
        reply(req)
    })
    .unwrap();
    worker.submit(request(0)).unwrap();
    assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
    for req in 1..=u32::try_from(MAX_PENDING).unwrap() {
        worker.submit(request(req)).unwrap();
    }
    assert_eq!(
        worker.submit(request(99)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    for req in 1..=u32::try_from(MAX_PENDING).unwrap() {
        release.send(()).unwrap();
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), req);
    }
    release.send(()).unwrap();
    drop(worker);
}
