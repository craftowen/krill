//! MK-042: 실제 CLI → krilld → `GitQueue` → git, 승인 후 holder setup 세션을 검증한다.

#![cfg_attr(
    unix,
    expect(
        clippy::unwrap_used,
        reason = "통합 테스트 fixture 실패는 테스트 실패다"
    )
)]
#![cfg(unix)]

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::control::{self, FEAT_CONTROL, Failure, Reply, Request};
use krill_proto::daemon::{self, client_hello};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg;
use std::io::Write;

const MK: &str = env!("CARGO_BIN_EXE_krill");

struct Fixture {
    dir: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    holder: Child,
    krilld: u32,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from(format!("/tmp/mkwt-cli-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let repo = dir.join("레포");
        let state = dir.join("state");
        fs::create_dir_all(&repo).unwrap();
        fs::write(
            repo.join("krill.toml"),
            "[worktree]\ninclude = ['.env']\nsetup = 'printf one > setup-result'\n",
        )
        .unwrap();
        fs::write(repo.join(".gitignore"), ".env\nsetup-result\n").unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "init"],
        ] {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        }
        fs::write(repo.join(".env"), "LOCAL=1").unwrap();
        let bin = Path::new(MK).parent().unwrap();
        let socket = state.join("run/krilld.sock");
        let holder = Command::new(bin.join("krill-holder"))
            .arg(bin.join("krilld"))
            .arg(&socket)
            .env("KRILL_STATE_DIR", &state)
            .env("KRILL_WORKTREES_DIR", dir.join("worktrees"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut f = Self {
            dir,
            repo,
            state,
            holder,
            krilld: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(c) = daemon::handshake(
                &socket,
                &client_hello(ClientRole::Cli, "test"),
                Duration::from_millis(100),
            ) {
                f.krilld = u32::try_from(c.server.instance_id).unwrap();
                return f;
            }
            assert!(Instant::now() < deadline, "daemon did not start");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn krill(&self, args: &[&str]) -> Output {
        Command::new(MK)
            .args(["worktree", "new"])
            .args(args)
            .arg("--repo")
            .arg(&self.repo)
            .env("KRILL_STATE_DIR", &self.state)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn rm(&self, path: &Path, force: bool) -> Output {
        let mut cmd = Command::new(MK);
        cmd.args(["worktree", "rm"])
            .arg(path)
            .arg("--repo")
            .arg(&self.repo);
        if force {
            cmd.arg("--force");
        }
        cmd.env("KRILL_STATE_DIR", &self.state)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn git(path: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.holder.kill().ok();
        self.holder.wait().ok();
        if self.krilld != 0 {
            Command::new("/bin/kill")
                .args(["-9", &self.krilld.to_string()])
                .status()
                .ok();
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

fn output_path(out: &Output) -> PathBuf {
    assert!(out.status.success(), "{out:?}");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).lines().next().unwrap())
}

fn hash(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(1));
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|s| s.strip_prefix("SHA-256: "))
        .unwrap()
        .to_owned()
}

fn setup_result(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(path.join("setup-result"))
        .ok()
        .as_deref()
        != Some(expected)
    {
        assert!(
            Instant::now() < deadline,
            "setup did not complete at {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cli_new_worktree_starts_session_in_the_created_path() {
    let f = Fixture::new("session-new");
    let run = |cwd: &Path, args: &[&str]| {
        Command::new(MK)
            .args(args)
            .current_dir(cwd)
            .env("KRILL_STATE_DIR", &f.state)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    // The shortcut must keep the existing setup approval boundary.
    let untrusted = run(&f.repo, &["new", "--worktree", "feat-x"]);
    assert_eq!(untrusted.status.code(), Some(1), "{untrusted:?}");
    assert!(String::from_utf8_lossy(&untrusted.stderr).contains("setup approval required"));
    assert!(!f.dir.join("worktrees").exists());
    fs::write(f.repo.join("krill.toml"), "[worktree]\n").unwrap();
    fs::write(
        f.state.join("agents.toml"),
        "[agent.cwd_probe]\nname = 'Cwd probe'\nlaunch = ['/bin/pwd', '-P']\n",
    )
    .unwrap();
    let invalid = run(&f.repo, &["new", "--worktree", "invalid", "--rows", "0"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(!f.dir.join("worktrees").exists());

    for (index, args) in [
        vec!["new", "--worktree", "feat-x", "--", "/bin/pwd", "-P"],
        vec![
            "new",
            "--cwd",
            f.repo.to_str().unwrap(),
            "--worktree",
            "feat-x",
            "--agent",
            "cwd_probe",
        ],
    ]
    .iter()
    .enumerate()
    {
        let cwd = if index == 0 { &f.repo } else { &f.dir };
        let created = run(cwd, args);
        assert!(created.status.success(), "{created:?}");
        let id = String::from_utf8(created.stdout).unwrap();
        assert!(id.trim().parse::<u64>().is_ok(), "{id:?}");
        assert_eq!(
            id.lines().count(),
            1,
            "stdout must contain only the session ID"
        );
        let name = if index == 0 { "feat-x" } else { "feat-x-2" };
        let path = fs::canonicalize(f.dir.join("worktrees/레포").join(name)).unwrap();
        let condition = format!("pattern:{}", path.display());
        // Agent programs retain a login shell after printing; wait for their output.
        let waited = run(
            cwd,
            &["wait", id.trim(), "--for", &condition, "--timeout", "5"],
        );
        assert!(waited.status.success(), "{waited:?}");
        let output = run(cwd, &["read", id.trim()]);
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(path.to_str().unwrap()),
            "{output:?}"
        );
        assert_eq!(
            Fixture::git(&path, &["branch", "--show-current"]).trim(),
            name
        );
    }
}

#[test]
fn cli_creates_and_rechecks_setup_then_runs_in_a_session() {
    let f = Fixture::new("new");
    let first = f.krill(&["새 기능"]);
    let initial_hash = hash(&first);
    assert!(String::from_utf8_lossy(&first.stderr).contains("printf one"));
    assert!(!f.dir.join("worktrees").exists());

    let approved = f.krill(&["새 기능", "--trust-setup", &initial_hash]);
    let path = output_path(&approved);
    assert!(String::from_utf8_lossy(&approved.stdout).contains("setup session "));
    assert_eq!(fs::read_to_string(path.join(".env")).unwrap(), "LOCAL=1");
    setup_result(&path, "one");
    let duplicate = output_path(&f.krill(&["새 기능", "--setup", "skip"]));
    assert_eq!(duplicate.file_name().unwrap(), "새-기능-2");
    assert!(!duplicate.join("setup-result").exists());

    let known = output_path(&f.krill(&["trusted"]));
    setup_result(&known, "one");
    fs::write(
        f.repo.join("krill.toml"),
        "[worktree]\nsetup = 'printf two > setup-result'\n",
    )
    .unwrap();
    let changed_hash = hash(&f.krill(&["changed", "--trust-setup", &initial_hash]));
    assert_ne!(initial_hash, changed_hash);
    assert!(!f.dir.join("worktrees/레포/changed").exists());
    let changed = output_path(&f.krill(&["changed", "--trust-setup", &changed_hash]));
    setup_result(&changed, "two");

    // CLI가 요청 직후 사라져도 데몬은 생성을 끝낸다.
    let mut detached = daemon::handshake(
        &f.state.join("run/krilld.sock"),
        &client_hello(ClientRole::Cli, "disconnected"),
        Duration::from_secs(2),
    )
    .unwrap();
    detached
        .stream
        .write_all(
            &Request::NewWorktree(control::NewWorktree {
                repo: f.repo.as_os_str().as_bytes().to_vec(),
                name: "disconnected".to_owned(),
                base: None,
                setup: control::Setup::Skip,
            })
            .encode()
            .unwrap(),
        )
        .unwrap();
    drop(detached);
    let completed = f.dir.join("worktrees/레포/disconnected/krill.toml");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !completed.is_file() {
        assert!(
            Instant::now() < deadline,
            "creation stopped when the client disconnected"
        );
        thread::sleep(Duration::from_millis(10));
    }

    // 구 클라이언트는 새 요청을 보내도 기존 BadRequest 응답만 받는다.
    let mut hello = client_hello(ClientRole::Cli, "old");
    hello.features = FEAT_CONTROL;
    let mut conn = daemon::handshake(
        &f.state.join("run/krilld.sock"),
        &hello,
        Duration::from_secs(2),
    )
    .unwrap();
    let req = Request::NewWorktree(control::NewWorktree {
        repo: Vec::new(),
        name: "must-not-create".to_owned(),
        base: None,
        setup: control::Setup::Skip,
    });
    conn.stream.write_all(&req.encode().unwrap()).unwrap();
    let (kind, body) = ipc::read_frame(&mut conn.stream).unwrap().unwrap();
    assert_eq!(
        Reply::decode(kind, &body).unwrap(),
        Reply::Failed(Failure::BadRequest)
    );
}

#[test]
fn cli_removal_refuses_dirty_and_locked_and_preserves_unmerged_branch() {
    let f = Fixture::new("rm");
    let path = output_path(&f.krill(&["valuable", "--setup", "skip"]));
    fs::remove_file(path.join(".env")).unwrap();
    fs::write(path.join("work"), "valuable").unwrap();
    let rejected = f.rm(&path, false);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("dirty"));
    assert_eq!(fs::read_to_string(path.join("work")).unwrap(), "valuable");
    Fixture::git(&path, &["add", "work"]);
    Fixture::git(&path, &["commit", "-q", "-m", "valuable"]);
    let commit = Fixture::git(&f.repo, &["rev-parse", "valuable"]);
    Fixture::git(
        &f.repo,
        &[
            "worktree",
            "lock",
            "--reason",
            "protect",
            path.to_str().unwrap(),
        ],
    );
    for force in [false, true] {
        let rejected = f.rm(&path, force);
        assert_eq!(rejected.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("locked"));
        assert!(path.exists());
    }
    Fixture::git(&f.repo, &["worktree", "unlock", path.to_str().unwrap()]);
    let removed = f.rm(&path, false);
    assert!(removed.status.success(), "{removed:?}");
    assert!(String::from_utf8_lossy(&removed.stdout).contains("branch valuable preserved"));
    assert!(String::from_utf8_lossy(&removed.stderr).contains("warning:"));
    assert!(!path.exists());
    assert_eq!(Fixture::git(&f.repo, &["rev-parse", "valuable"]), commit);

    let dirty = output_path(&f.krill(&["dirty", "--setup", "skip"]));
    assert_eq!(f.rm(&dirty, false).status.code(), Some(1));
    assert!(f.rm(&dirty, true).status.success());
    assert!(!dirty.exists());
    assert!(Fixture::git(&f.repo, &["branch", "--list", "dirty"]).is_empty());

    // MK-042까지만 지원하는 클라이언트가 삭제 variant를 보내도 실행하지 않는다.
    let pending = output_path(&f.krill(&["pending", "--setup", "skip"]));
    fs::remove_file(pending.join(".env")).unwrap();
    let req = Request::RemoveWorktree(control::RemoveWorktree {
        repo: f.repo.as_os_str().as_bytes().to_vec(),
        path: pending.as_os_str().as_bytes().to_vec(),
        force: false,
    });
    let mut hello = client_hello(ClientRole::Cli, "old-worktree-client");
    hello.features = FEAT_CONTROL | control::FEAT_WORKTREE;
    let mut conn = daemon::handshake(
        &f.state.join("run/krilld.sock"),
        &hello,
        Duration::from_secs(2),
    )
    .unwrap();
    conn.stream.write_all(&req.encode().unwrap()).unwrap();
    let (kind, body) = ipc::read_frame(&mut conn.stream).unwrap().unwrap();
    assert_eq!(
        Reply::decode(kind, &body).unwrap(),
        Reply::Failed(Failure::BadRequest)
    );
    assert!(pending.exists());
    drop(conn);

    // 요청 직후 연결을 닫아도 데몬이 삭제·브랜치 정리를 완료한다.
    let mut conn = daemon::handshake(
        &f.state.join("run/krilld.sock"),
        &client_hello(ClientRole::Cli, "disconnected-rm"),
        Duration::from_secs(2),
    )
    .unwrap();
    conn.stream.write_all(&req.encode().unwrap()).unwrap();
    drop(conn);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !Fixture::git(&f.repo, &["branch", "--list", "pending"]).is_empty() {
        assert!(
            Instant::now() < deadline,
            "removal stopped when the client disconnected"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!pending.exists());

    // 세션 안 피어도 전체 권한이라 자기 cwd 밖의 worktree도 지우고 만든다(Orca 소유자 모델).
    let target_wt = output_path(&f.krill(&["other-wt", "--setup", "skip"]));
    let other_rm = f.dir.join("other-rm");
    let outside_new = f.dir.join("outside-new");
    let script = format!(
        "KRILL_STATE_DIR='{state}' '{krill}' worktree rm '{wt}' --repo '{repo}' --force >'{orm}' 2>&1; \
         printf '%s' $? >'{orm}.rc'; \
         KRILL_STATE_DIR='{state}' '{krill}' worktree new outside-wt --repo '{repo}' --setup skip >'{onew}' 2>&1; \
         printf '%s' $? >'{onew}.rc'",
        state = f.state.display(),
        krill = MK,
        wt = target_wt.display(),
        repo = f.repo.display(),
        orm = other_rm.display(),
        onew = outside_new.display(),
    );
    let sid = Command::new(MK)
        .args([
            "new",
            "--cwd",
            f.dir.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            &script,
        ])
        .env("KRILL_STATE_DIR", &f.state)
        .output()
        .unwrap();
    assert!(sid.status.success(), "{sid:?}");
    let sid = String::from_utf8(sid.stdout).unwrap();
    let waited = Command::new(MK)
        .args(["wait", sid.trim(), "--timeout", "10"])
        .env("KRILL_STATE_DIR", &f.state)
        .output()
        .unwrap();
    assert!(waited.status.success(), "{waited:?}");
    assert_eq!(
        fs::read_to_string(other_rm.with_extension("rc")).unwrap(),
        "0",
        "{}",
        fs::read_to_string(&other_rm).unwrap_or_default()
    );
    assert!(!target_wt.exists(), "session peer removes another worktree");
    assert_eq!(
        fs::read_to_string(outside_new.with_extension("rc")).unwrap(),
        "0",
        "{}",
        fs::read_to_string(&outside_new).unwrap_or_default()
    );
    assert!(!f.state.join("audit.log").exists());
}

#[test]
fn cli_does_not_send_removal_to_an_old_daemon() {
    use std::os::unix::net::UnixListener;
    let dir = PathBuf::from(format!("/tmp/mkwt-old-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let state = dir.join("state");
    fs::create_dir_all(state.join("run")).unwrap();
    let socket = UnixListener::bind(state.join("run/krilld.sock")).unwrap();
    let server = thread::spawn(move || {
        let hello = ipc::ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: FEAT_CONTROL | control::FEAT_WORKTREE,
            build: "old".to_owned(),
            instance_id: 1,
        };
        // OWE-493: 초기 상태 확인용 감시 연결 뒤 실제 제어 연결이 온다. 구 데몬은 감시 기능을 협상하지 않는다.
        // OWE-586: 감시 연결은 하트비트 비트를 함께 싣는다(구 데몬과 협상 시 제외).
        for features in [
            daemon::FEAT_HOLDER_WATCH | daemon::FEAT_HEARTBEAT,
            ipc::FEATURES,
        ] {
            let (mut stream, _) = socket.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream.write_all(&hello.encode().unwrap()).unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            let client = ipc::ClientHello::decode(kind, &body).unwrap();
            assert_eq!(client.features, features);
            assert_eq!(
                ipc::negotiate(&hello, &client).unwrap().features & control::FEAT_WORKTREE_REMOVE,
                0
            );
            assert!(
                ipc::read_frame(&mut stream).unwrap().is_none(),
                "no deletion request before feature negotiation"
            );
        }
    });
    let out = Command::new(MK)
        .args(["worktree", "rm"])
        .arg(&dir)
        .arg("--repo")
        .arg(&dir)
        .env("KRILL_STATE_DIR", &state)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not support worktree removal"));
    server.join().unwrap();
    assert!(dir.exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cli_changes_queries_daemon_and_preserves_feature_gate() {
    let f = Fixture::new("changes");
    fs::write(f.repo.join("changed\t한글"), "one\n").unwrap();
    Fixture::git(&f.repo, &["add", "changed\t한글"]);
    fs::write(f.repo.join("changed\t한글"), "one\ntwo\n").unwrap();
    let out = Command::new(MK)
        .args(["changes", "--repo"])
        .arg(&f.repo)
        .env("KRILL_STATE_DIR", &f.state)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("AM staged=+1/-0 unstaged=+1/-0"), "{text}");
    assert!(text.contains("changed\\t한글"), "{text}");
    let mut hello = client_hello(ClientRole::Cli, "old-changes-client");
    hello.features = FEAT_CONTROL;
    let mut conn = daemon::handshake(
        &f.state.join("run/krilld.sock"),
        &hello,
        Duration::from_secs(2),
    )
    .unwrap();
    conn.stream
        .write_all(
            &Request::GitChanges {
                repo: f.repo.as_os_str().as_bytes().to_vec(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (kind, body) = ipc::read_frame(&mut conn.stream).unwrap().unwrap();
    assert_eq!(
        Reply::decode(kind, &body).unwrap(),
        Reply::Failed(Failure::BadRequest)
    );
}

/// MK-080 완료 조건: Orca 설정만 있는 레포에서 include를 복사하고 승인된 setup을 자동 실행한다.
#[test]
fn orca_repository_copies_env_and_automatically_runs_trusted_setup_with_aliases() {
    let f = Fixture::new("orca-compat");
    fs::remove_file(f.repo.join("krill.toml")).unwrap();
    fs::write(f.repo.join(".worktreeinclude"), "# local secrets\n.env\n").unwrap();
    fs::write(
        f.repo.join("orca.yaml"),
        r#"scripts:
  setup: |
    test -f .env || exit 1
    test "$KRILL_ROOT_PATH" = "$ORCA_ROOT_PATH" || exit 2
    test "$KRILL_ROOT_PATH" = "$CONDUCTOR_ROOT_PATH" || exit 3
    test "$KRILL_ROOT_PATH" = "$GHOSTX_ROOT_PATH" || exit 4
    test "$KRILL_WORKTREE_PATH" = "$ORCA_WORKTREE_PATH" || exit 5
    test "$KRILL_WORKSPACE_NAME" = "$ORCA_WORKSPACE_NAME" || exit 6
    printf '%s\n%s\n%s' "$KRILL_ROOT_PATH" "$KRILL_WORKTREE_PATH" "$ORCA_WORKSPACE_NAME" > setup-result
"#,
    )
    .unwrap();
    Fixture::git(&f.repo, &["add", "-A"]);
    Fixture::git(&f.repo, &["commit", "-q", "-m", "orca config"]);
    let first = f.krill(&["approved"]);
    let approval = hash(&first);
    assert!(!f.dir.join("worktrees").exists());
    let created = f.krill(&["approved", "--trust-setup", &approval]);
    let wt = output_path(&created);
    let root = fs::canonicalize(&f.repo).unwrap();
    setup_result(
        &wt,
        &format!("{}\n{}\napproved", root.display(), wt.display()),
    );
    assert_eq!(fs::read(wt.join(".env")).unwrap(), b"LOCAL=1");
    let automatic = output_path(&f.krill(&["automatic"]));
    setup_result(
        &automatic,
        &format!("{}\n{}\nautomatic", root.display(), automatic.display()),
    );
    let skipped = output_path(&f.krill(&["skipped", "--setup", "skip"]));
    assert!(skipped.join(".env").is_file());
    assert!(!skipped.join("setup-result").exists());
    fs::write(
        f.repo.join("orca.yaml"),
        "scripts:\n  setup: echo changed\n",
    )
    .unwrap();
    assert_ne!(
        hash(&f.krill(&["changed", "--trust-setup", &approval])),
        approval
    );
    assert!(!f.dir.join("worktrees/레포/changed").exists());
}

#[test]
fn worktree_symlink_and_hooks_cli_end_to_end() {
    let dir = PathBuf::from(format!("/tmp/mkwt-cli-{}-symlink-cli", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let repo = dir.join("repo");
    let state = dir.join("state");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&state).unwrap();
    Fixture::git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(
        repo.join("orca.yaml"),
        "scripts:\n  setup: pnpm install\nworktree:\n  sharedDirectories:\n    - .env\n",
    )
    .unwrap();

    // Path traversal is rejected by CLI with exit code 2.
    let bad = Command::new(MK)
        .args(["worktree", "symlink", "add", "../escape", "--repo"])
        .arg(&repo)
        .env("KRILL_STATE_DIR", &state)
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));

    // Register .env.local symlink via CLI and verify merged effective_symlinks (.env + .env.local).
    let add = Command::new(MK)
        .args(["worktree", "symlink", "add", ".env.local", "--repo"])
        .arg(&repo)
        .env("KRILL_STATE_DIR", &state)
        .output()
        .unwrap();
    assert!(add.status.success(), "{add:?}");
    let listed: Vec<&str> = std::str::from_utf8(&add.stdout).unwrap().lines().collect();
    assert_eq!(listed, vec![".env", ".env.local"]);

    // Register setup and archive hooks via CLI and inspect JSON output.
    let hook = Command::new(MK)
        .args([
            "worktree",
            "hooks",
            "--setup",
            "cargo check",
            "--archive",
            "./scripts/cleanup.sh",
            "--json",
            "--repo",
        ])
        .arg(&repo)
        .env("KRILL_STATE_DIR", &state)
        .output()
        .unwrap();
    assert!(hook.status.success(), "{hook:?}");
    let summary: krill_proto::orchestration::RepoHookSummary =
        serde_json::from_slice(&hook.stdout).unwrap();
    assert_eq!(summary.effective_setup.as_deref(), Some("cargo check"));
    assert_eq!(
        summary.effective_archive.as_deref(),
        Some("./scripts/cleanup.sh")
    );
    assert_eq!(summary.effective_symlinks, vec![".env", ".env.local"]);

    // Remove .env.local symlink and clear setup override (falls back to orca.yaml setup).
    let rm_sym = Command::new(MK)
        .args(["worktree", "symlink", "rm", ".env.local", "--repo"])
        .arg(&repo)
        .env("KRILL_STATE_DIR", &state)
        .output()
        .unwrap();
    assert!(rm_sym.status.success(), "{rm_sym:?}");
    assert_eq!(String::from_utf8_lossy(&rm_sym.stdout).trim(), ".env");

    let clear_hook = Command::new(MK)
        .args(["worktree", "hooks", "--clear-setup", "--json", "--repo"])
        .arg(&repo)
        .env("KRILL_STATE_DIR", &state)
        .output()
        .unwrap();
    assert!(clear_hook.status.success(), "{clear_hook:?}");
    let after: krill_proto::orchestration::RepoHookSummary =
        serde_json::from_slice(&clear_hook.stdout).unwrap();
    assert_eq!(after.store_setup, None);
    assert_eq!(after.effective_setup.as_deref(), Some("pnpm install"));
    assert_eq!(
        after.effective_archive.as_deref(),
        Some("./scripts/cleanup.sh")
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn worktree_local_only_subcommands_reject_host() {
    let state = PathBuf::from(format!("/tmp/mkwt-host-{}", std::process::id()));
    fs::create_dir_all(&state).unwrap();
    for sub in [
        "list", "show", "current", "cleanup", "space", "set", "symlink", "hooks",
    ] {
        let out = Command::new(MK)
            .env("KRILL_STATE_DIR", &state)
            .args(["worktree", sub, "--host", "nobody@invalid.invalid"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{sub}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--host"),
            "{sub}: {out:?}"
        );
    }
    fs::remove_dir_all(&state).ok();
}

#[test]
fn account_daemon_failure_is_not_a_usage_error() {
    for args in [
        &["account", "policy", "predict", "on"][..],
        &["account", "rule", "claude", "work", "on"],
        &["account", "auto-switch", "on", "80"],
    ] {
        let out = Command::new(MK)
            .env("KRILL_STATE_DIR", "/dev/null/no-such-state")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
    }
}

#[test]
fn account_invalid_arguments_are_usage_errors_even_without_a_daemon() {
    for args in [
        &["account", "policy", "a", "b"][..],
        &["account", "rule", "x", "y"],
        &["account", "rule", "claude", "work", "maybe"],
        &["account", "auto-switch", "on", "z"],
    ] {
        let out = Command::new(MK)
            .env("KRILL_STATE_DIR", "/dev/null/no-such-state")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
}

fn krill_in(state: &Path, args: &[&str]) -> Output {
    Command::new(MK)
        .args(args)
        .env("KRILL_STATE_DIR", state)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// `busy`에는 실행 중인 세션을 두고 `idle`은 비워 둔다. 세션 ID를 돌려준다.
fn busy_and_idle_worktrees(f: &Fixture) -> (PathBuf, PathBuf, String) {
    let busy = f.dir.join("wt-busy");
    let idle = f.dir.join("wt-idle");
    for (wt, branch) in [(&busy, "busy"), (&idle, "idle")] {
        Fixture::git(
            &f.repo,
            &["worktree", "add", "-q", "-b", branch, wt.to_str().unwrap()],
        );
    }
    let created = krill_in(
        &f.state,
        &[
            "new",
            "--cwd",
            busy.to_str().unwrap(),
            "--",
            "/bin/sleep",
            "600",
        ],
    );
    assert!(created.status.success(), "{created:?}");
    let session = String::from_utf8(created.stdout).unwrap().trim().to_owned();
    (busy, idle, session)
}

#[test]
fn cleanup_removes_nothing_when_the_daemon_cannot_report_sessions() {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new("cleanup-guard");
    let (busy, idle, session) = busy_and_idle_worktrees(&f);
    let repo = f.repo.to_str().unwrap();
    let cleanup = [
        "worktree", "cleanup", "--repo", repo, "--pr", "any", "--yes",
    ];

    let socket = f.state.join("run/krilld.sock");
    let mode = fs::metadata(&socket).unwrap().permissions().mode();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o000)).unwrap();
    let blind = krill_in(&f.state, &cleanup);
    fs::set_permissions(&socket, fs::Permissions::from_mode(mode)).unwrap();
    assert!(
        busy.exists() && idle.exists(),
        "nothing may be removed while running sessions cannot be checked: {blind:?}"
    );
    assert_eq!(blind.status.code(), Some(1), "{blind:?}");

    let guarded = krill_in(&f.state, &cleanup);
    assert_eq!(guarded.status.code(), Some(1), "{guarded:?}");
    assert!(
        String::from_utf8_lossy(&guarded.stderr).contains("running session"),
        "{guarded:?}"
    );
    assert!(busy.exists(), "{guarded:?}");
    assert!(!idle.exists(), "{guarded:?}");

    let killed = krill_in(&f.state, &["kill", &session]);
    assert!(killed.status.success(), "{killed:?}");
    let no_daemon = krill_in(&f.dir.join("state-without-daemon"), &cleanup);
    assert!(no_daemon.status.success(), "{no_daemon:?}");
    assert!(!busy.exists(), "{no_daemon:?}");
}

#[test]
fn cleanup_preview_shows_worktrees_with_running_sessions_as_skipped() {
    let f = Fixture::new("cleanup-preview");
    let (busy, idle, session) = busy_and_idle_worktrees(&f);
    let repo = f.repo.to_str().unwrap();

    let preview = krill_in(
        &f.state,
        &["worktree", "cleanup", "--repo", repo, "--pr", "any"],
    );
    assert!(preview.status.success(), "{preview:?}");
    let stdout = String::from_utf8_lossy(&preview.stdout);
    let line = |name: &str| {
        stdout
            .lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("{name} missing from preview:\n{stdout}"))
    };
    assert!(line("wt-busy").contains("running session"), "{stdout}");
    assert!(!line("wt-idle").contains("skipped"), "{stdout}");

    let json = krill_in(
        &f.state,
        &[
            "worktree", "cleanup", "--repo", repo, "--pr", "any", "--json",
        ],
    );
    assert!(json.status.success(), "{json:?}");
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let skipped: Vec<&str> = report["summary"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| {
            i["error"]
                .as_str()
                .is_some_and(|e| e.contains("running session"))
        })
        .filter_map(|i| i["path"].as_str())
        .collect();
    assert!(
        skipped.len() == 1 && skipped[0].ends_with("wt-busy"),
        "{report}"
    );
    assert!(busy.exists() && idle.exists(), "a preview removes nothing");

    let killed = krill_in(&f.state, &["kill", &session]);
    assert!(killed.status.success(), "{killed:?}");
}
