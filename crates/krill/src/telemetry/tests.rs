use super::*;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::time::Duration;

#[test]
fn help_and_invalid_arguments() {
    for arg in ["--help", "-h", "help"] {
        assert_eq!(run(&[arg.into()]), ExitCode::SUCCESS);
    }
    assert_eq!(
        run(&["status".into(), "--unknown".into()]),
        ExitCode::from(2)
    );
    assert_eq!(run(&["unknown".into()]), ExitCode::from(2));
}
#[test]
fn endpoint_validation_and_curl_limits() {
    for url in [
        "https://telemetry.krill.page/v1/telemetry",
        "http://127.0.0.1:123/v1/telemetry",
        "http://localhost:123/v1/telemetry",
        "http://[::1]:123/v1/telemetry",
    ] {
        assert!(transport::Endpoint::parse(url).is_ok());
    }
    for url in [
        "http://example.com/v1/telemetry",
        "file:///private",
        "https://user:pass@example.com/",
        "https://example.com/?q=secret",
        "https://example.com/#x",
        "https://example.com/\n",
        "http://localhost.evil/",
    ] {
        assert!(transport::Endpoint::parse(url).is_err(), "{url}");
    }
    let endpoint = transport::Endpoint::parse(telemetry::ENDPOINT).unwrap();
    let args = transport::args(&endpoint);
    assert_eq!(args[0], "--disable");
    assert!(args.contains(&"--max-time"));
    assert!(args.contains(&"--connect-timeout"));
    assert!(!args.contains(&"-L"));
    assert!(!args.contains(&"-k"));
}
#[test]
fn real_loopback_flush_uses_closed_batch_and_exact_ack() {
    let dir = std::env::temp_dir().join(format!(
        "krill-telemetry-http-{}",
        telemetry::random_uuid().unwrap()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    telemetry::record_open(&dir, &Env::default(), 1_793_000_000).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = transport::Endpoint::parse(&format!(
        "http://{}/v1/telemetry",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let (start, length) = loop {
            let n = stream.read(&mut buffer).unwrap();
            assert_ne!(n, 0);
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = std::str::from_utf8(&bytes[..pos]).unwrap();
                assert!(head.starts_with("POST /v1/telemetry HTTP/1.1"));
                assert!(
                    head.to_ascii_lowercase()
                        .contains("content-type: application/json")
                );
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|v| v.parse().unwrap())
                    })
                    .unwrap();
                break (pos + 4, length);
            }
        };
        while bytes.len() < start + length {
            let n = stream.read(&mut buffer).unwrap();
            assert_ne!(n, 0);
            bytes.extend_from_slice(&buffer[..n]);
        }
        let batch: telemetry::Batch =
            serde_json::from_slice(&bytes[start..start + length]).unwrap();
        assert!(batch.valid());
        assert_eq!(batch.events.len(), 2);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"accepted\":2}").unwrap();
        batch
    });
    let report =
        telemetry::flush_with(
            &dir,
            &Env::default(),
            1_793_000_000,
            |body| match transport::post(
                &transport::system_curl(),
                &endpoint,
                body,
                Duration::from_secs(5),
            ) {
                transport::Reply::Http { status, body } => Ok((status, body)),
                transport::Reply::Transport => Err(io::Error::other("upload failed")),
            },
        )
        .unwrap();
    let batch = server.join().unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(report.outcome, Outcome::Sent);
    assert_eq!(report.accepted, 2);
    assert_eq!(report.pending, 0);
    let status = telemetry::status_with(&dir, &Env::default()).unwrap();
    let json = serde_json::to_value(status).unwrap();
    assert_eq!(json["last_outcome"], "sent");
    assert_eq!(json["last_success_unix"], 1_793_000_000_u64);
    std::fs::remove_dir_all(dir).ok();
}
#[cfg(unix)]
#[test]
fn timeout_and_output_cap_kill_and_reap_helper() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!(
        "krill-telemetry-curl-{}",
        telemetry::random_uuid().unwrap()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = transport::Endpoint::parse(telemetry::ENDPOINT).unwrap();
    let fake = dir.join("curl");
    for script in [
        "#!/bin/sh\nexec /bin/sleep 10\n",
        "#!/bin/sh\nexec /usr/bin/yes X\n",
    ] {
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(
            transport::post(&fake, &endpoint, b"{}", Duration::from_millis(100)),
            transport::Reply::Transport
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    std::fs::remove_dir_all(dir).ok();
}
proptest::proptest! {
    #[test]
    fn arbitrary_responses_do_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..8192)) {
        drop(transport::parse(&bytes));
    }
}
