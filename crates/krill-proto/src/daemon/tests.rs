use std::process::Command;

use super::*;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mk025-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    d
}

fn paths(dir: &Path, holder: &str, krilld: &str) -> Paths {
    Paths {
        socket: dir.join("run/krilld.sock"),
        holder: PathBuf::from(holder),
        krilld: PathBuf::from(krilld),
    }
}

fn hello() -> ClientHello {
    client_hello(ClientRole::Cli, "test")
}

/// 명시한 handshake 기한은 그대로 존중한다. 서버가 2초 늦어도 0.2초로 부르면
/// 0.2초 근처에서 끝난다. `krill wait --timeout 0.2`가 30초 붙잡히던 회귀 방지.
#[test]
fn explicit_handshake_timeout_is_respected() {
    for role in [ClientRole::App, ClientRole::Cli, ClientRole::Hook] {
        let d = dir(&format!("explicit-hello-{role:?}"));
        std::fs::create_dir_all(&d).unwrap();
        let socket = d.join("k.sock");
        let listener = crate::os::net::UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(2));
            let server = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "slow-signature".into(),
                instance_id: 1,
            };
            // 클라이언트는 이미 기한으로 끊었으므로 쓰기·읽기 실패는 무시한다.
            stream.write_all(&server.encode().unwrap()).ok();
            ipc::read_frame(&mut stream).ok();
        });
        let began = Instant::now();
        let result = handshake(
            &socket,
            &client_hello(role, "test"),
            Duration::from_millis(200),
        );
        let elapsed = began.elapsed();
        assert!(
            result.is_err(),
            "{role:?}: slow hello must fail on explicit 0.2s"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "{role:?}: took {elapsed:?}, explicit deadline ignored"
        );
        worker.join().unwrap();
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 기본 앱·CLI 연결(`connect`)은 macOS 서명 검증 4초 지연을 견딘다.
/// 감시 연결과 실제 연결 두 번 모두 서명 여유를 받는다.
#[test]
fn default_connect_allows_slow_signature_verification() {
    for role in [ClientRole::App, ClientRole::Cli] {
        let d = dir(&format!("default-hello-{role:?}"));
        std::fs::create_dir_all(d.join("run")).unwrap();
        let socket = d.join("run/krilld.sock");
        let listener = crate::os::net::UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            // 감시 handshake 한 번 + 실제 handshake 한 번.
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                // macOS Security.framework 검증이 Hello 전 4초 걸리는 상황.
                thread::sleep(Duration::from_secs(4));
                let server = ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES,
                    build: "slow-signature".into(),
                    instance_id: 1,
                };
                stream.write_all(&server.encode().unwrap()).unwrap();
                let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
                let client = ClientHello::decode(kind, &body).unwrap();
                assert_eq!(client.role, role);
                if is_watch(client.features) && server.features & FEAT_HOLDER_WATCH != 0 {
                    stream
                        .write_all(&HolderStatus::Linked.encode().unwrap())
                        .unwrap();
                }
            }
        });
        let paths = Paths {
            socket: socket.clone(),
            holder: PathBuf::from("/nonexistent/krill-holder"),
            krilld: PathBuf::from("/nonexistent/krilld"),
        };
        let connected = connect(&paths, &client_hello(role, "test"));
        assert!(connected.is_ok(), "{role:?}: {connected:?}");
        assert!(!connected.unwrap().launched);
        worker.join().unwrap();
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 스냅샷: 바뀌면 launchd 잡 정의가 바뀐 것이다. 의도한 변경이면 `daemon/holder.plist`를 함께 고친다.
#[test]
fn holder_plist_snapshot() {
    let plist = holder_plist(
        "/Applications/Krill.app/Contents/Helpers/R&D/krill-holder",
        "/Applications/Krill.app/Contents/Helpers/R&D/krilld",
        "/Users/me/Library/Application Support/dev.krill.app/run/krilld.sock",
    );
    assert_eq!(plist, include_str!("holder.plist"));
    assert!(plist.contains("<key>AssociatedBundleIdentifiers</key>"));
    // launchd가 띄운 holder는 자기 setsid 금지(CONVENTIONS A1).
    assert!(!plist.contains(DETACH_ARG));
}

/// 생성한 plist를 macOS가 읽을 수 있다.
#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: plutil(launchd plist)")]
fn holder_plist_lints() {
    let d = dir("plist");
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("holder.plist");
    std::fs::write(&f, holder_plist("/a&b/krill-holder", "/a<b>/krilld", "/s")).unwrap();
    let out = Command::new("/usr/bin/plutil")
        .arg("-lint")
        .arg(&f)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn missing_holder_is_reported() {
    let d = dir("missing");
    let p = paths(&d, "/nonexistent/krill-holder", "/usr/bin/true");
    let e = connect(&p, &hello()).unwrap_err();
    assert!(matches!(&e, Error::Missing(x) if x == &p.holder), "{e}");
    assert!(e.to_string().contains("/nonexistent/krill-holder"));
    assert!(
        e.to_string().contains("cargo build --workspace --bins"),
        "{e}"
    );
}

#[test]
fn holder_that_dies_is_reported() {
    let d = dir("dies");
    #[cfg(unix)]
    let p = paths(&d, "/usr/bin/false", "/usr/bin/true");
    #[cfg(windows)]
    let p = {
        let system = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap()
            .join("System32");
        Paths {
            socket: d.join("run/krilld.sock"),
            holder: system.join("where.exe"),
            krilld: system.join("cmd.exe"),
        }
    };
    let e = connect(&p, &hello()).unwrap_err();
    assert!(matches!(e, Error::HolderExited(s) if !s.success()), "{e}");
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn long_socket_path_is_rejected() {
    let d = dir(&"x".repeat(100));
    let e = connect(&paths(&d, "/usr/bin/true", "/usr/bin/true"), &hello()).unwrap_err();
    assert!(matches!(e, Error::PathTooLong(_)), "{e}");
}

#[test]
fn escape_xml() {
    assert_eq!(escape("a&<b>\"한"), "a&amp;&lt;b&gt;\"한");
}

/// 다른 기동자가 락을 놓지 않으면 무한히 기다리지 않고 `LockTimeout`으로 끝난다(OWE-484).
#[test]
fn held_launch_lock_times_out() {
    let d = dir("lock");
    let sock = d.join("run/krilld.sock");
    let _held = launch_lock(&sock, Duration::ZERO).unwrap();
    // flock은 열린 파일 설명마다라 같은 프로세스에서 다시 열어도 막힌다.
    let e = launch_lock(&sock, Duration::from_millis(50)).unwrap_err();
    assert!(matches!(e, Error::LockTimeout), "{e}");
    std::fs::remove_dir_all(&d).ok();
}

/// krilld가 Hello 직후 연결을 닫아도 협상은 성공한다. macOS는 피어가 닫힌 소켓의 `setsockopt`를 EINVAL로
/// 거부해 타임아웃 해제에서 실패했다(OWE-487). 닫힘 시점이 경합이라 여러 번 돌린다.
#[test]
fn handshake_survives_server_closing_after_hello() {
    const N: usize = 20_000;
    let d = dir("close");
    std::fs::create_dir_all(&d).unwrap();
    let sock = d.join("k.sock");
    let lis = crate::os::net::UnixListener::bind(&sock).unwrap();
    let server = ServerHello {
        proto_min: ipc::PROTO_MIN,
        proto_max: ipc::PROTO_MAX,
        features: ipc::FEATURES,
        build: "test".to_owned(),
        instance_id: 1,
    };
    let frame = server.encode().unwrap();
    let t = thread::spawn(move || {
        for _ in 0..N {
            let (mut s, _) = lis.accept().unwrap();
            s.write_all(&frame).unwrap();
            ipc::read_frame(&mut s).unwrap();
        }
    });
    for _ in 0..N {
        handshake(&sock, &hello(), HELLO_TIMEOUT).unwrap();
    }
    t.join().unwrap();
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn retry_delay_doubles_to_cap() {
    let got: Vec<u128> = (0..6).map(|k| retry_delay(k).as_millis()).collect();
    assert_eq!(got, [50, 100, 200, 400, 500, 500]);
    assert_eq!(retry_delay(u32::MAX), Duration::from_millis(500));
}

#[test]
fn watch_hello_negotiates_only_holder_watch() {
    let h = watch_hello(ClientRole::App, "w");
    assert_eq!(h.features, FEAT_HOLDER_WATCH);
    assert_eq!((h.proto_min, h.proto_max), (ipc::PROTO_MIN, ipc::PROTO_MAX));
}

/// holder 감시 비트는 128(postcard varint 0x80 0x01)이다. 삭제 확장(32)과 혼동하면 fixture가 실패한다.
#[test]
fn watch_hello_feature_fixture() {
    let hello = watch_hello(ClientRole::App, "w");
    let fixture = [8, 0, 0, 0, 0x02, 1, 1, 0x80, 0x01, 0, 1, b'w'];
    assert_eq!(hello.encode().unwrap(), fixture);
    let (kind, body) = ipc::read_frame(&mut &fixture[..]).unwrap().unwrap();
    assert_eq!(ClientHello::decode(kind, &body).unwrap(), hello);
    let server = ServerHello {
        proto_min: ipc::PROTO_MIN,
        proto_max: ipc::PROTO_MAX,
        features: ipc::FEATURES,
        build: "test".to_owned(),
        instance_id: 1,
    };
    let negotiated = ipc::negotiate(&server, &hello).unwrap();
    assert_eq!(negotiated.features, 128);
    assert_eq!(
        negotiated.features & crate::control::FEAT_WORKTREE_REMOVE,
        0
    );
    assert_eq!(negotiated.features & crate::control::FEAT_WORKTREE, 0);
}

#[test]
fn holder_status_frozen_fixtures() {
    for (status, byte) in [
        (HolderStatus::Linked, 0),
        (HolderStatus::Orphan, 1),
        (HolderStatus::Standalone, 2),
    ] {
        let fixture = [2, 0, 0, 0, 0x29, byte];
        assert_eq!(status.encode().unwrap(), fixture);
        let (kind, body) = ipc::read_frame(&mut &fixture[..]).unwrap().unwrap();
        assert_eq!(HolderStatus::decode(kind, &body).unwrap(), status);
    }
    assert!(HolderStatus::decode(HOLDER_STATUS, &[3]).is_err());
    assert!(HolderStatus::decode(0, &[0]).is_err());
}

#[test]
fn holder_loss_wakes_watch_without_waiting_for_eof() {
    let (mut client, mut server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    server
        .write_all(&HolderStatus::Orphan.encode().unwrap())
        .unwrap();
    let started = Instant::now();
    wait_closed(&mut client);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "watch only returned on its read timeout"
    );
    // 서버 연결은 여전히 열려 있다: 프레임 자체가 감시를 깨웠다.
    server.write_all(&[0]).unwrap();
}

#[test]
fn old_daemon_watch_needs_no_status_frame() {
    let d = dir("old-watch");
    std::fs::create_dir_all(&d).unwrap();
    let sock = d.join("k.sock");
    let lis = crate::os::net::UnixListener::bind(&sock).unwrap();
    let t = thread::spawn(move || {
        let (mut s, _) = lis.accept().unwrap();
        s.write_all(
            &ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: 0,
                build: "old".to_owned(),
                instance_id: 1,
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
        ipc::read_frame(&mut s).unwrap();
    });
    let c = handshake(&sock, &watch_hello(ClientRole::App, "old"), HELLO_TIMEOUT).unwrap();
    assert_eq!(c.holder_status, None);
    t.join().unwrap();
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[cfg(unix)]
fn preflight_validates_executable() {
    use std::os::unix::fs::PermissionsExt as _;

    const EPERM: i32 = 1;
    const ENOENT: i32 = 2;
    const ENOEXEC: i32 = 8;
    const EACCES: i32 = 13;

    let d = dir("preflight");
    std::fs::create_dir_all(&d).unwrap();

    assert_eq!(
        preflight(&d.join("missing")).unwrap_err().raw_os_error(),
        Some(ENOENT)
    );
    assert_eq!(
        preflight(Path::new("relative/bin"))
            .unwrap_err()
            .raw_os_error(),
        Some(ENOENT)
    );
    assert_eq!(preflight(&d).unwrap_err().raw_os_error(), Some(ENOENT));

    let noexec = d.join("noexec");
    std::fs::write(&noexec, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&noexec, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(preflight(&noexec).unwrap_err().raw_os_error(), Some(EACCES));

    let badfmt = d.join("badfmt");
    std::fs::write(&badfmt, [0xff_u8; 64]).unwrap();
    std::fs::set_permissions(&badfmt, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        preflight(&badfmt).unwrap_err().raw_os_error(),
        Some(ENOEXEC)
    );

    let valid = d.join("true-copy");
    std::fs::copy("/usr/bin/true", &valid).unwrap();
    preflight(&valid).unwrap();

    // 코드 페이지 해시가 깨진 Mach-O는 macOS arm64에서 페이지 폴트 시 커널이 자식을 SIGKILL로 종료한다 -> EPERM.
    let corrupt = d.join("corrupt");
    let mut bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    for b in bytes.iter_mut().skip(8192).step_by(16384) {
        *b ^= 0xff;
    }
    std::fs::write(&corrupt, &bytes).unwrap();
    std::fs::set_permissions(&corrupt, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(preflight(&corrupt).unwrap_err().raw_os_error(), Some(EPERM));

    std::fs::remove_dir_all(d).ok();
}

proptest::proptest! {
    #[test]
    fn holder_status_decoder_never_panics(body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64)) {
        if let Ok(status) = HolderStatus::decode(HOLDER_STATUS, &body) {
            let frame = status.encode().unwrap();
            let (kind, encoded) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
            proptest::prop_assert_eq!(HolderStatus::decode(kind, &encoded).unwrap(), status);
        }
    }
}

#[test]
#[cfg(unix)]
fn paths_resolves_symlink_and_bundle_candidates() {
    use std::os::unix::fs::symlink;

    let d = dir("paths-symlink");
    let state = d.join("state");
    let local_bin = d.join("local-bin");
    let bundle_macos = d.join("Krill.app/Contents/MacOS");
    let bundle_helpers = d.join("Krill.app/Contents/Helpers");
    std::fs::create_dir_all(&local_bin).unwrap();
    std::fs::create_dir_all(&bundle_macos).unwrap();
    std::fs::create_dir_all(&bundle_helpers).unwrap();

    let krill = bundle_macos.join("krill");
    let holder = bundle_helpers.join("krill-holder");
    let krilld = bundle_helpers.join("krilld");
    for f in [&krill, &holder, &krilld] {
        std::fs::write(f, b"").unwrap();
    }
    let link = local_bin.join("krill");
    symlink(&krill, &link).unwrap();

    let resolved = Paths::from_exe(&link, &state, &[]).unwrap();
    assert_eq!(resolved.holder, std::fs::canonicalize(&holder).unwrap());
    assert_eq!(resolved.krilld, std::fs::canonicalize(&krilld).unwrap());

    // 심볼릭 링크가 아닌 복사본이어도 앱 번들 후보가 주어지면 거기서 찾는다.
    let copied = d.join("copied/krill");
    std::fs::create_dir_all(copied.parent().unwrap()).unwrap();
    std::fs::write(&copied, b"").unwrap();
    let from_app = Paths::from_exe(&copied, &state, &[d.join("Krill.app")]).unwrap();
    assert_eq!(from_app.holder, holder);
    assert_eq!(from_app.krilld, krilld);

    // 온보딩 헬퍼 점검(OWE-714): 번들 Contents/MacOS 기준으로 Helpers의 holder·krilld를 찾는다.
    assert_eq!(exe_near(&bundle_macos, "krill"), krill);
    assert_eq!(exe_near(&bundle_macos, "krill-holder"), holder);
    assert_eq!(exe_near(&bundle_macos, "krilld"), krilld);

    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn stopped_daemon_is_detected_without_eof() {
    let (mut client, _stopped_server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let result = wait_responsive(&mut client, FEAT_HEARTBEAT);
    assert!(result.is_err_and(|e| e.kind() == io::ErrorKind::TimedOut));
}

/// 멈춘 데몬 감지 번호는 고정이다(ARCHITECTURE §4.3). 바뀌면 N-1 호환 위반이다.
#[test]
fn heartbeat_numbers_are_frozen() {
    assert_eq!(FEAT_HEARTBEAT, 1 << 45);
    assert_eq!((PING, PONG), (0x43, 0x63));
    assert_eq!(
        heartbeat_hello(ClientRole::App, "w").features,
        (1 << 7) | (1 << 45)
    );
    assert!(is_watch(heartbeat_hello(ClientRole::App, "w").features));
    assert!(is_watch(FEAT_HOLDER_WATCH));
    assert!(!is_watch(FEAT_HOLDER_WATCH | crate::input::FEAT_INPUT));
    assert!(!is_watch(0));
}

/// ping/pong nonce는 그대로 되돌아온다. krilld가 답하는 형식과 같다.
#[test]
fn ping_pong_roundtrip() {
    let frame = ping(7).unwrap();
    let (kind, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
    assert_eq!(ping_nonce(kind, &body).unwrap(), 7);
    let frame = pong(7).unwrap();
    let (kind, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
    assert_eq!(pong_nonce(kind, &body).unwrap(), 7);
    assert!(ping_nonce(PONG, &body).is_err());
    assert!(pong_nonce(PING, &body).is_err());
}

/// 미지원 데몬에는 ping을 보내지 않고 기존 EOF 감시로 동작한다.
#[test]
fn unsupported_daemon_falls_back_to_eof_watch() {
    // Orphan 통보가 오면 응답 있음으로 본다.
    let (mut client, mut server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    server
        .write_all(&HolderStatus::Orphan.encode().unwrap())
        .unwrap();
    wait_responsive(&mut client, FEAT_HOLDER_WATCH).unwrap();
    // 조용히 닫힌 연결도 정상 종료로 본다.
    let (mut client, server) = UnixStream::pair().unwrap();
    drop(server);
    wait_responsive(&mut client, 0).unwrap();
}

/// OWE-771: Cargo 테스트 실행 파일(`deps/`)이나 임시 `HOME`(`HOME=/tmp/...`)에서는
/// 설치본 `/Applications/Krill.app` 또는 `~/Applications/Krill.app`을 후보로 쓰지 않는다.
#[test]
fn paths_dev_excludes_installed_krill_app_in_tests_and_temp_home() {
    assert!(!allow_installed_apps_for_paths(
        Path::new("/Volumes/krill-build/targets/shared/debug/deps/krill_app-1234"),
        Some(Path::new("/Users/me")),
    ));
    assert!(!allow_installed_apps_for_paths(
        Path::new("/usr/local/bin/krill"),
        Some(Path::new("/tmp/kint-home.abc")),
    ));
    assert!(!allow_installed_apps_for_paths(
        Path::new("/usr/local/bin/krill"),
        Some(Path::new("/private/tmp/krill-home-123")),
    ));
    assert!(!allow_installed_apps_for_paths(
        Path::new("/usr/local/bin/krill"),
        Some(&std::env::temp_dir().join("owe728-home")),
    ));
    assert!(allow_installed_apps_for_paths(
        Path::new("/usr/local/bin/krill"),
        Some(Path::new("/Users/me")),
    ));

    let dev = Paths::dev().unwrap();
    assert!(
        !dev.holder.starts_with("/Applications/Krill.app"),
        "test Paths::dev() must not fall back to /Applications/Krill.app: {:?}",
        dev.holder
    );
    assert!(
        !dev.krilld.starts_with("/Applications/Krill.app"),
        "test Paths::dev() must not fall back to /Applications/Krill.app: {:?}",
        dev.krilld
    );
}

#[test]
fn absent_socket_errors_are_platform_specific() {
    assert!(is_absent(&io::Error::from(io::ErrorKind::NotFound)));
    assert!(is_absent(&io::Error::from(
        io::ErrorKind::ConnectionRefused
    )));
    assert_eq!(
        is_absent(&io::Error::from_raw_os_error(10050)),
        cfg!(windows)
    );
    assert!(!is_absent(&io::Error::from(
        io::ErrorKind::PermissionDenied
    )));
}

#[cfg(windows)]
#[test]
fn windows_executable_prefixes_preserve_dos_unc_and_device_paths() {
    for (input, expected) in [
        (
            r"\\?\C:\사용자 이름\Krill\krill.exe",
            r"C:\사용자 이름\Krill\krill.exe",
        ),
        (
            r"\\?\UNC\server\share\Krill\krill.exe",
            r"\\server\share\Krill\krill.exe",
        ),
        (r"\\.\device", r"\\.\device"),
    ] {
        assert_eq!(executable_path(Path::new(input)), Path::new(expected));
    }
}

#[cfg(windows)]
#[test]
fn windows_paths_choose_flat_then_exact_version_holder() {
    let root = dir("windows-holder-candidates");
    std::fs::create_dir_all(&root).unwrap();
    let app = root.join("krill-app.exe");
    let holder = root.join("krill-holder.exe");
    let current = root.join(format!("krill-holder-{}.exe", env!("CARGO_PKG_VERSION")));
    let old = root.join("krill-holder-0.0.0.exe");
    for path in [&app, &current, &old] {
        std::fs::write(path, b"MZ").unwrap();
    }
    let paths = Paths::from_exe(&app, &root.join("state"), &[]).unwrap();
    assert_eq!(paths.holder, executable_path(&current));
    assert!(!paths.holder.to_string_lossy().starts_with(r"\\?\"));
    std::fs::write(&holder, b"MZ").unwrap();
    assert_eq!(exe_near(&root, "krill-holder"), holder);
    std::fs::remove_file(&holder).unwrap();
    std::fs::remove_file(&current).unwrap();
    assert_eq!(exe_near(&root, "krill-holder"), holder);
    std::fs::remove_dir_all(root).unwrap();
}
