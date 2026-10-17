//! MK-133a (`krill-link`): 오프라인(`relay = "off"` + 직접 주소) 통합 테스트.
//!
//! 검증 항목:
//! 1. 짝짓기 성공 (`invite` -> `pair` -> 서버 `devices.toml` 및 클라이언트 `hosts.toml` 등록)
//! 2. 만료 토큰 거부, 재사용 토큰 거부, 짝짓기 속도 제한(rate limit) 거부
//! 3. 비인가 기기(`devices.toml` 미등록 `NodeId`)의 `ALPN_LINK` 접속 거부
//! 4. `revoke` 호출 시 활성 스트림·연결 즉시 해제 및 재접속 거부
//! 5. 격리된 임시 `krilld`에 대해 터널(`krill-link serve` + `krill-link connect`) 너머로
//!    `krill ls`·`krill new`·`krill read`가 그대로 동작하는지 검증
//! 6. `krill-link serve` 재기동 시 `krill-link connect`가 로컬 소켓을 유지한 채 자동 재연결되는지 검증

#![expect(clippy::unwrap_used, reason = "integration test assertions")]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Once;
use std::thread;
use std::time::{Duration, Instant};

use iroh::Endpoint;
use krill_link::{
    ALPN_LINK, ConnectOptions, DeviceIdentity, Error, LinkConfig, RelaySetting, RemoteHost,
    RouteKind, STREAM_INIT_BYTE, ServeOptions, TunnelState, create_invite_command, host_status,
    load_devices, load_hosts, pair_with_server, revoke_device_command, start_server, start_tunnel,
    status_report, upsert_host,
};
use krill_proto::control::{Reply, Request};
use krill_proto::daemon::{client_hello, handshake};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const MK_LINK_BIN: &str = env!("CARGO_BIN_EXE_krill-link");
const WAIT: Duration = Duration::from_secs(15);

static BUILD_SIBLINGS: Once = Once::new();

fn ensure_sibling_bins() -> PathBuf {
    let bin_dir = Path::new(MK_LINK_BIN).parent().unwrap().to_path_buf();
    BUILD_SIBLINGS.call_once(|| {
        let needed = ["krill", "krill-holder", "krilld"];
        if needed.iter().all(|name| bin_dir.join(name).is_file()) {
            return;
        }
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let status = Command::new("cargo")
            .args([
                "build",
                "-p",
                "krill",
                "-p",
                "krill-holder",
                "-p",
                "krilld",
                "--bins",
                "--locked",
            ])
            .current_dir(workspace_root)
            .status()
            .unwrap();
        assert!(status.success(), "failed to build sibling binaries");
    });
    bin_dir
}

/// UDS 경로 길이 상한(104바이트)을 넘지 않도록 짧은 임시 디렉터리를 만든다.
fn short_temp_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from("/tmp").join(format!("mkl-{}-{tag}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_offline_config(state_dir: &Path, server_name: Option<&str>) {
    let cfg = LinkConfig {
        relay: RelaySetting::Off,
        server_name: server_name.map(str::to_owned),
        bind_port: None,
        lan_discovery: true,
        ..LinkConfig::default()
    };
    cfg.save(state_dir).unwrap();
}

#[tokio::test]
async fn offline_pair_expired_reused_and_rate_limit() {
    let base = short_temp_dir("pair");
    let server_dir = base.join("s");
    let client_dir = base.join("c");
    fs::create_dir_all(&server_dir).unwrap();
    fs::create_dir_all(&client_dir).unwrap();

    write_offline_config(&server_dir, Some("집 맥미니"));
    write_offline_config(&client_dir, None);

    let server = start_server(ServeOptions::new(server_dir.clone()))
        .await
        .unwrap();

    // 1) 만료된 초대 토큰 거부 검증
    let expired_invite = create_invite_command(&server_dir, Some(Duration::from_millis(1)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;

    let err = pair_with_server(&client_dir, &expired_invite.link, "오웬 맥북")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::PairRejected(msg) if msg.contains("expired")),
        "expected expired error, got: {err:?}"
    );
    assert!(load_devices(&server_dir).unwrap().is_empty());

    // 2) 정상 초대 링크 생성 + 1회 짝짓기 성공 검증
    let invite = create_invite_command(&server_dir, Some(Duration::from_secs(600)))
        .await
        .unwrap();
    assert!(invite.link.starts_with("krill://pair?"));
    assert!(!invite.qr_unicode.is_empty());

    let host = pair_with_server(&client_dir, &invite.link, "오웬 맥북")
        .await
        .unwrap();
    assert_eq!(host.name, "집 맥미니");
    assert_eq!(host.node_id, server.node_id().to_string());

    let devices = load_devices(&server_dir).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "오웬 맥북");
    let client_id = DeviceIdentity::load_or_create(&client_dir)
        .unwrap()
        .node_id();
    assert_eq!(devices[0].node_id, client_id.to_string());

    let hosts = load_hosts(&client_dir).unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].id, host.id);

    // 3) 동일 초대 링크 재사용 시 즉시 거부 검증
    let reuse_err = pair_with_server(&client_dir, &invite.link, "두번째 기기")
        .await
        .unwrap_err();
    assert!(
        matches!(&reuse_err, Error::PairRejected(msg) if msg.contains("already been used")),
        "expected already used error, got: {reuse_err:?}"
    );

    // 4) 짝짓기 요청 속도 제한(rate limit) 검증 (burst 상한 초과 시 거부)
    let mut saw_rate_limited = false;
    for _ in 0..10 {
        let res = pair_with_server(&client_dir, &invite.link, "스팸 기기").await;
        if let Err(Error::PairRejected(msg)) = res
            && msg.contains("rate limit")
        {
            saw_rate_limited = true;
            break;
        }
    }
    assert!(saw_rate_limited, "expected pairing rate limit to trigger");

    server.shutdown().await;
    fs::remove_dir_all(&base).ok();
}

#[tokio::test]
async fn unauthorized_device_rejected_and_revoke_disconnects_immediately() {
    let base = short_temp_dir("rev");
    let server_dir = base.join("s");
    let client_dir = base.join("c");
    let rogue_dir = base.join("r");
    fs::create_dir_all(&server_dir).unwrap();
    fs::create_dir_all(&client_dir).unwrap();
    fs::create_dir_all(&rogue_dir).unwrap();

    write_offline_config(&server_dir, Some("사무실 맥스튜디오"));
    write_offline_config(&client_dir, None);
    write_offline_config(&rogue_dir, None);

    // 서버 측 가짜 krilld 소켓(수신 바이트를 그대로 에코하고 연결을 유지)
    let mock_sock = server_dir.join("run/krilld.sock");
    fs::create_dir_all(mock_sock.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&mock_sock).unwrap();
    let echo_task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if stream.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    let server = start_server(ServeOptions::new(server_dir.clone()))
        .await
        .unwrap();
    let server_addr = server.endpoint_addr();

    // 1) 허용 목록(devices.toml)에 없는 비인가 기기(rogue)가 ALPN_LINK로 직접 접속 시도 → 거부 확인
    let rogue_ep = Endpoint::builder(iroh::endpoint::presets::N0)
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind_addr(std::net::SocketAddrV4::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            0,
        ))
        .unwrap()
        .bind()
        .await
        .unwrap();

    let rogue_res = tokio::time::timeout(Duration::from_secs(3), async {
        let conn = rogue_ep
            .connect(server_addr.clone(), ALPN_LINK)
            .await
            .map_err(|e| e.to_string())?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        send.write_all(&[STREAM_INIT_BYTE])
            .await
            .map_err(|e| e.to_string())?;
        let mut ack = [0u8; 1];
        recv.read_exact(&mut ack).await.map_err(|e| e.to_string())?;
        Ok::<u8, String>(ack[0])
    })
    .await;
    assert!(
        rogue_res.is_err() || rogue_res.unwrap().is_err(),
        "unauthorized device must not complete ALPN_LINK stream handshake"
    );
    rogue_ep.close().await;

    // 짝짓기 없이 hosts.toml만 수동 기입한 비인가 터널도 스트림이 즉시 닫혀야 한다
    upsert_host(
        &rogue_dir,
        RemoteHost::from_endpoint_addr("rogue-host".to_owned(), "rogue", &server_addr, 1),
    )
    .unwrap();
    let rogue_tunnel = start_tunnel(ConnectOptions::new(
        rogue_dir.clone(),
        "rogue-host".to_owned(),
    ))
    .await
    .unwrap();
    let mut rogue_stream = UnixStream::connect(rogue_tunnel.socket_path())
        .await
        .unwrap();
    rogue_stream.write_all(b"unauth").await.ok();
    let mut rbuf = [0u8; 8];
    let rread = tokio::time::timeout(Duration::from_secs(2), rogue_stream.read(&mut rbuf))
        .await
        .expect("rogue tunnel stream must close");
    assert!(matches!(rread, Ok(0) | Err(_)));
    rogue_tunnel.shutdown().await;

    // 2) 인가된 클라이언트 짝짓기 및 터널 연결
    let invite = create_invite_command(&server_dir, Some(Duration::from_secs(600)))
        .await
        .unwrap();
    let host = pair_with_server(&client_dir, &invite.link, "인가된 맥북")
        .await
        .unwrap();

    let tunnel = start_tunnel(ConnectOptions::new(client_dir.clone(), host.id.clone()))
        .await
        .unwrap();

    // 상태가 Connected + Direct + RTT 측정값 있음으로 올라올 때까지 대기
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let st = host_status(&client_dir, &host.id).unwrap();
        if st.state == TunnelState::Connected {
            assert_eq!(st.route, RouteKind::Direct);
            assert!(st.rtt_ms.is_some());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tunnel did not reach Connected state: {st:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    let report = status_report(&client_dir).unwrap();
    assert_eq!(report.hosts.len(), 1);
    assert_eq!(report.hosts[0].state, TunnelState::Connected);

    // 3) 터널 소켓을 통해 실시간 양방향 스트림을 연다
    let mut client_stream = UnixStream::connect(tunnel.socket_path()).await.unwrap();
    client_stream.write_all(b"ping-over-iroh").await.unwrap();
    let mut reply = [0u8; 14];
    client_stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"ping-over-iroh");

    // 4) 서버에서 기기 권한 해제(revoke) 호출 → 열려 있던 스트림이 즉시 EOF로 끊기는지 검증
    let devices = load_devices(&server_dir).unwrap();
    assert_eq!(devices.len(), 1);
    let removed = revoke_device_command(&server_dir, &devices[0].id)
        .await
        .unwrap();
    assert_eq!(removed.id, devices[0].id);
    assert!(load_devices(&server_dir).unwrap().is_empty());

    let mut eof_buf = [0u8; 16];
    let read_after_revoke =
        tokio::time::timeout(Duration::from_secs(3), client_stream.read(&mut eof_buf))
            .await
            .expect("active stream must close promptly on revoke");
    assert!(
        matches!(read_after_revoke, Ok(0) | Err(_)),
        "expected EOF or reset after revoke, got: {read_after_revoke:?}"
    );

    let status_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let status = tunnel.status();
        if status.last_error_kind == Some(krill_link::TunnelErrorKind::Unauthorized) {
            assert_eq!(status.state, TunnelState::Disconnected);
            assert!(status.last_error.is_some());
            break;
        }
        assert!(
            Instant::now() < status_deadline,
            "missing revoke cause: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let revoked_status = tunnel.status();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(tunnel.status().last_error, revoked_status.last_error);
    assert_eq!(tunnel.status().state, TunnelState::Disconnected);

    // 5) 해제된 기기가 새 연결을 열려고 해도 즉시 실패해야 한다
    let mut post_revoke_stream = UnixStream::connect(tunnel.socket_path()).await.unwrap();
    post_revoke_stream.write_all(b"after-revoke").await.ok();
    let mut post_buf = [0u8; 16];
    let post_read = tokio::time::timeout(
        Duration::from_secs(2),
        post_revoke_stream.read(&mut post_buf),
    )
    .await
    .expect("revoked tunnel stream must close");
    assert!(matches!(post_read, Ok(0) | Err(_)));

    tunnel.shutdown().await;
    server.shutdown().await;
    echo_task.abort();
    fs::remove_dir_all(&base).ok();
}

/// 격리된 임시 `krilld`를 띄우는 헬퍼. Drop 시 모든 세션을 닫고 `holder`/`krilld`를 정리한다.
struct IsolatedKrilld {
    holder: Child,
    krilld_pid: u32,
    state_dir: PathBuf,
    mk_bin: PathBuf,
}

impl IsolatedKrilld {
    fn start(state_dir: PathBuf) -> Self {
        let bin_dir = ensure_sibling_bins();
        let mk_bin = bin_dir.join("krill");
        let socket = state_dir.join("run/krilld.sock");
        let holder = Command::new(bin_dir.join("krill-holder"))
            .arg(bin_dir.join("krilld"))
            .arg(&socket)
            .env("KRILL_STATE_DIR", &state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        let krilld_pid = loop {
            if let Ok(c) = handshake(&socket, &client_hello(ClientRole::Cli, "mk133a"), WAIT) {
                break u32::try_from(c.server.instance_id).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "isolated krilld startup timed out at {}",
                socket.display()
            );
            thread::sleep(Duration::from_millis(25));
        };
        Self {
            holder,
            krilld_pid,
            state_dir,
            mk_bin,
        }
    }
}

impl Drop for IsolatedKrilld {
    fn drop(&mut self) {
        let socket = self.state_dir.join("run/krilld.sock");
        if let Ok(mut c) = handshake(&socket, &client_hello(ClientRole::Cli, "cleanup"), WAIT) {
            c.stream.write_all(&Request::List.encode().unwrap()).ok();
            if let Ok(Some((kind, body))) = ipc::read_frame(&mut c.stream)
                && let Ok(Reply::Sessions(sessions)) = Reply::decode(kind, &body)
            {
                for info in sessions {
                    c.stream
                        .write_all(
                            &Request::Close {
                                session: info.session,
                            }
                            .encode()
                            .unwrap(),
                        )
                        .ok();
                    ipc::read_frame(&mut c.stream).ok();
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
        self.holder.kill().ok();
        self.holder.wait().ok();
        if let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(self.krilld_pid).unwrap()) {
            rustix::process::kill_process(pid, rustix::process::Signal::KILL).ok();
        }
    }
}

#[tokio::test]
async fn tunnel_end_to_end_mk_ls_new_read_and_server_restart_auto_reconnect() {
    let base = short_temp_dir("e2e");
    let server_dir = base.join("s");
    let client_dir = base.join("c");
    let client_mk_dir = base.join("m");
    fs::create_dir_all(&server_dir).unwrap();
    fs::create_dir_all(&client_dir).unwrap();
    fs::create_dir_all(client_mk_dir.join("run")).unwrap();

    write_offline_config(&server_dir, Some("원격 맥미니"));
    write_offline_config(&client_dir, None);

    // 1) 서버 상태 디렉터리에 실제 격리된 krill-holder + krilld 기동
    let daemon = IsolatedKrilld::start(server_dir.clone());

    // 2) krill-link serve 기동 + 짝짓기 + krill-link connect 기동
    let server = start_server(ServeOptions::new(server_dir.clone()))
        .await
        .unwrap();
    let first_addr = server.endpoint_addr();

    let invite = create_invite_command(&server_dir, Some(Duration::from_secs(600)))
        .await
        .unwrap();
    let host = pair_with_server(&client_dir, &invite.link, "클라이언트 맥")
        .await
        .unwrap();

    let tunnel = start_tunnel(ConnectOptions::new(client_dir.clone(), host.id.clone()))
        .await
        .unwrap();

    // 클라이언트 측 `krill`가 터널 소켓(`run/hosts/<id>.sock`)을 바라보도록 연결
    let linked_krilld_sock = client_mk_dir.join("run/krilld.sock");
    symlink(tunnel.socket_path(), &linked_krilld_sock).unwrap();

    // 3) 터널 너머로 `krill ls`, `krill new`, `krill read` 실행 검증
    let ls_initial = tokio::task::spawn_blocking({
        let dir = client_mk_dir.clone();
        let mk_bin = daemon.mk_bin.clone();
        move || {
            Command::new(mk_bin)
                .args(["ls"])
                .env("KRILL_STATE_DIR", dir)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(
        ls_initial.status.success(),
        "krill ls over tunnel failed: {}",
        String::from_utf8_lossy(&ls_initial.stderr)
    );

    let new_out = tokio::task::spawn_blocking({
        let dir = client_mk_dir.clone();
        let mk_bin = daemon.mk_bin.clone();
        move || {
            Command::new(mk_bin)
                .args([
                    "new",
                    "--",
                    "/bin/sh",
                    "-c",
                    "printf 'TUNNEL_SESSION_READY_42\\n'; sleep 30",
                ])
                .env("KRILL_STATE_DIR", dir)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(
        new_out.status.success(),
        "krill new over tunnel failed: {}",
        String::from_utf8_lossy(&new_out.stderr)
    );
    let session_id = String::from_utf8(new_out.stdout).unwrap().trim().to_owned();
    assert!(!session_id.is_empty());

    // `krill ls`에 새 세션이 나타나는지 확인
    let ls_after = tokio::task::spawn_blocking({
        let dir = client_mk_dir.clone();
        let mk_bin = daemon.mk_bin.clone();
        move || {
            Command::new(mk_bin)
                .args(["ls"])
                .env("KRILL_STATE_DIR", dir)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(ls_after.status.success());
    let ls_text = String::from_utf8(ls_after.stdout).unwrap();
    assert!(
        ls_text
            .lines()
            .skip(1)
            .any(|l| l.split('\t').next() == Some(session_id.as_str())),
        "krill ls output missing session {session_id}: {ls_text}"
    );

    // `krill read <id>`로 터널 너머의 터미널 출력 확인
    let deadline = Instant::now() + WAIT;
    loop {
        let read_out = tokio::task::spawn_blocking({
            let dir = client_mk_dir.clone();
            let mk_bin = daemon.mk_bin.clone();
            let sid = session_id.clone();
            move || {
                Command::new(mk_bin)
                    .args(["read", &sid])
                    .env("KRILL_STATE_DIR", dir)
                    .output()
                    .unwrap()
            }
        })
        .await
        .unwrap();
        assert!(read_out.status.success());
        let text = String::from_utf8_lossy(&read_out.stdout);
        if text.contains("TUNNEL_SESSION_READY_42") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for TUNNEL_SESSION_READY_42 over tunnel; got: {text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 4) `krill-link serve` 재기동 시 `krill-link connect` 자동 재연결 검증
    //    (로컬 `run/hosts/<id>.sock`과 `tunnel`은 멈추지 않고 그대로 유지!)
    server.shutdown().await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    let server_restarted = start_server(ServeOptions::new(server_dir.clone()))
        .await
        .unwrap();
    // 영속화된 UDP 포트 덕분에 재기동 후에도 직접 주소 포트가 유지되는지 확인
    assert_eq!(
        first_addr.ip_addrs().next(),
        server_restarted.endpoint_addr().ip_addrs().next(),
        "server should re-bind the persisted UDP port across restarts"
    );

    // `krill-link connect`를 재시작하지 않은 상태에서 곧바로 `krill read`·`krill ls`가 성공해야 한다
    let reconnect_deadline = Instant::now() + WAIT;
    loop {
        let read_out = tokio::task::spawn_blocking({
            let dir = client_mk_dir.clone();
            let mk_bin = daemon.mk_bin.clone();
            let sid = session_id.clone();
            move || {
                Command::new(mk_bin)
                    .args(["read", &sid])
                    .env("KRILL_STATE_DIR", dir)
                    .output()
                    .unwrap()
            }
        })
        .await
        .unwrap();
        if read_out.status.success() {
            let text = String::from_utf8_lossy(&read_out.stdout);
            if text.contains("TUNNEL_SESSION_READY_42") {
                break;
            }
        }
        assert!(
            Instant::now() < reconnect_deadline,
            "connect tunnel did not auto-reconnect after serve restart"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let status_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let st_after = host_status(&client_dir, &host.id).unwrap();
        if st_after.state == TunnelState::Connected {
            assert_eq!(st_after.route, RouteKind::Direct);
            break;
        }
        assert!(
            Instant::now() < status_deadline,
            "expected Connected status after reconnect, got: {st_after:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    tunnel.shutdown().await;
    server_restarted.shutdown().await;
    drop(daemon);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn cli_subcommands_end_to_end_offline() {
    let base = short_temp_dir("cli");
    let server_dir = base.join("s");
    let client_dir = base.join("c");
    fs::create_dir_all(&server_dir).unwrap();
    fs::create_dir_all(&client_dir).unwrap();

    // 1) krill-link config --relay off --server-name "거실 맥미니" --json
    let cfg_out = Command::new(MK_LINK_BIN)
        .args([
            "--state-dir",
            server_dir.to_str().unwrap(),
            "config",
            "--relay",
            "off",
            "--server-name",
            "거실 맥미니",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(cfg_out.status.success());
    let cfg_json: serde_json::Value = serde_json::from_slice(&cfg_out.stdout).unwrap();
    assert_eq!(cfg_json["relay_mode"], "off");
    assert_eq!(cfg_json["server_name"], "거실 맥미니");

    let client_cfg = Command::new(MK_LINK_BIN)
        .args([
            "--state-dir",
            client_dir.to_str().unwrap(),
            "config",
            "--relay",
            "off",
        ])
        .output()
        .unwrap();
    assert!(client_cfg.status.success());

    // 2) krill-link serve 백그라운드 프로세스 기동
    let mut serve_child = Command::new(MK_LINK_BIN)
        .args(["--state-dir", server_dir.to_str().unwrap(), "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // link-ctl.sock 및 serve-ticket.txt 생성 대기
    let deadline = Instant::now() + WAIT;
    while !server_dir.join("run/link-ctl.sock").exists() {
        assert!(
            Instant::now() < deadline,
            "krill-link serve startup timed out"
        );
        thread::sleep(Duration::from_millis(25));
    }

    // 3) krill-link invite --json
    let inv_out = Command::new(MK_LINK_BIN)
        .args([
            "--state-dir",
            server_dir.to_str().unwrap(),
            "invite",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(inv_out.status.success());
    let inv_json: serde_json::Value = serde_json::from_slice(&inv_out.stdout).unwrap();
    let link = inv_json["link"].as_str().unwrap().to_owned();
    assert!(link.starts_with("krill://pair?"));

    // 4) krill-link pair <link> --name "내 맥북에어" --json
    let pair_out = Command::new(MK_LINK_BIN)
        .args([
            "--state-dir",
            client_dir.to_str().unwrap(),
            "pair",
            &link,
            "--name",
            "내 맥북에어",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        pair_out.status.success(),
        "pair failed: {}",
        String::from_utf8_lossy(&pair_out.stderr)
    );
    let pair_json: serde_json::Value = serde_json::from_slice(&pair_out.stdout).unwrap();
    let host_id = pair_json["id"].as_str().unwrap().to_owned();
    assert_eq!(pair_json["name"], "거실 맥미니");

    // 5) krill-link devices --json
    let dev_out = Command::new(MK_LINK_BIN)
        .args([
            "--state-dir",
            server_dir.to_str().unwrap(),
            "devices",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(dev_out.status.success());
    let dev_json: serde_json::Value = serde_json::from_slice(&dev_out.stdout).unwrap();
    let dev_arr = dev_json.as_array().unwrap();
    assert_eq!(dev_arr.len(), 1);
    let dev_id = dev_arr[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(dev_arr[0]["name"], "내 맥북에어");

    // 6) krill-link connect <host_id> 백그라운드 프로세스 기동 + krill-link status --json 확인
    let mut connect_child = Command::new(MK_LINK_BIN)
        .args([
            "--state-dir",
            client_dir.to_str().unwrap(),
            "connect",
            &host_id,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let status_deadline = Instant::now() + WAIT;
    loop {
        let st_out = Command::new(MK_LINK_BIN)
            .args([
                "--state-dir",
                client_dir.to_str().unwrap(),
                "status",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(st_out.status.success());
        let st_json: serde_json::Value = serde_json::from_slice(&st_out.stdout).unwrap();
        if st_json["hosts"][0]["state"] == "connected" {
            assert_eq!(st_json["hosts"][0]["route"], "direct");
            break;
        }
        assert!(
            Instant::now() < status_deadline,
            "status did not become connected: {st_json}"
        );
        thread::sleep(Duration::from_millis(40));
    }

    // 7) krill-link revoke <dev_id> --json
    let rev_out = Command::new(MK_LINK_BIN)
        .args([
            "--state-dir",
            server_dir.to_str().unwrap(),
            "revoke",
            &dev_id,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(rev_out.status.success());

    connect_child.kill().ok();
    connect_child.wait().ok();
    serve_child.kill().ok();
    serve_child.wait().ok();
    fs::remove_dir_all(&base).ok();
}

#[path = "support/resume_real.rs"]
mod resume_real;

/// OWE-669: 격리 상태 디렉터리로 `enable`하면 실제 라벨이 아닌 해시 라벨 plist에 `KRILL_STATE_DIR`이 들어가고,
/// `disable`은 그 항목만 지운다. `launchctl`은 건너뛰고 `LaunchAgents` 디렉터리는 임시 디렉터리다.
#[test]
fn isolated_enable_writes_distinct_label_with_state_dir_env() {
    let base = short_temp_dir("agent");
    let state = base.join("state");
    let agents = base.join("agents");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&agents).unwrap();
    let real = agents.join(format!("{}.plist", krill_link::LINK_LABEL));
    fs::write(&real, "real").unwrap();
    let run = |cmd: &str| {
        let out = Command::new(MK_LINK_BIN)
            .arg(cmd)
            .env("HOME", &base)
            .env("KRILL_STATE_DIR", &state)
            .env("KRILL_LINK_LAUNCH_AGENTS_DIR", &agents)
            .env("KRILL_LINK_SKIP_LAUNCHCTL", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run("enable");
    let label = krill_link::link_label(Some(&state));
    let plist = fs::read_to_string(agents.join(format!("{label}.plist"))).unwrap();
    assert!(plist.contains(&format!("<string>{label}</string>")));
    assert!(plist.contains(&format!(
        "<key>KRILL_STATE_DIR</key>\n\t\t<string>{}</string>",
        state.display()
    )));
    assert_eq!(
        fs::read_to_string(&real).unwrap(),
        "real",
        "real agent untouched"
    );

    run("disable");
    assert!(!agents.join(format!("{label}.plist")).exists());
    assert_eq!(
        fs::read_to_string(&real).unwrap(),
        "real",
        "real agent untouched"
    );
    fs::remove_dir_all(&base).ok();
}
