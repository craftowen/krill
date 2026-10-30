use std::fs;
use std::path::{Path, PathBuf};

use super::super::Report;
use super::super::history_store::archive_root;
use super::{BACKUP_NAME, share};

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("krill-claude-config-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let host = root.join("home/.claude");
    let profile = root.join("profile");
    fs::create_dir_all(&host).unwrap();
    fs::create_dir_all(&profile).unwrap();
    let root = root.canonicalize().unwrap();
    (
        root.clone(),
        root.join("home/.claude"),
        root.join("profile"),
    )
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn run(host: &Path, profile: &Path) -> Report {
    let mut report = Report::default();
    share(host, profile, &mut report).unwrap();
    report
}

#[test]
fn every_user_extension_is_linked_and_account_state_stays_private() {
    let (root, host, profile) = fixture("scan");
    for name in ["output-styles", "custom-tools", "plugins"] {
        fs::create_dir_all(host.join(name)).unwrap();
    }
    for name in [
        "cache",
        "sessions",
        "backups",
        "debug",
        "state",
        "projects-merged-backup",
    ] {
        fs::create_dir_all(host.join(name)).unwrap();
    }
    fs::create_dir_all(host.join("daemon-x")).unwrap();
    fs::create_dir_all(host.join("agent-memory")).unwrap();
    write(&host.join(".credentials.json"), "secret");
    write(&host.join("usage_cache.json"), "{}");
    write(&host.join("keybindings.json"), "{}");
    run(&host, &profile);
    for name in [
        "output-styles",
        "custom-tools",
        "plugins",
        "rules",
        "skills",
    ] {
        assert_eq!(fs::read_link(profile.join(name)).unwrap(), host.join(name));
    }
    assert_eq!(
        fs::read_link(profile.join("keybindings.json")).unwrap(),
        host.join("keybindings.json")
    );
    for name in [
        "cache",
        "sessions",
        "backups",
        "debug",
        "state",
        "projects-merged-backup",
        "daemon-x",
        "agent-memory",
        ".credentials.json",
        "usage_cache.json",
    ] {
        assert!(profile.join(name).symlink_metadata().is_err(), "{name}");
    }
    assert_eq!(run(&host, &profile), Report::default());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_are_seeded_once_and_written_through_the_link() {
    let (root, host, profile) = fixture("settings");
    run(&host, &profile);
    assert_eq!(
        fs::read_to_string(host.join("settings.json")).unwrap(),
        "{}\n"
    );
    fs::write(profile.join("settings.json"), "{\"model\":\"opus\"}").unwrap();
    assert_eq!(
        fs::read_to_string(host.join("settings.json")).unwrap(),
        "{\"model\":\"opus\"}"
    );
    assert_eq!(run(&host, &profile), Report::default());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn replaced_link_is_restored_without_losing_enabled_plugins() {
    let (root, host, profile) = fixture("relink");
    write(
        &host.join("settings.json"),
        r#"{"theme":"dark","enabledPlugins":{"host@m":true}}"#,
    );
    run(&host, &profile);
    fs::remove_file(profile.join("settings.json")).unwrap();
    write(
        &profile.join("settings.json"),
        r#"{"theme":"light","enabledPlugins":{"host@m":false,"own@m":true}}"#,
    );
    run(&host, &profile);
    assert_eq!(
        fs::read_link(profile.join("settings.json")).unwrap(),
        host.join("settings.json")
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(host.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings["theme"], "dark", "기본 설정이 우선한다");
    assert_eq!(settings["enabledPlugins"]["host@m"], true);
    assert_eq!(settings["enabledPlugins"]["own@m"], true);
    let archive = archive_root(&host, &profile, BACKUP_NAME);
    assert!(
        fs::read_to_string(archive.join("settings.json/original"))
            .unwrap()
            .contains("light")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn profile_only_plugin_moves_to_the_host_registry_with_its_files() {
    let (root, host, profile) = fixture("plugins");
    let own = profile.join("plugins");
    write(
        &host.join("plugins/installed_plugins.json"),
        r#"{"version":2,"plugins":{"shared@m":[{"installPath":"/host/shared"}]}}"#,
    );
    write(
        &host.join("plugins/known_marketplaces.json"),
        r#"{"m":{"installLocation":"/host/m"}}"#,
    );
    write(&host.join("plugins/marketplaces/m/README"), "host clone");
    write(&own.join("marketplaces/m/README"), "profile clone");
    write(&own.join("marketplaces/m/only-in-profile"), "stale");
    write(&own.join("marketplaces/n/README"), "profile only");
    write(&own.join("cache/n/own/1.0.0/skills/a/SKILL.md"), "skill");
    write(
        &own.join("installed_plugins.json"),
        &format!(
            r#"{{"version":2,"plugins":{{"shared@m":[{{"installPath":"/profile/shared"}}],"own@n":[{{"installPath":"{}"}}]}}}}"#,
            own.join("cache/n/own/1.0.0").display()
        ),
    );
    write(
        &own.join("known_marketplaces.json"),
        &format!(
            r#"{{"m":{{"installLocation":"/profile/m"}},"n":{{"installLocation":"{}"}}}}"#,
            own.join("marketplaces/n").display()
        ),
    );
    write(
        &profile.join("settings.json"),
        r#"{"enabledPlugins":{"own@n":true},"extraKnownMarketplaces":{"n":{"source":{"source":"github","repo":"o/n"}}}}"#,
    );
    run(&host, &profile);

    assert_eq!(fs::read_link(&own).unwrap(), host.join("plugins"));
    assert!(
        host.join("plugins/cache/n/own/1.0.0/skills/a/SKILL.md")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(host.join("plugins/marketplaces/m/README")).unwrap(),
        "host clone"
    );
    assert!(
        !host.join("plugins/marketplaces/m/only-in-profile").exists(),
        "완결된 저장소는 파일별로 섞지 않는다"
    );
    let installed: serde_json::Value =
        serde_json::from_slice(&fs::read(host.join("plugins/installed_plugins.json")).unwrap())
            .unwrap();
    assert_eq!(
        installed["plugins"]["shared@m"][0]["installPath"],
        "/host/shared"
    );
    assert_eq!(
        installed["plugins"]["own@n"][0]["installPath"],
        host.join("plugins/cache/n/own/1.0.0").to_str().unwrap()
    );
    let known: serde_json::Value =
        serde_json::from_slice(&fs::read(host.join("plugins/known_marketplaces.json")).unwrap())
            .unwrap();
    assert_eq!(known["m"]["installLocation"], "/host/m");
    assert_eq!(
        known["n"]["installLocation"],
        host.join("plugins/marketplaces/n").to_str().unwrap()
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(host.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings["enabledPlugins"]["own@n"], true);
    assert_eq!(
        settings["extraKnownMarketplaces"]["n"]["source"]["repo"],
        "o/n"
    );
    let archive = archive_root(&host, &profile, BACKUP_NAME);
    assert_eq!(
        fs::read_to_string(archive.join("plugins/original/marketplaces/m/only-in-profile"))
            .unwrap(),
        "stale"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_is_created_with_host_paths_when_the_host_has_none() {
    let (root, host, profile) = fixture("registry-new");
    let own = profile.join("plugins");
    write(
        &own.join("installed_plugins.json"),
        &format!(
            r#"{{"version":2,"plugins":{{"own@n":[{{"installPath":"{}"}}]}}}}"#,
            own.join("cache/n/own/1.0.0").display()
        ),
    );
    run(&host, &profile);
    let installed: serde_json::Value =
        serde_json::from_slice(&fs::read(host.join("plugins/installed_plugins.json")).unwrap())
            .unwrap();
    assert_eq!(installed["version"], 2);
    assert_eq!(
        installed["plugins"]["own@n"][0]["installPath"],
        host.join("plugins/cache/n/own/1.0.0").to_str().unwrap()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn symlinks_inside_extensions_are_kept_and_user_managed_links_stay() {
    let (root, host, profile) = fixture("links");
    let external = root.join("dotfiles/skills");
    fs::create_dir_all(&external).unwrap();
    std::os::unix::fs::symlink(&external, host.join("skills")).unwrap();
    write(&profile.join("commands/real.md"), "cmd");
    std::os::unix::fs::symlink("real.md", profile.join("commands/alias.md")).unwrap();
    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, profile.join("agents")).unwrap();
    let report = run(&host, &profile);
    assert_eq!(
        fs::read_link(host.join("commands/alias.md")).unwrap(),
        Path::new("real.md")
    );
    assert_eq!(
        fs::read_link(profile.join("skills")).unwrap(),
        host.join("skills")
    );
    assert_eq!(fs::read_link(host.join("skills")).unwrap(), external);
    assert_eq!(fs::read_link(profile.join("agents")).unwrap(), elsewhere);
    assert!(report.conflicts.contains(&"agents"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn locked_migration_leaves_the_profile_directory_untouched() {
    let (root, host, profile) = fixture("locked");
    write(&profile.join("plugins/own"), "keep");
    let lock = fs::File::create(host.join(".krill-history-migration.lock")).unwrap();
    lock.lock().unwrap();
    let mut report = Report::default();
    assert!(share(&host, &profile, &mut report).is_err());
    assert!(!profile.join("plugins").is_symlink());
    assert!(
        profile.join("settings.json").is_symlink(),
        "잠긴 폴더 하나가 나머지 공유를 막지 않는다"
    );
    drop(lock);
    run(&host, &profile);
    assert!(profile.join("plugins").is_symlink());
    fs::remove_dir_all(root).unwrap();
}
