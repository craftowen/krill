//! Adversarial policy tests over real pipes with a scripted backend (no AX, no input).

use std::io::{BufReader, PipeReader, Read as _};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use serde_json::{Value, json};

use super::*;
use crate::platform::computer::protocol::{Coverage, MAX_BATCH_STEPS, MAX_FRAME_BYTES};

const WAIT: Duration = Duration::from_secs(5);

/// What the scripted backend did, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Prepared(String),
    Dispatched(String),
}

struct Script {
    events: Sender<Event>,
    /// When set, `dispatch` blocks until a unit arrives (simulates a hung AX call).
    hold: Option<Receiver<()>>,
    delivery: Delivery,
    refuse_prepare: bool,
    /// Size of the string an `apps` read returns (0 = small answer).
    blob: usize,
}

fn op_name(op: &Op) -> String {
    serde_json::to_value(op).unwrap()["kind"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Batch scripts pick a step's behaviour by its target element; anything else uses the harness defaults.
fn element(op: &Op) -> &str {
    match op {
        Op::Action { target, .. }
        | Op::SetValue { target, .. }
        | Op::Click { target }
        | Op::TypeText { target, .. } => &target.element,
        _ => "",
    }
}

impl Backend for Script {
    type Prepared = Op;
    fn supported(&self) -> bool {
        true
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec!["observe", "action"]
    }
    fn unsupported(&self) -> Vec<&'static str> {
        vec!["screenshot"]
    }
    fn read(&mut self, op: &Op, _deadline: Instant) -> Result<ReadResult, Refusal> {
        match op {
            Op::Observe { pid: 13, .. } => Err(Refusal::new("accessibility_not_granted", "no")),
            Op::Apps if self.blob > 0 => Ok(ReadResult {
                observation: Observation::NotRequested,
                data: json!({"blob": "x".repeat(self.blob)}),
            }),
            _ => Ok(ReadResult {
                observation: Observation::Observed {
                    snapshot: "E/C/1".into(),
                    coverage: Coverage::default(),
                },
                data: json!({"op": op_name(op)}),
            }),
        }
    }
    fn prepare(&mut self, op: &Op, _deadline: Instant) -> Result<Op, Refusal> {
        self.events.send(Event::Prepared(op_name(op))).unwrap();
        if self.refuse_prepare || element(op) == "gone" {
            return Err(Refusal::new("element_gone", "gone"));
        }
        Ok(op.clone())
    }
    fn dispatch(&mut self, op: Op, _deadline: Instant) -> Dispatched {
        self.events.send(Event::Dispatched(op_name(&op))).unwrap();
        if let Some(hold) = &self.hold {
            hold.recv().ok();
        }
        let mut out = Dispatched {
            delivery: self.delivery.clone(),
            verification: Verification::NotRequested,
            cleanup: Cleanup::NotNeeded,
            data: None,
        };
        match (element(&op), &op) {
            ("unknown", _) => {
                out.delivery = Delivery::Unknown {
                    reason: "ax_timeout".into(),
                };
            }
            ("refused", _) => {
                out.delivery = Delivery::NotDispatched {
                    reason: "action_unsupported".into(),
                };
            }
            ("partial", _) => {
                out.delivery = Delivery::Partial {
                    issued_units: 2,
                    reason: "focus_changed".into(),
                };
                out.cleanup = Cleanup::Completed;
            }
            ("sat", _) => {
                out.verification = Verification::Satisfied {
                    predicate: "value_equals".into(),
                };
            }
            ("unsat", _) => {
                out.verification = Verification::Unsatisfied {
                    predicate: "value_equals".into(),
                };
            }
            ("dirty", _) => {
                out.cleanup = Cleanup::Incomplete {
                    reason: "key_up_failed".into(),
                };
            }
            // A down/up pair per UTF-16 unit, like the real typing path.
            (_, Op::TypeText { text, .. }) => {
                out.delivery = Delivery::Submitted {
                    issued_units: 2 * u32::try_from(text.encode_utf16().count()).unwrap(),
                };
                out.cleanup = Cleanup::Completed;
            }
            _ => {}
        }
        out
    }
}

struct Harness {
    input: Option<std::io::PipeWriter>,
    frames: Receiver<Value>,
    /// Worker stdout when the test plays a consumer that does not read.
    stdout: Option<BufReader<PipeReader>>,
    events: Receiver<Event>,
    release: Option<Sender<()>>,
    join: Option<JoinHandle<u8>>,
    lock_path: PathBuf,
}

#[derive(Default)]
struct Options {
    hold: bool,
    delivery: Option<Delivery>,
    refuse_prepare: bool,
    blob: usize,
    /// `false`: a reader thread parses every frame (optionally slowly). `true`: nobody reads stdout.
    unread: bool,
    read_delay: Duration,
    write_stall: Option<Duration>,
}

impl Harness {
    fn start(name: &str, options: Options) -> Self {
        let (in_read, in_write) = std::io::pipe().unwrap();
        let (out_read, out_write) = std::io::pipe().unwrap();
        let (events_tx, events) = channel();
        let (release, hold) = if options.hold {
            let (tx, rx) = channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let dir = std::env::temp_dir().join(format!(
            "krill-computer-worker-test-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let lock_path = dir.join("input.lock");
        let backend = Script {
            events: events_tx,
            hold,
            delivery: options
                .delivery
                .unwrap_or(Delivery::Submitted { issued_units: 1 }),
            refuse_prepare: options.refuse_prepare,
            blob: options.blob,
        };
        let config = Config {
            lock_path: lock_path.clone(),
            write_stall: options.write_stall.unwrap_or(WRITE_STALL),
        };
        let join = std::thread::spawn(move || {
            serve(
                BufReader::new(in_read),
                out_write,
                Identity {
                    epoch: "E".into(),
                    context: "C".into(),
                },
                config,
                backend,
            )
        });
        let (frames_tx, frames) = channel();
        let mut stdout = BufReader::new(out_read);
        let stdout = if options.unread {
            let mut hello = String::new();
            stdout.read_line(&mut hello).unwrap();
            frames_tx
                .send(serde_json::from_str::<Value>(&hello).unwrap())
                .unwrap();
            Some(stdout)
        } else {
            let delay = options.read_delay;
            std::thread::spawn(move || {
                for line in stdout.lines() {
                    let Ok(line) = line else { break };
                    std::thread::sleep(delay);
                    if frames_tx
                        .send(serde_json::from_str::<Value>(&line).unwrap())
                        .is_err()
                    {
                        break;
                    }
                }
            });
            None
        };
        let mut harness = Self {
            input: Some(in_write),
            frames,
            stdout,
            events,
            release,
            join: Some(join),
            lock_path,
        };
        let hello = harness.next();
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["contract"], CONTRACT);
        harness
    }

    /// Writes `count` status queries from another thread; returns once all were written or stdin closed.
    /// Finishing within the test deadline proves the reader neither hung on stdout nor stopped settling.
    fn flood_status(&mut self, seq: u64, count: usize) {
        let mut input = self.input.take().unwrap();
        let line = format!("{{\"type\":\"status\",\"seq\":{seq}}}\n");
        let input = within(move || {
            for _ in 0..count {
                if input.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
            input
        });
        self.input = Some(input);
    }

    fn exit_code(&mut self) -> u8 {
        let join = self.join.take().unwrap();
        within(move || join.join().unwrap())
    }

    /// Reads whatever reached the unread stdout. Bytes end on a frame boundary and every line is JSON.
    fn unread_frames(&mut self) -> Vec<Value> {
        let mut stdout = self.stdout.take().unwrap();
        let bytes = within(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        assert!(
            bytes.is_empty() || bytes.ends_with(b"\n"),
            "a frame was cut while the worker was still alive"
        );
        bytes
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }

    fn remove_lock_dir(&self) {
        if let Some(dir) = self.lock_path.parent() {
            std::fs::remove_dir_all(dir).ok();
        }
    }

    fn raw(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().unwrap();
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
    }

    fn send(&mut self, frame: &Value) {
        let mut line = serde_json::to_vec(frame).unwrap();
        line.push(b'\n');
        self.raw(&line);
    }

    fn request(&mut self, seq: u64, op: &Value) {
        self.send(&json!({"type":"request","epoch":"E","context":"C","seq":seq,"deadlineMs":5000,"op":op}));
    }

    fn next(&mut self) -> Value {
        self.frames.recv_timeout(WAIT).expect("worker frame")
    }

    fn event(&mut self) -> Event {
        self.events.recv_timeout(WAIT).expect("backend event")
    }

    fn no_more_events(&mut self) {
        assert!(
            self.events.try_recv().is_err(),
            "unexpected backend side effect"
        );
    }

    fn release(&mut self) {
        self.release.as_ref().unwrap().send(()).unwrap();
    }

    /// Whether another thread of this process (own open file description) is shut out of the input lock.
    fn lock_held(&self) -> bool {
        lock::acquire(
            &self.lock_path,
            Instant::now() + Duration::from_millis(30),
            || true,
        )
        .is_err()
    }

    /// The same question from another process (`lockf(1)` takes a BSD `flock`, `-k` keeps the file).
    fn lock_held_by_process(&self) -> bool {
        let status = std::process::Command::new("/usr/bin/lockf")
            .args(["-k", "-s", "-t", "0"])
            .arg(&self.lock_path)
            .arg("/usr/bin/true")
            .status()
            .unwrap();
        // 75 = EX_TEMPFAIL: the lock is taken.
        match status.code() {
            Some(0) => false,
            Some(75) => true,
            other => panic!("lockf exited {other:?}"),
        }
    }

    /// Closes stdin and returns the exit code plus every remaining frame.
    fn finish(mut self) -> (u8, Vec<Value>) {
        drop(self.input.take());
        // Unblock any held dispatch so the executor can drain.
        drop(self.release.take());
        let code = self.join.take().unwrap().join().unwrap();
        let mut rest = Vec::new();
        while let Ok(frame) = self.frames.recv_timeout(WAIT) {
            rest.push(frame);
        }
        self.remove_lock_dir();
        (code, rest)
    }
}

/// Runs `f` on another thread and fails the test instead of hanging if it does not finish in time.
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = channel();
    std::thread::spawn(move || tx.send(f()).ok());
    rx.recv_timeout(WAIT)
        .expect("blocked past the test deadline")
}

fn click() -> Value {
    json!({"kind":"click","target":{"snapshot":"E/C/1","element":"e1"}})
}

#[test]
fn read_request_round_trip() {
    let mut h = Harness::start("read", Options::default());
    h.request(1, &json!({"kind":"capabilities"}));
    assert_eq!(h.next(), json!({"type":"accepted","seq":1}));
    let result = h.next();
    assert_eq!(result["type"], "result");
    assert_eq!(result["state"], "completed");
    assert_eq!(result["delivery"]["state"], "read_only");
    assert_eq!(result["data"]["capabilities"], json!(["observe", "action"]));
    assert_eq!(result["data"]["quiescing"], false);
    assert_eq!(result["data"]["limits"]["maxBatchSteps"], MAX_BATCH_STEPS);

    h.request(2, &json!({"kind":"observe","pid":13}));
    h.next();
    let refused = h.next();
    assert_eq!(refused["observation"]["state"], "failed");
    assert_eq!(refused["error"]["code"], "accessibility_not_granted");
    let (code, _) = h.finish();
    assert_eq!(code, 0);
}

#[test]
fn duplicate_while_dispatching_never_reruns_and_controls_answer_during_hang() {
    let mut h = Harness::start(
        "dup",
        Options {
            hold: true,
            ..Options::default()
        },
    );
    h.request(1, &click());
    assert_eq!(h.next()["type"], "accepted");
    assert_eq!(h.event(), Event::Prepared("click".into()));
    assert_eq!(h.event(), Event::Dispatched("click".into()));
    // Backend is now blocked inside dispatch; the reader must still answer.
    h.request(1, &click());
    assert_eq!(
        h.next(),
        json!({"type":"duplicate","seq":1,"state":"dispatching"})
    );
    h.send(&json!({"type":"status","seq":1}));
    assert_eq!(
        h.next(),
        json!({"type":"status","seq":1,"state":"dispatching"})
    );
    h.send(&json!({"type":"cancel","seq":1}));
    assert_eq!(
        h.next(),
        json!({"type":"cancel","seq":1,"outcome":"too_late"})
    );
    // Same seq, different payload.
    h.request(
        1,
        &json!({"kind":"click","target":{"snapshot":"E/C/1","element":"e2"}}),
    );
    assert_eq!(h.next()["code"], "payload_mismatch");
    h.release();
    let result = h.next();
    assert_eq!(result["state"], "completed");
    assert_eq!(result["delivery"]["issuedUnits"], 1);
    h.request(1, &click());
    assert_eq!(
        h.next(),
        json!({"type":"duplicate","seq":1,"state":"completed"})
    );
    h.no_more_events();
    h.finish();
}

#[test]
fn cancel_and_deadline_before_dispatch_have_no_side_effects() {
    let mut h = Harness::start(
        "cancel",
        Options {
            hold: true,
            ..Options::default()
        },
    );
    h.request(1, &click());
    h.next();
    h.event();
    h.event();
    h.request(2, &click());
    assert_eq!(h.next()["type"], "accepted");
    h.send(
        &json!({"type":"request","epoch":"E","context":"C","seq":3,"deadlineMs":1,"op":click()}),
    );
    assert_eq!(h.next()["type"], "accepted");
    h.send(&json!({"type":"cancel","seq":2}));
    assert_eq!(
        h.next(),
        json!({"type":"cancel","seq":2,"outcome":"cancelled"})
    );
    std::thread::sleep(Duration::from_millis(20));
    h.release();
    assert_eq!(h.next()["seq"], 1);
    let cancelled = h.next();
    assert_eq!(cancelled["seq"], 2);
    assert_eq!(cancelled["state"], "not_dispatched");
    assert_eq!(cancelled["delivery"]["reason"], "cancelled");
    let late = h.next();
    assert_eq!(late["seq"], 3);
    assert_eq!(late["delivery"]["reason"], "deadline_exceeded");
    h.no_more_events();
    h.finish();
}

#[test]
fn receipt_rejections_do_not_reserve_or_execute() {
    let mut h = Harness::start("reject", Options::default());
    let base = |seq: u64| json!({"type":"request","epoch":"E","context":"C","seq":seq,"deadlineMs":100,"op":{"kind":"apps"}});
    let mut stale = base(1);
    stale["epoch"] = json!("OLD");
    h.send(&stale);
    assert_eq!(h.next()["code"], "stale_epoch");
    let mut foreign = base(1);
    foreign["context"] = json!("OTHER");
    h.send(&foreign);
    assert_eq!(h.next()["code"], "foreign_context");
    let mut unbounded = base(1);
    unbounded["deadlineMs"] = json!(MAX_DEADLINE_MS + 1);
    h.send(&unbounded);
    assert_eq!(h.next()["code"], "invalid_request");
    h.send(&json!({"type":"request","epoch":"E","context":"C","seq":1,"deadlineMs":100,"op":{"kind":"screenshot"}}));
    let unknown_op = h.next();
    assert_eq!(unknown_op["code"], "invalid_request");
    assert_eq!(unknown_op["seq"], 1);
    // Valid JSON outside the contract keeps the connection; only non-JSON is fatal.
    h.raw(b"[1,2]\n");
    let garbage = h.next();
    assert_eq!(garbage["code"], "invalid_request");
    assert!(garbage.get("seq").is_none());
    // None of the above consumed seq 1.
    h.send(&base(1));
    assert_eq!(h.next()["type"], "accepted");
    h.next();
    h.send(&base(0));
    assert_eq!(h.next()["code"], "seq_out_of_range");
    h.send(&base(2 + crate::platform::computer::protocol::MAX_SEQ_GAP));
    assert_eq!(h.next()["code"], "seq_out_of_range");
    h.send(&base(5));
    h.next();
    h.next();
    h.send(&base(4));
    assert_eq!(h.next()["code"], "expired");
    h.send(&json!({"type":"status","seq":4}));
    assert_eq!(h.next()["state"], "expired");
    h.send(&json!({"type":"status","seq":6}));
    assert_eq!(h.next()["state"], "unseen");
    h.finish();
}

#[test]
fn full_queue_rejects_busy_and_seq_stays_spent() {
    let mut h = Harness::start(
        "busy",
        Options {
            hold: true,
            ..Options::default()
        },
    );
    h.request(1, &click());
    h.next();
    h.event();
    h.event();
    for seq in 2..=u64::try_from(QUEUE_CAPACITY).unwrap() + 1 {
        h.request(seq, &click());
        assert_eq!(h.next()["type"], "accepted", "seq {seq}");
    }
    let overflow = u64::try_from(QUEUE_CAPACITY).unwrap() + 2;
    h.request(overflow, &click());
    assert_eq!(h.next()["code"], "busy");
    h.request(overflow, &click());
    assert_eq!(
        h.next(),
        json!({"type":"duplicate","seq":overflow,"state":"not_dispatched"})
    );
    // Drain: each queued click dispatches once.
    for _ in 1..=QUEUE_CAPACITY + 1 {
        h.release.as_ref().unwrap().send(()).unwrap();
    }
    let (code, rest) = h.finish();
    assert_eq!(code, 0);
    let results: Vec<_> = rest.iter().filter(|f| f["type"] == "result").collect();
    assert_eq!(results.len(), QUEUE_CAPACITY + 1);
}

#[test]
fn eof_settles_queued_requests_without_dispatch() {
    let mut h = Harness::start(
        "eof",
        Options {
            hold: true,
            ..Options::default()
        },
    );
    h.request(1, &click());
    h.next();
    h.event();
    h.event();
    h.request(2, &click());
    h.next();
    drop(h.input.take());
    // The reader settles queued work on EOF; give it time before the executor is unblocked.
    std::thread::sleep(Duration::from_millis(100));
    let (code, rest) = h.finish();
    assert_eq!(code, 0);
    let second = rest.iter().find(|f| f["seq"] == 2).unwrap();
    assert_eq!(second["state"], "not_dispatched");
}

#[test]
fn unknown_delivery_quiesces_mutations_but_not_reads() {
    let mut h = Harness::start(
        "quiesce",
        Options {
            delivery: Some(Delivery::Unknown {
                reason: "ax_timeout".into(),
            }),
            ..Options::default()
        },
    );
    h.request(1, &click());
    h.next();
    let first = h.next();
    assert_eq!(first["state"], "unknown");
    h.event();
    h.event();
    h.request(2, &click());
    h.next();
    let refused = h.next();
    assert_eq!(refused["delivery"]["reason"], "executor_quiescing");
    h.no_more_events();
    // The quiescing executor still holds the desktop lock.
    let other = lock::acquire(
        &h.lock_path,
        Instant::now() + Duration::from_millis(30),
        || true,
    );
    assert_eq!(other.unwrap_err(), lock::LockError::Timeout);
    h.request(3, &json!({"kind":"capabilities"}));
    h.next();
    let caps = h.next();
    assert_eq!(caps["state"], "completed");
    assert_eq!(caps["data"]["quiescing"], true);
    h.finish();
}

#[test]
fn held_lock_times_out_as_not_dispatched() {
    let mut h = Harness::start("locked", Options::default());
    let held = lock::acquire(
        &h.lock_path,
        Instant::now() + Duration::from_millis(100),
        || true,
    )
    .unwrap();
    h.send(
        &json!({"type":"request","epoch":"E","context":"C","seq":1,"deadlineMs":50,"op":click()}),
    );
    h.next();
    let result = h.next();
    assert_eq!(result["delivery"]["reason"], "lock_timeout");
    h.no_more_events();
    drop(held);
    h.finish();
}

#[test]
fn revalidation_refusal_is_not_dispatched() {
    let mut h = Harness::start(
        "refuse",
        Options {
            refuse_prepare: true,
            ..Options::default()
        },
    );
    h.request(1, &click());
    h.next();
    let result = h.next();
    assert_eq!(result["state"], "not_dispatched");
    assert_eq!(result["error"]["code"], "element_gone");
    assert_eq!(h.event(), Event::Prepared("click".into()));
    h.no_more_events();
    h.finish();
}

#[test]
fn oversized_frame_is_fatal_protocol_error() {
    let mut h = Harness::start("oversize", Options::default());
    h.raw(&vec![b' '; MAX_FRAME_BYTES + 1]);
    let error = h.next();
    assert_eq!(error["type"], "protocol_error");
    let (code, _) = h.finish();
    assert_eq!(code, 2);
}

#[test]
fn non_json_is_fatal_protocol_error() {
    let mut h = Harness::start("garbage", Options::default());
    h.raw(b"{invalid-json}\n");
    assert_eq!(h.next()["type"], "protocol_error");
    let (code, _) = h.finish();
    assert_eq!(code, 2);
}

#[test]
fn nonreading_consumer_fails_transport_while_dispatch_hangs_and_runs_nothing_more() {
    let mut h = Harness::start(
        "nonreading",
        Options {
            hold: true,
            unread: true,
            write_stall: Some(Duration::from_millis(300)),
            ..Options::default()
        },
    );
    h.request(1, &click());
    assert_eq!(h.event(), Event::Prepared("click".into()));
    assert_eq!(h.event(), Event::Dispatched("click".into()));
    h.request(2, &click());
    // ~48 bytes per answer: far beyond the pipe buffer plus the bounded outbox. The reader backs off, stdin
    // fills, and once the stuck write passes `write_stall` the transport is declared dead and stdin is closed.
    h.flood_status(1, 5_000);
    // The hung AX call returns; the worker must stop instead of dispatching seq 2 or blocking on stdout.
    h.release();
    assert_eq!(h.exit_code(), EXIT_TRANSPORT);
    h.no_more_events();
    let frames = h.unread_frames();
    assert_eq!(frames[0]["type"], "accepted");
    assert!(
        frames
            .iter()
            .all(|f| f["seq"] != 2 || f["type"] == "accepted")
    );
    h.remove_lock_dir();
}

#[test]
fn disappeared_consumer_fails_transport_without_further_dispatch() {
    let mut h = Harness::start(
        "gone",
        Options {
            hold: true,
            unread: true,
            ..Options::default()
        },
    );
    h.request(1, &click());
    h.event();
    h.event();
    drop(h.stdout.take());
    // The next answer hits EPIPE.
    h.request(2, &click());
    std::thread::sleep(Duration::from_millis(100));
    h.release();
    assert_eq!(h.exit_code(), EXIT_TRANSPORT);
    h.no_more_events();
    h.remove_lock_dir();
}

#[test]
fn stalled_write_without_more_input_is_fatal_and_frames_stay_whole() {
    let mut h = Harness::start(
        "stall",
        Options {
            unread: true,
            blob: 256 * 1024,
            write_stall: Some(Duration::from_millis(200)),
            ..Options::default()
        },
    );
    h.request(1, &json!({"kind":"apps"}));
    // Nothing else arrives on stdin: the executor's idle check alone must notice the stuck write.
    assert_eq!(h.exit_code(), EXIT_TRANSPORT);
    let frames = h.unread_frames();
    assert_eq!(frames[0]["type"], "accepted");
    assert_eq!(frames[1]["type"], "result");
    assert_eq!(
        frames[1]["data"]["blob"].as_str().unwrap().len(),
        256 * 1024
    );
}

#[test]
fn slow_consumer_gets_every_answer_while_dispatch_hangs() {
    let mut h = Harness::start(
        "slow",
        Options {
            hold: true,
            read_delay: Duration::from_micros(200),
            ..Options::default()
        },
    );
    h.request(1, &click());
    h.next();
    h.event();
    h.event();
    // More answers than the outbox holds: a slow reader gets backpressure, never a dropped frame.
    for _ in 0..2_000 {
        h.send(&json!({"type":"status","seq":1}));
    }
    for _ in 0..2_000 {
        assert_eq!(
            h.next(),
            json!({"type":"status","seq":1,"state":"dispatching"})
        );
    }
    h.release();
    assert_eq!(h.next()["state"], "completed");
    let (code, _) = h.finish();
    assert_eq!(code, 0);
}

#[test]
fn shutdown_exits_cleanly() {
    let mut h = Harness::start("shutdown", Options::default());
    h.send(&json!({"type":"shutdown"}));
    let code = h.join.take().unwrap().join().unwrap();
    assert_eq!(code, 0);
}

fn step(kind: &str, element: &str) -> Value {
    let mut op = json!({"kind":kind,"target":{"snapshot":"E/C/1","element":element}});
    match kind {
        "type_text" | "set_value" => op["text"] = json!("abc"),
        "action" => op["name"] = json!("AXPress"),
        _ => {}
    }
    op
}

fn batch(steps: &[Value]) -> Value {
    json!({"kind":"batch","steps":steps})
}

fn result_of(h: &mut Harness, seq: u64) -> Value {
    loop {
        let frame = h.next();
        if frame["type"] == "result" && frame["seq"] == seq {
            return frame;
        }
    }
}

#[test]
fn batch_holds_one_lock_across_fresh_prepares_and_sums_units() {
    let mut h = Harness::start(
        "batch-lock",
        Options {
            hold: true,
            ..Options::default()
        },
    );
    h.request(
        1,
        &batch(&[
            step("click", "e1"),
            step("type_text", "e1"),
            step("action", "e1"),
        ]),
    );
    assert_eq!(h.next()["type"], "accepted");
    for kind in ["click", "type_text", "action"] {
        assert_eq!(h.event(), Event::Prepared(kind.into()));
        assert_eq!(h.event(), Event::Dispatched(kind.into()));
        // Held inside this step's dispatch: nobody else, in-process or not, can take the desktop.
        assert!(h.lock_held(), "{kind}");
        assert!(h.lock_held_by_process(), "{kind}");
        h.send(&json!({"type":"status","seq":1}));
        assert_eq!(
            h.next(),
            json!({"type":"status","seq":1,"state":"dispatching"})
        );
        h.release();
    }
    let result = h.next();
    assert_eq!(result["state"], "completed");
    // Units are primitive events (1 + 2*3 + 1), not steps.
    assert_eq!(
        result["delivery"],
        json!({"state":"submitted","issuedUnits":8})
    );
    assert_eq!(result["cleanup"]["state"], "completed");
    assert!(result.get("error").is_none());
    assert_eq!(result["data"]["issuedSteps"], 3);
    assert_eq!(result["data"]["steps"][1]["delivery"]["issuedUnits"], 6);
    assert!(!h.lock_held(), "lock released after a clean batch");
    h.no_more_events();
    h.finish();
}

/// One stopping step at index 1 of three: what the result says and whether the executor stays usable.
#[test]
fn batch_stops_at_each_stop_point_without_retry() {
    struct Case {
        element: &'static str,
        dispatched: bool,
        state: &'static str,
        delivery: Value,
        step_code: &'static str,
        quiesces: bool,
    }
    let cases = [
        Case {
            element: "gone",
            dispatched: false,
            state: "completed",
            delivery: json!({"state":"partial","issuedUnits":1,"reason":"element_gone"}),
            step_code: "element_gone",
            quiesces: false,
        },
        Case {
            element: "refused",
            dispatched: true,
            state: "completed",
            delivery: json!({"state":"partial","issuedUnits":1,"reason":"action_unsupported"}),
            step_code: "action_unsupported",
            quiesces: false,
        },
        Case {
            element: "partial",
            dispatched: true,
            state: "completed",
            delivery: json!({"state":"partial","issuedUnits":3,"reason":"focus_changed"}),
            step_code: "focus_changed",
            quiesces: false,
        },
        Case {
            element: "unsat",
            dispatched: true,
            state: "completed",
            delivery: json!({"state":"partial","issuedUnits":2,"reason":"verification_unsatisfied"}),
            step_code: "verification_unsatisfied",
            quiesces: false,
        },
        Case {
            element: "dirty",
            dispatched: true,
            state: "unknown",
            delivery: json!({"state":"partial","issuedUnits":2,"reason":"cleanup_incomplete"}),
            step_code: "cleanup_incomplete",
            quiesces: true,
        },
        Case {
            element: "unknown",
            dispatched: true,
            state: "unknown",
            delivery: json!({"state":"unknown","reason":"ax_timeout"}),
            step_code: "ax_timeout",
            quiesces: true,
        },
    ];
    for case in cases {
        let mut h = Harness::start(&format!("batch-stop-{}", case.element), Options::default());
        h.request(
            1,
            &batch(&[
                step("click", "e1"),
                step("click", case.element),
                step("click", "e1"),
            ]),
        );
        let result = result_of(&mut h, 1);
        let name = case.element;
        assert_eq!(result["state"], case.state, "{name}");
        assert_eq!(result["delivery"], case.delivery, "{name}");
        assert_eq!(result["error"]["code"], BATCH_STOPPED, "{name}");
        let steps = result["data"]["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 3, "{name}");
        assert_eq!(steps[1]["error"]["code"], case.step_code, "{name}");
        assert_eq!(
            steps[2]["delivery"],
            json!({"state":"not_dispatched","reason":BATCH_STOPPED}),
            "{name}"
        );
        // An unknown delivery makes the count of steps that really went out unknown too.
        let issued = match name {
            "unknown" => Value::Null,
            "gone" | "refused" => json!(1),
            _ => json!(2),
        };
        assert_eq!(result["data"]["issuedSteps"], issued, "{name}");
        assert_eq!(
            result["cleanup"]["state"],
            if name == "dirty" {
                "incomplete"
            } else if name == "partial" {
                "completed"
            } else {
                "not_needed"
            },
            "{name}"
        );
        if name == "unsat" {
            assert_eq!(result["verification"]["state"], "unsatisfied");
        }
        // Step 0 ran once; step 1 was prepared once and dispatched at most once; step 2 never started.
        assert_eq!(h.event(), Event::Prepared("click".into()), "{name}");
        assert_eq!(h.event(), Event::Dispatched("click".into()), "{name}");
        assert_eq!(h.event(), Event::Prepared("click".into()), "{name}");
        if case.dispatched {
            assert_eq!(h.event(), Event::Dispatched("click".into()), "{name}");
        }
        h.no_more_events();
        assert_eq!(h.lock_held(), case.quiesces, "{name}");
        h.request(2, &click());
        let next = result_of(&mut h, 2);
        if case.quiesces {
            assert_eq!(next["delivery"]["reason"], "executor_quiescing", "{name}");
            h.no_more_events();
        } else {
            assert_eq!(next["state"], "completed", "{name}");
        }
        h.finish();
    }
}

#[test]
fn batch_refused_before_any_input_is_not_dispatched() {
    let mut h = Harness::start("batch-first", Options::default());
    h.request(1, &batch(&[step("click", "gone"), step("click", "e1")]));
    let result = result_of(&mut h, 1);
    assert_eq!(result["state"], "not_dispatched");
    assert_eq!(result["delivery"]["reason"], "element_gone");
    assert_eq!(result["error"]["code"], "element_gone");
    assert_eq!(result["data"]["issuedSteps"], 0);
    assert_eq!(h.event(), Event::Prepared("click".into()));
    h.no_more_events();
    h.finish();
}

#[test]
fn batch_duplicate_has_no_extra_effects_and_status_keeps_bounded_summary() {
    let mut h = Harness::start("batch-dup", Options::default());
    let steps = batch(&[step("type_text", "e1"), step("set_value", "e1")]);
    h.request(1, &steps);
    let result = result_of(&mut h, 1);
    assert_eq!(result["state"], "completed");
    for _ in 0..4 {
        h.event();
    }
    h.request(1, &steps);
    assert_eq!(
        h.next(),
        json!({"type":"duplicate","seq":1,"state":"completed"})
    );
    h.request(1, &batch(&[step("type_text", "e1")]));
    assert_eq!(h.next()["code"], "payload_mismatch");
    h.send(&json!({"type":"status","seq":1}));
    let status = h.next();
    assert_eq!(
        status,
        json!({"type":"status","seq":1,"state":"completed","summary":{
            "delivery":"submitted","issuedUnits":7,"verification":"not_requested","cleanup":"completed"}})
    );
    assert!(
        !status.to_string().contains("abc"),
        "no user payload in status"
    );
    h.no_more_events();
    h.finish();
}

#[test]
fn cancel_queued_batch_is_cancelled_and_cancel_mid_batch_stops_remaining_steps() {
    let mut h = Harness::start(
        "batch-cancel",
        Options {
            hold: true,
            ..Options::default()
        },
    );
    let three = batch(&[
        step("click", "e1"),
        step("click", "e1"),
        step("click", "e1"),
    ]);
    h.request(1, &three);
    h.next();
    assert_eq!(h.event(), Event::Prepared("click".into()));
    assert_eq!(h.event(), Event::Dispatched("click".into()));
    h.request(2, &three);
    assert_eq!(h.next()["type"], "accepted");
    h.send(&json!({"type":"cancel","seq":2}));
    assert_eq!(
        h.next(),
        json!({"type":"cancel","seq":2,"outcome":"cancelled"})
    );
    // Step 0 of seq 1 is already issued: too late to undo, but nothing more may follow.
    h.send(&json!({"type":"cancel","seq":1}));
    assert_eq!(
        h.next(),
        json!({"type":"cancel","seq":1,"outcome":"too_late"})
    );
    h.release();
    let first = result_of(&mut h, 1);
    assert_eq!(first["state"], "completed");
    assert_eq!(
        first["delivery"],
        json!({"state":"partial","issuedUnits":1,"reason":"cancelled"})
    );
    assert_eq!(first["error"]["code"], BATCH_STOPPED);
    assert_eq!(first["data"]["issuedSteps"], 1);
    assert_eq!(first["data"]["steps"][1]["error"]["code"], "cancelled");
    let second = result_of(&mut h, 2);
    assert_eq!(second["state"], "not_dispatched");
    assert_eq!(second["delivery"]["reason"], "cancelled");
    h.no_more_events();
    assert!(!h.lock_held());
    h.finish();
}

#[test]
fn batch_with_reads_nesting_or_bad_size_is_rejected_without_reserving_seq() {
    let mut h = Harness::start("batch-reject", Options::default());
    for op in [
        batch(&[step("click", "e1"), json!({"kind":"observe","pid":1})]),
        batch(&[json!({"kind":"apps"})]),
        batch(&[batch(&[step("click", "e1")])]),
        batch(&[]),
        batch(&std::iter::repeat_n(step("click", "e1"), MAX_BATCH_STEPS + 1).collect::<Vec<_>>()),
        batch(&[json!({"kind":"key","key":"a"})]),
    ] {
        h.request(1, &op);
        let rejected = h.next();
        assert_eq!(rejected["type"], "rejected", "{op}");
        assert_eq!(rejected["code"], "invalid_request", "{op}");
    }
    h.no_more_events();
    h.request(1, &batch(&[step("click", "e1")]));
    assert_eq!(h.next()["type"], "accepted");
    assert_eq!(result_of(&mut h, 1)["state"], "completed");
    h.finish();
}

#[test]
fn batch_verification_is_satisfied_only_when_every_step_is() {
    let mut h = Harness::start("batch-verify", Options::default());
    // A verified value write followed by an unverified press proves nothing about the whole batch.
    h.request(1, &batch(&[step("set_value", "sat"), step("action", "e1")]));
    let mixed = result_of(&mut h, 1);
    assert_eq!(mixed["state"], "completed");
    assert_eq!(mixed["verification"], json!({"state":"not_requested"}));
    assert_eq!(
        mixed["data"]["steps"][0]["verification"],
        json!({"state":"satisfied","predicate":"value_equals"})
    );
    assert_eq!(
        mixed["data"]["steps"][1]["verification"]["state"],
        "not_requested"
    );
    h.request(
        2,
        &batch(&[step("set_value", "sat"), step("set_value", "sat")]),
    );
    let all = result_of(&mut h, 2);
    assert_eq!(
        all["verification"],
        json!({"state":"satisfied","predicate":ALL_STEPS_SATISFIED})
    );
    h.finish();
}
