use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use crate::agent::hooks::Agent;
use crate::hook_install::refresh_codex_trust;

/// Account homes get hashes from their own Codex for the exact command, never copied ones.
#[test]
fn account_home_trust_is_computed_by_codex_and_keeps_user_config() {
    let root = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("kp-codex-trust-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let bin = root.join("bin");
    let home = root.join("account");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&home).unwrap();
    let codex = bin.join("codex");
    fs::write(
        &codex,
        include_str!("../../../../krill/tests/fixtures/mk413-codex.py"),
    )
    .unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    let key = "/<session-flags>/config.toml:stop:0:0";
    let config = home.join("config.toml");
    // Provisioning copies the host config once, including hashes for an older command.
    fs::write(&config, format!("model = 'mine'\n[hooks.state.\"user-key\"]\ntrusted_hash = 'user-hash'\n[hooks.state.\"{key}\"]\ntrusted_hash = 'copied-from-host'\n")).unwrap();
    let command = "exec '/new/krill' hook codex";
    let search = Some(bin.as_os_str());

    assert!(refresh_codex_trust(&home, command, search).unwrap());
    let written = fs::read_to_string(&config).unwrap();
    let value: toml::Value = toml::from_str(&written).unwrap();
    let state = value["hooks"]["state"].as_table().unwrap();
    assert_eq!(state.len(), Agent::Codex.events().len() + 1);
    assert_eq!(
        state["user-key"]["trusted_hash"].as_str(),
        Some("user-hash")
    );
    assert_ne!(
        state[key]["trusted_hash"].as_str(),
        Some("copied-from-host")
    );
    assert_eq!(value["model"].as_str(), Some("mine"));

    // Already trusted: nothing is written.
    assert!(!refresh_codex_trust(&home, command, search).unwrap());
    assert_eq!(fs::read_to_string(&config).unwrap(), written);

    // A linked home is refused before Codex can write through it.
    let alias = root.join("alias");
    symlink(&home, &alias).unwrap();
    assert!(refresh_codex_trust(&alias, "exec '/moved/krill' hook codex", search).is_err());
    assert_eq!(fs::read_to_string(&config).unwrap(), written);
    fs::remove_dir_all(root).unwrap();
}
