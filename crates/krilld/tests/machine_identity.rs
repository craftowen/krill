#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트의 준비와 관찰 실패를 즉시 알린다"
)]

use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use krill_proto::ipc::{ClientRole, DaemonIdentity, MachineIdentity};

struct Directory(PathBuf);

impl Directory {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kmi-{}-{tag}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let state = path.join("state");
        fs::create_dir(&state).unwrap();
        let shell = path.join("fixture-shell");
        fs::write(
            &shell,
            "#!/bin/sh\nif [ \"$1\" = -l ]; then shift; fi\nif [ \"$1\" = -i ]; then shift; fi\nexec /bin/sh \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        let mut agents = krill_proto::agent::Agents::load(None).unwrap();
        for (id, profile) in &mut agents.profiles {
            profile.launch = vec![
                path.join(format!("uninstalled-{id}"))
                    .to_string_lossy()
                    .into_owned(),
            ];
        }
        fs::write(
            state.join(krill_proto::agent::FILE_NAME),
            toml::to_string(&agents).unwrap(),
        )
        .unwrap();
        Self(path)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("daemon.sock")
    }

    fn spawn(&self) -> Daemon {
        Daemon(
            Command::new(env!("CARGO_BIN_EXE_krilld"))
                .arg(self.socket())
                .env("KRILL_STATE_DIR", self.0.join("state"))
                .env("KRILL_LOG_DIR", self.0.join("logs"))
                .env("HOME", &self.0)
                .env("ZDOTDIR", &self.0)
                .env("SHELL", self.0.join("fixture-shell"))
                .env("ENV", "/dev/null")
                .env("BASH_ENV", "/dev/null")
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .env("DO_NOT_TRACK", "1")
                .env("CLAUDE_CONFIG_DIR", self.0.join("claude"))
                .env("CODEX_HOME", self.0.join("codex"))
                .env("XDG_CONFIG_HOME", self.0.join("config"))
                .env("XDG_DATA_HOME", self.0.join("data"))
                .env("XDG_STATE_HOME", self.0.join("state"))
                .env("XDG_CACHE_HOME", self.0.join("cache"))
                .env_remove("ANTHROPIC_API_KEY")
                .env_remove("OPENAI_API_KEY")
                .env_remove("GEMINI_API_KEY")
                .env_remove("GOOGLE_API_KEY")
                .env_remove("KRILL_ACCOUNT_PROVIDER")
                .env_remove("KRILL_ACCOUNT_DIR")
                .env_remove("KRILL_TEST_BALLAST_MB")
                .current_dir(&self.0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn identity(socket: &Path) -> Option<(DaemonIdentity, Option<MachineIdentity>)> {
    let hello = krill_proto::daemon::client_hello(ClientRole::App, "machine-identity-test");
    let connection = krill_proto::daemon::handshake(socket, &hello, Duration::from_secs(1)).ok()?;
    Some((connection.daemon_identity?, connection.machine_identity))
}

fn wait_identity(socket: &Path) -> (DaemonIdentity, Option<MachineIdentity>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = identity(socket) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not publish its identity"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn real_daemon_restart_preserves_machine_identity_and_changes_epoch() {
    let directory = Directory::new("restart");
    let first = directory.spawn();
    let (epoch, machine) = wait_identity(&directory.socket());
    assert!(machine.is_some());
    drop(first);
    let _second = directory.spawn();
    let (new_epoch, new_machine) = wait_identity(&directory.socket());
    assert_ne!(new_epoch, epoch);
    assert_eq!(new_machine, machine);
    assert!(
        !directory
            .0
            .join("state/run/krilld.sock.machine-id")
            .exists()
    );
}

#[test]
fn corrupt_machine_identity_does_not_disable_real_daemon() {
    let directory = Directory::new("corrupt");
    let stored = directory.0.join("daemon.sock.machine-id");
    fs::write(&stored, b"damaged").unwrap();
    let _daemon = directory.spawn();
    let (_, machine) = wait_identity(&directory.socket());
    assert_eq!(machine, None);
    assert_eq!(fs::read(stored).unwrap(), b"damaged");
}
