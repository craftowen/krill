//! Windows worktree lifecycle through the real CLI, krilld and Git, in a Korean path with spaces.
//! krilld used to hand Git `\\?\C:\...` paths, which Git rewrote to `//?/C:/...` and failed with
//! `could not open .../.git for writing`. The session step needs bundled `ConPTY`:
//! `KRILL_TEST_CONPTY_DIR=<conpty.dll, x64\OpenConsole.exe> cargo nextest run -p krill --test windows_worktree`.
#![cfg(windows)]
#![expect(clippy::unwrap_used, reason = "isolated integration test helpers")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::daemon;
use krill_proto::ipc::ClientRole;
use serde_json::Value;

const KRILL: &str = env!("CARGO_BIN_EXE_krill");

struct Rig {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
    holder: Child,
}

impl Rig {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("krill-wt 한글 공백-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let (repo, state, home, install) = (
            root.join("레포 이름"),
            root.join("state"),
            root.join("home"),
            root.join("install"),
        );
        for dir in [&repo, &state, &home, &install] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(repo.join("README.md"), "hi\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "."],
            &["commit", "-qm", "init"],
        ] {
            git(&repo, args);
        }
        let bin = Path::new(KRILL).parent().unwrap();
        for name in ["krill-holder.exe", "krilld.exe"] {
            fs::copy(bin.join(name), install.join(name)).unwrap();
        }
        if let Some(bundle) = std::env::var_os("KRILL_TEST_CONPTY_DIR").map(PathBuf::from) {
            fs::copy(bundle.join("conpty.dll"), install.join("conpty.dll")).unwrap();
            fs::create_dir_all(install.join("x64")).unwrap();
            fs::copy(
                bundle.join("x64/OpenConsole.exe"),
                install.join("x64/OpenConsole.exe"),
            )
            .unwrap();
        }
        let socket = state.join("run/krilld.sock");
        let holder = Command::new(install.join("krill-holder.exe"))
            .arg(install.join("krilld.exe"))
            .arg(&socket)
            .env("KRILL_STATE_DIR", &state)
            .env("KRILL_WORKTREES_DIR", root.join("작업 트리"))
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LOCALAPPDATA", home.join("Local"))
            .env("APPDATA", home.join("Roaming"))
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("KRILL_HOLDER_STATE")
            .env_remove("KRILL_HOLDER_LINK")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let rig = Self {
            root,
            repo,
            state,
            home,
            holder,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let hello = daemon::client_hello(ClientRole::Cli, "windows-worktree-test");
        while daemon::handshake(&socket, &hello, Duration::from_secs(2)).is_err() {
            assert!(Instant::now() < deadline, "daemon did not start");
            thread::sleep(Duration::from_millis(20));
        }
        rig
    }

    fn krill(&self, args: &[&str]) -> Output {
        Command::new(KRILL)
            .args(args)
            .current_dir(&self.repo)
            .env("KRILL_STATE_DIR", &self.state)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.krill(args);
        assert!(out.status.success(), "krill {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    /// `krill worktree new` prints the created path first.
    fn create(&self, args: &[&str]) -> (PathBuf, String) {
        let out = self.ok(&[&["worktree", "new"], args].concat());
        let mut lines = out.lines();
        let path = PathBuf::from(lines.next().unwrap());
        assert_plain(&path.to_string_lossy());
        assert!(path.join(".git").is_file(), "{}", path.display());
        (path, lines.next().unwrap_or_default().to_owned())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let script = "$r=$env:KRILL_WT_TEST_ROOT; Get-Process krilld,krill-holder,OpenConsole -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($r + '\\', [StringComparison]::OrdinalIgnoreCase) } | Stop-Process -Force -ErrorAction SilentlyContinue";
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("KRILL_WT_TEST_ROOT", &self.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
        self.holder.kill().ok();
        self.holder.wait().ok();
        fs::remove_dir_all(&self.root).ok();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn assert_plain(text: &str) {
    assert!(
        !text.contains(r"\\?\") && !text.contains("//?/"),
        "verbatim path leaked: {text}"
    );
}

/// Git lists `C:/x`; compare as paths so separators do not matter.
fn registered(repo: &Path, path: &Path) -> bool {
    git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| Path::new(listed) == path)
}

#[test]
fn worktree_create_list_session_and_remove_in_korean_path_with_spaces() {
    let rig = Rig::new();
    let repo = rig.repo.to_str().unwrap();

    let (first, branch) = rig.create(&["vt1", "--repo", repo]);
    assert!(first.ends_with("레포 이름/vt1"), "{}", first.display());
    assert!(branch.starts_with("branch vt1 "), "{branch}");
    assert!(registered(&rig.repo, &first));
    // The name is taken, so krilld must see Git's `C:/...` entry as the same path and pick `-2`.
    let (second, _) = rig.create(&["vt1", "--repo", repo]);
    assert!(second.ends_with("레포 이름/vt1-2"), "{}", second.display());
    let (child, branch) = rig.create(&["child", "--parent", first.to_str().unwrap()]);
    assert!(branch.ends_with("(base vt1)"), "{branch}");

    let list = rig.ok(&["worktree", "list", "--repo", repo, "--json"]);
    assert_plain(&list);
    let list: Value = serde_json::from_str(&list).unwrap();
    for path in [&first, &second, &child] {
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|row| row["path"].as_str().map(Path::new) == Some(path)),
            "{} not in {list}",
            path.display()
        );
    }

    let show = rig.ok(&["worktree", "show", child.to_str().unwrap(), "--json"]);
    assert_plain(&show);
    let show: Value = serde_json::from_str(&show).unwrap();
    assert_eq!(
        show["worktree"].as_str().map(Path::new),
        Some(child.as_path())
    );
    assert_eq!(
        show["parent"].as_str().map(Path::new),
        Some(first.as_path())
    );

    if std::env::var_os("KRILL_TEST_CONPTY_DIR").is_some() {
        let out = rig.ok(&[
            "new",
            "--worktree",
            "vt3",
            "--json",
            "--",
            "cmd.exe",
            "/d",
            "/k",
        ]);
        assert_plain(&out);
        let created: Value = serde_json::from_str(&out).unwrap();
        let cwd = PathBuf::from(created["cwd"].as_str().unwrap());
        assert!(cwd.ends_with("레포 이름/vt3"), "{}", cwd.display());
        assert!(registered(&rig.repo, &cwd));
        let id = created["session"].as_u64().unwrap().to_string();
        let ls = rig.ok(&["ls", "--json"]);
        assert_plain(&ls);
        let ls: Value = serde_json::from_str(&ls).unwrap();
        let row = ls
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"].as_u64().map(|n| n.to_string()) == Some(id.clone()))
            .unwrap_or_else(|| panic!("session {id} not in {ls}"));
        assert_eq!(row["worktree"].as_str().map(Path::new), Some(cwd.as_path()));
        rig.ok(&["kill", &id]);
    } else {
        eprintln!("KRILL_TEST_CONPTY_DIR not set: skipping the session step");
    }

    rig.ok(&["worktree", "rm", second.to_str().unwrap(), "--repo", repo]);
    assert!(!second.exists());
    assert!(!registered(&rig.repo, &second));
    assert!(registered(&rig.repo, &first));
}
