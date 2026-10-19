use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;
use std::thread;

pub(crate) struct Dir(pub PathBuf);
impl Dir {
    pub fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mkchanges-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
    pub fn script(&self, body: &str) -> PathBuf {
        let path = self.0.join("fake-git");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    pub fn query(&self) -> Changes {
        let q = GitQueue::new().unwrap();
        query(&q, self.0.clone(), Arc::new(AtomicBool::new(false)))
            .recv()
            .unwrap()
            .unwrap()
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn real_git_staged_unstaged_binary_rename_and_untracked_paths() {
    let dir = Dir::new("real");
    dir.git(&["init", "-q"]);
    fs::write(dir.0.join("text"), "one\ntwo\n").unwrap();
    fs::write(dir.0.join("old"), "rename\n").unwrap();
    fs::write(dir.0.join("binary"), [0, 1, 2]).unwrap();
    dir.git(&["add", "."]);
    dir.git(&["commit", "-qm", "initial"]);
    fs::write(dir.0.join("text"), "one\ntwo\nthree\n").unwrap();
    fs::write(dir.0.join("binary"), [0, 9, 2]).unwrap();
    dir.git(&["add", "text", "binary"]);
    fs::write(dir.0.join("text"), "one\nthree\n").unwrap();
    dir.git(&["mv", "old", "새 이름"]);
    fs::write(dir.0.join("tab\tline\n🦐"), "untracked").unwrap();
    let result = dir.query();
    assert!(!result.limited);
    assert_eq!(result.files.len(), 4);
    let text = result.files.iter().find(|f| f.path == b"text").unwrap();
    assert_eq!(text.status, *b"MM");
    assert_eq!(
        text.staged,
        Some(Numstat {
            added: Some(1),
            deleted: Some(0)
        })
    );
    assert_eq!(
        text.unstaged,
        Some(Numstat {
            added: Some(0),
            deleted: Some(1)
        })
    );
    let binary = result.files.iter().find(|f| f.path == b"binary").unwrap();
    assert_eq!(
        binary.staged,
        Some(Numstat {
            added: None,
            deleted: None
        })
    );
    let rename = result
        .files
        .iter()
        .find(|f| f.path == "새 이름".as_bytes())
        .unwrap();
    assert_eq!(rename.old_path.as_deref(), Some(b"old".as_slice()));
    assert_eq!(
        rename.staged,
        Some(Numstat {
            added: Some(0),
            deleted: Some(0)
        })
    );
    assert!(
        result
            .files
            .iter()
            .any(|f| f.path == "tab\tline\n🦐".as_bytes())
    );
}

#[test]
fn unborn_head_and_real_thousand_file_limit() {
    let dir = Dir::new("thousand");
    dir.git(&["init", "-q"]);
    fs::write(dir.0.join("staged"), "first\n").unwrap();
    dir.git(&["add", "staged"]);
    let initial = dir.query();
    assert_eq!(
        initial.files[0].staged,
        Some(Numstat {
            added: Some(1),
            deleted: Some(0)
        })
    );
    for n in 0..1_100 {
        fs::write(dir.0.join(format!("file-{n:04}")), "untracked\n").unwrap();
    }
    let result = dir.query();
    assert_eq!(result.files.len(), MAX_FILES);
    assert!(result.limited);
}

#[test]
fn cap_reaps_a_producer_that_never_closes_stdout() {
    let dir = Dir::new("reap");
    let script = dir.script("case \"$3\" in\nstatus) echo $$ > pid; i=0; while [ $i -lt 1200 ]; do printf '? f%s\\0' \"$i\"; i=$((i+1)); done; exec sleep 30;;\n*) exit 0;;\nesac");
    let q = GitQueue::with_program(script, 1).unwrap();
    let start = Instant::now();
    let result = query(&q, dir.0.clone(), Arc::new(AtomicBool::new(false)))
        .recv()
        .unwrap()
        .unwrap();
    assert_eq!(result.files.len(), MAX_FILES);
    assert!(result.limited);
    assert!(start.elapsed() < TIMEOUT);
    let pid: i32 = fs::read_to_string(dir.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "child survived query"
    );
}

#[test]
fn cancellation_reaps_running_and_skips_queued_work() {
    let dir = Dir::new("cancel");
    let script = dir.script("echo $$ > pid\nexec sleep 30");
    let q = GitQueue::with_program(script, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let rx = query(&q, dir.0.clone(), Arc::clone(&cancel));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !dir.0.join("pid").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    cancel.store(true, Ordering::Release);
    assert!(rx.recv().unwrap().unwrap_err().contains("cancelled"));
    let pid: i32 = fs::read_to_string(dir.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
    fs::remove_file(dir.0.join("pid")).unwrap();
    assert!(query(&q, dir.0.clone(), cancel).recv().unwrap().is_err());
    assert!(!dir.0.join("pid").exists());
}

#[test]
fn timeout_and_parse_failure_reap_children() {
    let dir = Dir::new("timeout");
    for (body, error) in [
        ("echo $$ > pid; exec sleep 30", "timed out"),
        (
            "echo $$ > pid; printf 'invalid\\0'; exec sleep 30",
            "invalid porcelain",
        ),
    ] {
        let q = GitQueue::with_program(dir.script(body), 1).unwrap();
        let result = query(&q, dir.0.clone(), Arc::new(AtomicBool::new(false)))
            .recv()
            .unwrap();
        assert!(result.unwrap_err().contains(error));
        let pid: i32 = fs::read_to_string(dir.0.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
    }
}

#[test]
fn parser_conflict_and_rename_with_tabs() {
    let mut raw = StatusParser::default();
    raw.record(b"? \xffx").unwrap();
    assert_eq!(raw.changes.files[0].path, b"\xffx");
    let mut status = StatusParser::default();
    status
        .record(b"u UU N... 100644 100644 100644 100644 a b c conflict")
        .unwrap();
    assert_eq!(status.changes.files[0].status, *b"UU");
    status
        .record(b"2 R. N... 100644 100644 100644 a b R100 new\tpath")
        .unwrap();
    status.record(b"old\npath").unwrap();
    assert_eq!(
        status.changes.files[1].old_path.as_deref(),
        Some(b"old\npath".as_slice())
    );
    let mut stats = StatsParser::default();
    assert!(stats.record(b"2\t3\t").unwrap().is_none());
    assert!(stats.record(b"old\npath").unwrap().is_none());
    assert_eq!(
        stats.record(b"new\tpath").unwrap(),
        Some((
            b"new\tpath".to_vec(),
            Numstat {
                added: Some(2),
                deleted: Some(3)
            }
        ))
    );
}

proptest::proptest! {
    #[test]
    fn arbitrary_records_do_not_panic(records in proptest::collection::vec(proptest::collection::vec(proptest::num::u8::ANY, 0..300), 0..40)) {
        let mut status = StatusParser::default();
        let mut stats = StatsParser::default();
        for record in records { status.record(&record).ok(); stats.record(&record).ok(); }
    }
}

#[test]
fn path_budget_stops_before_adding_an_oversized_entry() {
    let mut parser = StatusParser::default();
    let record = [b"? ".as_slice(), &vec![b'a'; 32_000]].concat();
    while !parser.record(&record).unwrap() {}
    assert!(parser.changes.limited);
    assert!(parser.changes.files.len() < MAX_FILES);
    assert!(
        parser
            .changes
            .files
            .iter()
            .map(|f| f.path.len())
            .sum::<usize>()
            <= MAX_PATH_BYTES
    );
}

#[test]
fn status_branch_header_matches_show_current() {
    let dir = Dir::new("branch");
    let q = GitQueue::new().unwrap();
    let branch = |dir: &Dir| {
        query_with_branch(&q, dir.0.clone(), Arc::new(AtomicBool::new(false)))
            .recv()
            .unwrap()
            .unwrap()
            .1
    };
    let show_current = |dir: &Dir| {
        let out = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&dir.0)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    dir.git(&["init", "-q", "-b", "새-브랜치"]);
    assert_eq!(branch(&dir), "새-브랜치", "unborn HEAD");
    fs::write(dir.0.join("a"), "a\n").unwrap();
    dir.git(&["add", "a"]);
    dir.git(&["commit", "-qm", "a"]);
    assert_eq!(branch(&dir), show_current(&dir));
    dir.git(&["checkout", "-q", "--detach"]);
    assert_eq!(branch(&dir), "");
    assert_eq!(show_current(&dir), "");
    let mut status = StatusParser::default();
    status.record(b"# branch.head (detached)").unwrap();
    assert_eq!(status.branch, "");
}
