use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

struct Temp(std::path::PathBuf);
impl Temp {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mk101-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn claim(minute: u64) -> Run {
    Run {
        id: "test".into(),
        minute,
        phase: "started".into(),
        session: None,
        error: None,
        status: None,
        cwd: None,
        output: None,
    }
}

#[test]
fn claims_survive_restart_and_truncated_tail() {
    let dir = Temp::new("recover");
    Journal::open(&dir.0).unwrap().append(&claim(42)).unwrap();
    OpenOptions::new()
        .append(true)
        .open(dir.0.join(RUNS))
        .unwrap()
        .write_all(b"{\"truncated")
        .unwrap();
    let mut journal = Journal::open(&dir.0).unwrap();
    assert!(journal.claimed("test", 42));
    assert!(journal.claimed("test", 41));
    assert!(!journal.claimed("test", 43));
    journal.append(&claim(43)).unwrap();
    drop(journal);
    assert!(Journal::open(&dir.0).unwrap().claimed("test", 43));
    assert_eq!(
        fs::metadata(dir.0.join(RUNS)).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn rotation_retains_current_minute_claims_and_lock_excludes_other_daemons() {
    let dir = Temp::new("rotate");
    let guard = exclusive(&dir.0).unwrap();
    assert!(exclusive(&dir.0).is_err());
    // Lock contention does not burn the current minute on the registry.
    fs::write(
        dir.0.join(krill_proto::schedule::FILE),
        "[[schedule]]\nid = \"test\"\ncron = \"* * * * *\"\ncwd = \"/tmp\"\ncommand = [\"/bin/echo\"]\nprompt = \"hi\"\n",
    )
    .unwrap();
    let now = UNIX_EPOCH + Duration::from_mins(40);
    let registry = Registry::new(None, Some(&dir.0));
    assert!(registry.poll_schedules(now).is_err());
    drop(guard);
    registry.poll_schedules(now).unwrap();
    let _guard = exclusive(&dir.0).unwrap();
    let mut journal = Journal::open(&dir.0).unwrap();
    assert!(journal.claimed("test", 40));
    let mut active = claim(40);
    active.phase = "spawned".into();
    active.session = Some(777);
    journal.append(&active).unwrap();
    let bytes = serde_json::to_vec(&claim(40)).unwrap();
    for minute in [41, 42] {
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.0.join(RUNS))
            .unwrap();
        while file.metadata().unwrap().len() < MAX_RUNS {
            file.write_all(&bytes).unwrap();
            file.write_all(b"\n").unwrap();
        }
        journal.append(&claim(minute)).unwrap();
    }
    drop(journal);
    let journal = Journal::open(&dir.0).unwrap();
    assert!(journal.claimed("test", 42));
    assert!(journal.sessions.contains_key(&777));
    assert!(dir.0.join(PREVIOUS_RUNS).exists());
}

#[test]
fn utc_calendar_and_no_holder_failure_are_recorded_once() {
    assert_eq!(crate::sys::cron_calendar(0).unwrap(), [0, 0, 1, 1, 4]);
    // Leap day, 2024-02-29 12:34 UTC.
    assert_eq!(
        crate::sys::cron_calendar(1_709_210_040).unwrap(),
        [34, 12, 29, 2, 4]
    );
    let dir = Temp::new("failure");
    fs::write(
        dir.0.join(krill_proto::schedule::FILE),
        r#"[[schedule]]
id = "test"
cron = "* * * * *"
cwd = "/tmp"
command = ["/bin/echo"]
prompt = "hello"
"#,
    )
    .unwrap();
    let now = UNIX_EPOCH + Duration::from_mins(28_486_834);
    let registry = Registry::new(None, Some(&dir.0));
    registry.poll_schedules(now).unwrap();
    registry.poll_schedules(now).unwrap();
    let bytes = fs::read(dir.0.join(RUNS)).unwrap();
    let runs: Vec<Run> = bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs.last().unwrap().phase, "failed");
    drop(registry);
    Registry::new(None, Some(&dir.0))
        .poll_schedules(now)
        .unwrap();
    assert_eq!(fs::read(dir.0.join(RUNS)).unwrap(), bytes);
}

#[test]
fn cron_waits_for_due_minute_and_finished_runs_record_wait_status() {
    let dir = Temp::new("due");
    fs::write(
        dir.0.join(krill_proto::schedule::FILE),
        r#"[[schedule]]
id = "test"
cron = "35 12 29 2 *"
cwd = "/tmp"
command = ["/bin/echo"]
prompt = "hello"
"#,
    )
    .unwrap();
    let now = UNIX_EPOCH + Duration::from_mins(28_486_834);
    let registry = Registry::new(None, Some(&dir.0));
    registry.poll_schedules(now).unwrap();
    assert!(fs::read(dir.0.join(RUNS)).unwrap().is_empty());
    registry
        .poll_schedules(now + Duration::from_mins(1))
        .unwrap();
    let mut spawned = claim(28_486_835);
    spawned.phase = "spawned".into();
    spawned.session = Some(999);
    Journal::open(&dir.0).unwrap().append(&spawned).unwrap();
    lock(&registry.state)
        .exited
        .insert(krill_proto::SessionId(999), 256);
    registry
        .poll_schedules(now + Duration::from_mins(2))
        .unwrap();
    let text = fs::read_to_string(dir.0.join(RUNS)).unwrap();
    let runs: Vec<Run> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(runs.iter().filter(|run| run.phase == "started").count(), 1);
    let finished = runs.last().unwrap();
    assert_eq!(finished.phase, "finished");
    assert_eq!(finished.status, Some(256));
}

#[test]
fn exited_root_does_not_release_a_still_open_pty() {
    use crate::{Appearance, Session};
    use std::sync::Arc;
    let dir = Temp::new("live-pty");
    let mut spawned = claim(42);
    spawned.phase = "spawned".into();
    spawned.session = Some(999);
    Journal::open(&dir.0).unwrap().append(&spawned).unwrap();
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let registry = Registry::new(None, Some(&dir.0));
    let id = krill_proto::SessionId(999);
    let session = Session::start(
        id,
        pty.controller,
        krill_proto::holder::WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(dir.0.join("theme"))),
        None,
    )
    .unwrap();
    {
        let mut state = lock(&registry.state);
        state.sessions.insert(
            id,
            super::super::Entry {
                session,
                pid: 0,
                cwd: dir.0.clone(),
            },
        );
        state.exited.insert(id, 0);
    }
    registry
        .poll_schedules(UNIX_EPOCH + Duration::from_mins(42))
        .unwrap();
    assert!(
        !fs::read_to_string(dir.0.join(RUNS))
            .unwrap()
            .contains("finished")
    );
    drop(pty.user);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !lock(&registry.state)
        .sessions
        .get(&id)
        .unwrap()
        .session
        .is_closed()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "reader did not see EOF"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    registry
        .poll_schedules(UNIX_EPOCH + Duration::from_mins(43))
        .unwrap();
    assert!(
        fs::read_to_string(dir.0.join(RUNS))
            .unwrap()
            .contains("finished")
    );
}

/// K-30: 완전히 사라진 세션의 옛 spawned 기록은 tombstone을 남긴다.
/// 두지 않으면 id가 재사용됐을 때 무관한 새 세션을 close한다.
#[test]
fn reap_tombstones_runs_of_gone_sessions() {
    use std::time::SystemTime;
    let dir = Temp::new("tombstone");
    fs::write(
        dir.0.join(RUNS),
        "{\"id\":\"cron-a\",\"minute\":1,\"phase\":\"spawned\",\"session\":99,\"error\":null,\"status\":null}\n",
    )
    .unwrap();
    let registry = Registry::new(None, Some(&dir.0));
    registry.poll_schedules(SystemTime::now()).unwrap();
    let runs = fs::read_to_string(dir.0.join(RUNS)).unwrap();
    assert!(
        runs.lines()
            .any(|l| l.contains("\"session\":99") && l.contains("\"finished\"")),
        "gone session run is tombstoned: {runs}"
    );
    // 다시 읽어도 추적하지 않는다. id가 재사용돼도 close하지 않는다.
    assert!(!Journal::open(&dir.0).unwrap().sessions.contains_key(&99));
}

#[test]
fn precheck_gate_skips_nonzero_and_isolate_creates_worktree_and_run_now_triggers() {
    use std::process::Command;

    let dir = Temp::new("precheck-isolate");
    let repo = dir.0.join("repo");
    fs::create_dir_all(&repo).unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.email", "test@example.com"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.name", "Test"])
        .output()
        .unwrap();
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["commit", "-m", "initial"])
        .output()
        .unwrap();

    fs::write(
        dir.0.join(krill_proto::schedule::FILE),
        format!(
            r#"[[schedule]]
id = "skip-me"
cron = "* * * * *"
cwd = "{cwd}"
command = ["/bin/echo"]
prompt = "ignored"
precheck = "printf 'nothing to do\n' && exit 3"
workspace_mode = "isolate"

[[schedule]]
id = "pass-me"
cron = "* * * * *"
cwd = "{cwd}"
command = ["/bin/echo"]
prompt = "run"
precheck = "test -f README.md"
workspace_mode = "isolate"
"#,
            cwd = repo.display()
        ),
    )
    .unwrap();

    let now = UNIX_EPOCH + Duration::from_mins(50);
    let registry = Registry::new(None, Some(&dir.0));
    registry.poll_schedules(now).unwrap();

    let coalesced = krill_proto::schedule::load_coalesced_runs(&dir.0).unwrap();
    assert_eq!(coalesced.len(), 2);
    let skipped = coalesced.iter().find(|r| r.id == "skip-me").unwrap();
    assert_eq!(skipped.phase, "skipped");
    assert_eq!(skipped.status, Some(3));
    assert_eq!(skipped.output.as_deref(), Some("nothing to do"));
    assert!(
        skipped.cwd.is_none(),
        "skipped precheck must not create an isolated worktree"
    );

    let passed = coalesced.iter().find(|r| r.id == "pass-me").unwrap();
    // Without a holder, session creation fails AFTER isolated worktree creation.
    let isolated_path = passed
        .cwd
        .as_deref()
        .expect("passing precheck must create isolated worktree");
    assert!(Path::new(isolated_path).join("README.md").is_file());

    // Explicit Run Now triggers within the same minute even though minute 50 was already claimed.
    krill_proto::schedule::request_run_now(&dir.0, "skip-me").unwrap();
    registry.poll_schedules(now).unwrap();
    let coalesced_after = krill_proto::schedule::load_coalesced_runs(&dir.0).unwrap();
    assert_eq!(
        coalesced_after.iter().filter(|r| r.id == "skip-me").count(),
        2
    );
}

#[test]
fn run_now_survives_load_or_journal_error_and_corrupted_journal_lines_are_skipped() {
    let dir = Temp::new("owe754-journal-run-now");
    let valid_toml = r#"[[schedule]]
id = "manual-job"
cron = "0 0 31 2 *"
cwd = "/tmp"
command = ["/bin/echo"]
prompt = "run-now"
precheck = "printf 'ran-after-recovery\n' && exit 4"
"#;
    fs::write(dir.0.join(krill_proto::schedule::FILE), valid_toml).unwrap();
    krill_proto::schedule::request_run_now(&dir.0, "manual-job").unwrap();

    // 1. Corrupt schedules.toml AFTER request_run_now was queued: poll_schedules fails on
    //    Schedules::load, but MUST NOT consume schedule-run-now.
    fs::write(dir.0.join(krill_proto::schedule::FILE), "invalid = [toml").unwrap();
    let now = UNIX_EPOCH + Duration::from_mins(60);
    let registry = Registry::new(None, Some(&dir.0));
    assert!(registry.poll_schedules(now).is_err());
    assert!(
        dir.0.join(RUN_NOW).exists(),
        "schedule-run-now must survive Schedules::load failure"
    );

    // 2. Restore valid schedules.toml, and write a journal with a corrupted middle line
    //    plus a valid line after it and a truncated tail without newline.
    fs::write(dir.0.join(krill_proto::schedule::FILE), valid_toml).unwrap();
    let mut prior = claim(58);
    prior.id = "other-job".into();
    let mut after_corrupt = claim(59);
    after_corrupt.id = "other-job".into();
    let raw_journal = format!(
        "{}\n{{\"corrupted_line\": true, \"id\": 123}}\n{}\n{{\"half-written-tail",
        serde_json::to_string(&prior).unwrap(),
        serde_json::to_string(&after_corrupt).unwrap()
    );
    fs::write(dir.0.join(RUNS), raw_journal).unwrap();

    // 3. Next poll in the same minute skips the corrupted line, preserves valid records,
    //    and consumes the preserved Run Now request.
    registry.poll_schedules(now).unwrap();
    assert!(!dir.0.join(RUN_NOW).exists());

    let coalesced = krill_proto::schedule::load_coalesced_runs(&dir.0).unwrap();
    let manual = coalesced
        .iter()
        .find(|r| r.id == "manual-job")
        .expect("preserved run_now request must execute after recovery");
    assert_eq!(manual.phase, "skipped");
    assert_eq!(manual.status, Some(4));
    assert_eq!(manual.output.as_deref(), Some("ran-after-recovery"));

    let journal = Journal::open(&dir.0).unwrap();
    assert!(journal.claimed("other-job", 59));
}

#[test]
fn precheck_enforces_timeout_and_caps_pipe_output_bytes() {
    let dir = Temp::new("owe754-precheck");

    // 1. Hanging precheck command must time out and kill its process group without blocking forever.
    let started = Instant::now();
    let timed_out = run_precheck_with_timeout(
        &dir.0,
        "printf 'before-hang\n'; sleep 30",
        Duration::from_millis(120),
    );
    assert!(!timed_out.passed);
    assert_eq!(timed_out.status, None);
    assert_eq!(timed_out.output.as_deref(), Some("before-hang"));
    assert_eq!(timed_out.error.as_deref(), Some("precheck timed out"));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "timed-out precheck must kill child process group promptly"
    );

    // 2. Large output (>100 KiB) must not stall the pipe or exceed MAX_OUTPUT_BYTES.
    let noisy = run_precheck_with_timeout(
        &dir.0,
        "dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\\0' 'A' && exit 7",
        Duration::from_secs(5),
    );
    assert!(!noisy.passed);
    assert_eq!(noisy.status, Some(7));
    let out = noisy.output.expect("captured bounded output");
    assert_eq!(out.len(), MAX_OUTPUT_BYTES);
    assert!(out.bytes().all(|b| b == b'A'));
}
