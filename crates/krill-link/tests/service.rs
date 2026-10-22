//! 수신 서버의 실제 프로세스 시작·정상 종료 및 기본 허용 설정 회귀 검사.
#![expect(clippy::unwrap_used, reason = "integration assertions")]

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_link::{LinkConfig, RelaySetting, is_serve_running};

struct Fixture {
    dir: PathBuf,
    child: Option<Child>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        // macOS의 AF_UNIX 경로 길이 제한 이내로 유지한다.
        #[cfg(unix)]
        let base = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        let dir = base.join(format!("kls-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let cfg = LinkConfig {
            relay: RelaySetting::Off,
            lan_discovery: false,
            ..LinkConfig::default()
        };
        cfg.save(&dir).unwrap();
        Self { dir, child: None }
    }

    fn start(&mut self) {
        #[cfg(windows)]
        {
            let output = Command::new(env!("CARGO_BIN_EXE_krill-link"))
                .args(["enable", "--state-dir"])
                .arg(&self.dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        #[cfg(not(windows))]
        {
            self.child = Some(
                Command::new(env!("CARGO_BIN_EXE_krill-link"))
                    .args(["serve", "--state-dir"])
                    .arg(&self.dir)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while !is_serve_running(&self.dir) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(is_serve_running(&self.dir));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(windows)]
        krill_link::service::disable_server(&self.dir).ok();
        if let Some(child) = self.child.as_mut() {
            child.kill().ok();
            child.wait().ok();
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

#[test]
fn disabling_exits_serve_process_and_keeps_local_socket_alive() {
    let mut fixture = Fixture::new("stop");
    let local_path = fixture.dir.join("run/krilld.sock");
    fs::create_dir_all(local_path.parent().unwrap()).unwrap();
    let local = krill_proto::os::net::UnixListener::bind(&local_path).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_krill-link"))
        .args(["serve", "--state-dir"])
        .arg(&fixture.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    fixture.child = Some(child);
    let deadline = Instant::now() + Duration::from_secs(15);
    while !is_serve_running(&fixture.dir) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(is_serve_running(&fixture.dir));
    let result = Command::new(env!("CARGO_BIN_EXE_krill-link"))
        .args(["disable", "--state-dir"])
        .arg(&fixture.dir)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = fixture.child.as_mut().unwrap().try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "serve process did not exit");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!is_serve_running(&fixture.dir));
    assert!(!krill_link::service::incoming_enabled(&fixture.dir).unwrap());
    krill_link::service::ensure_default_server(std::path::Path::new("missing"), &fixture.dir)
        .unwrap();
    // 수신 서버 종료가 로컬 세션 데몬의 리스너를 닫지 않는다.
    let stream = krill_proto::os::net::UnixStream::connect(&local_path).unwrap();
    drop(stream);
    drop(local);
}

/// Windows에서는 CLI enable로 시작한 서버에 초대 링크로 페어링하고 실제 터널을 통과한다.
#[test]
fn incoming_server_pairs_and_forwards_to_local_socket() {
    use krill_link::local::{Listener, Stream};
    use krill_link::{ConnectOptions, create_invite_command, pair_with_server, start_tunnel};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut fixture = Fixture::new("pair-tunnel");
    fixture.start();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(20), async {
            let local = Listener::bind(fixture.dir.join("run/krilld.sock")).unwrap();
            let client = fixture.dir.join("client");
            LinkConfig {
                relay: RelaySetting::Off,
                ..LinkConfig::default()
            }
            .save(&client)
            .unwrap();
            let invite = create_invite_command(&fixture.dir, None).await.unwrap();
            let host = pair_with_server(&client, &invite.link, "remote client")
                .await
                .unwrap();
            assert_eq!(krill_link::load_devices(&fixture.dir).unwrap().len(), 1);
            let tunnel = start_tunnel(ConnectOptions::new(client, host.id))
                .await
                .unwrap();
            let mut remote = Stream::connect(tunnel.socket_path()).await.unwrap();
            let (mut upstream, _) = local.accept().await.unwrap();
            remote.write_all(b"request").await.unwrap();
            let mut request = [0; 7];
            upstream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"request");
            upstream.write_all(b"response").await.unwrap();
            let mut response = [0; 8];
            remote.read_exact(&mut response).await.unwrap();
            assert_eq!(&response, b"response");
            tunnel.shutdown().await;
            // 원격 터널 종료가 로컬 데몬 리스너를 닫지 않는다.
            let _local_connection = Stream::connect(fixture.dir.join("run/krilld.sock"))
                .await
                .unwrap();
        })
        .await
        .unwrap();
    });
    krill_link::service::disable_server(&fixture.dir).unwrap();
    assert!(!is_serve_running(&fixture.dir));
}

#[cfg(windows)]
#[test]
fn windows_default_server_starts_reuses_and_remembers_off() {
    let fixture = Fixture::new("default");
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_krill-link"));
    assert!(krill_link::service::incoming_enabled(&fixture.dir).unwrap());
    krill_link::service::ensure_default_server(executable, &fixture.dir).unwrap();
    assert!(is_serve_running(&fixture.dir));
    let ticket = fs::read(fixture.dir.join("link/serve.ticket")).unwrap();
    krill_link::service::enable_server(executable, &fixture.dir).unwrap();
    assert_eq!(
        ticket,
        fs::read(fixture.dir.join("link/serve.ticket")).unwrap()
    );
    krill_link::service::disable_server(&fixture.dir).unwrap();
    assert!(!is_serve_running(&fixture.dir));
    krill_link::service::ensure_default_server(executable, &fixture.dir).unwrap();
    assert!(!is_serve_running(&fixture.dir));
    krill_link::service::enable_server(executable, &fixture.dir).unwrap();
    assert!(is_serve_running(&fixture.dir));
    krill_link::service::disable_server(&fixture.dir).unwrap();
}
