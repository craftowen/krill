#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

const HELPER_ROLE: &str = "KRILL_LEASE_TEST_ROLE";
const HELPER_PATH: &str = "KRILL_LEASE_TEST_PATH";
const HELPER_MARKER: &str = "KRILL_LEASE_TEST_MARKER";
const HELPER_CHILD: &str = "KRILL_LEASE_TEST_CHILD";
const HELPER_PIDFILE: &str = "KRILL_LEASE_TEST_PIDFILE";

fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("krill-lifecycle-{}-{tag}-{n}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn lease_path(tag: &str) -> PathBuf {
    scratch(tag).join("lease").join("input.lock")
}

fn after(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script);
    cmd
}

fn opts(lease: Option<&InputLease>, deadline: Instant) -> RunOpts<'_> {
    RunOpts {
        lease: lease.map(|l| (l, LeaseSlot::Stdin)),
        stdin: None,
        deadline,
        cancel: None,
        max_output: 64 * 1024,
    }
}

fn succeeded(fin: &Finished) -> bool {
    matches!(fin.end, End::Exited(status) if status.success())
}

fn pid_gone(pid: i32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    rustix::process::test_kill_process(pid).is_err()
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    done()
}

/// 도우미 역할로 이 테스트 바이너리를 다시 실행하는 인자.
fn helper_args() -> [String; 4] {
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    [
        "--exact".to_owned(),
        format!("{module}::lease_helper_process"),
        "--nocapture".to_owned(),
        "--test-threads=1".to_owned(),
    ]
}

/// 이 테스트 바이너리를 다시 실행해 도우미 역할을 맡긴다(실제 별도 프로세스·SIGKILL 시험용).
fn helper(role: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(helper_args())
        .env(HELPER_ROLE, role)
        .stdin(Stdio::null());
    cmd
}

/// `sh -c`로 도우미를 띄우는 셸 명령 조각(역할은 환경변수로 넘긴다).
fn helper_shell() -> String {
    let exe = std::env::current_exe().unwrap();
    let args: Vec<String> = helper_args().iter().map(|a| format!("'{a}'")).collect();
    format!("'{}' {}", exe.display(), args.join(" "))
}

/// 이 프로세스의 자식 `pid`가 끝났지만 아직 회수되지 않았는지(좀비).
fn is_unreaped_zombie(pid: i32) -> bool {
    use rustix::process::{WaitId, WaitIdOptions, waitid};
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    let flags = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    matches!(waitid(WaitId::Pid(pid), flags), Ok(Some(_)))
}

fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// 도우미 프로세스 진입점. 역할 환경변수가 없으면(일반 테스트 실행) 아무것도 하지 않는다.
#[test]
fn lease_helper_process() {
    let Some(role) = std::env::var_os(HELPER_ROLE) else {
        return;
    };
    match role.to_str().unwrap() {
        "print-path" => println!("LEASE_PATH={}", default_lease_path().display()),
        // 자기 세션으로 빠져나가(그룹 신호가 닿지 않는다) 물려받은 출력 파이프를 쥔 채 오래 산다.
        "escape" => {
            let pid = rustix::process::setsid().unwrap();
            fs::write(
                std::env::var_os(HELPER_PIDFILE).unwrap(),
                pid.as_raw_nonzero().to_string(),
            )
            .unwrap();
            thread::sleep(Duration::from_secs(20));
        }
        "hold" => {
            let path = PathBuf::from(std::env::var_os(HELPER_PATH).unwrap());
            let marker = PathBuf::from(std::env::var_os(HELPER_MARKER).unwrap());
            let child = std::env::var(HELPER_CHILD).unwrap();
            let lease = InputLease::acquire(&path, after(5_000), None).unwrap();
            let cmd = match child.as_str() {
                "sh" => sh("echo $$ > \"$KRILL_LEASE_TEST_PIDFILE\"; exec /bin/sleep 2"),
                "osascript" => {
                    let mut cmd = Command::new("/usr/bin/osascript");
                    cmd.args(["-l", "JavaScript", "-e", "delay(2)"]);
                    cmd
                }
                other => panic!("unknown helper child {other}"),
            };
            fs::write(&marker, b"spawning").unwrap();
            let fin = run(
                cmd,
                &RunOpts {
                    lease: Some((&lease, LeaseSlot::Stdin)),
                    stdin: None,
                    deadline: after(30_000),
                    cancel: None,
                    max_output: 1024,
                },
            );
            // 정상이라면 부모가 SIGKILL해서 여기까지 오지 않는다.
            fs::write(&marker, format!("finished {fin:?}")).unwrap();
        }
        other => panic!("unknown helper role {other}"),
    }
}

#[test]
fn routing_marks_every_input_path_as_mutation() {
    let none: [String; 0] = [];
    let restore = ["--restore-window".to_owned()];
    let id = ["--id".to_owned(), "accessibility".to_owned()];
    for observe in [
        "capabilities",
        "list-apps",
        "list-windows",
        "permissions",
        "get-app-state",
        "snapshot",
    ] {
        assert_eq!(access_for(observe, &none), Access::Observe, "{observe}");
    }
    assert_eq!(access_for("get-app-state", &restore), Access::Mutate);
    assert_eq!(access_for("snapshot", &restore), Access::Mutate);
    assert_eq!(access_for("permissions", &id), Access::Mutate);
    for mutate in [
        "click",
        "perform-secondary-action",
        "secondary-action",
        "scroll",
        "drag",
        "type-text",
        "press-key",
        "hotkey",
        "paste-text",
        "set-value",
        "some-future-command",
    ] {
        assert_eq!(access_for(mutate, &none), Access::Mutate, "{mutate}");
    }
}

#[test]
fn default_path_is_shared_across_state_dirs_and_env() {
    let expected = default_lease_path();
    let uid = rustix::process::geteuid().as_raw();
    // /tmp는 periodic 정리가 잠금 inode를 지울 수 있어 쓰지 않는다. 네이티브 실행기도 같은 경로를 쓴다.
    #[cfg(target_os = "macos")]
    assert_eq!(
        expected,
        PathBuf::from(format!("/Users/Shared/.krill-computer-{uid}/input.lock"))
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        expected,
        PathBuf::from(format!("/tmp/krill-computer-{uid}/input.lock"))
    );
    let out = helper("print-path")
        .env("KRILL_STATE_DIR", "/nonexistent/dev-state")
        .env("HOME", "/nonexistent/home")
        .env("TMPDIR", "/nonexistent/tmp")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let printed = stdout
        .lines()
        .find_map(|l| l.split_once("LEASE_PATH=").map(|(_, path)| path.trim()))
        .unwrap_or_else(|| panic!("helper printed no path: {stdout}"));
    assert_eq!(PathBuf::from(printed), expected);
}

#[test]
fn concurrent_contenders_never_overlap() {
    let path = lease_path("contend");
    let active = AtomicUsize::new(0);
    let entered = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..5 {
                    let lease = InputLease::acquire(&path, after(20_000), None).unwrap();
                    assert_eq!(
                        active.fetch_add(1, Ordering::SeqCst),
                        0,
                        "two lease holders"
                    );
                    entered.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(2));
                    active.fetch_sub(1, Ordering::SeqCst);
                    drop(lease);
                }
            });
        }
    });
    assert_eq!(entered.load(Ordering::SeqCst), 40);
}

#[test]
fn busy_lease_times_out_and_cancels_without_blocking() {
    let path = lease_path("busy");
    let held = InputLease::acquire(&path, after(1_000), None).unwrap();

    let start = Instant::now();
    let busy = InputLease::acquire(&path, after(150), None);
    assert!(matches!(busy, Err(LeaseError::Busy)), "{busy:?}");
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert!(start.elapsed() < Duration::from_secs(2));

    let cancel = Cancel::default();
    let start = Instant::now();
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        InputLease::acquire(&path, after(10_000), Some(&cancel))
    });
    assert!(matches!(result, Err(LeaseError::Cancelled)), "{result:?}");
    assert!(start.elapsed() < Duration::from_secs(2));

    drop(held);
    InputLease::acquire(&path, after(100), None).unwrap();
}

#[test]
fn expired_or_cancelled_request_never_takes_a_free_lease() {
    let path = lease_path("expired");
    let res = InputLease::acquire(&path, Instant::now(), None);
    assert!(matches!(res, Err(LeaseError::DeadlineExpired)), "{res:?}");
    // 만료 요청은 잠금 파일조차 만들지 않는다.
    assert!(!path.exists());

    let cancel = Cancel::default();
    cancel.cancel();
    let res = InputLease::acquire(&path, after(5_000), Some(&cancel));
    assert!(matches!(res, Err(LeaseError::Cancelled)), "{res:?}");

    // 비어 있는 잠금도 마찬가지다.
    drop(InputLease::acquire(&path, after(1_000), None).unwrap());
    let res = InputLease::acquire(&path, Instant::now(), None);
    assert!(matches!(res, Err(LeaseError::DeadlineExpired)), "{res:?}");
    let res = InputLease::acquire(&path, after(5_000), Some(&cancel));
    assert!(matches!(res, Err(LeaseError::Cancelled)), "{res:?}");
}

#[test]
fn rejects_unsafe_lease_paths() {
    let root = scratch("unsafe");

    let open_dir = root.join("open");
    fs::create_dir(&open_dir).unwrap();
    fs::set_permissions(&open_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let res = InputLease::acquire(&open_dir.join("input.lock"), after(100), None);
    assert!(matches!(res, Err(LeaseError::UnsafePath(_))), "{res:?}");

    let real = root.join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let linked_dir = root.join("linked");
    symlink(&real, &linked_dir).unwrap();
    let res = InputLease::acquire(&linked_dir.join("input.lock"), after(100), None);
    assert!(matches!(res, Err(LeaseError::UnsafePath(_))), "{res:?}");

    let target = real.join("target");
    fs::write(&target, b"").unwrap();
    symlink(&target, real.join("input.lock")).unwrap();
    let res = InputLease::acquire(&real.join("input.lock"), after(100), None);
    assert!(res.is_err(), "symlinked lock file accepted: {res:?}");

    let private = root.join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(private.join("other"), b"").unwrap();
    fs::hard_link(private.join("other"), private.join("input.lock")).unwrap();
    let res = InputLease::acquire(&private.join("input.lock"), after(100), None);
    assert!(matches!(res, Err(LeaseError::UnsafePath(_))), "{res:?}");
}

#[test]
fn lease_file_is_never_unlinked_or_replaced() {
    let path = lease_path("inode");
    let first = {
        let _lease = InputLease::acquire(&path, after(1_000), None).unwrap();
        fs::symlink_metadata(&path).unwrap()
    };
    let _again = InputLease::acquire(&path, after(1_000), None).unwrap();
    let second = fs::symlink_metadata(&path).unwrap();
    assert_eq!((first.dev(), first.ino()), (second.dev(), second.ino()));
}

#[test]
fn child_stdin_is_the_empty_lease_file() {
    let path = lease_path("dup");
    let lease = InputLease::acquire(&path, after(1_000), None).unwrap();
    let marker = path.with_file_name("ran");
    let fin = run(
        sh(&format!(
            "[ -s /dev/stdin ] || touch '{}'",
            marker.display()
        )),
        &opts(Some(&lease), after(5_000)),
    )
    .unwrap();
    assert!(succeeded(&fin), "{fin:?}");
    assert!(marker.exists(), "child stdin must be the empty lease file");
}

fn assert_child_retains_lease_after_parent_sigkill(kind: &str) {
    let dir = scratch(kind);
    let path = dir.join("lease").join("input.lock");
    let marker = dir.join("marker");
    let pidfile = dir.join("pid");
    let mut parent = helper("hold")
        .env(HELPER_PATH, &path)
        .env(HELPER_MARKER, &marker)
        .env(HELPER_CHILD, kind)
        .env(HELPER_PIDFILE, &pidfile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(20), || marker.exists()),
        "helper never reached spawn"
    );
    // fork는 표시 직후다. 실행기가 확실히 시작하도록 잠깐 둔 뒤 CLI 역할 프로세스를 SIGKILL한다.
    thread::sleep(Duration::from_millis(300));
    parent.kill().unwrap();
    parent.wait().unwrap();
    let killed_at = Instant::now();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "spawning");

    let busy = InputLease::acquire(&path, after(100), None);
    assert!(
        matches!(busy, Err(LeaseError::Busy)),
        "lease released when the CLI died while the executor ran: {busy:?}"
    );
    let child_pid = fs::read_to_string(&pidfile)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    if kind != "osascript" {
        let pid = child_pid.expect("child wrote its pid");
        assert!(!pid_gone(pid), "executor died with its parent");
    }
    let lease = InputLease::acquire(&path, after(10_000), None)
        .expect("lease must free once the orphaned executor exits");
    assert!(
        killed_at.elapsed() >= Duration::from_millis(800),
        "lease freed {:?} after parent death, before the executor finished",
        killed_at.elapsed()
    );
    drop(lease);
}

#[test]
fn executor_retains_lease_after_parent_sigkill() {
    assert_child_retains_lease_after_parent_sigkill("sh");
}

#[cfg(target_os = "macos")]
#[test]
fn osascript_executor_retains_lease_after_parent_sigkill() {
    if !Path::new("/usr/bin/osascript").exists() {
        return;
    }
    assert_child_retains_lease_after_parent_sigkill("osascript");
}

#[test]
fn saturated_pipes_are_bounded_without_deadlock() {
    // stderr를 먼저 가득 채워 stdout EOF 전에 stderr도 읽어야만 끝나게 한다.
    let fin = run(
        sh("head -c 3000000 /dev/zero >&2; head -c 3000000 /dev/zero; printf done >&2"),
        &RunOpts {
            max_output: 4096,
            ..opts(None, after(20_000))
        },
    )
    .unwrap();
    assert!(succeeded(&fin), "{fin:?}");
    assert!(fin.reaped);
    assert_eq!(fin.stdout.bytes.len(), 4096);
    assert_eq!(fin.stderr.bytes.len(), 4096);
    assert!(fin.stdout.truncated && fin.stderr.truncated);
    assert!(fin.stdout.bytes.iter().all(|b| *b == 0));
}

#[test]
fn stdin_payload_is_delivered_and_unread_payload_cannot_hang() {
    let fin = run(
        Command::new("/bin/cat"),
        &RunOpts {
            stdin: Some(b"hello lease"),
            ..opts(None, after(5_000))
        },
    )
    .unwrap();
    assert!(succeeded(&fin), "{fin:?}");
    assert_eq!(fin.stdout.bytes, b"hello lease");

    let big = vec![b'x'; 8 * 1024 * 1024];
    let start = Instant::now();
    let fin = run(
        sh("exec /bin/sleep 30"),
        &RunOpts {
            stdin: Some(&big),
            ..opts(None, after(300))
        },
    )
    .unwrap();
    assert!(matches!(fin.end, End::TimedOut), "{fin:?}");
    assert!(fin.reaped);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn stdin_payload_with_stdin_lease_is_rejected_before_spawn() {
    let path = lease_path("slot");
    let lease = InputLease::acquire(&path, after(1_000), None).unwrap();
    let marker = path.with_file_name("ran");
    let res = run(
        sh(&format!("touch '{}'", marker.display())),
        &RunOpts {
            stdin: Some(b"x"),
            ..opts(Some(&lease), after(5_000))
        },
    );
    assert!(
        matches!(res, Err(NotDispatched::InvalidOptions(_))),
        "{res:?}"
    );
    assert!(!marker.exists());
}

#[test]
fn timeout_kills_and_reaps_the_whole_group() {
    let start = Instant::now();
    let fin = run(
        sh("/bin/sleep 30 & echo $!; echo $$; wait"),
        &opts(None, after(300)),
    )
    .unwrap();
    assert!(matches!(fin.end, End::TimedOut), "{fin:?}");
    assert!(fin.reaped, "timed out child not reaped");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    let pids: Vec<i32> = String::from_utf8_lossy(&fin.stdout.bytes)
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2, "{fin:?}");
    for pid in pids {
        assert!(
            wait_until(Duration::from_secs(3), || pid_gone(pid)),
            "pid {pid} survived the timeout"
        );
    }
}

#[test]
fn no_replay_or_late_effect_after_timeout() {
    let path = lease_path("replay");
    let marker = path.with_file_name("effects");
    let lease = InputLease::acquire(&path, after(1_000), None).unwrap();
    let script = format!(
        "echo start >> '{m}'; /bin/sleep 1; echo late >> '{m}'",
        m = marker.display()
    );
    let fin = run(sh(&script), &opts(Some(&lease), after(300))).unwrap();
    // 시작된 자식은 Ok로 돌아와 NotDispatched(재시도 허용)와 구분된다.
    assert!(matches!(fin.end, End::TimedOut), "{fin:?}");
    assert!(!succeeded(&fin));
    drop(lease);
    // 자식의 잠금 사본도 죽으면서 닫혔다.
    InputLease::acquire(&path, after(200), None).expect("killed executor still holds the lease");
    thread::sleep(Duration::from_millis(1_500));
    assert_eq!(fs::read_to_string(&marker).unwrap(), "start\n");
}

#[test]
fn cancel_kills_a_running_executor() {
    let cancel = Cancel::default();
    let start = Instant::now();
    let fin = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        run(
            sh("exec /bin/sleep 30"),
            &RunOpts {
                cancel: Some(&cancel),
                ..opts(None, after(20_000))
            },
        )
    })
    .unwrap();
    assert!(matches!(fin.end, End::Cancelled), "{fin:?}");
    assert!(fin.reaped);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn pre_spawn_failures_are_not_dispatched() {
    let dir = scratch("prespawn");
    let marker = dir.join("ran");
    let touch = || sh(&format!("touch '{}'", marker.display()));

    let cancel = Cancel::default();
    cancel.cancel();
    let res = run(
        touch(),
        &RunOpts {
            cancel: Some(&cancel),
            ..opts(None, after(5_000))
        },
    );
    assert!(matches!(res, Err(NotDispatched::Cancelled)), "{res:?}");

    let res = run(touch(), &opts(None, Instant::now()));
    assert!(
        matches!(res, Err(NotDispatched::DeadlineExpired)),
        "{res:?}"
    );
    assert!(!marker.exists());

    let res = run(
        Command::new(dir.join("no-such-executor")),
        &opts(None, after(5_000)),
    );
    assert!(matches!(res, Err(NotDispatched::Spawn(_))), "{res:?}");
}

#[test]
fn exit_status_and_leftover_pipe_holders_are_bounded() {
    let fin = run(
        sh("echo out; echo err >&2; exit 3"),
        &opts(None, after(5_000)),
    )
    .unwrap();
    assert!(
        matches!(fin.end, End::Exited(s) if s.code() == Some(3)),
        "{fin:?}"
    );
    assert_eq!(fin.stdout.bytes, b"out\n");
    assert_eq!(fin.stderr.bytes, b"err\n");

    // 리더는 바로 끝나지만 후손이 stdout을 쥐고 있다. 기한에서 그룹을 정리하고 돌아와야 한다.
    let start = Instant::now();
    let fin = run(sh("/bin/sleep 30 & echo $!"), &opts(None, after(500))).unwrap();
    assert!(succeeded(&fin), "{fin:?}");
    assert!(fin.reaped);
    // 그룹 SIGKILL로 파이프가 닫혀 끝까지 읽었다.
    assert!(!fin.stdout.truncated, "{fin:?}");
    assert!(
        start.elapsed() < Duration::from_millis(500) + CLEANUP_GRACE,
        "{:?}",
        start.elapsed()
    );
    let pid: i32 = String::from_utf8_lossy(&fin.stdout.bytes)
        .trim()
        .parse()
        .unwrap();
    assert!(wait_until(Duration::from_secs(3), || pid_gone(pid)));
}

#[test]
fn waiter_on_an_unlinked_inode_does_not_join_the_new_holder() {
    let path = lease_path("replaced");
    let first = InputLease::acquire(&path, after(1_000), None).unwrap();
    thread::scope(|scope| {
        // 대기자는 지금 경로의 inode(첫 파일)를 열고 기다린다.
        let waiter = scope.spawn(|| InputLease::acquire(&path, after(1_500), None));
        thread::sleep(Duration::from_millis(150));
        // /tmp 정리기 같은 외부 삭제 뒤 새 실행기가 새 inode로 잠근다.
        fs::remove_file(&path).unwrap();
        let replacement = InputLease::acquire(&path, after(1_000), None).unwrap();
        drop(first);
        let res = waiter.join().unwrap();
        assert!(
            matches!(res, Err(LeaseError::Busy)),
            "waiter locked the unlinked inode while a new holder owns the path: {res:?}"
        );
        drop(replacement);
    });
}

/// 셸 리더가 그룹을 벗어난 도우미를 띄우고 바로(또는 `tail`을 실행하며) 끝나는 명령.
fn escaping(dir: &Path, tail: &str) -> (Command, PathBuf, PathBuf) {
    let leader = dir.join("leader");
    let escaped = dir.join("escaped");
    let mut cmd = sh(&format!(
        "echo $$ > '{}'; {} & {tail}",
        leader.display(),
        helper_shell()
    ));
    cmd.env(HELPER_ROLE, "escape").env(HELPER_PIDFILE, &escaped);
    (cmd, leader, escaped)
}

fn kill_escaped(pid: i32) {
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::KILL).ok();
}

#[test]
fn leader_stays_unreaped_until_escaped_pipe_holder_is_abandoned() {
    let dir = scratch("escape-exit");
    let (cmd, leader_file, escaped_file) = escaping(&dir, "exit 0");
    let start = Instant::now();
    let (fin, zombie_seen) = thread::scope(|scope| {
        let runner = scope.spawn(|| run(cmd, &opts(None, after(1_500))));
        let escaped_up = wait_until(Duration::from_secs(5), || {
            read_pid(&escaped_file).is_some() && read_pid(&leader_file).is_some()
        });
        assert!(escaped_up, "escaped helper never started");
        let leader = read_pid(&leader_file).unwrap();
        // 리더는 이미 끝났지만 그룹 정리 전이라 회수되지 않은 좀비여야 한다(pgid 재사용 차단).
        let zombie_seen = wait_until(Duration::from_millis(500), || is_unreaped_zombie(leader));
        (runner.join().unwrap().unwrap(), zombie_seen)
    });
    let elapsed = start.elapsed();
    let leader = read_pid(&leader_file).unwrap();
    let escaped = read_pid(&escaped_file).unwrap();
    let escaped_alive = !pid_gone(escaped);
    kill_escaped(escaped);
    assert!(
        zombie_seen,
        "leader was reaped while its pipes were still open"
    );
    assert!(fin.reaped, "{fin:?}");
    assert!(!is_unreaped_zombie(leader), "run returned without reaping");
    assert!(succeeded(&fin), "{fin:?}");
    // 파이프를 끝까지 읽지 못했으니 출력은 불완전으로 표시한다.
    assert!(fin.stdout.truncated && fin.stderr.truncated, "{fin:?}");
    // 그룹 밖으로 빠져나간 후손에게는 신호를 보내지 않는다.
    assert!(escaped_alive, "escaped descendant was signalled");
    assert!(elapsed >= Duration::from_millis(1_500), "{elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(1_500) + CLEANUP_GRACE + Duration::from_millis(700),
        "{elapsed:?}"
    );
}

#[test]
fn timeout_with_escaped_pipe_holder_spends_one_cleanup_grace() {
    let dir = scratch("escape-timeout");
    let (cmd, leader_file, escaped_file) = escaping(&dir, "exec /bin/sleep 30");
    let start = Instant::now();
    let fin = run(cmd, &opts(None, after(1_000))).unwrap();
    let elapsed = start.elapsed();
    let leader = read_pid(&leader_file).unwrap();
    let escaped = read_pid(&escaped_file);
    let escaped_alive = escaped.is_some_and(|pid| !pid_gone(pid));
    if let Some(pid) = escaped {
        kill_escaped(pid);
    }
    assert!(matches!(fin.end, End::TimedOut), "{fin:?}");
    assert!(fin.reaped, "{fin:?}");
    assert!(pid_gone(leader), "timed out leader survived");
    assert!(fin.stdout.truncated, "{fin:?}");
    assert!(escaped_alive, "escaped descendant was signalled");
    // 정리 상한은 한 번만 쓴다: 기한 + CLEANUP_GRACE(+스케줄링 여유) 안에 돌아온다.
    assert!(
        elapsed < Duration::from_millis(1_000) + CLEANUP_GRACE + Duration::from_millis(700),
        "{elapsed:?}"
    );
}
