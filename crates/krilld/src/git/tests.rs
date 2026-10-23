use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;

use super::*;

/// 테스트마다 임시 디렉터리.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mkgit-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        Self(d)
    }

    /// 실행 가능한 가짜 git 스크립트.
    fn script(&self, body: &str) -> PathBuf {
        let p = self.0.join("fake-git");
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// 완료 조건: 동시 요청 20개(레포 5개 × 4)에서 git 동시 실행 ≤ 4, 같은 레포는 겹치지 않고 제출 순서대로.
#[test]
fn twenty_requests_cap_concurrency_and_serialize_per_repo() {
    let dir = Dir::new("cap");
    let log = dir.0.join("log");
    // O_APPEND 한 줄 쓰기는 원자적이라 줄이 섞이지 않는다. 인자: -c core.quotePath=false <n>
    let git = dir.script(&format!(
        "echo \"start ${{PWD##*/}} $3\" >> '{l}'\nsleep 0.2\necho \"end ${{PWD##*/}} $3\" >> '{l}'",
        l = log.display()
    ));
    let repos: Vec<PathBuf> = (0..5)
        .map(|r| {
            let p = dir.0.join(format!("r{r}"));
            fs::create_dir(&p).unwrap();
            p
        })
        .collect();
    let queue = GitQueue::with_program(git, MAX_CONCURRENT).unwrap();

    // 20개를 한꺼번에 넣는다: 0번→r0, 1번→r1, … (레포마다 4개).
    let rxs: Vec<_> = (0..20)
        .map(|n| queue.submit(&repos[n % 5], [n.to_string()]))
        .collect();
    for rx in rxs {
        assert!(rx.recv().unwrap().unwrap().status.success());
    }

    let text = fs::read_to_string(&log).unwrap();
    let (mut running, mut max) = (0usize, 0usize);
    let mut busy = HashSet::new();
    let mut order: HashMap<&str, Vec<usize>> = HashMap::new();
    for line in text.lines() {
        let mut w = line.split(' ');
        let (ev, repo, n) = (w.next().unwrap(), w.next().unwrap(), w.next().unwrap());
        if ev == "start" {
            running += 1;
            max = max.max(running);
            assert!(busy.insert(repo), "{repo} 동시 실행: {text}");
            order.entry(repo).or_default().push(n.parse().unwrap());
        } else {
            running -= 1;
            busy.remove(repo);
        }
    }
    assert_eq!(text.lines().count(), 40);
    assert!(max <= MAX_CONCURRENT, "동시 {max}개");
    // 레포가 5개라 상한까지는 실제로 병렬이어야 한다(직렬 큐로 통과하는 것 방지).
    assert!(max >= 2, "병렬 실행이 없었다: {max}");
    for (repo, ns) in order {
        assert!(ns.is_sorted(), "{repo} 순서 {ns:?}");
        assert_eq!(ns.len(), 4);
    }
}

#[test]
fn fixed_options_and_env_reach_git() {
    let dir = Dir::new("env");
    let git =
        dir.script("printf '%s\\n%s\\n' \"$GIT_OPTIONAL_LOCKS $GIT_TERMINAL_PROMPT ${GIT_DIR-none} $*\" \"$PATH\"");
    let queue = GitQueue::with_program(git, 1).unwrap();
    let out = queue.submit(&dir.0, ["status"]).recv().unwrap().unwrap();
    let want = format!(
        "0 0 none -c core.quotePath=false status\n{}\n",
        String::from_utf8_lossy(crate::shell::login_path())
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), want);
}

/// 실제 git이 고정 옵션을 받는다(`core.quotePath`가 사용자 설정과 무관하게 false).
#[test]
fn real_git_sees_quote_path_false() {
    let dir = Dir::new("real");
    let queue = GitQueue::new().unwrap();
    let out = queue
        .submit(&dir.0, ["config", "--get", "core.quotePath"])
        .recv()
        .unwrap()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"false\n");
}

/// 버리면 시작 전 작업은 채널이 끊기고, 실행 중 작업은 끝까지 돈다. 멈추지 않는다.
#[test]
fn drop_finishes_running_and_discards_pending() {
    let dir = Dir::new("drop");
    let git = dir.script("sleep 0.2; echo done");
    let queue = GitQueue::with_program(git, 1).unwrap();
    let first = queue.submit(&dir.0, ["a"]);
    let second = queue.submit(&dir.0, ["b"]);
    // 첫 작업이 워커에 잡힐 때까지 기다린다.
    while !lock(&queue.shared.state)
        .busy
        .contains(&fs::canonicalize(&dir.0).unwrap())
    {
        thread::yield_now();
    }
    drop(queue);
    assert_eq!(first.recv().unwrap().unwrap().stdout, b"done\n");
    assert!(second.recv().is_err());
}

#[test]
fn hanging_git_times_out_reaps_child_and_queue_proceeds() {
    let dir = Dir::new("timeout");
    let pid_file = dir.0.join("pids");
    let git = dir.script(&format!(
        "if [ \"$3\" = \"hang\" ]; then\n  sleep 5 &\n  printf '%s\\n%s\\n' \"$$\" \"$!\" > '{}'\n  wait\nfi\necho \"ok:$3\"",
        pid_file.display()
    ));
    let queue = GitQueue::with_program(git, 1).unwrap();
    let hung = queue.submit(&dir.0, ["hang"]);
    let expired = queue.submit(&dir.0, ["next"]);
    let err = hung.recv().unwrap().expect_err("hanging git must time out");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err:?}");
    let pids: Vec<i32> = fs::read_to_string(&pid_file)
        .unwrap()
        .lines()
        .map(|l| l.trim().parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    for raw in pids {
        let pid = rustix::process::Pid::from_raw(raw).unwrap();
        let deadline = Instant::now() + Duration::from_millis(200);
        while rustix::process::test_kill_process(pid).is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            rustix::process::test_kill_process(pid).is_err(),
            "timed-out git or hook child {raw} survived"
        );
    }

    assert_eq!(
        expired.recv().unwrap().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    let out = queue.submit(&dir.0, ["next"]).recv().unwrap().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"ok:next\n");
}

/// K-15: 타임아웃 때는 SIGTERM 유예를 먼저 줘 git이 `index.lock`을 정리하고 죽는다.
#[test]
fn timeout_sends_term_first_so_git_cleans_up_locks() {
    let dir = Dir::new("term");
    let held = dir.0.join("index.lock");
    // `exec` 없이 sleep을 자식으로 두어 실제 git처럼 프로세스 안에서 TERM을 받아 정리한다
    // (`exec`는 트랩을 버리고 자식 sleep만 죽어 정리를 검증할 수 없다).
    let git = dir.script(&format!(
        "if [ \"$3\" = \"locking\" ]; then touch '{l}'; trap 'rm -f \"{l}\"; exit 0' TERM; sleep 30; fi\necho ok",
        l = held.display()
    ));
    let queue = GitQueue::with_program(git, 1).unwrap();
    // 기한을 넘겼으므로 결과는 여전히 타임아웃이다. 중요한 것은 TERM 유예 안에
    // git이 `index.lock`을 정리하고 자식이 회수되는 것이다.
    let err = queue
        .submit(&dir.0, ["locking"])
        .recv()
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err:?}");
    assert!(!held.exists(), "SIGKILL-only shutdown left the lock behind");
}

/// K-15: TERM을 무시하는 자식은 유예 뒤 기존처럼 그룹 SIGKILL로 회수된다.
#[test]
fn timeout_escalates_to_kill_when_term_is_ignored() {
    let dir = Dir::new("term-escalate");
    let git = dir.script(
        "if [ \"$3\" = \"hang\" ]; then echo $$ > pid; trap '' TERM; exec sleep 30; fi\necho ok",
    );
    let queue = GitQueue::with_program(git, 1).unwrap();
    let start = Instant::now();
    let err = queue.submit(&dir.0, ["hang"]).recv().unwrap().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err:?}");
    assert!(
        start.elapsed() < TIMEOUT + TERMINATE_GRACE + Duration::from_secs(5),
        "escalation took too long"
    );
    let pid: i32 = fs::read_to_string(dir.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
    // 큐는 다음 작업을 계속 처리한다.
    let out = queue.submit(&dir.0, ["next"]).recv().unwrap().unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn cross_review_term_reaps_descendants_when_parent_exits_first() {
    use std::os::unix::process::CommandExt as _;
    let dir = Dir::new("cross-term-child");
    let mut parent = Command::new("/bin/sh")
        .args(["-c", "sh -c 'trap \"\" TERM; echo $$ > survivor; exec sleep 30' & while [ ! -f survivor ]; do sleep 0.01; done; trap 'exit 0' TERM; wait"])
        .current_dir(&dir.0).process_group(0).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let path = dir.0.join("survivor");
    while !path.exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let raw = fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let pid = rustix::process::Pid::from_raw(raw).unwrap();
    terminate_gracefully(&mut parent);
    let deadline = Instant::now() + Duration::from_secs(1);
    while rustix::process::test_kill_process(pid).is_ok() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let survived = rustix::process::test_kill_process(pid).is_ok();
    rustix::process::kill_process(pid, rustix::process::Signal::KILL).ok();
    assert!(
        !survived,
        "TERM cleanup must reap a child that ignores TERM after git exits"
    );
}

#[test]
fn speculative_checkout_cancels_queued_and_running_children() {
    let dir = Dir::new("speculative-cancel");
    let pid_file = dir.0.join("child-pid");
    let program = dir.script(&format!(
        "if [ \"$3\" = hang ]; then echo $$ > '{}'; exec sleep 30; fi\necho ok",
        pid_file.display()
    ));
    let queue = GitQueue::with_program(program, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(true));
    let result = queue
        .speculative(
            dir.0.clone(),
            ["hang"],
            cancel.clone(),
            dir.0.join("tree"),
            1024,
        )
        .recv()
        .unwrap()
        .unwrap_err();
    assert_eq!(result.kind(), io::ErrorKind::Interrupted);
    assert!(!pid_file.exists(), "cancelled queued job must never spawn");
    cancel.store(false, Ordering::Release);
    let rx = queue.speculative(
        dir.0.clone(),
        ["hang"],
        cancel.clone(),
        dir.0.join("tree"),
        1024,
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !pid_file.exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let pid = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    cancel.store(true, Ordering::Release);
    assert_eq!(
        rx.recv().unwrap().unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
    assert!(
        queue
            .submit(&dir.0, ["next"])
            .recv()
            .unwrap()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn speculative_checkout_stops_on_disk_growth_and_reaps_child() {
    let dir = Dir::new("speculative-budget");
    let tree = dir.0.join("tree");
    fs::create_dir(&tree).unwrap();
    let program = dir.script("echo $$ > child-pid; dd if=/dev/zero of=tree/file bs=1024 count=128 2>/dev/null; exec sleep 30");
    let queue = GitQueue::with_program(program, 1).unwrap();
    let result = queue
        .speculative(
            dir.0.clone(),
            ["grow"],
            Arc::new(AtomicBool::new(false)),
            tree,
            64 * 1024,
        )
        .recv()
        .unwrap()
        .unwrap_err();
    assert!(result.to_string().contains("disk budget"));
    let pid = fs::read_to_string(dir.0.join("child-pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
}
