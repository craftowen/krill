//! Actual holder/krilld survival, CLI input, and persistent raw input over a QUIC outage.
use super::*;
use krill_proto::input::{Input, InputAck, InputId, InputResult};
use krill_proto::screen::{Attach, Attached, Seq, Stream};

async fn cli(bin: PathBuf, dir: PathBuf, args: Vec<String>) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        Command::new(bin)
            .args(args)
            .env("KRILL_STATE_DIR", dir)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}
async fn suspended(tunnel: &krill_link::TunnelHandle) {
    tokio::time::timeout(WAIT, async {
        loop {
            if tunnel
                .status()
                .streams
                .iter()
                .any(|s| s.state == krill_link::StreamPhase::Suspended && s.disconnect_count > 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_krilld_cli_and_buffered_input_apply_once_and_survive_expiry() {
    let base = short_temp_dir("resume-real");
    let server_dir = base.join("s");
    let client_dir = base.join("c");
    let mk_dir = base.join("m");
    fs::create_dir_all(mk_dir.join("run")).unwrap();
    for dir in [&server_dir, &client_dir] {
        LinkConfig {
            relay: RelaySetting::Off,
            resume_grace_ms: 1500,
            ..LinkConfig::default()
        }
        .save(dir)
        .unwrap();
    }
    let daemon = IsolatedKrilld::start(server_dir.clone());
    let server = start_server(ServeOptions::new(server_dir.clone()))
        .await
        .unwrap();
    let invite = server.create_invite(Duration::from_secs(60)).unwrap();
    let host = pair_with_server(&client_dir, &invite.link, "resume real")
        .await
        .unwrap();
    let tunnel = start_tunnel(ConnectOptions::new(client_dir, host.id))
        .await
        .unwrap();
    symlink(tunnel.socket_path(), mk_dir.join("run/krilld.sock")).unwrap();
    let marker = base.join("applied.txt");
    let script = format!(
        "stty -echo; printf 'RESUME_READY\\n'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; printf 'APPLIED:%s\\n' \"$line\"; done",
        marker.display()
    );
    let created = cli(
        daemon.mk_bin.clone(),
        mk_dir.clone(),
        vec![
            "new".to_owned(),
            "--".to_owned(),
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            script,
        ],
    )
    .await;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let sid = String::from_utf8(created.stdout).unwrap().trim().to_owned();
    let session = krill_proto::SessionId(sid.parse().unwrap());
    let socket = tunnel.socket_path().to_owned();
    let mut raw = tokio::task::spawn_blocking(move || {
        let mut raw = handshake(
            &socket,
            &client_hello(ClientRole::Cli, "resume persistent"),
            WAIT,
        )
        .unwrap();
        raw.stream.set_read_timeout(Some(WAIT)).unwrap();
        raw.stream
            .write_all(
                &Attach {
                    session,
                    stream: Stream::Raw { from: Seq(0) },
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut raw.stream).unwrap().unwrap();
        Attached::decode(kind, &body).unwrap();
        raw
    })
    .await
    .unwrap();
    // Wait for the shell's no-echo read loop before injecting input.
    let deadline = Instant::now() + WAIT;
    loop {
        let read = cli(
            daemon.mk_bin.clone(),
            mk_dir.clone(),
            vec!["read".to_owned(), sid.clone()],
        )
        .await;
        if String::from_utf8_lossy(&read.stdout).contains("RESUME_READY") {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tunnel.set_transport_offline(true);
    suspended(&tunnel).await;
    raw.stream
        .write_all(
            &Input {
                id: InputId(1),
                bytes: b"RAW_OUTAGE_NONCE\r".to_vec(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let send = tokio::spawn(cli(
        daemon.mk_bin.clone(),
        mk_dir.clone(),
        vec![
            "send".to_owned(),
            sid.clone(),
            "--text".to_owned(),
            "CLI_OUTAGE_NONCE".to_owned(),
            "--enter".to_owned(),
        ],
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !marker.exists(),
        "offline input reached krilld before redial"
    );
    tunnel.set_transport_offline(false);
    let raw = tokio::task::spawn_blocking(move || {
        loop {
            let (kind, body) = ipc::read_frame(&mut raw.stream).unwrap().unwrap();
            if kind == krill_proto::input::kind::INPUT_ACK {
                let ack = InputAck::decode(kind, &body).unwrap();
                assert_eq!(ack.id, InputId(1));
                assert_eq!(ack.result, InputResult::Applied);
                break;
            }
        }
        raw
    })
    .await
    .unwrap();
    let sent = send.await.unwrap();
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    let deadline = Instant::now() + WAIT;
    loop {
        let records = fs::read_to_string(&marker).unwrap_or_default();
        if records.lines().count() == 2 {
            assert_eq!(
                records.lines().filter(|s| *s == "RAW_OUTAGE_NONCE").count(),
                1
            );
            assert_eq!(
                records.lines().filter(|s| *s == "CLI_OUTAGE_NONCE").count(),
                1
            );
            break;
        }
        assert!(Instant::now() < deadline, "applied records: {records}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let read = cli(
        daemon.mk_bin.clone(),
        mk_dir.clone(),
        vec!["read".to_owned(), sid.clone()],
    )
    .await;
    assert!(read.status.success());
    let screen = String::from_utf8_lossy(&read.stdout);
    assert!(
        screen.contains("APPLIED:RAW_OUTAGE_NONCE") && screen.contains("APPLIED:CLI_OUTAGE_NONCE")
    );
    // Expiry closes only the client socket, with the same shell still present in krilld.
    tunnel.set_transport_offline(true);
    suspended(&tunnel).await;
    tokio::time::sleep(Duration::from_millis(1700)).await;
    let eof = tokio::task::spawn_blocking(move || {
        let mut stream = raw.stream;
        loop {
            match ipc::read_frame(&mut stream) {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return true,
            }
        }
    })
    .await
    .unwrap();
    assert!(eof);
    let listed = cli(
        daemon.mk_bin.clone(),
        server_dir.clone(),
        vec!["ls".to_owned(), "--json".to_owned()],
    )
    .await;
    assert!(listed.status.success());
    let sessions: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(
        sessions
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == session.0 && s["process"] == "running")
    );
    tunnel.set_transport_offline(false);
    let sent = cli(
        daemon.mk_bin.clone(),
        mk_dir.clone(),
        vec![
            "send".to_owned(),
            sid,
            "--text".to_owned(),
            "AFTER_EXPIRY".to_owned(),
            "--enter".to_owned(),
        ],
    )
    .await;
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    tunnel.shutdown().await;
    server.shutdown().await;
    drop(daemon);
    fs::remove_dir_all(base).ok();
}
