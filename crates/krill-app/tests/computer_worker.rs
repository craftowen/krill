//! M13: exercise the actual pre-GPUI worker entry point and its replay boundary.
//! These operations inspect only protocol capabilities; they never emit desktop input.

#![cfg(target_os = "macos")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test process and bounded-response helpers"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Worker {
    child: Child,
    input: ChildStdin,
    output: Receiver<Value>,
    hello: Value,
}

impl Worker {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_krill-app"))
            .arg("--computer-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        // A guard exists before any assertion so a failed handshake cannot leave an app behind.
        let mut worker = Self {
            child,
            input,
            output,
            hello: Value::Null,
        };
        worker.hello = worker.next();
        assert_eq!(worker.hello["type"], "hello");
        assert_eq!(worker.hello["contract"], "krill-computer-worker/1");
        assert!(
            worker.hello["epoch"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
        assert!(
            worker.hello["context"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
        worker
    }

    fn next(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(10))
            .expect("worker response deadline")
    }

    fn send(&mut self, value: &Value) {
        serde_json::to_writer(&mut self.input, value).unwrap();
        self.input.write_all(b"\n").unwrap();
        self.input.flush().unwrap();
    }

    fn request(&self, seq: u64) -> Value {
        json!({"type":"request", "epoch":self.hello["epoch"],
            "context":self.hello["context"], "seq":seq, "deadlineMs":5000,
            "op":{"kind":"capabilities"}})
    }

    fn run(&mut self, request: &Value) -> Value {
        self.send(request);
        let accepted = self.next();
        assert_eq!(accepted["type"], "accepted", "{accepted}");
        let result = self.next();
        assert_eq!(result["type"], "result", "{result}");
        assert_eq!(result["seq"], request["seq"]);
        result
    }

    fn expect_exit(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success, "{status}");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not exit before deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

#[test]
fn isolated_worker_rejects_foreign_context_and_never_replays_evicted_requests() {
    let mut worker = Worker::start();
    let request = worker.request(1);
    let mut foreign = request.clone();
    foreign["context"] = json!("another-caller");
    worker.send(&foreign);
    let rejected = worker.next();
    assert_eq!(rejected["type"], "rejected");
    assert_eq!(rejected["code"], "foreign_context");

    // Rejection must not reserve somebody else's sequence number.
    worker.run(&request);
    worker.send(&request);
    assert_eq!(worker.next()["type"], "duplicate");

    let mut different = request.clone();
    different["deadlineMs"] = json!(4000);
    worker.send(&different);
    let mismatch = worker.next();
    assert_eq!(mismatch["type"], "rejected");
    assert_eq!(mismatch["code"], "payload_mismatch");

    let capacity = worker.hello["limits"]["ledgerCapacity"].as_u64().unwrap();
    assert!((1..=256).contains(&capacity));
    for seq in 2..=capacity + 2 {
        let request = worker.request(seq);
        worker.run(&request);
    }
    worker.send(&request);
    let expired = worker.next();
    assert_eq!(expired["type"], "rejected");
    assert_eq!(expired["code"], "expired");
    worker.send(&json!({"type":"status", "seq":1}));
    let status = worker.next();
    assert_eq!(status["type"], "status");
    assert_eq!(status["state"], "expired");

    worker.send(&json!({"type":"shutdown"}));
    worker.expect_exit(true);
}

#[test]
fn isolated_worker_rejects_malformed_input_without_opening_gpui() {
    let mut worker = Worker::start();
    worker.input.write_all(b"{invalid-json}\n").unwrap();
    worker.input.flush().unwrap();
    assert_eq!(worker.next()["type"], "protocol_error");
    worker.expect_exit(false);
}
