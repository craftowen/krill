use super::*;
use std::io::Read as _;

#[test]
fn disconnected_writer_queue_closes_the_socket_and_releases_budget() {
    let (socket, mut peer) = UnixStream::pair().unwrap();
    let mut reader = socket.try_clone().unwrap();
    for stream in [&reader, &peer] {
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
    }
    let (send, recv) = mpsc::sync_channel(1);
    drop(recv);
    let bytes = Arc::new(AtomicUsize::new(0));
    let mut writer = Writer {
        send,
        bytes: Arc::clone(&bytes),
        socket: Arc::new(socket),
        failed: false,
    };
    assert_eq!(
        writer.write(b"not queued").unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn normal_writer_drop_still_drains_accepted_frames_in_order() {
    let (ours, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut writer = Writer::spawn(ours).unwrap();
    let mut expected = Vec::new();
    for byte in 0..16 {
        let chunk = vec![byte; 64 << 10];
        writer.write_all(&chunk).unwrap();
        expected.extend(chunk);
    }
    drop(writer);
    let mut delivered = Vec::new();
    peer.read_to_end(&mut delivered).unwrap();
    assert_eq!(delivered, expected);
}

#[test]
fn rejected_write_closes_the_old_stream_without_flushing_its_backlog() {
    let (ours, mut peer) = UnixStream::pair().unwrap();
    rustix::net::sockopt::set_socket_send_buffer_size(&ours, 16 << 10).unwrap();
    rustix::net::sockopt::set_socket_recv_buffer_size(&peer, 16 << 10).unwrap();
    let mut reader = ours.try_clone().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut writer = Writer::spawn(ours).unwrap();
    let chunk = vec![b'x'; 64 << 10];
    let mut accepted = 0;
    for _ in 0..256 {
        match writer.write(&chunk) {
            Ok(n) => accepted += n,
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                break;
            }
        }
    }
    assert!(accepted > 0 && accepted < chunk.len() * 256);
    // Pane drops a writer after any enqueue failure. Its reader must wake even
    // while the peer is not reading, so the same session can reconnect promptly.
    drop(writer);
    let closed = reader.read(&mut [0; 1]);
    // Drain after observing EOF, so even the broken implementation cleans up.
    let mut delivered = Vec::new();
    peer.read_to_end(&mut delivered).unwrap();
    assert_eq!(closed.unwrap(), 0, "failed stream must wake its reader");
    assert!(
        delivered.len() < accepted,
        "an abandoned queue must not continue sending after reconnect starts"
    );
}
