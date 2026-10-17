//! MK-063 e2e: 로컬 HTTP 서버 + 실제 `krill-holder`·`krilld` 데몬 세션 + `krill-app --relaunch-update` 프로세스로
//! 위조 패키지 거부, 정상 업데이트(0.1.0 → 0.2.0) 중 세션·출력 무손실 유지, 재실행 실패(0.2.0 → 0.3.0) 시
//! 자동 롤백과 세션 유지를 검증한다.

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_app::updater::ed25519::{encode_hex, sha256_hex};
use krill_app::updater::{
    Channel, CodesignVerifier, Error, Journal, Manifest, READY_ENV, Stage, download_and_stage,
    fetch_bytes, fetch_manifest, prev_path, spawn_detached_relauncher,
};
use krill_proto::SessionId;
use krill_proto::control::{Create, Info, Program, Reply, Request};
use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::{Attach, Msg as _, Seq, Stream, decode_output};
use ring::signature::{Ed25519KeyPair, KeyPair as _};

const APP: &str = env!("CARGO_BIN_EXE_krill-app");
const WAIT: Duration = Duration::from_secs(15);

fn sign_manifest(m: &mut Manifest, secret: &[u8; 32]) {
    let kp = Ed25519KeyPair::from_seed_unchecked(secret).unwrap();
    let sig = kp.sign(&m.signing_payload());
    m.ed25519_sig = encode_hex(sig.as_ref());
}

struct DaemonGuard {
    holder: Child,
    krilld: Option<u32>,
    session: Option<u32>,
    dir: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.holder.kill().ok();
        self.holder.wait().ok();
        for pid in [self.krilld, self.session].into_iter().flatten() {
            Command::new("/bin/kill")
                .args(["-9", &pid.to_string()])
                .status()
                .ok();
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn connect(paths: &Paths) -> daemon::Connection {
    let c = daemon::handshake(
        &paths.socket,
        &daemon::client_hello(ClientRole::App, "mk063-e2e"),
        WAIT,
    )
    .unwrap();
    c.stream.set_read_timeout(Some(WAIT)).unwrap();
    c
}

fn request(paths: &Paths, req: &Request) -> Reply {
    let mut c = connect(paths);
    c.stream.write_all(&req.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

fn session_info(paths: &Paths, session: SessionId) -> Info {
    let Reply::Sessions(list) = request(paths, &Request::List) else {
        unreachable!("expected session list");
    };
    let info = list.into_iter().find(|i| i.session == session).unwrap();
    assert_eq!(info.status, None, "session process must stay alive");
    info
}

fn build_bundle(bundle: &Path, version: &str, script: &str) {
    let macos = bundle.join("Contents/MacOS");
    let resources = bundle.join("Contents/Resources");
    fs::create_dir_all(&macos).unwrap();
    fs::create_dir_all(&resources).unwrap();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>dev.krill.app</string>
	<key>CFBundleExecutable</key>
	<string>krill-app</string>
	<key>CFBundleName</key>
	<string>Krill</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
</dict>
</plist>
"#
    );
    fs::write(bundle.join("Contents/Info.plist"), plist).unwrap();
    fs::write(resources.join("version.txt"), version).unwrap();
    let exe = macos.join("krill-app");
    fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();

    let s = Command::new("/usr/bin/codesign")
        .args(["--force", "-s", "-"])
        .arg(bundle)
        .status()
        .unwrap();
    assert!(s.success());
}

fn zip_bundle(bundle: &Path, zip_path: &Path) -> Vec<u8> {
    let s = Command::new("/usr/bin/ditto")
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(bundle)
        .arg(zip_path)
        .status()
        .unwrap();
    assert!(s.success());
    fs::read(zip_path).unwrap()
}

fn read_all_raw(paths: &Paths, session: SessionId, expected_sub: &[u8]) -> Vec<u8> {
    let mut c = connect(paths);
    c.stream
        .write_all(
            &Attach {
                session,
                stream: Stream::Raw { from: Seq(0) },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (k, b) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    let _ = krill_proto::screen::Attached::decode(k, &b).unwrap();
    let mut got = Vec::new();
    let deadline = Instant::now() + WAIT;
    while !got.windows(expected_sub.len()).any(|w| w == expected_sub) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out waiting for expected output");
        c.stream.set_read_timeout(Some(left)).unwrap();
        let (k, b) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
        let (seq, bytes) = decode_output(k, &b).unwrap();
        assert_eq!(seq.0, u64::try_from(got.len()).unwrap(), "output seq gap");
        got.extend_from_slice(bytes);
    }
    got
}

#[test]
fn update_and_rollback_preserve_live_session_and_reject_forged_package() {
    let bin = Path::new(APP).parent().unwrap();
    let paths_holder = bin.join("krill-holder");
    let paths_krilld = bin.join("krilld");
    for b in [&paths_holder, &paths_krilld] {
        assert!(
            b.is_file(),
            "{} is not built next to krill-app",
            b.display()
        );
    }

    // UDS 104바이트 제한 안의 짧은 임시 디렉터리
    let dir = PathBuf::from(format!("/tmp/mk063-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let paths = Paths {
        socket: dir.join("run/krilld.sock"),
        holder: paths_holder,
        krilld: paths_krilld,
    };

    let holder = Command::new(&paths.holder)
        .arg(daemon::DETACH_ARG)
        .arg(&paths.krilld)
        .arg(&paths.socket)
        .env("KRILL_STATE_DIR", &dir)
        .env("HOME", &dir)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut guard = DaemonGuard {
        holder,
        krilld: None,
        session: None,
        dir: dir.clone(),
    };

    wait_for("daemon socket", || {
        paths
            .socket
            .metadata()
            .is_ok_and(|m| m.file_type().is_socket())
    });
    let daemon_instance = connect(&paths).server.instance_id;
    guard.krilld = Some(u32::try_from(daemon_instance).unwrap());

    // 연속으로 줄을 찍는 세션 생성
    let script = "stty raw -echo; i=1; while [ $i -le 60 ]; do printf 'TICK-%03d\\r\\n' $i; i=$((i+1)); sleep 0.02; done; while :; do sleep 1; done";
    let Reply::Created { session, pid } = request(
        &paths,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![b"-c".to_vec(), script.as_bytes().to_vec()],
            },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        }),
    ) else {
        unreachable!("session create failed");
    };
    guard.session = Some(pid);
    let _ = read_all_raw(&paths, session, b"TICK-005\r\n");

    let sk = [42; 32];
    let pk: [u8; 32] = Ed25519KeyPair::from_seed_unchecked(&sk)
        .unwrap()
        .public_key()
        .as_ref()
        .try_into()
        .unwrap();
    // 업데이터는 키 미설정 시 비활성이다. 이 fixture만의 키를 명시적으로 설정한다.
    fs::write(
        dir.join(krill_app::updater::CONFIG_FILE),
        format!("public_key = \"{}\"\n", encode_hex(&pk)),
    )
    .unwrap();

    let app = dir.join("Krill.app");
    let boot_log = dir.join("boot.log");
    build_bundle(
        &app,
        "0.1.0",
        &format!(
            "[ -n \"${READY_ENV}\" ] && echo ok > \"${READY_ENV}\"\necho 0.1.0 >> '{}'",
            boot_log.display()
        ),
    );

    let v2_src = dir.join("v2/Krill.app");
    build_bundle(
        &v2_src,
        "0.2.0",
        &format!(
            "[ -n \"${READY_ENV}\" ] && echo ok > \"${READY_ENV}\"\necho 0.2.0 >> '{}'",
            boot_log.display()
        ),
    );
    let v2_zip = zip_bundle(&v2_src, &dir.join("v2.zip"));

    let v3_src = dir.join("v3/Krill.app");
    build_bundle(&v3_src, "0.3.0", "exit 1");
    let v3_zip = zip_bundle(&v3_src, &dir.join("v3.zip"));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());

    let mut m_v2 = Manifest {
        version: "0.2.0".to_owned(),
        url: format!("{base_url}/v2.zip"),
        sha256: sha256_hex(&v2_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest(&mut m_v2, &sk);
    let mut m_forged = m_v2.clone();
    m_forged.url = format!("{base_url}/forged.zip");
    sign_manifest(&mut m_forged, &sk);

    let mut m_v3 = Manifest {
        version: "0.3.0".to_owned(),
        url: format!("{base_url}/v3.zip"),
        sha256: sha256_hex(&v3_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest(&mut m_v3, &sk);

    let m_v2_bytes = serde_json::to_vec(&m_v2).unwrap();
    let m_forged_bytes = serde_json::to_vec(&m_forged).unwrap();
    let m_v3_bytes = serde_json::to_vec(&m_v3).unwrap();
    let mut forged_zip = v2_zip.clone();
    forged_zip[0] ^= 0xff;

    let server = thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let body: &[u8] = match path {
                "/forged/latest.json" => &m_forged_bytes,
                "/forged.zip" => &forged_zip,
                "/v2/latest.json" => &m_v2_bytes,
                "/v2.zip" => &v2_zip,
                "/v3/latest.json" => &m_v3_bytes,
                "/v3.zip" => &v3_zip,
                "/stop" => break,
                _ => b"not found",
            };
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(hdr.as_bytes()).ok();
            stream.write_all(body).ok();
        }
    });

    // 1) 위조 패키지 거부
    let forged_manifest = fetch_manifest(&format!("{base_url}/forged/latest.json")).unwrap();
    let forged_res = download_and_stage(
        &forged_manifest,
        &pk,
        "0.1.0",
        Channel::Stable,
        &app,
        &CodesignVerifier::adhoc(),
        0,
    );
    assert!(matches!(forged_res, Err(Error::Sha256Mismatch { .. })));
    assert_eq!(
        fs::read_to_string(app.join("Contents/Resources/version.txt")).unwrap(),
        "0.1.0"
    );
    assert_eq!(session_info(&paths, session).pid, pid);

    // 2) 별도 relauncher(`krill-app --relaunch-update`)로 0.1.0 -> 0.2.0 업데이트 + 세션 유지
    let v2_manifest = fetch_manifest(&format!("{base_url}/v2/latest.json")).unwrap();
    download_and_stage(
        &v2_manifest,
        &pk,
        "0.1.0",
        Channel::Stable,
        &app,
        &CodesignVerifier::adhoc(),
        0,
    )
    .unwrap();
    let mut relauncher = spawn_detached_relauncher(
        Path::new(APP),
        &app,
        &dir,
        "0.1.0",
        "0.2.0",
        None,
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(relauncher.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(app.join("Contents/Resources/version.txt")).unwrap(),
        "0.2.0"
    );
    assert_eq!(
        fs::read_to_string(prev_path(&app).join("Contents/Resources/version.txt")).unwrap(),
        "0.1.0"
    );
    assert_eq!(Journal::load(&dir).unwrap().unwrap().stage, Stage::Healthy);
    assert_eq!(session_info(&paths, session).pid, pid);

    // 3) 고장 난 0.3.0 재실행 실패 시 relauncher가 0.2.0으로 자동 롤백 + 세션·출력 무손실 유지
    let v3_manifest = fetch_manifest(&format!("{base_url}/v3/latest.json")).unwrap();
    download_and_stage(
        &v3_manifest,
        &pk,
        "0.2.0",
        Channel::Stable,
        &app,
        &CodesignVerifier::adhoc(),
        0,
    )
    .unwrap();
    let mut relauncher_v3 = spawn_detached_relauncher(
        Path::new(APP),
        &app,
        &dir,
        "0.2.0",
        "0.3.0",
        None,
        Duration::from_millis(500),
    )
    .unwrap();
    assert!(relauncher_v3.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(app.join("Contents/Resources/version.txt")).unwrap(),
        "0.2.0"
    );
    assert!(matches!(
        Journal::load(&dir).unwrap().unwrap().stage,
        Stage::RolledBack { .. }
    ));

    // 업데이트 + 롤백 내내 세션 PID가 같고 TICK-001..TICK-060 바이트가 빠짐없이 이어진다.
    assert_eq!(session_info(&paths, session).pid, pid);
    assert_eq!(connect(&paths).server.instance_id, daemon_instance);
    let raw_bytes = read_all_raw(&paths, session, b"TICK-060\r\n");
    let raw_text = String::from_utf8(raw_bytes).unwrap();
    for i in 1..=60 {
        let line = format!("TICK-{i:03}\r\n");
        assert!(raw_text.contains(&line), "missing {line:?}");
    }

    fetch_bytes(&format!("{base_url}/stop"), 64).ok();
    server.join().ok();
    assert_eq!(request(&paths, &Request::Close { session }), Reply::Closed);
    guard.session = None;
}
