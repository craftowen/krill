//! Automatic bundled skill discovery/update without a GUI or provider/model call.
#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "isolated integration test helpers")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const CLI: &str = env!("CARGO_BIN_EXE_krill");

fn home(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("krill-skill-cli-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    root
}

fn command(home: &Path) -> Command {
    let mut cmd = Command::new(CLI);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("KRILL_") || name.starts_with("MK_") || name.starts_with("MINIKRILL_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home.join("local"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("CODEX_HOME", home.join(".codex"))
        .stdin(Stdio::null());
    cmd
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn cli_start_automatically_installs_and_updates_owned_skills_and_keeps_orca() {
    let home = home("auto");
    let orca = home.join(".gemini/config/skills/orchestration");
    fs::create_dir_all(&orca).unwrap();
    fs::write(orca.join("SKILL.md"), "external Orca discovery stub").unwrap();
    success(
        command(&home)
            .args(["skills", "list", "--json"])
            .output()
            .unwrap(),
    );
    for root in krill_proto::orchestration::skill_directories(&home, "all") {
        assert!(
            root.join("krill-orchestration/references/worker-contract.md")
                .is_file()
        );
    }
    let installed = home.join(".gemini/antigravity-cli/skills/krill-orchestration/SKILL.md");
    fs::write(
        &installed,
        "old version\n<!-- minikrill-managed-skill -->\n",
    )
    .unwrap();
    let canonical = success(
        command(&home)
            .args(["skills", "get", "krill-orchestration"])
            .output()
            .unwrap(),
    );
    let current = fs::read_to_string(installed).unwrap();
    assert!(current.contains("name: krill-orchestration\n"));
    assert!(current.contains("# Krill orchestration"));
    assert!(canonical.contains("# Krill orchestration"));
    assert_eq!(
        fs::read_to_string(orca.join("SKILL.md")).unwrap(),
        "external Orca discovery stub"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn isolated_state_and_hot_hook_do_not_trigger_host_skill_installation() {
    let home = home("isolated");
    success(
        command(&home)
            .env("KRILL_STATE_DIR", home.join("state"))
            .args(["skills", "list", "--json"])
            .output()
            .unwrap(),
    );
    assert!(!home.join(".claude/skills").exists());
    success(
        command(&home)
            .args(["hook", "agy", "Stop"])
            .output()
            .unwrap(),
    );
    assert!(!home.join(".gemini/config/skills").exists());
    success(
        command(&home)
            .env("KRILL_STATE_DIR", home.join("state"))
            .args([
                "skills",
                "install",
                "krill-orchestration",
                "--target",
                "antigravity",
                "--global",
                "--json",
            ])
            .output()
            .unwrap(),
    );
    for root in krill_proto::orchestration::skill_directories(&home, "agy") {
        assert!(root.join("krill-orchestration/SKILL.md").is_file());
    }
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn relative_manual_install_refuses_a_symlink_directory() {
    use std::os::unix::fs::symlink;
    let home = home("relative-symlink");
    let outside = home.join("outside");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, home.join("redirect")).unwrap();
    let output = command(&home)
        .current_dir(&home)
        .env("KRILL_STATE_DIR", home.join("state"))
        .args([
            "skills",
            "install",
            "orchestration",
            "--dir",
            "redirect",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("symlink"));
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    fs::remove_dir_all(home).unwrap();
}
