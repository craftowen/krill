use super::transport::{self, Endpoint};
use super::*;
#[cfg(unix)]
use krill_proto::error_reporting::MAX_REQUEST_BYTES;
use krill_proto::error_reporting::{Code, Consent, ENDPOINT, Event, Process};
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// 테스트마다 따로 쓰는 임시 디렉터리. `state()`는 아직 없는 상태 디렉터리 경로다.
struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "krill-errors-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn consent(state: &Path) -> Consent {
    reporting::enable(state).unwrap();
    reporting::consent_with(state, &Env::stub(&[]))
        .unwrap()
        .unwrap()
}

fn record(state: &Path, consent: &Consent, n: usize) {
    for _ in 0..n {
        let event = Event::new(Process::Cli, Code::DaemonConnectFailed, None).unwrap();
        assert!(reporting::record_with_consent(state, event, consent).unwrap());
    }
}

fn ack_all(body: &[u8]) -> Reply {
    let request: reporting::Request = serde_json::from_slice(body).unwrap();
    let ids: Vec<String> = request.events.into_iter().map(|e| e.event_id).collect();
    Reply::Http {
        status: 200,
        retry_after_secs: None,
        body: serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "accepted_ids": ids,
            "duplicate_ids": [],
        }))
        .unwrap(),
    }
}

#[cfg(unix)]
fn fake_curl(dir: &Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn help_and_usage_errors() {
    for argv in [
        vec!["--help"],
        vec!["-h"],
        vec!["help"],
        vec!["flush", "--help"],
        vec!["status", "--json", "-h"],
    ] {
        let raw: Vec<OsString> = argv.iter().map(OsString::from).collect();
        assert_eq!(run(&raw), ExitCode::SUCCESS, "{argv:?}");
    }
    // 상태를 바꾸기 전에 끝나는 잘못된 호출만 실제 run으로 확인한다.
    for argv in [
        vec![],
        vec!["nope"],
        vec!["status", "--bogus"],
        vec!["status", "--endpoint", "https://example.com/v1/events"],
        vec!["flush", "--endpoint"],
        vec!["flush", "--endpoint", "http://example.com/v1/events"],
    ] {
        let raw: Vec<OsString> = argv.iter().map(OsString::from).collect();
        assert_eq!(run(&raw), ExitCode::from(2), "{argv:?}");
    }
}

#[test]
fn endpoint_accepts_https_and_explicit_loopback_http_only() {
    for ok in [
        ENDPOINT,
        "https://example.com:8443/v1/events",
        "http://127.0.0.1:8787/v1/events",
        "http://localhost:8787/v1/events",
        "http://[::1]:8787/v1/events",
    ] {
        assert!(Endpoint::parse(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "https://",
        "http://errors.krill.page/v1/events",
        "http://127.0.0.1.evil.example/v1/events",
        "http://localhost.evil.example/v1/events",
        "ftp://errors.krill.page/v1/events",
        "file:///etc/passwd",
        "https://user:pw@errors.krill.page/v1/events",
        "https://errors.krill.page/v1/events?x=1",
        "https://errors.krill.page/v1/events#x",
        "https://errors.krill.page/v1/ events",
        "https://errors.krill.page/v1/events\n",
        "errors.krill.page/v1/events",
    ] {
        assert!(Endpoint::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn curl_arguments_keep_tls_and_refuse_redirects() {
    let https = Endpoint::parse(ENDPOINT).unwrap();
    let args = transport::args(&https);
    assert_eq!(
        args.first(),
        Some(&"--disable"),
        "curlrc를 무시하려면 첫 인자여야 한다"
    );
    assert!(args.windows(2).any(|w| w == ["--proto", "=https"]));
    assert!(args.windows(2).any(|w| w == ["--data-binary", "@-"]));
    for banned in [
        "-k",
        "--insecure",
        "-L",
        "--location",
        "--location-trusted",
        "-K",
        "--config",
    ] {
        assert!(!args.contains(&banned), "{banned}");
    }
    assert_eq!(args.last(), Some(&ENDPOINT));
    let local = Endpoint::parse("http://127.0.0.1:9/v1/events").unwrap();
    assert!(
        transport::args(&local)
            .windows(2)
            .any(|w| w == ["--proto", "=http"])
    );
    assert!(transport::system_curl().is_absolute());
}

#[test]
fn response_parsing_uses_the_final_header_block() {
    let http = |status, retry_after_secs, body: &[u8]| Reply::Http {
        status,
        retry_after_secs,
        body: body.to_vec(),
    };
    assert_eq!(
        transport::parse(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 429\r\nretry-after: 120\r\ncontent-type: application/json\r\n\r\n{\"x\":1}\n429"
        ),
        Some(http(429, Some(120), b"{\"x\":1}"))
    );
    assert_eq!(
        transport::parse(
            b"HTTP/1.1 200 Connection established\r\nRetry-After: 5\r\n\r\nHTTP/1.1 503 Service Unavailable\r\n\r\n\n503"
        ),
        Some(http(503, None, b"")),
        "프록시 블록의 Retry-After는 쓰지 않는다"
    );
    for (value, secs) in [
        ("Wed, 21 Oct 2026 07:28:00 GMT", None),
        ("99999999999", None),
        ("-1", None),
        (" 30 ", Some(30)),
    ] {
        let out = format!("HTTP/1.1 429 Too Many Requests\r\nRetry-After:{value}\r\n\r\n\n429");
        assert_eq!(
            transport::parse(out.as_bytes()),
            Some(http(429, secs, b"")),
            "{value}"
        );
    }
    assert_eq!(transport::parse(b"{}\n200"), Some(http(200, None, b"{}")));
    for bad in [
        &b""[..],
        b"200",
        b"x\n000",
        b"x\nabc",
        b"HTTP/1.1 200 OK\r\nno-terminator\n200",
    ] {
        assert_eq!(transport::parse(bad), None, "{bad:?}");
    }
}

#[cfg(unix)]
#[test]
fn post_sends_the_body_on_stdin_and_reads_status_and_retry_after() {
    let temp = Temp::new();
    let args_log = temp.0.join("args");
    let body_file = temp.0.join("body");
    let curl = fake_curl(
        &temp.0,
        "curl",
        &format!(
            "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\ncat > '{}'\nprintf 'HTTP/1.1 429 Too Many Requests\\r\\nRetry-After: 7\\r\\n\\r\\n{{}}\\n429'\n",
            args_log.display(),
            body_file.display()
        ),
    );
    let endpoint = Endpoint::parse("https://errors.example/v1/events").unwrap();
    // 파이프 버퍼보다 큰 본문도 막히지 않는다.
    let body = vec![b'x'; MAX_REQUEST_BYTES];
    assert_eq!(
        transport::post(&curl, &endpoint, &body, Duration::from_secs(10)),
        Reply::Http {
            status: 429,
            retry_after_secs: Some(7),
            body: b"{}".to_vec(),
        }
    );
    assert_eq!(fs::read(&body_file).unwrap(), body);
    let args = fs::read_to_string(&args_log).unwrap();
    assert_eq!(args.lines().next(), Some("--disable"));
    assert_eq!(
        args.lines().last(),
        Some("https://errors.example/v1/events")
    );
}

#[cfg(unix)]
#[test]
fn hung_failing_or_oversized_curl_is_a_transport_failure() {
    let temp = Temp::new();
    let endpoint = Endpoint::parse(ENDPOINT).unwrap();
    let hung = fake_curl(&temp.0, "hung", "cat > /dev/null\nexec sleep 30\n");
    let started = Instant::now();
    assert_eq!(
        transport::post(&hung, &endpoint, b"{}", Duration::from_millis(300)),
        Reply::Transport
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    for (name, script) in [
        ("failing", "cat > /dev/null\nexit 7\n"),
        ("no-status", "cat > /dev/null\nprintf '\\n000'\n"),
        (
            "oversized",
            "cat > /dev/null\nhead -c 200000 /dev/zero\nprintf '\\n200'\n",
        ),
    ] {
        let curl = fake_curl(&temp.0, name, script);
        assert_eq!(
            transport::post(&curl, &endpoint, b"{}", Duration::from_secs(10)),
            Reply::Transport,
            "{name}"
        );
    }
    assert_eq!(
        transport::post(
            &temp.0.join("missing"),
            &endpoint,
            b"{}",
            Duration::from_secs(1)
        ),
        Reply::Transport
    );
}

#[cfg(unix)]
#[test]
fn flush_round_trips_through_curl_and_empties_the_queue() {
    let temp = Temp::new();
    let state = temp.state();
    let consent = consent(&state);
    record(&state, &consent, 3);
    let curl = fake_curl(
        &temp.0,
        "curl",
        r#"ids=$(grep -o '"event_id":"[0-9a-f]*"' | sed 's/"event_id"://' | paste -sd, -)
printf 'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{"schema_version":1,"accepted_ids":[%s],"duplicate_ids":[]}\n200' "$ids"
"#,
    );
    let endpoint = Endpoint::parse("http://127.0.0.1:9/v1/events").unwrap();
    let mut collected = 0;
    let report = run_flush(
        &state,
        &Env::stub(&[]),
        |_| {
            collected += 1;
            Ok(0)
        },
        |body| transport::post(&curl, &endpoint, body, Duration::from_secs(10)),
    )
    .unwrap();
    assert_eq!(collected, 1);
    assert_eq!(
        (
            report.outcome,
            report.requests,
            report.accepted,
            report.pending
        ),
        (FlushOutcome::Sent, 1, 3, 0)
    );
    assert_eq!(
        reporting::status_with(&state, &Env::stub(&[]))
            .unwrap()
            .pending,
        0
    );
}

#[test]
fn flush_without_consent_neither_collects_nor_sends() {
    let temp = Temp::new();
    let state = temp.state();
    let report = run_flush(
        &state,
        &Env::stub(&[]),
        |_| panic!("collected without consent"),
        |_| panic!("sent without consent"),
    )
    .unwrap();
    assert_eq!(report.outcome, FlushOutcome::NotConsented);
    assert!(!state.exists(), "동의 없이 상태 디렉터리를 만들지 않는다");
    let consent = consent(&state);
    record(&state, &consent, 1);
    let report = run_flush(
        &state,
        &Env::stub(&[("DO_NOT_TRACK", "1")]),
        |_| panic!("collected under veto"),
        |_| panic!("sent under veto"),
    )
    .unwrap();
    assert_eq!(
        (report.outcome, report.pending),
        (FlushOutcome::NotConsented, 1)
    );
}

#[test]
fn concurrent_flush_is_single_flight() {
    let temp = Temp::new();
    let state = temp.state();
    let consent = consent(&state);
    record(&state, &consent, 1);
    let held = UploadLock::try_acquire(&state).unwrap().unwrap();
    let report = run_flush(
        &state,
        &Env::stub(&[]),
        |_| panic!("collected while another upload runs"),
        |_| panic!("sent while another upload runs"),
    )
    .unwrap();
    assert_eq!((report.outcome, report.pending), (FlushOutcome::Busy, 1));
    drop(held);
    let report = run_flush(&state, &Env::stub(&[]), |_| Ok(0), ack_all).unwrap();
    assert_eq!((report.outcome, report.accepted), (FlushOutcome::Sent, 1));
}

#[test]
fn native_collection_runs_once_before_upload_and_failure_does_not_block_it() {
    let temp = Temp::new();
    let state = temp.state();
    let consent = consent(&state);
    record(&state, &consent, 1);
    let order = RefCell::new(Vec::new());
    let report = run_flush(
        &state,
        &Env::stub(&[]),
        |dir| {
            order.borrow_mut().push("collect");
            // 수집기가 같은 flush 안에서 넣은 이벤트도 이번 전송에 실린다.
            record(dir, &consent, 1);
            Err(io::Error::other("unreadable report"))
        },
        |body| {
            order.borrow_mut().push("send");
            ack_all(body)
        },
    )
    .unwrap();
    assert_eq!(*order.borrow(), ["collect", "send"]);
    assert_eq!(
        (report.outcome, report.accepted, report.pending),
        (FlushOutcome::Sent, 2, 0)
    );
}

#[test]
fn failed_upload_reports_deferred_with_retry_time() {
    let temp = Temp::new();
    let state = temp.state();
    let consent = consent(&state);
    record(&state, &consent, 2);
    let report = run_flush(&state, &Env::stub(&[]), |_| Ok(0), |_| Reply::Transport).unwrap();
    assert_eq!(
        (report.outcome, report.requests, report.pending),
        (FlushOutcome::Deferred, 1, 2)
    );
    let next = report.next_attempt_at_ms.unwrap();
    assert!(next >= reporting::now_ms() + 59_000);
    assert!(describe(&report).contains("upload incomplete; 2 pending; retrying in"));
    let report = run_flush(
        &state,
        &Env::stub(&[]),
        |_| Ok(0),
        |_| panic!("sent during backoff"),
    )
    .unwrap();
    assert_eq!(report.outcome, FlushOutcome::Backoff);
    assert_eq!(report.next_attempt_at_ms, Some(next));
}
