#![cfg(unix)]
//! Remote-only intent must not fall through to local-only cleanup, even before SSH starts.

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

#[test]
fn local_only_worktree_commands_refuse_remote_target_without_starting_ssh() {
    let root = std::env::temp_dir().join(format!("krill-remote-cleanup-{}", std::process::id()));
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let called = root.join("ssh-called");
    let ssh = bin.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\n: > \"$SSH_CALLED\"\nexit 90\n").unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin.as_os_str().to_owned();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    for action in ["cleanup", "clean", "space", "set", "hooks", "list"] {
        let output = Command::new(env!("CARGO_BIN_EXE_krill"))
            .args(["--host", "fixture.invalid", "worktree", action])
            .env("PATH", &path)
            .env("SSH_CALLED", &called)
            .env("KRILL_STATE_DIR", root.join("state"))
            .env("KRILL_LOG_DIR", root.join("logs"))
            .env("DO_NOT_TRACK", "1")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to operate on local"));
        assert!(!called.exists(), "SSH must not start for {action}");
    }
    std::fs::remove_dir_all(root).unwrap();
}
