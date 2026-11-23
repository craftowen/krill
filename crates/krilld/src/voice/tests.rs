use super::*;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
struct RecordedRequest {
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

struct MockPipito {
    port: u16,
    token: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockPipito {
    fn start(
        token: &str,
        handler: impl Fn(&RecordedRequest) -> (u16, String) + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reqs_bg = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let expected_auth = format!("Bearer {token}");
        let handler = Arc::new(handler);

        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS/BSD inherits O_NONBLOCK from listener; switch to blocking before reading.
                        stream.set_nonblocking(false).ok();
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                        let mut raw = Vec::new();
                        let mut buf = [0u8; 4096];
                        let mut content_len = None;
                        let mut header_end = None;
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    raw.extend_from_slice(&buf[..n]);
                                    if header_end.is_none()
                                        && let Some(pos) =
                                            raw.windows(4).position(|w| w == b"\r\n\r\n")
                                    {
                                        header_end = Some(pos + 4);
                                        let hstr = String::from_utf8_lossy(&raw[..pos]);
                                        for line in hstr.lines().skip(1) {
                                            if let Some((k, v)) = line.split_once(':')
                                                && k.trim().eq_ignore_ascii_case("content-length")
                                            {
                                                content_len = v.trim().parse::<usize>().ok();
                                            }
                                        }
                                    }
                                    if let Some(hend) = header_end {
                                        let want = content_len.unwrap_or(0);
                                        if raw.len().saturating_sub(hend) >= want {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        let Some(hend) = header_end else {
                            continue;
                        };
                        let head_str = String::from_utf8_lossy(&raw[..hend - 4]).to_string();
                        let mut lines = head_str.split("\r\n");
                        let first = lines.next().unwrap_or("");
                        let mut parts = first.split_whitespace();
                        let _method = parts.next().unwrap_or("");
                        let path = parts.next().unwrap_or("").to_owned();
                        let mut headers = HashMap::new();
                        for line in lines {
                            if let Some((k, v)) = line.split_once(':') {
                                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
                            }
                        }
                        let body = String::from_utf8_lossy(&raw[hend..]).to_string();
                        let rec = RecordedRequest {
                            path,
                            headers: headers.clone(),
                            body,
                        };
                        reqs_bg.lock().unwrap().push(rec.clone());
                        let (status, resp_body) =
                            if headers.get("authorization").map(String::as_str)
                                == Some(expected_auth.as_str())
                            {
                                handler(&rec)
                            } else {
                                (401, r#"{"ok":false,"error":"unauthorized"}"#.to_owned())
                            };
                        let resp = format!(
                            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                            resp_body.len()
                        );
                        stream.write_all(resp.as_bytes()).ok();
                        stream.flush().ok();
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            token: token.to_owned(),
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn write_bridge_file(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let json = serde_json::json!({
            "version": 1,
            "pid": std::process::id(),
            "port": self.port,
            "token": self.token,
            "started_at_ms": 1_763_900_000_000_u64,
        });
        fs::write(path, json.to_string()).unwrap();
    }
}

impl Drop for MockPipito {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "krill-voice-test-{name}-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn bridge_health_states_ready_loading_not_running_and_not_installed() {
    let dir = temp_dir("states");
    let bridge_path = dir.join("bridge.json");

    // 1. NotInstalled (override file does not exist)
    let svc = VoiceService::with_bridge_override(Some(&dir), Some(bridge_path.clone()));
    let st = svc.status();
    assert_eq!(st.status, VoiceConnectionStatus::NotInstalled);
    assert!(!st.available);

    // 2. NotRunning (bridge.json exists but port closed)
    fs::write(
        &bridge_path,
        r#"{"version":1,"pid":99999,"port":1,"token":"deadbeef","started_at_ms":1}"#,
    )
    .unwrap();
    let st = svc.status();
    assert_eq!(st.status, VoiceConnectionStatus::NotRunning);
    assert!(!st.available);

    // 3. Loading (engine_ready: false)
    let mock_loading = MockPipito::start("tok-loading", |req| {
        assert_eq!(req.path, "/local/health");
        assert!(!req.headers.contains_key("origin"));
        (
            200,
            r#"{"ok":true,"version":1,"app_version":"1.4.0","engine_ready":false,"engine_error":null,"refine_available":true}"#
                .to_owned(),
        )
    });
    mock_loading.write_bridge_file(&bridge_path);
    let st = svc.status();
    assert_eq!(st.status, VoiceConnectionStatus::Loading);
    assert!(!st.available);
    assert_eq!(st.app_version.as_deref(), Some("1.4.0"));
    drop(mock_loading);

    // 4. Ready (engine_ready: true)
    let mock_ready = MockPipito::start("tok-ready", |req| {
        assert!(!req.headers.contains_key("origin"));
        (
            200,
            r#"{"ok":true,"version":1,"app_version":"1.5.0","engine_ready":true,"engine_error":null,"refine_available":true}"#
                .to_owned(),
        )
    });
    mock_ready.write_bridge_file(&bridge_path);
    let st = svc.status();
    assert_eq!(st.status, VoiceConnectionStatus::Ready);
    assert!(st.available);
    assert!(st.engine_ready);
    assert!(st.refine_available);
    assert!(st.refine_enabled);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn bridge_transcribe_single_and_chunked_with_context_vocab_and_refine_toggle() {
    let dir = temp_dir("transcribe");
    let bridge_path = dir.join("bridge.json");
    let mock = MockPipito::start("secret-token-64", |req| match req.path.as_str() {
        "/local/warm" => (200, r#"{"ok":true}"#.to_owned()),
        "/local/transcribe" => {
            assert!(!req.headers.contains_key("origin"));
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
            let refine = body["refine"].as_bool().unwrap();
            let app = body["app"].as_str().unwrap();
            assert_eq!(app, "Krill");
            (
                200,
                serde_json::json!({
                    "ok": true,
                    "silence": false,
                    "raw": "raw text",
                    "text": if refine { "Refined text." } else { "raw text" },
                    "refined": refine,
                    "refine_error": null,
                    "asr_ms": 180,
                    "refine_ms": if refine { 90 } else { 0 },
                })
                .to_string(),
            )
        }
        "/local/learn" => (
            200,
            r#"{"ok":true,"learned":[["크릴","Krill"]]}"#.to_owned(),
        ),
        _ => (404, r#"{"ok":false}"#.to_owned()),
    });
    mock.write_bridge_file(&bridge_path);

    let svc = VoiceService::with_bridge_override(Some(&dir), Some(bridge_path));
    svc.config_set(VoiceConfigPatch {
        add_pinned: Some("Pipito".to_owned()),
        ignore_word: Some("ignoredTerm".to_owned()),
        ..VoiceConfigPatch::default()
    })
    .unwrap();
    svc.observe_prompt(
        VoiceTargetKind::Session,
        "42",
        "Check pwa_rpc.rs and ignoredTerm for E2EE framing",
        None,
    );

    // Single-frame transcribe with refine ON
    let reply = svc
        .transcribe(TranscribeInput {
            audio_wav: b"RIFF0000WAVE".to_vec(),
            target_kind: Some(VoiceTargetKind::Session),
            target_id: Some("42"),
            refine_override: None,
            upload_id: None,
            chunk_index: 0,
            total_chunks: 1,
        })
        .unwrap();
    match reply {
        VoiceReply::Transcribed(res) => {
            assert!(res.refined);
            assert_eq!(res.text, "Refined text.");
            assert_eq!(res.asr_ms, 180);
        }
        other => panic!("unexpected reply: {other:?}"),
    }

    // Verify request body had surrounding, app_category="code", Pipito, pwa_rpc.rs, and NOT ignoredTerm
    {
        let reqs = mock.requests.lock().unwrap();
        let tr = reqs.iter().find(|r| r.path == "/local/transcribe").unwrap();
        let body: serde_json::Value = serde_json::from_str(&tr.body).unwrap();
        assert_eq!(body["app_category"], "code");
        assert!(body["surrounding"].as_str().unwrap().contains("pwa_rpc.rs"));
        let vocab: Vec<String> = body["vocab"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert!(vocab.iter().any(|w| w == "Pipito"));
        assert!(vocab.iter().any(|w| w == "pwa_rpc.rs"));
        assert!(!vocab.iter().any(|w| w.eq_ignore_ascii_case("ignoredTerm")));
    }

    // Multi-chunk transcribe with refine OFF override and bot_chat category="chat"
    let first = svc
        .transcribe(TranscribeInput {
            audio_wav: b"RIFF11".to_vec(),
            target_kind: Some(VoiceTargetKind::BotChat),
            target_id: Some("bot1:chat1"),
            refine_override: Some(false),
            upload_id: Some("upload-99"),
            chunk_index: 0,
            total_chunks: 2,
        })
        .unwrap();
    assert_eq!(
        first,
        VoiceReply::ChunkAccepted {
            upload_id: "upload-99".to_owned(),
            received: 1,
            total: 2,
        }
    );
    let second = svc
        .transcribe(TranscribeInput {
            audio_wav: b"22WAVE".to_vec(),
            target_kind: Some(VoiceTargetKind::BotChat),
            target_id: Some("bot1:chat1"),
            refine_override: Some(false),
            upload_id: Some("upload-99"),
            chunk_index: 1,
            total_chunks: 2,
        })
        .unwrap();
    match second {
        VoiceReply::Transcribed(res) => {
            assert!(!res.refined);
            assert_eq!(res.text, "raw text");
        }
        other => panic!("unexpected reply: {other:?}"),
    }
    {
        let reqs = mock.requests.lock().unwrap();
        let last_tr = reqs
            .iter()
            .rev()
            .find(|r| r.path == "/local/transcribe")
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&last_tr.body).unwrap();
        assert_eq!(body["app_category"], "chat");
        assert_eq!(body["refine"], false);
        assert_eq!(
            bridge::base64_decode(body["audio_wav_b64"].as_str().unwrap()).unwrap(),
            b"RIFF1122WAVE"
        );
    }

    // Feedback boosts auto_vocab immediately and fires /local/learn asynchronously
    let fb = svc.feedback("크릴 데몬 확인", "krilld 확인", None, None);
    assert!(fb.boosted.iter().any(|w| w == "krilld"));
    let cfg = svc.config_get();
    assert!(
        cfg.auto_vocab
            .iter()
            .any(|e| e.word == "krilld" && e.count >= 3)
    );

    // Wait briefly for async /local/learn request
    for _ in 0..40 {
        if mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.path == "/local/learn")
        {
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.path == "/local/learn")
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn bridge_rejects_bad_auth_token() {
    let dir = temp_dir("auth");
    let bridge_path = dir.join("bridge.json");
    let mock = MockPipito::start("expected-token", |_| (200, r#"{"ok":true}"#.to_owned()));
    // Write WRONG token into bridge.json
    fs::write(
        &bridge_path,
        serde_json::json!({
            "version": 1,
            "pid": 1,
            "port": mock.port,
            "token": "wrong-token",
            "started_at_ms": 1,
        })
        .to_string(),
    )
    .unwrap();
    let svc = VoiceService::with_bridge_override(Some(&dir), Some(bridge_path));
    let err = svc
        .transcribe(TranscribeInput {
            audio_wav: b"RIFFWAVE".to_vec(),
            target_kind: None,
            target_id: None,
            refine_override: None,
            upload_id: None,
            chunk_index: 0,
            total_chunks: 1,
        })
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn vocab_extractor_filters_noise_and_keeps_high_signal_tokens() {
    let text = "Check https://example.com/foo and 9f8a7b6c5d4e3f2a 123456 in crates/krilld/src/mobile/pwa_rpc.rs for E2EE Tailscale camelCaseToken snake_case_id 피피토에서";
    let tokens = vocab::extract_vocab_tokens(text);
    assert!(tokens.iter().any(|t| t == "pwa_rpc.rs"));
    assert!(tokens.iter().any(|t| t == "E2EE"));
    assert!(tokens.iter().any(|t| t == "Tailscale"));
    assert!(tokens.iter().any(|t| t == "camelCaseToken"));
    assert!(tokens.iter().any(|t| t == "snake_case_id"));
    assert!(tokens.iter().any(|t| t == "피피토"));
    assert!(!tokens.iter().any(|t| t.contains("https://")));
    assert!(!tokens.iter().any(|t| t == "9f8a7b6c5d4e3f2a"));
    assert!(!tokens.iter().any(|t| t == "123456"));
}

#[test]
fn context_cache_strips_ansi_code_fences_json_and_caps_utf16() {
    let cache = context::ContextCache::new();
    let dirty = "\x1b[32mHello\x1b[0m\n```rust\nfn huge_fn() {}\n```\n{\"tool\":\"raw_json\"}\nFix pwa_rpc.rs now";
    cache.push_turn(VoiceTargetKind::Session, "9", "user", dirty, None);
    let snap = cache.snapshot(Some(VoiceTargetKind::Session), Some("9"));
    assert_eq!(snap.app_category, "code");
    assert!(!snap.surrounding.contains("\x1b["));
    assert!(!snap.surrounding.contains("raw_json"));
    assert!(snap.surrounding.contains("[code]"));
    assert!(snap.surrounding.contains("pwa_rpc.rs"));
    assert!(snap.surrounding.encode_utf16().count() <= context::MAX_SURROUNDING_UTF16);
    assert!(snap.session_vocab.iter().any(|w| w == "pwa_rpc.rs"));

    // Huge turn still strictly <= 550 UTF-16 units
    let huge = "가나다라마바사😀".repeat(200);
    cache.push_turn(VoiceTargetKind::GroupChat, "g1", "user", &huge, None);
    cache.push_turn(VoiceTargetKind::GroupChat, "g1", "assistant", &huge, None);
    cache.push_turn(VoiceTargetKind::GroupChat, "g1", "user", &huge, None);
    let snap2 = cache.snapshot(Some(VoiceTargetKind::GroupChat), Some("g1"));
    assert_eq!(snap2.app_category, "chat");
    assert!(snap2.surrounding.encode_utf16().count() <= context::MAX_SURROUNDING_UTF16);
}

#[test]
fn base64_roundtrip() {
    for sample in [
        b"".as_slice(),
        b"f",
        b"fo",
        b"foo",
        b"foobar",
        b"\x00\xff\x10\x80",
    ] {
        let enc = bridge::base64_encode(sample);
        let dec = bridge::base64_decode(&enc).unwrap();
        assert_eq!(dec, sample);
    }
}
