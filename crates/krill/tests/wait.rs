//! MK-047: wait modes through real holder/krilld, without an application or agent CLI.
#![expect(clippy::unwrap_used, reason = "integration test helpers")]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::control::{AgentState, FEAT_CONTROL, FEAT_WAIT_STATUS, Reply, Request};
use krill_proto::daemon::{client_hello, handshake};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

const MK: &str = env!("CARGO_BIN_EXE_krill");
const WAIT: Duration = Duration::from_secs(10);

struct Daemon {
    holder: Child,
    krilld: u32,
    dir: PathBuf,
}
impl Daemon {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mk047-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let bin = Path::new(MK).parent().unwrap();
        let socket = dir.join("run/krilld.sock");
        let holder = Command::new(bin.join("krill-holder"))
            .arg(bin.join("krilld"))
            .arg(&socket)
            .env("KRILL_STATE_DIR", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        let krilld = loop {
            if let Ok(c) = handshake(&socket, &client_hello(ClientRole::Cli, "mk045"), WAIT) {
                break u32::try_from(c.server.instance_id).unwrap();
            }
            assert!(Instant::now() < deadline, "daemon startup timed out");
            thread::sleep(Duration::from_millis(20));
        };
        Self {
            holder,
            krilld,
            dir,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(MK)
            .args(args)
            .env("KRILL_STATE_DIR", &self.dir)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn wait_text(&self, id: &str, text: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let out = self.ok(&["read", id]);
            if out.contains(text) {
                return out;
            }
            assert!(Instant::now() < deadline, "missing {text:?}: {out:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        // Close our sessions first so a failing test never leaves a live shell behind.
        if let Ok(mut c) = handshake(
            &self.dir.join("run/krilld.sock"),
            &client_hello(ClientRole::Cli, "cleanup"),
            WAIT,
        ) {
            c.stream.write_all(&Request::List.encode().unwrap()).ok();
            if let Ok(Some((kind, body))) = ipc::read_frame(&mut c.stream)
                && let Ok(Reply::Sessions(sessions)) = Reply::decode(kind, &body)
            {
                for info in sessions {
                    c.stream
                        .write_all(
                            &Request::Close {
                                session: info.session,
                            }
                            .encode()
                            .unwrap(),
                        )
                        .ok();
                    ipc::read_frame(&mut c.stream).ok();
                }
                thread::sleep(Duration::from_millis(150));
            }
        }
        self.holder.kill().ok();
        self.holder.wait().ok();
        Command::new("/bin/kill")
            .args(["-9", &self.krilld.to_string()])
            .status()
            .ok();
        fs::remove_dir_all(&self.dir).ok();
    }
}

impl Daemon {
    fn session(&self, body: &str) -> String {
        let script = format!(
            "D='{}'; MK='{MK}'; {body}; exec sleep 600",
            self.dir.display()
        );
        self.ok(&["new", "--", "/bin/sh", "-c", &script])
            .trim()
            .to_owned()
    }
    fn assert_timeout(&self, id: &str, mode: &str, extra: &[&str]) {
        let mut args = vec!["wait", id, "--for", mode, "--timeout", "0.2"];
        args.extend_from_slice(extra);
        let out = self.run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("wait timed out"));
    }
    fn probe(&self, id: &str, mode: &str) -> Output {
        self.run(&["wait", id, "--for", mode, "--timeout", "0"])
    }
    /// Hook state is eventual: a hook that misses its socket budget (parallel load) spools, and krilld applies the
    /// spool within `SPOOL_POLL` (1s). Never probe it with `--timeout 0`.
    fn reach(&self, id: &str, mode: &str) {
        assert!(
            self.ok(&["wait", id, "--for", mode, "--timeout", "5"])
                .contains(mode)
        );
    }
}

#[test]
fn idle_requires_known_nonworking_state_and_quiet_output() {
    let d = Daemon::new("idle");
    let unknown = d.session("printf 'unknown-ready\\n'");
    d.wait_text(&unknown, "unknown-ready");
    d.assert_timeout(&unknown, "idle", &["--quiet-ms", "0"]);
    assert!(
        d.ok(&["wait", &unknown, "--for", "quiet:0", "--timeout", "0"])
            .contains("quiet")
    );

    let working = d.session(r#"printf '%s' '{"hook_event_name":"UserPromptSubmit"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null; printf 'working-ready\n'"#);
    d.wait_text(&working, "working-ready");
    d.reach(&working, "working");
    d.assert_timeout(&working, "idle", &["--quiet-ms", "0"]);

    let done = d.session(r#"printf '%s' '{"hook_event_name":"Stop"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null; printf 'done-ready\n'"#);
    d.wait_text(&done, "done-ready");
    assert!(
        d.ok(&[
            "wait",
            &done,
            "--for",
            "idle",
            "--quiet-ms",
            "0",
            "--timeout",
            "5"
        ])
        .contains("idle")
    );
    d.assert_timeout(&done, "idle", &["--quiet-ms", "1000"]);
    assert!(
        d.ok(&[
            "wait",
            &done,
            "--for",
            "idle",
            "--quiet-ms",
            "1000",
            "--timeout",
            "5"
        ])
        .contains("idle")
    );
    // A new invocation uses daemon output age; it need not observe another quiet window.
    assert!(
        d.ok(&[
            "wait",
            &done,
            "--for",
            "idle",
            "--quiet-ms",
            "1000",
            "--timeout",
            "0"
        ])
        .contains("idle")
    );
    let exited = d.ok(&["new", "--", "/bin/sh", "-c", "exit 0"]);
    let exited = exited.trim();
    assert!(d.ok(&["wait", exited, "--timeout", "5"]).contains("exited"));
    assert!(
        d.ok(&[
            "wait",
            exited,
            "--for",
            "idle",
            "--quiet-ms",
            "0",
            "--timeout",
            "0"
        ])
        .contains("idle")
    );
}

#[test]
fn waiting_and_blocked_use_daemon_evidence_and_are_idle_when_quiet() {
    let d = Daemon::new("states");
    let waiting = d.session(r#"printf '%s' '{"hook_event_name":"PermissionRequest"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null; printf 'waiting-ready\n'"#);
    d.wait_text(&waiting, "waiting-ready");
    d.reach(&waiting, "waiting");
    d.assert_timeout(&waiting, "blocked", &[]);
    assert!(
        d.ok(&[
            "wait",
            &waiting,
            "--for",
            "idle",
            "--quiet-ms",
            "0",
            "--timeout",
            "0"
        ])
        .contains("idle")
    );

    let blocked = d.session("printf 'Do you trust this folder?\\n1. Yes, I trust this folder\\n'");
    assert!(
        d.ok(&["wait", &blocked, "--for", "blocked", "--timeout", "5"])
            .contains("blocked")
    );
    d.assert_timeout(&blocked, "waiting", &[]);
    assert!(
        d.ok(&[
            "wait",
            &blocked,
            "--for",
            "idle",
            "--quiet-ms",
            "0",
            "--timeout",
            "0"
        ])
        .contains("idle")
    );
}

#[test]
fn pattern_matches_regex_on_plain_unicode_screen() {
    let d = Daemon::new("pattern");
    let id = d.session("sleep 0.3; printf '\\033[31m결과:42 완료\\033[0m\\n'");
    assert!(
        d.ok(&[
            "wait",
            &id,
            "--for",
            "pattern:결과:[0-9]+ 완료",
            "--timeout",
            "5"
        ])
        .contains("pattern")
    );
    assert!(
        d.ok(&[
            "wait",
            &id,
            "--for",
            "pattern",
            "--pattern",
            "결과:[0-9]+ 완료",
            "--timeout",
            "0"
        ])
        .contains("pattern")
    );
    d.assert_timeout(&id, "pattern:absent", &[]);
    d.assert_timeout(&id, r"pattern:\x1b\[31m", &[]);
    assert_eq!(d.probe(&id, "pattern:[").status.code(), Some(2));
    assert_eq!(d.probe(&id, "pattern").status.code(), Some(2));
}

#[test]
fn invalid_wait_options_fail_before_contacting_daemon() {
    let d = Daemon::new("options");
    for args in [
        vec!["wait", "1", "--for", "quiet:-1"],
        vec!["wait", "1", "--for", "pattern:"],
        vec!["wait", "1", "--for", "idle", "--pattern", "x"],
        vec!["wait", "1", "--for", "waiting", "--quiet-ms", "3"],
        vec!["wait", "1", "--timeout", "NaN"],
        vec!["wait", "1", "--timeout", "-1"],
        vec!["wait", "1", "--timeout", "inf"],
        vec!["wait", "1", "--for", "idle", "--for", "quiet"],
    ] {
        assert_eq!(d.run(&args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn quiet_counts_invisible_output_and_reports_timeout_two() {
    let d = Daemon::new("quiet");
    let id = d.session("printf 'stream-ready\\n'; while [ ! -f \"$D/stop\" ]; do printf '\\033[0m'; sleep 0.02; done; touch \"$D/stopped\"");
    d.wait_text(&id, "stream-ready");
    d.assert_timeout(&id, "quiet:200", &[]);
    fs::write(d.dir.join("stop"), b"").unwrap();
    assert!(
        d.ok(&[
            "wait",
            &id,
            "--for",
            "quiet",
            "--quiet-ms",
            "200",
            "--timeout",
            "5"
        ])
        .contains("quiet")
    );
    assert!(d.dir.join("stopped").exists());
    assert!(d.probe(&id, "quiet:200").status.success());
}

#[test]
fn wait_query_enforces_feature_and_session_scope() {
    let d = Daemon::new("scope");
    let target = d.session("printf 'target-ready\\n'");
    d.wait_text(&target, "target-ready");
    let mut hello = client_hello(ClientRole::Cli, "old-client");
    hello.features = FEAT_CONTROL;
    let mut c = handshake(&d.dir.join("run/krilld.sock"), &hello, WAIT).unwrap();
    c.stream
        .write_all(
            &Request::WaitStatus {
                session: krill_proto::SessionId(target.parse().unwrap()),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (kind, bytes) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    assert_eq!(
        Reply::decode(kind, &bytes).unwrap(),
        Reply::Failed(krill_proto::control::Failure::BadRequest)
    );

    hello.features |= FEAT_WAIT_STATUS;
    let mut c = handshake(&d.dir.join("run/krilld.sock"), &hello, WAIT).unwrap();
    c.stream
        .write_all(
            &Request::WaitStatus {
                session: krill_proto::SessionId(target.parse().unwrap()),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (kind, bytes) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    assert!(matches!(
        Reply::decode(kind, &bytes).unwrap(),
        Reply::WaitStatus {
            state: AgentState::Unknown,
            ..
        }
    ));

    let attacker = d.session(&format!(r#"KRILL_STATE_DIR="$D" "$MK" wait {target} --for quiet:0 --timeout 0 >"$D/rejected" 2>&1; printf '%s' $? >"$D/status"; printf 'attacker-ready\n'"#));
    d.wait_text(&attacker, "attacker-ready");
    assert_eq!(fs::read_to_string(d.dir.join("status")).unwrap(), "1");
    assert!(
        fs::read_to_string(d.dir.join("rejected"))
            .unwrap()
            .contains("no such session")
    );
    assert_eq!(d.probe("999999", "idle").status.code(), Some(1));
}

#[test]
fn timeout_bounds_unresponsive_daemon_reads() {
    let d = Daemon::new("timeout");
    let id = d.session("printf 'ready\\n'");
    d.wait_text(&id, "ready");
    let waiter = Command::new(MK)
        .args(["wait", &id, "--for", "done", "--timeout", "0.4"])
        .env("KRILL_STATE_DIR", &d.dir)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Let Hello complete before pausing the daemon; the next status read must hit the deadline.
    thread::sleep(Duration::from_millis(150));
    Command::new("/bin/kill")
        .args(["-STOP", &d.krilld.to_string()])
        .status()
        .unwrap();
    let start = Instant::now();
    let out = waiter.wait_with_output().unwrap();
    Command::new("/bin/kill")
        .args(["-CONT", &d.krilld.to_string()])
        .status()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn timeout_during_hello_exits_two() {
    let d = Daemon::new("hello-timeout");
    Command::new("/bin/kill")
        .args(["-STOP", &d.krilld.to_string()])
        .status()
        .unwrap();
    let start = Instant::now();
    let out = d.run(&["wait", "1", "--for", "idle", "--timeout", "0.2"]);
    Command::new("/bin/kill")
        .args(["-CONT", &d.krilld.to_string()])
        .status()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

/// OWE-501: under load `krill hook` can miss its socket budget and spool; krilld applies the spool within
/// `SPOOL_POLL`. SIGSTOP forces that path, so wait assertions must not assume immediate socket delivery.
#[test]
fn spooled_hook_state_is_visible_to_wait() {
    let d = Daemon::new("spooled");
    let id = d.session(r#"while [ ! -f "$D/go" ]; do sleep 0.02; done; printf '%s' '{"hook_event_name":"UserPromptSubmit"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null; touch "$D/sent""#);
    let kill = |sig: &str| {
        Command::new("/bin/kill")
            .args([sig, &d.krilld.to_string()])
            .status()
            .unwrap()
    };
    kill("-STOP");
    fs::write(d.dir.join("go"), b"").unwrap();
    let deadline = Instant::now() + WAIT;
    while !d.dir.join("sent").exists() {
        assert!(Instant::now() < deadline, "hook did not finish");
        thread::sleep(Duration::from_millis(20));
    }
    kill("-CONT");
    assert!(
        fs::metadata(d.dir.join(krill_proto::hook::SPOOL_FILE)).is_ok_and(|m| m.len() > 0),
        "hook was not spooled"
    );
    d.reach(&id, "working");
}

#[test]
fn wait_for_done_ignores_session_start_and_prior_stop_until_next_stop() {
    let d = Daemon::new("done-transition");
    let id = d.session(
        r#"stty -echo
printf '%s' '{"hook_event_name":"SessionStart","source":"startup"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null
printf 'started\n'
IFS= read -r _
printf '%s' '{"hook_event_name":"Stop"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null
printf 'turn1-done\n'
IFS= read -r _
while [ ! -f "$D/finish-turn2" ]; do sleep 0.02; done
printf '%s' '{"hook_event_name":"Stop"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null
printf 'turn2-done\n'"#,
    );
    d.wait_text(&id, "started");
    // SessionStart must not satisfy `--for done`.
    d.assert_timeout(&id, "done", &[]);

    // Finish turn 1 while a waiter is watching so the first Stop transitions into Done.
    let waiter1 = Command::new(MK)
        .args(["wait", &id, "--for", "done", "--timeout", "5"])
        .env("KRILL_STATE_DIR", &d.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    d.ok(&["send", &id, "--enter", "go-turn1"]);
    let out1 = waiter1.wait_with_output().unwrap();
    assert!(out1.status.success(), "{out1:?}");
    d.wait_text(&id, "turn1-done");

    // Immediately after `krill send` for turn 2, the prior Stop's Done state is still present,
    // so `--for done` must not succeed before turn 2 emits its own Stop.
    d.ok(&["send", &id, "--enter", "go-turn2"]);
    d.assert_timeout(&id, "done", &[]);

    let waiter2 = Command::new(MK)
        .args(["wait", &id, "--for", "done", "--timeout", "5"])
        .env("KRILL_STATE_DIR", &d.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
    fs::write(d.dir.join("finish-turn2"), b"").unwrap();
    let out2 = waiter2.wait_with_output().unwrap();
    assert!(out2.status.success(), "{out2:?}");
    d.wait_text(&id, "turn2-done");
}

#[test]
fn session_start_allows_idle_wait_without_a_completed_turn() {
    let d = Daemon::new("session-start-idle");
    let id = d.session(r#"printf '%s' '{"hook_event_name":"SessionStart"}' | KRILL_STATE_DIR="$D" "$MK" hook claude >/dev/null; printf 'start-idle-ready\n'"#);
    d.wait_text(&id, "start-idle-ready");
    d.ok(&[
        "wait",
        &id,
        "--for",
        "idle",
        "--quiet-ms",
        "0",
        "--timeout",
        "3",
    ]);
    d.assert_timeout(&id, "done", &[]);
}
