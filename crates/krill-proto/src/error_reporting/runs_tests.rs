#![cfg(unix)]
//! 실행 표식 테스트. 다른 프로세스의 잠금·SIGKILL·자식 상속·panic hook은 이 테스트 바이너리를 도우미로 다시
//! 띄워 확인한다(`helper_process`). 상태 디렉터리는 테스트마다 새 임시 디렉터리다.

use super::*;
use crate::error_reporting::{RUNS_DIR, consent, dir, event_id, now_ms};
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

static SEQ: AtomicU64 = AtomicU64::new(0);

const HELPER_MODE: &str = "KRILL_RUNS_TEST_HELPER";
const HELPER_STATE: &str = "KRILL_RUNS_TEST_STATE";
const HELPER_NAME: &str = "error_reporting::runs::tests::helper_process";
/// 도우미 준비 표시. libtest가 같은 줄 앞에 테스트 이름을 찍을 수 있어 줄 안에서 찾는다.
const READY: &str = "KRILL_RUNS_HELPER_READY";

/// 테스트마다 다른 임시 디렉터리. 놓으면 지운다.
pub(in crate::error_reporting) struct Scratch(pub(in crate::error_reporting) PathBuf);

impl Scratch {
    pub(in crate::error_reporting) fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "krill-error-reporting-{tag}-{}-{seq}",
            std::process::id()
        ));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// 동의한 상태 디렉터리와 그 동의(환경 거부와 무관한 파일 내용).
pub(in crate::error_reporting) fn consented(tag: &str) -> (Scratch, Consent) {
    let scratch = Scratch::new(tag);
    crate::error_reporting::enable(&scratch.0).unwrap();
    let consent = consent::read(&scratch.0).unwrap().unwrap();
    (scratch, consent)
}

fn runs_dir(state: &Path) -> PathBuf {
    dir(state).join(RUNS_DIR)
}

fn sample(consent: &Consent, process: Process, pid: u32, started_at_ms: u64) -> Record {
    Record {
        clean: false,
        panicked: false,
        process,
        pid,
        started_at_ms,
        run_id: random_id().unwrap(),
        generation: generation_hash(consent),
        seen_ms: 0,
        release: "0.1.40".to_owned(),
        build_id: "0.1.40+dev.test".to_owned(),
    }
}

fn write_record(state: &Path, record: &Record) -> PathBuf {
    fs::create_dir_all(runs_dir(state)).unwrap();
    let path = runs_dir(state).join(format!("{}{MARKER_EXT}", record.run_id));
    fs::write(&path, record.encode().unwrap()).unwrap();
    path
}

/// 잠금 없이 남은(끝난 실행의) 표식을 직접 만든다. release `0.1.40`, build `0.1.40+dev.test`.
pub(in crate::error_reporting) fn plant_marker(
    state: &Path,
    consent: &Consent,
    process: Process,
    pid: u32,
    started_at_ms: u64,
) -> PathBuf {
    write_record(state, &sample(consent, process, pid, started_at_ms))
}

/// 대기열에 들어간 이벤트(JSON), event ID 순.
pub(in crate::error_reporting) fn queued(state: &Path) -> Vec<serde_json::Value> {
    let Ok(items) = fs::read_dir(dir(state).join("outbox")) else {
        return Vec::new();
    };
    let mut events: Vec<serde_json::Value> = items
        .map(|item| {
            let entry: serde_json::Value =
                serde_json::from_slice(&fs::read(item.unwrap().path()).unwrap()).unwrap();
            entry["event"].clone()
        })
        .collect();
    events.sort_by_key(|event| event["event_id"].as_str().unwrap().to_owned());
    events
}

fn markers(state: &Path) -> Vec<PathBuf> {
    let Ok(items) = fs::read_dir(runs_dir(state)) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = items.map(|item| item.unwrap().path()).collect();
    paths.sort();
    paths
}

fn single_marker(state: &Path) -> PathBuf {
    let paths = markers(state);
    assert_eq!(paths.len(), 1, "{paths:?}");
    paths[0].clone()
}

fn record_of(path: &Path) -> Record {
    Record::decode(&fs::read(path).unwrap()).unwrap()
}

fn sweep(state: &Path, consent: &Consent, now: u64) -> usize {
    settle(state, consent, now, Stale::scan(state, consent))
}

/// 도우미 프로세스. 놓으면 SIGKILL로 정리한다.
struct Helper {
    child: Child,
    line: String,
}

impl Helper {
    fn spawn(mode: &str, state: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                HELPER_NAME,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_MODE, mode)
            .env(HELPER_STATE, state)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        // 도우미가 끝날 때까지 읽어 파이프가 막히지 않게 한다.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((_, rest)) = line.split_once(READY) {
                    tx.send(rest.trim().to_owned()).ok();
                }
            }
        });
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("helper did not report ready");
        Self { child, line }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// 손자 프로세스를 테스트가 끝날 때 정리한다.
struct Reaper(u32);

impl Drop for Reaper {
    fn drop(&mut self) {
        Command::new("/bin/kill")
            .args(["-9", &self.0.to_string()])
            .stderr(Stdio::null())
            .status()
            .ok();
    }
}

/// 격리 프로세스 도우미. 부모 테스트가 이 바이너리를 다시 띄울 때만 동작하고, 평소에는 바로 끝난다.
#[test]
#[ignore = "helper process for isolated run marker tests"]
fn helper_process() {
    let (Some(mode), Some(state)) = (
        std::env::var_os(HELPER_MODE),
        std::env::var_os(HELPER_STATE),
    ) else {
        return;
    };
    let state = PathBuf::from(state);
    let consent = consent::read(&state).unwrap().unwrap();
    let guard = RunGuard::start_with(&state, Process::Daemon, &consent)
        .unwrap()
        .unwrap();
    let mut grandchild = None;
    match mode.to_str().unwrap() {
        mode @ ("spawn" | "leak") => {
            if mode == "leak" {
                // 음성 대조: 표식 fd의 CLOEXEC를 일부러 끈다.
                rustix::io::fcntl_setfd(&*guard.marker, rustix::io::FdFlags::empty()).unwrap();
            }
            let child = Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!("{READY} {}", child.id());
            grandchild = Some(child);
        }
        "panic" => {
            std::panic::set_hook(Box::new(|_| note_panic()));
            println!("{READY}");
            panic!("helper panics while its run marker is live");
        }
        _ => println!("{READY}"),
    }
    std::thread::sleep(Duration::from_secs(60));
    drop(grandchild);
    drop(guard);
}

#[test]
fn record_layout_matches_in_place_offsets() {
    let consent = Consent {
        generation: "0".repeat(32),
        enabled_at_ms: 1,
    };
    let mut record = sample(&consent, Process::App, 4_294_967_295, u64::MAX);
    let bytes = record.encode().unwrap();
    assert_eq!(bytes.len(), RECORD_LEN);
    assert_eq!(bytes[6], b'R');
    assert_eq!(bytes[8], b'-');
    assert_eq!(&bytes[110..130], b"00000000000000000000");
    assert_eq!(Record::decode(&bytes), Some(record.clone()));
    record.clean = true;
    record.panicked = true;
    record.seen_ms = 12_345;
    let bytes = record.encode().unwrap();
    assert_eq!(usize::try_from(STATE_AT).unwrap(), 6);
    assert_eq!(usize::try_from(PANIC_AT).unwrap(), 8);
    assert_eq!(usize::try_from(SEEN_AT).unwrap(), 110);
    assert_eq!(bytes[6], CLEAN);
    assert_eq!(bytes[8], PANICKED);
    assert_eq!(&bytes[110..130], b"00000000000000012345");
    assert_eq!(Record::decode(&bytes), Some(record));
}

#[test]
fn owner_detects_a_marker_removed_or_replaced_before_its_first_lock() {
    let (state, consent) = consented("owner-publication");
    fs::create_dir_all(runs_dir(&state.0)).unwrap();
    let path = write_record(&state.0, &sample(&consent, Process::App, 42, now_ms()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert!(marker_is_current(&file, &path));
    fs::remove_file(&path).unwrap();
    assert!(!marker_is_current(&file, &path));
    fs::write(&path, b"replacement").unwrap();
    assert!(!marker_is_current(&file, &path));
}

#[test]
fn encode_refuses_fields_outside_the_record_format() {
    let consent = Consent {
        generation: "0".repeat(32),
        enabled_at_ms: 1,
    };
    let base = sample(&consent, Process::Daemon, 1, 1);
    for broken in [
        Record {
            process: Process::Cli,
            ..base.clone()
        },
        Record {
            process: Process::Holder,
            ..base.clone()
        },
        Record {
            release: "0.1.0 beta".to_owned(),
            ..base.clone()
        },
        Record {
            release: "#".to_owned(),
            ..base.clone()
        },
        Record {
            release: "1".repeat(65),
            ..base.clone()
        },
        Record {
            build_id: String::new(),
            ..base.clone()
        },
        Record {
            build_id: "b".repeat(81),
            ..base.clone()
        },
        Record {
            run_id: "Z".repeat(32),
            ..base.clone()
        },
    ] {
        assert_eq!(broken.encode(), None, "{broken:?}");
    }
    let bytes = base.encode().unwrap();
    let mut truncated = bytes.clone();
    truncated.pop();
    let mut swapped = bytes.clone();
    swapped[6] = b'X';
    let mut no_newline = bytes;
    *no_newline.last_mut().unwrap() = b' ';
    for broken in [truncated, swapped, no_newline, Vec::new()] {
        assert_eq!(Record::decode(&broken), None);
    }
}

#[test]
fn start_is_only_for_app_and_daemon_under_current_consent() {
    let (scratch, consent) = consented("kinds");
    for process in [Process::Cli, Process::Holder] {
        assert!(
            RunGuard::start_with(&scratch.0, process, &consent)
                .unwrap()
                .is_none()
        );
        assert!(
            RunGuard::start(&scratch.0, process).unwrap().is_none(),
            "{process:?}"
        );
    }
    assert!(markers(&scratch.0).is_empty());
    let bare = Scratch::new("bare");
    assert!(RunGuard::start(&bare.0, Process::App).unwrap().is_none());
    assert!(!dir(&bare.0).exists(), "no consent: nothing is created");
    let guard = RunGuard::start_with(&scratch.0, Process::App, &consent)
        .unwrap()
        .unwrap();
    let record = record_of(&single_marker(&scratch.0));
    assert_eq!(record.process, Process::App);
    assert_eq!(record.pid, std::process::id());
    assert_eq!(record.generation, generation_hash(&consent));
    assert_eq!(record.release, env!("CARGO_PKG_VERSION"));
    assert_eq!(record.build_id, crate::BUILD);
    assert!(!record.clean && !record.panicked);
    assert_eq!(record.seen_ms, 0);
    drop(guard);
}

#[test]
fn normal_drop_and_finish_write_clean_in_place_without_unlinking() {
    let (scratch, consent) = consented("clean");
    let guard = RunGuard::start_with(&scratch.0, Process::App, &consent)
        .unwrap()
        .unwrap();
    let first = single_marker(&scratch.0);
    let before = fs::metadata(&first).unwrap();
    assert!(!record_of(&first).clean);
    drop(guard);
    let after = fs::metadata(&first).unwrap();
    assert!(record_of(&first).clean, "normal return marks clean");
    assert_eq!(before.ino(), after.ino(), "clean is written in place");

    let guard = RunGuard::start_with(&scratch.0, Process::Daemon, &consent)
        .unwrap()
        .unwrap();
    guard.finish();
    let all = markers(&scratch.0);
    assert_eq!(all.len(), 2, "owners never unlink their markers");
    assert!(all.iter().all(|path| record_of(path).clean));

    // 스캐너는 clean 표식을 기록 없이 지운다.
    assert!(Stale::scan(&scratch.0, &consent).is_empty());
    assert!(markers(&scratch.0).is_empty());
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn unwinding_through_the_guard_leaves_the_run_unclean() {
    let (scratch, consent) = consented("unwind");
    let state = scratch.0.clone();
    let joined = std::thread::spawn(move || {
        let _guard = RunGuard::start_with(&state, Process::Daemon, &consent)
            .unwrap()
            .unwrap();
        panic!("unwind through a live run guard");
    })
    .join();
    assert!(joined.is_err());
    let record = record_of(&single_marker(&scratch.0));
    assert!(
        !record.clean,
        "a panic must not be recorded as a clean exit"
    );
}

#[test]
fn scanner_treats_a_locked_marker_in_this_process_as_live() {
    let (scratch, consent) = consented("samepid");
    let guard = RunGuard::start_with(&scratch.0, Process::Daemon, &consent)
        .unwrap()
        .unwrap();
    assert!(Stale::scan(&scratch.0, &consent).is_empty());
    assert_eq!(sweep(&scratch.0, &consent, now_ms()), 0);
    let record = record_of(&single_marker(&scratch.0));
    assert_eq!(record.seen_ms, 0, "a live marker is never touched");
    drop(guard);
}

#[test]
fn scanner_quietly_removes_clean_corrupt_and_other_generation_markers() {
    let (scratch, consent) = consented("sweep");
    let mut clean = sample(&consent, Process::App, 1, 1_000);
    clean.clean = true;
    let clean = write_record(&scratch.0, &clean);
    let other = Consent {
        generation: "f".repeat(32),
        enabled_at_ms: 1,
    };
    let foreign = plant_marker(&scratch.0, &other, Process::App, 2, 1_000);
    let corrupt = runs_dir(&scratch.0).join(format!("{}{MARKER_EXT}", "a".repeat(32)));
    fs::write(&corrupt, b"KRUN1 R - A garbage\n").unwrap();
    let stray = runs_dir(&scratch.0).join("notes.txt");
    fs::write(&stray, b"not a marker").unwrap();
    let dead = plant_marker(&scratch.0, &consent, Process::Daemon, 3, 1_000);

    let stale = Stale::scan(&scratch.0, &consent);
    assert_eq!(stale.len(), 1);
    assert!(!clean.exists() && !foreign.exists() && !corrupt.exists());
    assert!(
        dead.exists(),
        "a dead run of this generation waits for settle"
    );
    assert!(stray.exists(), "only marker names are considered");
    drop(stale);
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn a_lock_on_an_orphaned_inode_is_not_a_dead_run() {
    let (scratch, consent) = consented("orphan");
    let generation = generation_hash(&consent);
    // 스캐너가 연 뒤 다른 스캐너가 지웠다: 잠금은 얻어도 끝난 실행이 아니다.
    let gone = plant_marker(&scratch.0, &consent, Process::Daemon, 8, 1_000);
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&gone)
        .unwrap();
    fs::remove_file(&gone).unwrap();
    assert!(claim_open(opened, gone, &generation).is_none());
    // 같은 이름에 새 파일이 생겼어도 옛 inode의 잠금으로 회수하거나 지우지 않는다.
    let path = plant_marker(&scratch.0, &consent, Process::Daemon, 9, 1_000);
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let replacement = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, &replacement).unwrap();
    assert!(claim_open(opened, path.clone(), &generation).is_none());
    assert!(path.exists(), "the replacement marker is left alone");
    assert_eq!(
        Stale::scan(&scratch.0, &consent).len(),
        1,
        "the replacement itself is still reclaimable"
    );
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn dead_run_waits_for_grace_then_records_one_unclean_exit() {
    let (scratch, consent) = consented("grace");
    let marker = plant_marker(&scratch.0, &consent, Process::Daemon, 77, 1_000);
    let run_id = record_of(&marker).run_id;
    let now = now_ms();

    assert_eq!(sweep(&scratch.0, &consent, now), 0);
    assert_eq!(
        record_of(&marker).seen_ms,
        now,
        "first sighting starts the grace"
    );
    let almost = now.saturating_add(GRACE_MS).saturating_sub(1);
    assert_eq!(sweep(&scratch.0, &consent, almost), 0);
    assert!(queued(&scratch.0).is_empty());

    assert_eq!(sweep(&scratch.0, &consent, now.saturating_add(GRACE_MS)), 1);
    assert!(!marker.exists());
    let events = queued(&scratch.0);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(
        event["event_id"],
        event_id(ABNORMAL_DOMAIN, run_id.as_bytes())
    );
    assert_eq!(event["process"], "krilld");
    assert_eq!(event["kind"], "abnormal_exit");
    assert_eq!(event["error_code"], "unclean_exit");
    assert_eq!(event["release"], "0.1.40", "the dead run's own release");
    assert_eq!(event["build_id"], "0.1.40+dev.test");
    assert!(event["native"].is_null() && event["source_loc"].is_null());

    let later = now.saturating_add(GRACE_MS).saturating_add(GRACE_MS);
    assert_eq!(sweep(&scratch.0, &consent, later), 0, "never counted twice");
    assert_eq!(queued(&scratch.0).len(), 1);
}

#[test]
fn panic_or_native_evidence_suppresses_unclean_exit() {
    let (scratch, consent) = consented("evidence");
    let mut panicked = sample(&consent, Process::App, 10, 1_000);
    panicked.panicked = true;
    let panicked = write_record(&scratch.0, &panicked);
    let crashed = plant_marker(&scratch.0, &consent, Process::Daemon, 4242, 1_000_000);

    let mut stale = Stale::scan(&scratch.0, &consent);
    assert_eq!(stale.len(), 2);
    for run in &mut stale {
        if run.matches(Process::Daemon, 4242, 1_000_500, Some(999_000)) {
            run.claim_native();
        }
    }
    let far = now_ms().saturating_add(GRACE_MS).saturating_add(GRACE_MS);
    assert_eq!(settle(&scratch.0, &consent, far, stale), 0);
    assert!(!panicked.exists() && !crashed.exists());
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn native_match_requires_same_process_pid_and_time_window() {
    let (scratch, consent) = consented("match");
    plant_marker(&scratch.0, &consent, Process::Daemon, 100, 10_000_000);
    let stale = Stale::scan(&scratch.0, &consent);
    let run = &stale[0];
    assert!(run.matches(Process::Daemon, 100, 10_000_000, Some(9_999_000)));
    assert!(
        run.matches(Process::Daemon, 100, 9_999_000, None),
        "second truncation"
    );
    assert!(run.matches(Process::Daemon, 100, 10_000_000, Some(9_400_000)));
    assert!(!run.matches(Process::App, 100, 10_000_000, None));
    assert!(!run.matches(Process::Daemon, 101, 10_000_000, None));
    assert!(
        !run.matches(Process::Daemon, 100, 9_997_000, None),
        "crashed before the run"
    );
    assert!(
        !run.matches(Process::Daemon, 100, 10_000_000, Some(10_003_000)),
        "launched after the marker"
    );
    assert!(
        !run.matches(Process::Daemon, 100, 10_000_000, Some(9_399_999)),
        "launched too long before the marker"
    );
}

#[test]
fn busy_or_revoked_queue_keeps_the_marker_until_it_gives_up() {
    let (scratch, consent) = consented("giveup");
    crate::error_reporting::disable(&scratch.0).unwrap();
    crate::error_reporting::enable(&scratch.0).unwrap();
    // 옛 세대로는 기록할 수 없다(record_with_consent가 false).
    let marker = plant_marker(&scratch.0, &consent, Process::App, 5, 1_000);
    let now = now_ms();
    assert_eq!(sweep(&scratch.0, &consent, now), 0);
    assert_eq!(sweep(&scratch.0, &consent, now.saturating_add(GRACE_MS)), 0);
    assert!(marker.exists(), "not recorded yet: retried on a later pass");
    let seen = record_of(&marker).seen_ms;
    assert_eq!(
        sweep(&scratch.0, &consent, seen.saturating_add(GIVE_UP_MS)),
        0
    );
    assert!(!marker.exists(), "bounded: dropped after a week");
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn start_refuses_when_the_marker_directory_is_full() {
    let (scratch, consent) = consented("full");
    for pid in 0..u32::try_from(MAX_MARKERS).unwrap() {
        plant_marker(&scratch.0, &consent, Process::App, pid, 1_000);
    }
    assert!(
        RunGuard::start_with(&scratch.0, Process::App, &consent)
            .unwrap()
            .is_none()
    );
    assert_eq!(markers(&scratch.0).len(), MAX_MARKERS);
}

#[test]
fn start_across_a_consent_change_leaves_only_a_clean_marker() {
    let (scratch, consent) = consented("recheck");
    let old = Consent {
        generation: "0".repeat(32),
        enabled_at_ms: consent.enabled_at_ms,
    };
    assert!(
        RunGuard::start_with(&scratch.0, Process::App, &old)
            .unwrap()
            .is_none()
    );
    assert!(record_of(&single_marker(&scratch.0)).clean);
    assert!(Stale::scan(&scratch.0, &consent).is_empty());
    assert!(markers(&scratch.0).is_empty());
}

#[test]
fn symlinked_marker_is_neither_followed_nor_removed() {
    let (scratch, consent) = consented("symlink");
    let record = sample(&consent, Process::App, 6, 1_000);
    let target = scratch.0.join("elsewhere.run");
    fs::write(&target, record.encode().unwrap()).unwrap();
    fs::create_dir_all(runs_dir(&scratch.0)).unwrap();
    let link = runs_dir(&scratch.0).join(format!("{}{MARKER_EXT}", record.run_id));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(Stale::scan(&scratch.0, &consent).is_empty());
    assert_eq!(fs::read(&target).unwrap(), record.encode().unwrap());
}

#[test]
fn live_marker_in_another_process_is_not_reclaimed() {
    let (scratch, consent) = consented("live");
    let helper = Helper::spawn("hold", &scratch.0);
    assert!(Stale::scan(&scratch.0, &consent).is_empty());
    assert_eq!(sweep(&scratch.0, &consent, now_ms()), 0);
    let record = record_of(&single_marker(&scratch.0));
    assert_eq!(record.pid, helper.child.id());
    assert!(!record.clean);
    assert_eq!(
        record.seen_ms, 0,
        "the scanner never writes to a live marker"
    );
    drop(helper);
}

#[test]
fn sigkilled_run_is_reclaimed_after_grace_as_unclean_exit() {
    let (scratch, consent) = consented("sigkill");
    let mut helper = Helper::spawn("hold", &scratch.0);
    let pid = helper.child.id();
    helper.child.kill().unwrap();
    helper.child.wait().unwrap();
    let marker = single_marker(&scratch.0);
    let record = record_of(&marker);
    assert_eq!(record.pid, pid);
    assert!(!record.clean);
    let now = now_ms();
    assert_eq!(sweep(&scratch.0, &consent, now), 0);
    assert_eq!(sweep(&scratch.0, &consent, now.saturating_add(GRACE_MS)), 1);
    let events = queued(&scratch.0);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["event_id"],
        event_id(ABNORMAL_DOMAIN, record.run_id.as_bytes())
    );
    assert_eq!(events[0]["error_code"], "unclean_exit");
    assert!(!marker.exists());
}

#[test]
fn marker_lock_is_not_inherited_by_child_processes() {
    let (scratch, consent) = consented("inherit");
    let mut helper = Helper::spawn("spawn", &scratch.0);
    let grandchild: u32 = helper.line.parse().unwrap();
    let _reaper = Reaper(grandchild);
    helper.child.kill().unwrap();
    helper.child.wait().unwrap();
    assert!(alive(grandchild), "the child must outlive the marker owner");
    let stale = Stale::scan(&scratch.0, &consent);
    assert_eq!(stale.len(), 1, "a child process must not keep the run lock");
}

/// 음성 대조: fd가 CLOEXEC가 아니면 자식이 같은 잠금을 물려받아 스캐너가 살아 있는 실행으로 본다. 위 상속
/// 테스트가 실제 잠금 상속을 잡아낸다는 근거다.
#[test]
fn an_inherited_descriptor_would_keep_the_run_alive() {
    let (scratch, consent) = consented("leak");
    let mut helper = Helper::spawn("leak", &scratch.0);
    let grandchild: u32 = helper.line.parse().unwrap();
    let reaper = Reaper(grandchild);
    helper.child.kill().unwrap();
    helper.child.wait().unwrap();
    assert!(alive(grandchild));
    assert!(
        Stale::scan(&scratch.0, &consent).is_empty(),
        "the leaked descriptor still holds the run lock"
    );
    drop(reaper);
    let mut reclaimed = Vec::new();
    for _ in 0..100 {
        reclaimed = Stale::scan(&scratch.0, &consent);
        if !reclaimed.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(reclaimed.len(), 1, "released once the inheritor exits");
}

#[test]
fn panic_hook_marks_the_run_and_suppresses_unclean_exit() {
    let (scratch, consent) = consented("panic");
    let mut helper = Helper::spawn("panic", &scratch.0);
    let status = helper.child.wait().unwrap();
    assert!(!status.success());
    let marker = single_marker(&scratch.0);
    let record = record_of(&marker);
    assert!(record.panicked, "note_panic writes the flag in place");
    assert!(!record.clean, "unwinding is not a clean exit");
    let far = now_ms().saturating_add(GRACE_MS).saturating_add(GRACE_MS);
    assert_eq!(sweep(&scratch.0, &consent, far), 0);
    assert!(!marker.exists());
    assert!(queued(&scratch.0).is_empty());
}

proptest::proptest! {
    #[test]
    fn record_decode_never_panics(
        bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..400)
    ) {
        core::hint::black_box(Record::decode(&bytes));
    }

    #[test]
    fn record_round_trips_through_the_fixed_layout(
        clean in proptest::prelude::any::<bool>(),
        panicked in proptest::prelude::any::<bool>(),
        daemon in proptest::prelude::any::<bool>(),
        pid in proptest::prelude::any::<u32>(),
        started_at_ms in proptest::prelude::any::<u64>(),
        seen_ms in proptest::prelude::any::<u64>(),
        run_id in "[0-9a-f]{32}",
        generation in "[0-9a-f]{32}",
        release in "[0-9]{1,9}\\.[0-9]{1,9}\\.[0-9]{1,9}(-[0-9A-Za-z.]{1,20})?",
        build_id in "[A-Za-z0-9.+-]{1,80}",
    ) {
        let record = Record {
            clean,
            panicked,
            process: if daemon { Process::Daemon } else { Process::App },
            pid,
            started_at_ms,
            run_id,
            generation,
            seen_ms,
            release,
            build_id,
        };
        let bytes = record.encode().unwrap();
        proptest::prop_assert_eq!(bytes.len(), RECORD_LEN);
        proptest::prop_assert_eq!(bytes[6], if clean { CLEAN } else { b'R' });
        proptest::prop_assert_eq!(bytes[8], if panicked { PANICKED } else { b'-' });
        let seen = format!("{seen_ms:020}");
        proptest::prop_assert_eq!(&bytes[110..130], seen.as_bytes());
        proptest::prop_assert_eq!(Record::decode(&bytes), Some(record));
    }
}
