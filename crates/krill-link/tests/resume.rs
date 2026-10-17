//! MK-134: direct, relay-free QUIC outages with persistent Unix connections.
#![expect(clippy::unwrap_used, reason = "integration assertions")]

use krill_link::{
    ConnectOptions, DeviceIdentity, LinkConfig, RelaySetting, RemoteHost, ServeOptions,
    ServerHandle, StreamPhase, TunnelHandle, start_server, start_tunnel, upsert_device,
    upsert_host,
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const WAIT: Duration = Duration::from_secs(8);
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    base: PathBuf,
    server: ServerHandle,
    tunnel: TunnelHandle,
    listener: UnixListener,
}
impl Fixture {
    async fn new(grace_ms: u64, buffer: usize) -> Self {
        Self::with_graces(grace_ms, grace_ms, buffer).await
    }
    async fn with_graces(grace_ms: u64, client_grace_ms: u64, buffer: usize) -> Self {
        let base = PathBuf::from(format!(
            "/tmp/m134-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(base.join("s/run")).unwrap();
        fs::create_dir_all(base.join("c")).unwrap();
        let mut config = LinkConfig {
            relay: RelaySetting::Off,
            resume_grace_ms: grace_ms,
            replay_buffer_bytes: buffer,
            ..LinkConfig::default()
        };
        config.save(&base.join("s")).unwrap();
        config.resume_grace_ms = client_grace_ms;
        config.save(&base.join("c")).unwrap();
        let client = DeviceIdentity::load_or_create(&base.join("c")).unwrap();
        upsert_device(&base.join("s"), client.node_id(), "client", 0).unwrap();
        let listener = UnixListener::bind(base.join("s/run/krilld.sock")).unwrap();
        let server = start_server(ServeOptions::new(base.join("s")))
            .await
            .unwrap();
        upsert_host(
            &base.join("c"),
            RemoteHost::from_endpoint_addr(
                "server".to_owned(),
                "server",
                &server.endpoint_addr(),
                0,
            ),
        )
        .unwrap();
        let tunnel = start_tunnel(ConnectOptions::new(base.join("c"), "server"))
            .await
            .unwrap();
        Self {
            base,
            server,
            tunnel,
            listener,
        }
    }
    async fn pair(&self) -> (UnixStream, UnixStream) {
        let mut app = UnixStream::connect(self.tunnel.socket_path())
            .await
            .unwrap();
        let (mut upstream, _) = tokio::time::timeout(WAIT, self.listener.accept())
            .await
            .unwrap()
            .unwrap();
        app.write_all(b"ready").await.unwrap();
        let mut ready = [0; 5];
        tokio::time::timeout(WAIT, upstream.read_exact(&mut ready))
            .await
            .unwrap()
            .unwrap();
        upstream.write_all(b"ready").await.unwrap();
        tokio::time::timeout(WAIT, app.read_exact(&mut ready))
            .await
            .unwrap()
            .unwrap();
        self.wait_for(|s| s.state == StreamPhase::Normal).await;
        (app, upstream)
    }
    async fn wait_for(&self, condition: impl Fn(&krill_link::StreamStatus) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            if self.tunnel.status().streams.iter().any(&condition) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "status timeout: {:?}",
                self.tunnel.status()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn shutdown(self) {
        self.tunnel.shutdown().await;
        self.server.shutdown().await;
        fs::remove_dir_all(self.base).ok();
    }
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap().wrapping_add(seed))
        .collect()
}

#[tokio::test]
async fn outage_preserves_both_directions_and_half_close() {
    let f = Fixture::new(5000, 1024 * 1024).await;
    let (mut app, mut upstream) = f.pair().await;
    for iteration in 0..5 {
        f.tunnel.set_transport_offline(true);
        f.wait_for(|s| s.state == StreamPhase::Suspended && s.disconnect_count > iteration)
            .await;
        let input = pattern(65_537, u8::try_from(iteration).unwrap());
        let output = pattern(98_311, 90);
        let (a, b) = tokio::join!(app.write_all(&input), upstream.write_all(&output));
        a.unwrap();
        b.unwrap();
        f.wait_for(|s| s.buffered_bytes >= u64::try_from(input.len()).unwrap())
            .await;
        let mut received_input = vec![0; input.len()];
        let mut received_output = vec![0; output.len()];
        f.tunnel.set_transport_offline(false);
        tokio::time::timeout(WAIT, async {
            let (a, b) = tokio::join!(
                upstream.read_exact(&mut received_input),
                app.read_exact(&mut received_output)
            );
            a.unwrap();
            b.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(received_input, input);
        assert_eq!(received_output, output);
        f.wait_for(|s| s.state == StreamPhase::Normal && s.buffered_bytes == 0)
            .await;
        assert_eq!(f.tunnel.status().streams[0].disconnect_count, iteration + 1);
    }
    app.shutdown().await.unwrap();
    let mut eof = [0];
    assert_eq!(
        tokio::time::timeout(WAIT, upstream.read(&mut eof))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    upstream
        .write_all(b"response after request EOF")
        .await
        .unwrap();
    upstream.shutdown().await.unwrap();
    let mut tail = Vec::new();
    tokio::time::timeout(WAIT, app.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tail, b"response after request EOF");
    f.shutdown().await;
}

#[tokio::test]
async fn grace_expiry_closes_only_client_connections() {
    let f = Fixture::new(250, 1024 * 1024).await;
    let (mut app, mut upstream) = f.pair().await;
    f.tunnel.set_transport_offline(true);
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(WAIT, app.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(
        tokio::time::timeout(WAIT, upstream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    f.tunnel.set_transport_offline(false);
    let (mut app, mut upstream) = f.pair().await;
    app.write_all(b"healthy").await.unwrap();
    let mut bytes = [0; 7];
    upstream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"healthy");
    f.shutdown().await;
}

#[tokio::test]
async fn replay_cap_closes_only_overflowing_stream_and_revoke_covers_suspended() {
    let f = Fixture::new(5000, 16 * 1024).await;
    let (mut overflowing, _upstream) = f.pair().await;
    let (mut healthy, mut healthy_upstream) = f.pair().await;
    f.tunnel.set_transport_offline(true);
    f.wait_for(|s| s.state == StreamPhase::Suspended).await;
    overflowing.write_all(&pattern(32 * 1024, 1)).await.ok();
    let mut byte = [0];
    assert!(matches!(
        tokio::time::timeout(WAIT, overflowing.read(&mut byte))
            .await
            .unwrap(),
        Ok(0) | Err(_)
    ));
    healthy.write_all(b"unaffected").await.unwrap();
    f.tunnel.set_transport_offline(false);
    let mut bytes = [0; 10];
    tokio::time::timeout(WAIT, healthy_upstream.read_exact(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&bytes, b"unaffected");
    f.tunnel.set_transport_offline(true);
    f.wait_for(|s| s.state == StreamPhase::Suspended).await;
    f.server.revoke_device("client").await.unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if f.server
                .stream_statuses()
                .iter()
                .all(|s| s.state == StreamPhase::Closed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.tunnel.set_transport_offline(false);
    assert!(matches!(
        tokio::time::timeout(WAIT, healthy.read(&mut byte))
            .await
            .unwrap(),
        Ok(0) | Err(_)
    ));
    f.shutdown().await;
}

async fn raw_endpoint(dir: &std::path::Path) -> iroh::Endpoint {
    let text = fs::read_to_string(DeviceIdentity::key_path(dir)).unwrap();
    let mut raw = [0u8; 32];
    for (byte, hex) in raw
        .iter_mut()
        .zip(text.trim().as_bytes().as_chunks::<2>().0)
    {
        *byte = u8::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap();
    }
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&raw))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap()
}
fn hello(id: krill_link::StreamId, generation: u64, received: u64, resume: bool) -> [u8; 54] {
    let mut bytes = [0; 54];
    bytes[..4].copy_from_slice(b"MKL2");
    bytes[4] = u8::from(resume);
    bytes[5..21].copy_from_slice(&id.0);
    bytes[21..29].copy_from_slice(&generation.to_le_bytes());
    bytes[29..37].copy_from_slice(&received.to_le_bytes());
    bytes[37..45].copy_from_slice(&5000u64.to_le_bytes());
    if resume {
        bytes[46..54].copy_from_slice(&8192u64.to_le_bytes());
    }
    bytes
}
async fn open_raw(
    conn: &iroh::endpoint::Connection,
    request: [u8; 54],
) -> (
    iroh::endpoint::SendStream,
    iroh::endpoint::RecvStream,
    [u8; 54],
) {
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(&request).await.unwrap();
    let mut code = [255];
    tokio::time::timeout(WAIT, recv.read_exact(&mut code))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, [0]);
    let mut reply = [0; 54];
    recv.read_exact(&mut reply).await.unwrap();
    (send, recv, reply)
}
async fn raw_data(send: &mut iroh::endpoint::SendStream, offset: u64, data: &[u8]) {
    let mut header = [0; 14];
    header[0] = 1;
    header[2..10].copy_from_slice(&offset.to_le_bytes());
    header[10..14].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    send.write_all(&header).await.unwrap();
    send.write_all(data).await.unwrap();
}

#[tokio::test]
async fn other_authorized_node_cannot_resume_known_stream_id() {
    let f = Fixture::new(5000, 1024 * 1024).await;
    let (mut app, mut upstream) = f.pair().await;
    f.tunnel.set_transport_offline(true);
    f.wait_for(|s| s.state == StreamPhase::Suspended).await;
    let id = f.tunnel.status().streams[0].id;
    let other = f.base.join("other");
    fs::create_dir_all(&other).unwrap();
    LinkConfig {
        relay: RelaySetting::Off,
        ..LinkConfig::default()
    }
    .save(&other)
    .unwrap();
    let invite = f.server.create_invite(Duration::from_secs(60)).unwrap();
    krill_link::pair_with_server(&other, &invite.link, "other authorized node")
        .await
        .unwrap();
    let endpoint = raw_endpoint(&other).await;
    let conn = endpoint
        .connect(f.server.endpoint_addr(), krill_link::ALPN_LINK)
        .await
        .unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(&hello(id, 99, 5, true)).await.unwrap();
    let mut code = [0];
    tokio::time::timeout(WAIT, recv.read_exact(&mut code))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, [2]);
    f.tunnel.set_transport_offline(false);
    app.write_all(b"owner").await.unwrap();
    let mut data = [0; 5];
    tokio::time::timeout(WAIT, upstream.read_exact(&mut data))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&data, b"owner");
    endpoint.close().await;
    f.shutdown().await;
}

#[tokio::test]
async fn lost_ack_and_duplicate_replay_do_not_duplicate_upstream() {
    let f = Fixture::new(5000, 1024 * 1024).await;
    let endpoint = raw_endpoint(&f.base.join("c")).await;
    let conn = endpoint
        .connect(f.server.endpoint_addr(), krill_link::ALPN_LINK)
        .await
        .unwrap();
    let id = krill_link::StreamId([17; 16]);
    let (mut send, mut recv, _) = open_raw(&conn, hello(id, 1, 0, false)).await;
    let (mut upstream, _) = f.listener.accept().await.unwrap();
    let input = pattern(8192, 14);
    raw_data(&mut send, 0, &input).await;
    let mut received = vec![0; input.len()];
    upstream.read_exact(&mut received).await.unwrap();
    assert_eq!(received, input);
    let output = pattern(8192, 77);
    upstream.write_all(&output).await.unwrap();
    let (_, received_output) = raw_read_data(&mut recv).await;
    assert_eq!(received_output, output);
    // The receiver committed everything, while the sender never reads its ACK.
    conn.close(0u32.into(), b"lost acknowledgement");
    let conn = endpoint
        .connect(f.server.endpoint_addr(), krill_link::ALPN_LINK)
        .await
        .unwrap();
    let (mut send, mut recv, reply) = open_raw(&conn, hello(id, 2, 0, true)).await;
    let (offset, replayed) = raw_read_data(&mut recv).await;
    assert_eq!(offset, 0);
    assert_eq!(replayed, output);
    assert!(
        f.server
            .stream_statuses()
            .iter()
            .any(|s| s.id == id && s.retransmitted_bytes >= 8192)
    );
    assert_eq!(u64::from_le_bytes(reply[29..37].try_into().unwrap()), 8192);
    raw_data(&mut send, 0, &input).await;
    raw_data(&mut send, 8192, b"one suffix").await;
    let mut suffix = [0; 10];
    tokio::time::timeout(WAIT, upstream.read_exact(&mut suffix))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&suffix, b"one suffix");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), f.listener.accept())
            .await
            .is_err(),
        "Resume must reuse the original upstream connection"
    );
    let mut extra = [0];
    assert!(
        tokio::time::timeout(Duration::from_millis(50), upstream.read(&mut extra))
            .await
            .is_err(),
        "duplicate data leaked to upstream"
    );
    endpoint.close().await;
    f.shutdown().await;
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(8))]
    #[test]
    fn randomized_quic_outages_preserve_order(payloads in proptest::collection::vec(proptest::collection::vec(proptest::prelude::any::<u8>(),1..2048),2..8)) {
        let runtime=tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let f=Fixture::new(5000,1024*1024).await;
            let (mut app,mut upstream)=f.pair().await;
            for (index,payload) in payloads.into_iter().enumerate() {
                f.tunnel.set_transport_offline(true);
                f.wait_for(|s|s.state==StreamPhase::Suspended && s.disconnect_count>u64::try_from(index).unwrap()).await;
                app.write_all(&payload).await.unwrap(); upstream.write_all(&payload).await.unwrap();
                f.tunnel.set_transport_offline(false);
                let mut a=vec![0;payload.len()]; let mut b=vec![0;payload.len()];
                tokio::time::timeout(WAIT,async { let (x,y)=tokio::join!(upstream.read_exact(&mut a),app.read_exact(&mut b)); x.unwrap(); y.unwrap(); }).await.unwrap();
                assert_eq!(a,payload); assert_eq!(b,payload);
                f.wait_for(|s|s.state==StreamPhase::Normal).await;
            }
            f.shutdown().await;
        });
    }
}

async fn raw_read_data(recv: &mut iroh::endpoint::RecvStream) -> (u64, Vec<u8>) {
    loop {
        let mut header = [0; 14];
        tokio::time::timeout(WAIT, recv.read_exact(&mut header))
            .await
            .unwrap()
            .unwrap();
        let len = usize::try_from(u32::from_le_bytes(header[10..14].try_into().unwrap())).unwrap();
        assert!(len <= 8192);
        let mut payload = vec![0; len];
        recv.read_exact(&mut payload).await.unwrap();
        if header[0] == 1 {
            return (
                u64::from_le_bytes(header[2..10].try_into().unwrap()),
                payload,
            );
        }
    }
}

#[tokio::test]
async fn lost_ready_is_resumed_and_duplicate_open_never_recreates_upstream() {
    let f = Fixture::new(5000, 1024 * 1024).await;
    let endpoint = raw_endpoint(&f.base.join("c")).await;
    let conn = endpoint
        .connect(f.server.endpoint_addr(), krill_link::ALPN_LINK)
        .await
        .unwrap();
    let id = krill_link::StreamId([19; 16]);
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    send.write_all(&hello(id, 1, 0, false)).await.unwrap();
    let (mut upstream, _) = f.listener.accept().await.unwrap();
    // No Ready is consumed; creation is ambiguous from the caller's perspective.
    conn.close(0u32.into(), b"lost ready");
    let conn = endpoint
        .connect(f.server.endpoint_addr(), krill_link::ALPN_LINK)
        .await
        .unwrap();
    let mut resume = hello(id, 2, 0, true);
    resume[46..54].fill(0);
    let (mut send, _recv, _) = open_raw(&conn, resume).await;
    raw_data(&mut send, 0, b"exactly once").await;
    let mut received = [0; 12];
    upstream.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"exactly once");
    let (mut duplicate, mut response) = conn.open_bi().await.unwrap();
    duplicate.write_all(&hello(id, 1, 0, false)).await.unwrap();
    let mut code = [0];
    response.read_exact(&mut code).await.unwrap();
    assert_eq!(code, [2]);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), f.listener.accept())
            .await
            .is_err()
    );
    endpoint.close().await;
    f.shutdown().await;
}

#[tokio::test]
async fn status_json_and_lib_include_suspended_stream_metrics() {
    let f = Fixture::new(5000, 1024 * 1024).await;
    let (_app, _upstream) = f.pair().await;
    f.tunnel.set_transport_offline(true);
    f.wait_for(|s| s.state == StreamPhase::Suspended).await;
    let deadline = Instant::now() + WAIT;
    loop {
        let local = krill_link::status_report(&f.base.join("c")).unwrap();
        let remote = krill_link::status_report(&f.base.join("s")).unwrap();
        if local.hosts[0]
            .streams
            .iter()
            .any(|s| s.state == StreamPhase::Suspended && s.disconnect_count == 1)
            && remote
                .server_streams
                .iter()
                .any(|s| s.state == StreamPhase::Suspended && s.disconnect_count == 1)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let dir = f.base.join("c");
    let report = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_krill-link"))
            .args(["status", "--json", "--state-dir"])
            .arg(dir)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(report.status.success());
    let json: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    let stream = &json["hosts"][0]["streams"][0];
    assert_eq!(stream["state"], "suspended");
    assert_eq!(stream["disconnect_count"], 1);
    assert!(stream["last_disconnect_at_ms"].as_u64().is_some());
    assert!(stream["retransmitted_bytes"].as_u64().is_some());
    f.shutdown().await;
}

#[tokio::test]
async fn zero_byte_fin_survives_an_outage_in_both_directions() {
    let f = Fixture::new(5000, 1024 * 1024).await;
    let mut app = UnixStream::connect(f.tunnel.socket_path()).await.unwrap();
    let (mut upstream, _) = f.listener.accept().await.unwrap();
    f.wait_for(|s| s.state == StreamPhase::Normal).await;
    f.tunnel.set_transport_offline(true);
    f.wait_for(|s| s.state == StreamPhase::Suspended).await;
    app.shutdown().await.unwrap();
    upstream.shutdown().await.unwrap();
    f.tunnel.set_transport_offline(false);
    let mut local_bytes = Vec::new();
    let mut remote_bytes = Vec::new();
    let (local_read, remote_read) = tokio::time::timeout(WAIT, async {
        tokio::join!(
            app.read_to_end(&mut local_bytes),
            upstream.read_to_end(&mut remote_bytes)
        )
    })
    .await
    .unwrap();
    assert_eq!(local_read.unwrap(), 0);
    assert_eq!(remote_read.unwrap(), 0);
    f.shutdown().await;
}

#[tokio::test]
async fn slow_ready_does_not_discard_input_before_resume_grace() {
    let base = PathBuf::from(format!("/tmp/m134-slow-{}", std::process::id()));
    LinkConfig {
        relay: RelaySetting::Off,
        resume_grace_ms: 7000,
        ..LinkConfig::default()
    }
    .save(&base)
    .unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![krill_link::ALPN_LINK.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .addr_filter(iroh::address_lookup::AddrFilter::unfiltered())
        .clear_ip_transports()
        .bind_addr(("127.0.0.1", 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    upsert_host(
        &base,
        RemoteHost::from_endpoint_addr("slow".to_owned(), "slow", &endpoint.addr(), 0),
    )
    .unwrap();
    let peer = endpoint.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let fake_server = tokio::spawn(async move {
        let incoming = peer.accept().await.unwrap();
        let connection = incoming.accept().unwrap().await.unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        let mut preamble = [0; 54];
        recv.read_exact(&mut preamble).await.unwrap();
        // Exceeds the old three-second handshake timeout, while QUIC is alive.
        tokio::time::sleep(Duration::from_millis(3500)).await;
        preamble[29..37].fill(0);
        preamble[46..54].copy_from_slice(&20u64.to_le_bytes());
        send.write_all(&[0]).await.unwrap();
        send.write_all(&preamble).await.unwrap();
        let (offset, payload) = raw_read_data(&mut recv).await;
        assert_eq!(offset, 0);
        assert_eq!(payload, b"held while attaching");
        let mut ack = [0; 14];
        ack[0] = 2;
        ack[2..10].copy_from_slice(&20u64.to_le_bytes());
        send.write_all(&ack).await.unwrap();
        raw_data(&mut send, 0, &payload).await;
        done_rx.await.unwrap();
    });
    let tunnel = start_tunnel(ConnectOptions::new(base.clone(), "slow"))
        .await
        .unwrap();
    let mut app = UnixStream::connect(tunnel.socket_path()).await.unwrap();
    app.write_all(b"held while attaching").await.unwrap();
    let mut received = [0; 20];
    tokio::time::timeout(WAIT, app.read_exact(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&received, b"held while attaching");
    done_tx.send(()).unwrap();
    fake_server.await.unwrap();
    tunnel.shutdown().await;
    endpoint.close().await;
    fs::remove_dir_all(base).ok();
}

#[tokio::test]
async fn shorter_peer_grace_expires_the_local_socket_without_a_network_resume() {
    let fixture = Fixture::with_graces(250, 5000, 1024 * 1024).await;
    let (mut app, _upstream) = fixture.pair().await;
    fixture.tunnel.set_transport_offline(true);
    let mut byte = [0];
    let result = tokio::time::timeout(Duration::from_secs(2), app.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    fixture.shutdown().await;
}
