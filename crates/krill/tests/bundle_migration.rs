//! MK-170: 번들 id 이관(`dev.minikrill.app` → `dev.krill.app`)을 실제 holder·krilld·`krill`로 임시 `HOME`에서
//! 확인한다. 이 맥의 `~/Library`와 launchd는 건드리지 않는다(`HOME`을 바꾸고 launchctl을 끈다).
//!
//! - 옛 holder가 살아 있으면 새 CLI가 옛 소켓의 세션을 보고 새 holder를 띄우지 않는다(AGENTS 규칙 1).
//! - 세션 0개·holder 없음이면 옮기고, 새 경로에서 새 holder로 다시 떠도 같은 데이터가 보인다.
//!
//! 재현: `cargo nextest run -p krill --test bundle_migration`

#![cfg_attr(unix, expect(clippy::unwrap_used, reason = "integration test helpers"))]
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::daemon::{client_hello, handshake, migrate};
use krill_proto::ipc::ClientRole;

const MK: &str = env!("CARGO_BIN_EXE_krill");
const WAIT: Duration = Duration::from_secs(10);

/// 임시 `HOME`. 끝나면 띄운 holder·krilld를 죽이고 지운다.
struct Home {
    dir: PathBuf,
    pids: Vec<u32>,
    children: Vec<Child>,
}

impl Home {
    /// UDS 104바이트 안에 들게 `/tmp` 아래에 둔다.
    fn new(tag: &str) -> Self {
        let bin = Path::new(MK).parent().unwrap();
        for b in ["krill-holder", "krilld"] {
            assert!(
                bin.join(b).is_file(),
                "{b} is not built next to krill (run the whole workspace: cargo nextest run --workspace)"
            );
        }
        let dir = PathBuf::from(format!("/tmp/mk170-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            pids: Vec::new(),
            children: Vec::new(),
        }
    }

    fn support(&self, bundle: &str) -> PathBuf {
        self.dir.join("Library/Application Support").join(bundle)
    }

    fn old(&self) -> PathBuf {
        self.support("dev.minikrill.app")
    }

    fn new_dir(&self) -> PathBuf {
        self.support("dev.krill.app")
    }

    fn env(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.dir)
            .env(migrate::SKIP_LAUNCHCTL_ENV, "1")
            .env_remove("KRILL_STATE_DIR")
            .env_remove("KRILL_LOG_DIR");
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(MK);
        cmd.args(args);
        self.env(&mut cmd);
        cmd.output().unwrap()
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

    /// 구버전 앱이 띄운 holder처럼 옛 경로 소켓으로 holder를 띄우고 krilld pid를 돌려준다.
    fn legacy_holder(&mut self) -> u32 {
        let bin = Path::new(MK).parent().unwrap();
        let socket = self.old().join("run/krilld.sock");
        let mut cmd = Command::new(bin.join("krill-holder"));
        cmd.arg(bin.join("krilld"))
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.env(&mut cmd);
        let holder = cmd.spawn().unwrap();
        self.pids.push(holder.id());
        self.children.push(holder);
        let krilld = krilld_at(&socket);
        self.pids.push(krilld);
        krilld
    }

    /// 이 `HOME`을 쓰는 holder 프로세스 수.
    fn holders(&self) -> usize {
        let ps = Command::new("/bin/ps")
            .args(["-axwwo", "command="])
            .output()
            .unwrap();
        let home = self.dir.to_string_lossy().into_owned();
        String::from_utf8_lossy(&ps.stdout)
            .lines()
            .filter(|l| l.contains("krill-holder") && l.contains(&home))
            .count()
    }
}

fn krilld_at(socket: &Path) -> u32 {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(c) = handshake(socket, &client_hello(ClientRole::Cli, "mk170"), WAIT) {
            return u32::try_from(c.server.instance_id).unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "daemon at {} timed out",
            socket.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_file(path: &Path) {
    let deadline = Instant::now() + WAIT;
    while fs::read_dir(path).map_or(true, |mut d| d.next().is_none()) {
        assert!(Instant::now() < deadline, "missing {}", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

fn kill9(pid: u32) {
    Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status()
        .ok();
}

fn ppid(pid: u32) -> u32 {
    let o = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().parse().unwrap()
}

fn kill_daemons_under(dir: &Path) {
    let Ok(ps) = Command::new("/bin/ps")
        .args(["-axwwo", "pid=,command="])
        .output()
    else {
        return;
    };
    let needle = dir.to_string_lossy().into_owned();
    let alt = needle
        .strip_prefix("/private")
        .map_or_else(|| format!("/private{needle}"), str::to_owned);
    let canon = fs::canonicalize(dir)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let text = String::from_utf8_lossy(&ps.stdout);
    for role in ["krill-holder", "krilld"] {
        for line in text.lines() {
            let trimmed = line.trim_start();
            let Some((pid_str, cmd)) = trimmed.split_once(char::is_whitespace) else {
                continue;
            };
            let matches_dir = cmd.contains(&needle)
                || cmd.contains(&alt)
                || canon.as_deref().is_some_and(|c| cmd.contains(c));
            if matches_dir
                && cmd.contains(role)
                && let Ok(pid) = pid_str.trim().parse::<u32>()
            {
                kill9(pid);
            }
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // holder 먼저: krilld가 먼저 죽으면 holder가 다시 띄운다.
        for pid in &self.pids {
            kill9(*pid);
        }
        kill_daemons_under(&self.dir);
        for child in &mut self.children {
            child.wait().ok();
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

#[test]
fn live_legacy_holder_is_reused_without_new_holder() {
    let mut h = Home::new("live");
    fs::create_dir_all(h.old()).unwrap();
    let krilld = h.legacy_holder();
    assert_eq!(h.holders(), 1);
    let id = h.ok(&[
        "new",
        "--",
        "/bin/sh",
        "-c",
        "printf 'legacy-session\\n'; sleep 30",
    ]);
    let id = id.trim().to_owned();
    assert!(
        h.ok(&["ls"])
            .lines()
            .any(|l| l.starts_with(&format!("{id}\t")))
    );
    assert!(h.ok(&["read", &id]).contains("legacy-session"));
    // 새 경로도 새 holder도 없고, 같은 krilld가 답한다.
    assert!(!h.new_dir().exists());
    assert!(!h.old().is_symlink());
    assert_eq!(h.holders(), 1);
    assert_eq!(krilld_at(&h.old().join("run/krilld.sock")), krilld);
    // 세션이 끝나도 holder가 살아 있는 동안은 계속 옛 경로다.
    h.ok(&["kill", &id]);
    h.ok(&["ls"]);
    assert!(!h.new_dir().exists());
    assert_eq!(h.holders(), 1);
}

#[test]
fn idle_legacy_state_migrates_and_restarts_with_same_data() {
    let mut h = Home::new("idle");
    fs::create_dir_all(h.old().join("hooks")).unwrap();
    fs::write(
        h.old().join("agents.toml"),
        "[agent.mk170_probe]\nname = \"MK170 probe\"\nlaunch = [\"/bin/sh\", \"-c\", \"printf 'probe-ran\\\\n'\"]\n",
    )
    .unwrap();
    fs::write(h.old().join("hooks/codex.json"), "{}").unwrap();
    let logs = h.dir.join("Library/Logs");
    fs::create_dir_all(logs.join("Minikrill")).unwrap();
    let krilld = h.legacy_holder();
    let id = h.ok(&["new", "--", "/bin/sh", "-c", "printf 'before-migration\\n'"]);
    let id = id.trim().to_owned();
    h.ok(&["wait", &id, "--timeout", "5"]);
    wait_file(&h.old().join("sessions").join(format!("{id}.noindex")));
    // holder·krilld가 함께 사라진 상태(재부팅 등): 세션 0개, holder 없음.
    let holder = ppid(krilld);
    kill9(holder);
    kill9(krilld);
    assert_eq!(h.holders(), 0);

    // 새 CLI 한 번: 옮기고 새 경로에 holder를 띄운다.
    h.ok(&["ls"]);
    assert!(h.new_dir().is_dir() && !h.new_dir().is_symlink());
    assert_eq!(
        fs::read_link(h.old()).unwrap(),
        PathBuf::from("dev.krill.app")
    );
    assert_eq!(
        fs::read_link(logs.join("Minikrill")).unwrap(),
        PathBuf::from("Krill")
    );
    assert!(h.new_dir().join("hooks/codex.json").is_file());
    assert!(
        h.new_dir()
            .join("sessions")
            .join(format!("{id}.noindex"))
            .is_dir()
    );
    let new_krilld = krilld_at(&h.new_dir().join("run/krilld.sock"));
    h.pids.insert(0, ppid(new_krilld));
    h.pids.push(new_krilld);
    assert_eq!(h.holders(), 1);
    assert!(h.ok(&["agent", "list"]).contains("mk170_probe"));
    let probe = h.ok(&["new", "--agent", "mk170_probe"]);
    let probe = probe.trim();
    h.ok(&["wait", probe, "--timeout", "5"]);
    assert!(h.ok(&["read", probe]).contains("probe-ran"));
    // 다시 실행해도 그대로다.
    h.ok(&["ls"]);
    assert_eq!(h.holders(), 1);
    assert_eq!(krilld_at(&h.new_dir().join("run/krilld.sock")), new_krilld);
}
