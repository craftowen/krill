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

    #[cfg_attr(
        not(windows),
        expect(
            clippy::unused_self,
            reason = "Windows copies each fixture's server out of Cargo's shared output"
        )
    )]
    fn program(&self) -> PathBuf {
        #[cfg(windows)]
        {
            // A resident server must not lock Cargo's output during other builds.
            let program = self.dir.join("krill-link.exe");
            if !program.is_file() {
                fs::copy(env!("CARGO_BIN_EXE_krill-link"), &program).unwrap();
            }
            program
        }
        #[cfg(not(windows))]
        PathBuf::from(env!("CARGO_BIN_EXE_krill-link"))
    }

    fn disable(&self) {
        let result = Command::new(self.program())
            .args(["disable", "--state-dir"])
            .arg(&self.dir)
            .env("KRILL_LINK_SKIP_LAUNCHCTL", "1")
            .env("KRILL_LINK_LAUNCH_AGENTS_DIR", self.dir.join("agents"))
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    }

    fn start(&mut self) {
        #[cfg(windows)]
        {
            let output = Command::new(self.program())
                .args(["enable", "--state-dir"])
                .arg(&self.dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        #[cfg(not(windows))]
        {
            self.child = Some(
                Command::new(self.program())
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
    // 라이브러리의 자동 시작에는 배경 고아 정리도 포함되므로 테스트 전체에 훅을 강제한다.
    if std::env::var_os("KRILL_LINK_SKIP_LAUNCHCTL").is_none() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "disabling_exits_serve_process_and_keeps_local_socket_alive",
                "--nocapture",
            ])
            .env("KRILL_LINK_SKIP_LAUNCHCTL", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        return;
    }
    let mut fixture = Fixture::new("stop");
    let local_path = fixture.dir.join("run/krilld.sock");
    fs::create_dir_all(local_path.parent().unwrap()).unwrap();
    let local = krill_proto::os::net::UnixListener::bind(&local_path).unwrap();
    let child = Command::new(fixture.program())
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
    fixture.disable();
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
    fixture.disable();
    assert!(!is_serve_running(&fixture.dir));
}

#[cfg(windows)]
#[test]
fn windows_default_server_starts_reuses_and_remembers_off() {
    let fixture = Fixture::new("default");
    let program = fixture.program();
    let executable = program.as_path();
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

/// 실제 사용자 도메인에 접근할 수 없도록 훅을 강제한 자식에서 앱의 자동 시작 경로를 검사한다.
#[cfg(target_os = "macos")]
#[test]
fn temporary_home_prevents_default_autostart_even_without_state_override() {
    const CHILD: &str = "KRILL_LINK_TEST_TEMP_HOME";
    if std::env::var_os(CHILD).is_some() {
        let state = krill_link::resolve_state_dir(None).unwrap();
        let executable = std::path::Path::new(env!("CARGO_BIN_EXE_krill-link"));
        krill_link::service::ensure_default_server(executable, &state).unwrap();
        assert!(
            !state.exists(),
            "temporary HOME must not even create service state"
        );
        assert!(
            !std::path::Path::new(&std::env::var_os("HOME").unwrap())
                .join("Library/LaunchAgents")
                .exists()
        );
        return;
    }
    let fixture = Fixture::new("temp-home");
    for isolated in [false, true] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "temporary_home_prevents_default_autostart_even_without_state_override",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", &fixture.dir)
            .env_remove("KRILL_STATE_DIR")
            .env_remove("KRILL_LINK_LAUNCH_AGENTS_DIR")
            .env("KRILL_LINK_SKIP_LAUNCHCTL", "1");
        if isolated {
            command.env("KRILL_STATE_DIR", fixture.dir.join("isolated"));
        }
        let out = command.output().unwrap();
        assert!(out.status.success(), "{out:?}");
    }
}

/// 실제 HOME의 격리 상태 디렉터리는 자동 시작을 유지한다. plist 쓰기만 허용하고 launchctl은 막는다.
#[cfg(target_os = "macos")]
#[test]
fn real_home_keeps_default_autostart_for_custom_state_directory() {
    const CHILD: &str = "KRILL_LINK_TEST_REAL_HOME";
    if std::env::var_os(CHILD).is_some() {
        let state = krill_link::resolve_state_dir(None).unwrap();
        let executable = std::path::Path::new(env!("CARGO_BIN_EXE_krill-link"));
        krill_link::service::ensure_default_server(executable, &state).unwrap();
        let label = krill_link::link_label(Some(&state));
        let plist = krill_link::launch_agent_plist_path(&label).unwrap();
        let xml = fs::read_to_string(plist).unwrap();
        assert!(xml.contains("<key>KRILL_LINK_AGENT_PLIST</key>"));
        assert!(xml.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(xml.contains("<key>KeepAlive</key>\n\t<true/>"));
        return;
    }
    let fixture = Fixture::new("real-home");
    let account = Command::new("/usr/bin/dscacheutil")
        .args(["-q", "user", "-a", "uid"])
        .arg(rustix::process::getuid().as_raw().to_string())
        .output()
        .unwrap();
    assert!(account.status.success());
    let home = String::from_utf8(account.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("dir: ").map(str::to_owned))
        .unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "real_home_keeps_default_autostart_for_custom_state_directory",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("HOME", home)
        .env("KRILL_STATE_DIR", &fixture.dir)
        .env("KRILL_LINK_LAUNCH_AGENTS_DIR", fixture.dir.join("agents"))
        .env("KRILL_LINK_SKIP_LAUNCHCTL", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// debug 전용 실행기 + skip 훅을 함께 써서 실제 launchd 없이 `cmd_serve` 전체를 검사한다.
#[cfg(all(target_os = "macos", debug_assertions))]
mod managed_agent {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    fn start(fixture: &mut Fixture, state: &Path, plist: &Path) {
        let executor = fixture.dir.join("fake-launchctl");
        fs::write(
            &executor,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$KRILL_LINK_TEST_LOG\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&executor, fs::Permissions::from_mode(0o700)).unwrap();
        fixture.child = Some(
            Command::new(env!("CARGO_BIN_EXE_krill-link"))
                .args(["serve", "--state-dir"])
                .arg(state)
                .env("KRILL_LINK_SKIP_LAUNCHCTL", "1")
                .env("KRILL_LINK_TEST_LAUNCHCTL", executor)
                .env("KRILL_LINK_TEST_LOG", fixture.dir.join("calls"))
                .env("XPC_SERVICE_NAME", krill_link::link_label(Some(state)))
                .env("KRILL_LINK_AGENT_PLIST", plist)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
    }

    fn wait_for_exit(fixture: &mut Fixture) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = fixture.child.as_mut().unwrap().try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "managed serve did not exit");
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn assert_bootout(fixture: &Fixture, state: &Path) {
        let target = format!(
            "bootout gui/{}/{}\n",
            rustix::process::getuid().as_raw(),
            krill_link::link_label(Some(state))
        );
        assert_eq!(
            fs::read_to_string(fixture.dir.join("calls")).unwrap(),
            target
        );
    }

    #[test]
    fn missing_state_with_plist_retries_without_bootout_or_recreating_state() {
        let mut fixture = Fixture::new("managed-before");
        let state = fixture.dir.join("gone");
        let plist = fixture
            .dir
            .join(format!("{}.plist", krill_link::link_label(Some(&state))));
        fs::write(&plist, "plist").unwrap();
        start(&mut fixture, &state, &plist);
        assert!(!wait_for_exit(&mut fixture).success());
        assert!(!state.exists(), "preflight must not recreate deleted state");
        assert!(!fixture.dir.join("calls").exists());
        // D2: 자동 정리는 사용자 plist를 삭제하지 않는 보수적 정책을 유지한다.
        assert!(plist.exists());
    }

    #[test]
    fn unmounted_state_exits_unsuccessfully_without_bootout_or_directory_creation() {
        let mut fixture = Fixture::new("managed-volume");
        let state = PathBuf::from(format!(
            "/Volumes/krill-test-unmounted-{}/state",
            std::process::id()
        ));
        assert!(!state.exists());
        let plist = fixture
            .dir
            .join(format!("{}.plist", krill_link::link_label(Some(&state))));
        fs::write(&plist, "plist").unwrap();
        start(&mut fixture, &state, &plist);
        assert!(!wait_for_exit(&mut fixture).success());
        assert!(!fixture.dir.join("calls").exists());
        assert!(!state.exists());
        assert!(plist.exists());
    }

    #[test]
    fn live_serve_stops_through_self_monitor_when_plist_disappears() {
        let mut fixture = Fixture::new("managed-live");
        let state = fixture.dir.clone();
        let plist = fixture
            .dir
            .join(format!("{}.plist", krill_link::link_label(Some(&state))));
        fs::write(&plist, "plist").unwrap();
        start(&mut fixture, &state, &plist);
        let deadline = Instant::now() + Duration::from_secs(15);
        while !is_serve_running(&state) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(is_serve_running(&state));
        fs::remove_file(&plist).unwrap();
        let removed_at = Instant::now();
        assert!(wait_for_exit(&mut fixture).success());
        assert!(removed_at.elapsed() >= Duration::from_secs(2));
        assert!(
            !is_serve_running(&state),
            "select must shut down the server"
        );
        assert_bootout(&fixture, &state);
    }

    #[test]
    fn broken_injected_executor_never_falls_back_to_launchctl() {
        let fixture = Fixture::new("managed-broken");
        let state = fixture.dir.join("gone");
        let label = krill_link::link_label(Some(&state));
        let plist = fixture.dir.join(format!("{label}.plist"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_krill-link"))
            .args(["serve", "--state-dir"])
            .arg(&state)
            .env("KRILL_LINK_SKIP_LAUNCHCTL", "1")
            .env(
                "KRILL_LINK_TEST_LAUNCHCTL",
                fixture.dir.join("missing-executor"),
            )
            .env("XPC_SERVICE_NAME", label)
            .env("KRILL_LINK_AGENT_PLIST", plist)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // bootout 호출이 실패하므로 두 번째 관측 뒤에도 살아 있어야 한다.
        thread::sleep(Duration::from_millis(2500));
        let status = child.try_wait().unwrap();
        child.kill().ok();
        child.wait().unwrap();
        assert!(status.is_none());
        assert!(!state.exists());
    }
}
