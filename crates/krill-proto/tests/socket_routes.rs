//! Inventory of production defaults, plus an isolated Windows override/live IPC contract.
#[test]
fn production_socket_defaults_use_the_shared_resolver() {
    let callers = [
        (include_str!("../../krill/src/hook_windows.rs"), 1),
        (include_str!("../../krill/src/orca_import.rs"), 2),
        (include_str!("../src/orchestration.rs"), 1),
        (include_str!("../../krill-app/src/automations.rs"), 1),
        (include_str!("../../krill-app/src/settings/schedules.rs"), 2),
        (include_str!("../../krill-app/src/orca_import.rs"), 1),
        (include_str!("../../krill-link/src/serve.rs"), 1),
    ];
    for (source, count) in callers {
        // Inline fixtures may intentionally listen at the legacy endpoint.
        let production = source.split("\n#[cfg(test)]\nmod ").next().unwrap();
        assert_eq!(production.matches("daemon::socket_path(").count(), count);
        assert!(!production.contains("join(\"run/krilld.sock\")"));
        assert!(!production.contains("join(\"run\").join(\"krilld.sock\")"));
    }
}

#[cfg(windows)]
#[test]
fn relocated_socket_contract_subprocess() {
    use std::process::Command;
    let root = std::env::temp_dir().join(format!("ks-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "relocated_socket_child", "--nocapture"])
        .env("KRILL_SOCKET_TEST_CHILD", "1")
        .env("KRILL_SOCKET_DIR", &root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "relocated_socket_child", "--nocapture"])
        .env("KRILL_SOCKET_TEST_CHILD", "invalid")
        .env("KRILL_SOCKET_DIR", "relative")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn relocated_socket_child() {
    use krill_proto::{
        control::{Failure, Reply, Request},
        daemon, ipc,
        orchestration::{OrchAction, OrchestrationStore},
        screen::Msg as _,
    };
    use std::{io::Write as _, path::Path, time::Duration};
    let Ok(mode) = std::env::var("KRILL_SOCKET_TEST_CHILD") else {
        return;
    };
    let state = Path::new(r"C:\long-state-profile 한글").join("long".repeat(100));
    let action = OrchAction::Reset { run_id: None };
    if mode == "invalid" {
        assert_eq!(
            daemon::socket_path(&state).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            OrchestrationStore::try_local_daemon_orch(&state, &action, Duration::from_secs(2))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        return;
    }
    let endpoint = daemon::socket_path(&state).unwrap();
    assert!(endpoint.starts_with(std::env::var_os("KRILL_SOCKET_DIR").unwrap()));
    assert_ne!(endpoint, daemon::socket_path(&state.join("other")).unwrap());
    let listener = krill_proto::os::net::UnixListener::bind(&endpoint).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(
                &ipc::ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES,
                    build: "relocated-test".into(),
                    instance_id: 1,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        ipc::read_frame(&mut stream).unwrap().unwrap();
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        assert_eq!(
            Request::decode(kind, &body).unwrap(),
            Request::Orchestration(OrchAction::Reset { run_id: None })
        );
        stream
            .write_all(&Reply::Failed(Failure::BadRequest).encode().unwrap())
            .unwrap();
    });
    assert!(
        OrchestrationStore::try_local_daemon_orch(&state, &action, Duration::from_secs(2)).is_err()
    );
    server.join().unwrap();
    std::fs::remove_file(endpoint).unwrap();
}
