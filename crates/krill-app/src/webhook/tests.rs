use krill_proto::SessionId;
use std::io::{self, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

#[derive(Debug)]
pub(crate) struct Request {
    pub headers: String,
    pub body: String,
}

fn request(stream: &mut TcpStream) -> Request {
    // macOS can inherit O_NONBLOCK from the listener. Header bytes may arrive
    // after accept, so use blocking reads with a bounded timeout explicitly.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut raw = Vec::new();
    let header_end = loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        raw.push(byte[0]);
        if raw.ends_with(b"\r\n\r\n") {
            break raw.len();
        }
        assert!(raw.len() < 8192);
    };
    let headers = String::from_utf8(raw).unwrap();
    let len: usize = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    assert!(header_end < 8192);
    let mut body = vec![0; len];
    stream.read_exact(&mut body).unwrap();
    Request {
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

#[test]
fn initially_nonblocking_stream_waits_for_delayed_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    // Make the inherited macOS flag explicit so the regression is portable.
    stream.set_nonblocking(true).unwrap();
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        client
            .write_all(b"POST /topic HTTP/1.1\r\nContent-Length: 4\r\n\r\ndone")
            .unwrap();
    });
    assert_eq!(request(&mut stream).body, "done");
    writer.join().unwrap();
}

pub(crate) fn server(codes: Vec<u16>) -> (String, thread::JoinHandle<(TcpListener, Vec<Request>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/topic?token=secret",
        listener.local_addr().unwrap()
    );
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for code in codes {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing webhook POST");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => panic!("local HTTP server: {e}"),
                }
            };
            requests.push(request(&mut stream));
            if code == 0 {
                continue;
            }
            let location = if code == 302 {
                "Location: /redirected\r\n"
            } else {
                ""
            };
            write!(
                stream,
                "HTTP/1.1 {code} Test\r\n{location}Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
        (listener, requests)
    });
    (url, handle)
}

pub(crate) fn finish(handle: thread::JoinHandle<(TcpListener, Vec<Request>)>) -> Vec<Request> {
    let (listener, requests) = handle.join().unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    requests
}

#[test]
fn provider_posts_contain_only_fixed_completion_metadata() {
    for provider in ["ntfy", "telegram", "discord"] {
        let (url, handle) = server(vec![204]);
        let endpoint = match provider {
            "ntfy" => Endpoint::Ntfy { url },
            "telegram" => Endpoint::Telegram {
                url,
                chat_id: "-100123\"\\\n한글".into(),
            },
            _ => Endpoint::Discord { url },
        };
        deliver(&endpoint, SessionId(42)).unwrap();
        let requests = finish(handle);
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert!(
            req.headers
                .starts_with("POST /topic?token=secret HTTP/1.1\r\n")
        );
        let message = "Krill: session #42 completed.";
        match endpoint {
            Endpoint::Ntfy { .. } => {
                assert!(
                    req.headers
                        .contains("Content-Type: text/plain; charset=utf-8")
                );
                assert_eq!(req.body, message);
            }
            Endpoint::Telegram { chat_id, .. } => {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({"chat_id": chat_id, "text": message})
                );
            }
            Endpoint::Discord { .. } => {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({"content": message, "allowed_mentions": {"parse": []}})
                );
            }
        }
    }
}

#[test]
fn failed_post_retries_once_and_stops_after_success_or_second_failure() {
    for codes in [vec![503, 204], vec![500, 500], vec![302, 302]] {
        let success = codes.last() == Some(&204);
        let (url, handle) = server(codes);
        let result = deliver(&Endpoint::Ntfy { url }, SessionId(1));
        assert_eq!(result.is_ok(), success);
        let requests = finish(handle);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body, requests[1].body);
        assert!(requests.iter().all(|req| {
            req.headers
                .starts_with("POST /topic?token=secret HTTP/1.1\r\n")
        }));
    }
}

#[test]
fn transport_failure_retries_once() {
    // Drop each connection without an HTTP response.
    let (url, handle) = server(vec![0, 0]);
    assert!(deliver(&Endpoint::Ntfy { url }, SessionId(1)).is_err());
    assert_eq!(finish(handle).len(), 2);
}

#[test]
fn url_validation_and_debug_never_expose_credentials() {
    for url in [
        "file:///etc/passwd",
        "ftp://example.org/topic",
        "http://example.org/topic",
        "http://127.0.0.1:80@evil.example/topic",
        "https://user:secret@example.org/",
        "https://example.org/#secret",
        "https://example.org/\nurl=evil",
    ] {
        let error = validated_url(url).unwrap_err().to_string();
        assert_eq!(error, "invalid webhook URL");
    }
    for url in [
        "https://ntfy.sh/private",
        "http://127.0.0.1:1234/topic",
        "http://[::1]:1234/topic",
    ] {
        assert!(validated_url(url).is_ok());
    }
    let endpoint = Endpoint::Telegram {
        url: "https://api.telegram.org/botSECRET/sendMessage".into(),
        chat_id: "PRIVATE".into(),
    };
    let debug = format!("{endpoint:?}");
    assert!(!debug.contains("SECRET"));
    assert!(!debug.contains("PRIVATE"));
}

#[test]
fn provider_transitions_include_only_state_id_and_mobile_link() {
    for state in [
        krill_proto::control::AgentState::Waiting,
        krill_proto::control::AgentState::Blocked,
        krill_proto::control::AgentState::Done,
    ] {
        for provider in ["ntfy", "telegram", "discord"] {
            let (url, handle) = server(vec![204]);
            let endpoint = match provider {
                "ntfy" => Endpoint::Ntfy { url },
                "telegram" => Endpoint::Telegram {
                    url,
                    chat_id: "42".into(),
                },
                _ => Endpoint::Discord { url },
            };
            deliver_transition(
                &endpoint,
                SessionId(51),
                state,
                Some("https://mobile.example/?session=51"),
            )
            .unwrap();
            let requests = finish(handle);
            assert!(
                requests[0]
                    .body
                    .contains("https://mobile.example/?session=51")
            );
            assert!(!requests[0].body.contains("pair="));
            assert!(!requests[0].body.contains("key="));
        }
    }
}
