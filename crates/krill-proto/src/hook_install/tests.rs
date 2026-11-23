use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mk661-hooks-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(fs::canonicalize(dir).unwrap())
    }

    fn path(&self) -> PathBuf {
        self.0.join("settings.json")
    }

    fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    fn receipt(&self) -> PathBuf {
        self.state().join("hooks/claude.json")
    }

    fn prepare(&self, action: Action) -> Plan {
        Plan::prepare(
            action,
            TargetAgent::Claude,
            &self.path(),
            &self.state(),
            Path::new("/example/krill"),
        )
        .unwrap()
        .unwrap()
    }

    fn install(&self) -> io::Result<Option<Receipt>> {
        install_hooks(
            TargetAgent::Claude,
            &self.path(),
            &self.state(),
            Path::new("/example/krill"),
        )
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
#[cfg(unix)]
fn shared_install_receipt_backup_and_rollback_preserve_original_bytes_and_mode() {
    let home = Home::new();
    let original = "{\n\t\"user\": \"한글\", \"hooks\": {}\n}\n";
    fs::write(home.path(), original).unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o640)).unwrap();
    let plan = home.prepare(Action::Install);
    let mut diff = Vec::new();
    plan.write_diff(&mut diff).unwrap();
    assert!(String::from_utf8(diff).unwrap().contains("+++"));
    assert!(!home.state().exists(), "preparation must be read-only");
    let receipt = plan.apply().unwrap();
    assert!(receipt.complete);
    let saved: Receipt =
        serde_json::from_str(&fs::read_to_string(home.receipt()).unwrap()).unwrap();
    assert!(saved.complete);
    assert_eq!(saved.agent, "claude");
    assert_eq!(
        saved.changes.first().unwrap().before.as_deref(),
        Some(original)
    );
    assert!(home.install().unwrap().is_none());
    assert_eq!(
        fs::metadata(home.path()).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let backups: Vec<_> = fs::read_dir(&home.0)
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.file_name().to_string_lossy().contains("krill-bak"))
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = backups.first().unwrap();
    assert_eq!(fs::read_to_string(backup.path()).unwrap(), original);
    assert_eq!(
        backup.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(home.receipt()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    home.prepare(Action::Uninstall).apply().unwrap();
    assert_eq!(fs::read_to_string(home.path()).unwrap(), original);
    assert!(!home.receipt().exists());
}

#[test]
fn shared_install_refuses_lock_contention_and_stale_config_or_receipt_snapshots() {
    let home = Home::new();
    fs::write(home.path(), "{}").unwrap();
    let plan = home.prepare(Action::Install);
    let lock = store::lock(&home.state().join("hooks")).unwrap();
    assert!(
        plan.apply()
            .unwrap_err()
            .to_string()
            .contains("another hook operation")
    );
    assert_eq!(fs::read_to_string(home.path()).unwrap(), "{}");
    assert!(!home.receipt().exists());
    drop(lock);

    let plan = home.prepare(Action::Install);
    fs::write(home.path(), "{\"user\":true}").unwrap();
    assert!(
        plan.apply()
            .unwrap_err()
            .to_string()
            .contains("configuration changed")
    );
    assert_eq!(fs::read_to_string(home.path()).unwrap(), "{\"user\":true}");
    assert!(!home.receipt().exists());

    let first = home.prepare(Action::Install);
    let second = home.prepare(Action::Install);
    first.apply().unwrap();
    let installed = fs::read(home.path()).unwrap();
    let receipt = fs::read(home.receipt()).unwrap();
    assert!(
        second
            .apply()
            .unwrap_err()
            .to_string()
            .contains("hook receipt changed")
    );
    assert_eq!(fs::read(home.path()).unwrap(), installed);
    assert_eq!(fs::read(home.receipt()).unwrap(), receipt);
}

#[test]
#[cfg(unix)]
fn invalid_json_utf8_size_and_symlinks_preserve_originals_without_a_receipt() {
    let home = Home::new();
    for original in [
        b"{broken".to_vec(),
        b"[]".to_vec(),
        b"{\"hooks\":false}".to_vec(),
        vec![0xff, 0xfe],
        vec![
            b' ';
            usize::try_from(store::LIMIT)
                .unwrap()
                .checked_add(1)
                .unwrap()
        ],
    ] {
        fs::write(home.path(), &original).unwrap();
        assert!(home.install().is_err());
        assert_eq!(fs::read(home.path()).unwrap(), original);
        assert!(!home.state().exists());
    }
    fs::remove_file(home.path()).unwrap();
    let original = home.0.join("original.json");
    fs::write(&original, "{}").unwrap();
    symlink(&original, home.path()).unwrap();
    assert!(home.install().is_err());
    assert_eq!(fs::read_to_string(original).unwrap(), "{}");
    assert!(!home.state().exists());

    let link = home.0.join("linked-parent");
    symlink(&home.0, &link).unwrap();
    assert!(
        install_hooks(
            TargetAgent::Claude,
            &link.join("new.json"),
            &home.state(),
            Path::new("krill")
        )
        .is_err()
    );
    assert!(!home.0.join("new.json").exists());
}

#[test]
fn incomplete_receipt_can_be_recovered_and_uninstall_preserves_later_user_changes() {
    let home = Home::new();
    let original = "{\"user\":\"original\"}";
    fs::write(home.path(), original).unwrap();
    let plan = home.prepare(Action::Install);
    // Crash fixture: intent was written and config replaced, completion was never recorded.
    store::directory(&home.state().join("hooks")).unwrap();
    fs::write(
        home.receipt(),
        serde_json::to_string(&plan.receipt).unwrap(),
    )
    .unwrap();
    fs::write(home.path(), &plan.receipt.changes.first().unwrap().after).unwrap();
    assert!(
        home.install()
            .unwrap_err()
            .to_string()
            .contains("interrupted hook installation")
    );
    home.prepare(Action::Uninstall).apply().unwrap();
    assert_eq!(fs::read_to_string(home.path()).unwrap(), original);

    home.install().unwrap();
    let mut user: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.path()).unwrap()).unwrap();
    user.as_object_mut()
        .unwrap()
        .insert("later".to_owned(), serde_json::json!(true));
    fs::write(home.path(), user.to_string()).unwrap();
    home.prepare(Action::Uninstall).apply().unwrap();
    let current: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.path()).unwrap()).unwrap();
    assert_eq!(
        current,
        serde_json::json!({"user":"original", "later":true})
    );
}

// OWE-714 D2: 기존 플러그인과의 충돌은 `AlreadyExists`로 구분돼야 앱이 "실패"가 아닌 안내로 보여 준다.
#[test]
fn existing_opencode_plugin_is_an_already_exists_conflict_and_untouched() {
    let home = Home::new();
    let plugin = home.0.join("krill.js");
    fs::write(&plugin, "user plugin").unwrap();
    let error = Plan::prepare(
        Action::Install,
        TargetAgent::Other(Agent::OpenCode),
        &plugin,
        &home.state(),
        Path::new("/example/krill"),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
    assert_eq!(fs::read_to_string(&plugin).unwrap(), "user plugin");
    assert!(!home.state().exists());
}

#[cfg(unix)]
#[test]
fn claude_settings_path_follows_only_the_shared_account_link() {
    let root = std::env::temp_dir().join(format!("krill-shared-settings-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let host = root.join(".claude");
    std::fs::create_dir_all(&host).unwrap();
    let root = root.canonicalize().unwrap();
    let host = root.join(".claude");
    std::fs::write(host.join("settings.json"), "{}").unwrap();
    let shared = root.join("shared");
    let private = root.join("private");
    let custom = root.join("custom");
    for dir in [&shared, &private, &custom] {
        std::fs::create_dir(dir).unwrap();
    }
    std::os::unix::fs::symlink(host.join("settings.json"), shared.join("settings.json")).unwrap();
    std::fs::write(private.join("settings.json"), "{}").unwrap();
    std::fs::write(root.join("dotfile.json"), "{}").unwrap();
    std::os::unix::fs::symlink(root.join("dotfile.json"), custom.join("settings.json")).unwrap();
    assert_eq!(
        claude_settings_path(&shared, &root),
        host.join("settings.json")
    );
    assert_eq!(
        claude_settings_path(&private, &root),
        private.join("settings.json")
    );
    assert_eq!(
        claude_settings_path(&custom, &root),
        custom.join("settings.json"),
        "사용자가 만든 다른 링크는 기존처럼 저장소가 거부한다"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn legacy_agy_receipt_upgrades_in_place_preserving_later_user_edits() {
    let home = Home::new();
    let hooks_path = home.0.join(".gemini/config/hooks.json");
    fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    let receipt_path = hooks::receipt(&home.state(), Agent::Agy);
    let legacy_before = serde_json::json!({
        "orca-status": {
            "PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": "orca-v1"}]}]
        }
    })
    .to_string();
    let legacy_krill = serde_json::json!({
        "PreToolUse": [{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": "/bin/sh -c 'export MK_HOOK_EVENT='\"'\"'PreToolUse'\"'\"'; if [ -n \"${MK_PANE_ID:-}\" ]; then exec '\"'\"'/Applications/Krill.app/Contents/MacOS/krill'\"'\"' hook agy '\"'\"'PreToolUse'\"'\"'; else printf '\"'\"'{\"decision\":\"ask\"}\\n'\"'\"'; fi'",
                "timeout": 10
            }]
        }]
    });
    let legacy_after = serde_json::to_string_pretty(&serde_json::json!({
        "orca-status": {
            "PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": "orca-v1"}]}]
        },
        "krill": legacy_krill
    }))
    .unwrap()
        + "\n";
    // Simulate Orca updating its own bundle after Krill's legacy install.
    let current_on_disk = serde_json::to_string_pretty(&serde_json::json!({
        "orca-status": {
            "PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": "orca-v2"}]}]
        },
        "krill": legacy_krill
    }))
    .unwrap()
        + "\n";
    fs::write(&hooks_path, &current_on_disk).unwrap();
    store::directory(receipt_path.parent().unwrap()).unwrap();
    let legacy_receipt = Receipt {
        agent: "agy".to_owned(),
        complete: true,
        changes: vec![Change {
            path: hooks_path.clone(),
            before: Some(legacy_before),
            after: legacy_after,
        }],
        codex: None,
    };
    assert!(legacy_receipt.needs_upgrade());
    fs::write(
        &receipt_path,
        serde_json::to_string_pretty(&legacy_receipt).unwrap() + "\n",
    )
    .unwrap();

    // Installing again upgrades the legacy synchronous hook in place and keeps `orca-v2`.
    let upgraded = install_hooks(
        TargetAgent::Other(Agent::Agy),
        &hooks_path,
        &home.state(),
        Path::new("/Applications/Krill.app/Contents/MacOS/krill"),
    )
    .unwrap()
    .expect("legacy agy receipt should be upgraded");
    assert!(!upgraded.needs_upgrade());
    let installed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    assert_eq!(
        installed["orca-status"]["PreToolUse"][0]["hooks"][0]["command"],
        "orca-v2"
    );
    let pre_cmd = installed["krill"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(pre_cmd.contains(hooks::AGY_ASYNC_HOOK_MARKER));
    assert!(!pre_cmd.contains("then exec "));

    // Subsequent install is a no-op.
    assert!(
        install_hooks(
            TargetAgent::Other(Agent::Agy),
            &hooks_path,
            &home.state(),
            Path::new("/Applications/Krill.app/Contents/MacOS/krill"),
        )
        .unwrap()
        .is_none()
    );

    // Uninstall removes only the upgraded `krill` bundle and keeps `orca-v2`.
    Plan::prepare(
        Action::Uninstall,
        TargetAgent::Other(Agent::Agy),
        &hooks_path,
        &home.state(),
        Path::new("/Applications/Krill.app/Contents/MacOS/krill"),
    )
    .unwrap()
    .unwrap()
    .apply()
    .unwrap();
    let after_uninstall: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    assert!(after_uninstall.get("krill").is_none());
    assert_eq!(
        after_uninstall["orca-status"]["PreToolUse"][0]["hooks"][0]["command"],
        "orca-v2"
    );
}

#[test]
fn codex_same_config_path_matches_verbatim_and_noncanonical_spellings() {
    assert!(codex::same_config_path(
        Path::new(r"\\?\C:\Users\me\.codex\config.toml"),
        Path::new(r"C:\Users\me\.codex\config.toml"),
    ));
    assert!(codex::same_config_path(
        Path::new(r"\\?\C:\Users\me\.codex\config.toml"),
        Path::new("c:/users/me/.codex/config.toml"),
    ));
    assert!(!codex::same_config_path(
        Path::new(r"C:\Users\me\.codex\config.toml"),
        Path::new(r"C:\Users\other\.codex\config.toml"),
    ));

    let home = Home::new();
    let sub = home.0.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let canonical = home.0.join("config.toml");
    let non_canonical = home.0.join("sub/../config.toml");
    assert_ne!(canonical, non_canonical);
    assert!(codex::same_config_path(&non_canonical, &canonical));
}

#[cfg(unix)]
#[test]
fn legacy_codex_powershell_receipt_supersedes_equivalent_config_path() {
    let home = Home::new();
    let bin = home.0.join("bin");
    let codex_home = home.0.join("codex");
    let sub = codex_home.join("sub");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&sub).unwrap();

    let codex_exe = bin.join("codex");
    fs::write(
        &codex_exe,
        include_str!("../../../krill/tests/fixtures/mk413-codex.py"),
    )
    .unwrap();
    fs::set_permissions(&codex_exe, fs::Permissions::from_mode(0o700)).unwrap();

    let config = codex_home.join("config.toml");
    let legacy_path = codex_home.join("sub/../config.toml");
    assert_ne!(legacy_path, config);
    let legacy_command = format!("{}AAAA", hooks::POWERSHELL_CODEX_PREFIX);
    let legacy_receipt = Receipt {
        agent: "codex".to_owned(),
        complete: true,
        changes: Vec::new(),
        codex: Some(CodexRecord {
            path: legacy_path,
            command: legacy_command.clone(),
            added: std::collections::BTreeMap::new(),
            replaced: std::collections::BTreeMap::new(),
        }),
    };
    let receipt_path = hooks::receipt(&home.state(), Agent::Codex);
    store::directory(receipt_path.parent().unwrap()).unwrap();
    fs::write(
        &receipt_path,
        serde_json::to_string_pretty(&legacy_receipt).unwrap() + "\n",
    )
    .unwrap();

    let plan = Plan::prepare_with_trust(
        Action::Install,
        TargetAgent::Other(Agent::Codex),
        &config,
        &home.state(),
        Path::new("/example/krill"),
        Some(bin.as_os_str()),
        CodexTrust::Preserve,
    )
    .unwrap()
    .expect("legacy PowerShell Codex receipt with equivalent path must be superseded");
    let updated = plan.apply().unwrap();
    assert!(updated.complete);
    let codex = updated.codex.unwrap();
    assert_ne!(codex.command, legacy_command);
    assert_eq!(codex.path, config);
}

#[cfg(unix)]
#[test]
fn store_replace_preserves_group_writable_mode_past_umask() {
    let home = Home::new();
    let path = home.0.join("group-writable.json");
    fs::write(&path, "{\"a\":1}\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
    store::replace(&path, Some("{\"a\":1}\n"), Some("{\"a\":2}\n")).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o664
    );
}
