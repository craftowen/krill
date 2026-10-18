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
        .filter(|e| e.file_name().to_string_lossy().contains("minikrill-bak"))
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
    let plugin = home.0.join("minikrill.js");
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
