//! A fixed client must never invoke the unsafe legacy Windows upgrade path.
use super::*;

fn check_upgrade(name: &str, platform: UpgradePlatform, features: u64, should_upgrade: bool) {
    let root = dir(name);
    std::fs::create_dir_all(&root).unwrap();
    let paths = paths(&root, "/new/holder", "/new/krilld");
    std::fs::create_dir_all(paths.socket.parent().unwrap()).unwrap();
    let listener = crate::os::net::UnixListener::bind(&paths.socket).unwrap();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let server = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features,
            build: "0.1.0".into(),
            instance_id: 1,
        };
        stream.write_all(&server.encode().unwrap()).unwrap();
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        ClientHello::decode(kind, &body).unwrap();
        let next = ipc::read_frame(&mut stream).unwrap();
        if should_upgrade {
            let (kind, body) = next.expect("upgrade request");
            assert!(matches!(
                Request::decode(kind, &body).unwrap(),
                Request::Upgrade(_)
            ));
            stream
                .write_all(&Reply::Upgrade(Upgraded::NotNewer).encode().unwrap())
                .unwrap();
        } else {
            // There must not even be a List snapshot: a subsequent Spawn could
            // invalidate an idle result before the old daemon hands off.
            assert!(
                next.is_none(),
                "legacy runtime must receive no control request"
            );
        }
    });
    assert!(
        upgrade_as_on(&paths, &hello(), "0.2.0", platform)
            .unwrap()
            .is_none()
    );
    peer.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

const LEGACY: u64 = crate::control::FEAT_CONTROL | FEAT_UPGRADE;

#[test]
fn windows_legacy_upgrade_is_always_deferred_without_query_or_handoff() {
    check_upgrade("upgrade-legacy", UpgradePlatform::Windows, LEGACY, false);
}

#[test]
fn windows_safe_daemon_can_upgrade_without_legacy_session_query() {
    check_upgrade(
        "upgrade-safe",
        UpgradePlatform::Windows,
        LEGACY | FEAT_WINDOWS_UPGRADE,
        true,
    );
}

#[test]
fn unix_upgrade_does_not_use_windows_legacy_guard() {
    check_upgrade("upgrade-unix", UpgradePlatform::Other, LEGACY, true);
}

#[test]
fn unsupported_upgrade_does_not_send_any_request() {
    check_upgrade(
        "upgrade-unsupported",
        UpgradePlatform::Other,
        crate::control::FEAT_CONTROL,
        false,
    );
}
