use super::*;
use std::path::PathBuf;

// 프로세스 조회는 별도로 검증하고 파일 이행 테스트는 임시 HOME의 유휴 상태를 주입한다.
fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owe699-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 사용자 `~/.claude` 원본 상태(경로→내용)를 통째로 찍는다. 링크·복사 뒤에도 같아야 한다.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p.clone());
                out.push((p, Vec::new()));
            } else {
                let bytes = std::fs::read(&p).unwrap();
                out.push((p, bytes));
            }
        }
    }
    out.sort();
    out
}

fn host_fixture(home: &Path) -> PathBuf {
    let host = home.join(".claude");
    std::fs::create_dir_all(host.join("projects").join("-tmp-work")).unwrap();
    std::fs::write(host.join("projects/-tmp-work/s1.jsonl"), "{}\n").unwrap();
    for name in HISTORY
        .iter()
        .chain(&["skills", "commands", "agents", "plugins", "rules"])
    {
        std::fs::create_dir_all(host.join(name)).unwrap();
    }
    std::fs::write(host.join("history.jsonl"), "prompt history\n").unwrap();
    std::fs::create_dir_all(host.join("skills").join("krill")).unwrap();
    std::fs::write(host.join("settings.json"), "{\"theme\":\"dark\"}").unwrap();
    std::fs::write(host.join("CLAUDE.md"), "# 전역\n").unwrap();
    std::fs::write(host.join(".credentials.json"), "secret").unwrap();
    std::fs::write(host.join(".claude.json"), "{}").unwrap();
    host
}

#[test]
fn shares_all_resume_directories_without_touching_host() {
    let home = root("resume");
    let host = host_fixture(&home);
    let names = [
        "projects",
        "session-env",
        "file-history",
        "shell-snapshots",
        "todos",
        "paste-cache",
        "tasks",
        "plans",
        "transcripts",
    ];
    for name in names {
        std::fs::create_dir_all(host.join(name)).unwrap();
        std::fs::write(host.join(name).join("fixture"), name).unwrap();
    }
    let before = snapshot(&host);
    let profile = home.join("profile");
    std::fs::create_dir(&profile).unwrap();
    provision_claude(&home, &profile).unwrap();
    for name in names {
        assert_eq!(
            std::fs::read_link(profile.join(name)).unwrap(),
            host.join(name)
        );
    }
    assert_eq!(snapshot(&host), before);
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn merges_mcp_only_preserving_profile_keys_and_source() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = root("mcp");
    host_fixture(&home);
    let source = serde_json::json!({
        "mcpServers": {"shared": {"command": "source"}, "added": {"command": "new"}},
        "oauthAccount": {"emailAddress": "source@example.test"}, "userID": "source-id",
        "machineID": "machine", "primaryApiKey": "api-key", "claudeAiOauth": {"accessToken": "secret"},
        "accessToken": "secret", "refreshToken": "secret", "mcpOAuth": {"secret": true},
        "cachedUsageUtilization": 1, "projects": {"/tmp": {"hasTrustDialogAccepted": true}},
        "unknownFutureToken": "secret"
    });
    let source_file = home.join(".claude.json");
    std::fs::write(&source_file, serde_json::to_vec(&source).unwrap()).unwrap();
    let before = snapshot(&home);
    let profile = home.join("profile");
    std::fs::create_dir(&profile).unwrap();
    let file = profile.join(".claude.json");
    let own = serde_json::json!({"oauthAccount": {"emailAddress": "own@example.test"},
        "userID": "own-id", "theme": "own", "mcpServers": {
            "shared": {"command": "own"}, "own": {"command": "mine"}}});
    std::fs::write(&file, serde_json::to_vec(&own).unwrap()).unwrap();
    let first = provision_claude(&home, &profile).unwrap();
    assert!(first.changed.contains(&".claude.json"));
    assert!(first.conflicts.contains(&".claude.json"));
    let actual: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut expected = own;
    expected["mcpServers"]["added"] = source["mcpServers"]["added"].clone();
    assert_eq!(actual, expected);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = std::fs::read(&file).unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    assert!(
        provision_claude(&home, &profile)
            .unwrap()
            .changed
            .is_empty()
    );
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
    std::fs::remove_dir_all(&profile).unwrap();
    assert_eq!(snapshot(&home), before);
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn seeds_mcp_without_host_config_directory() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = root("mcp-only");
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"local":{"command":"example"}},"oauthAccount":{"userID":"secret"}}"#,
    )
    .unwrap();
    let profile = home.join("profile");
    std::fs::create_dir(&profile).unwrap();
    provision_claude(&home, &profile).unwrap();
    let file = profile.join(".claude.json");
    let actual: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"mcpServers":{"local":{"command":"example"}}})
    );
    assert_eq!(
        std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(home.join(".claude/projects").is_dir());
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn refuses_damaged_or_linked_profile_mcp_state() {
    let home = root("mcp-damaged");
    host_fixture(&home);
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"local":{"command":"example"}}}"#,
    )
    .unwrap();
    let profile = home.join("profile");
    std::fs::create_dir(&profile).unwrap();
    let file = profile.join(".claude.json");
    for invalid in ["{broken", "[]", "null", r#"{"mcpServers":[]}"#] {
        std::fs::write(&file, invalid).unwrap();
        assert!(provision_claude(&home, &profile).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), invalid);
    }
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(home.join(".claude.json"), &file).unwrap();
    let before = std::fs::read(home.join(".claude.json")).unwrap();
    let report = provision_claude(&home, &profile).unwrap();
    assert!(report.conflicts.contains(&".claude.json"));
    assert_eq!(std::fs::read(home.join(".claude.json")).unwrap(), before);
    assert!(file.symlink_metadata().unwrap().file_type().is_symlink());
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn mcp_merge_respects_claudes_existing_lock() {
    let home = root("mcp-lock");
    host_fixture(&home);
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"local":{"command":"example"}}}"#,
    )
    .unwrap();
    let profile = home.join("profile");
    std::fs::create_dir(&profile).unwrap();
    let file = profile.join(".claude.json");
    std::fs::write(&file, r#"{"userID":"own"}"#).unwrap();
    let lock = profile.join(".claude.json.lock");
    std::fs::create_dir(&lock).unwrap();
    let report = provision_claude(&home, &profile).unwrap();
    assert!(report.conflicts.contains(&".claude.json"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        r#"{"userID":"own"}"#
    );
    assert!(lock.is_dir(), "기존 Claude 잠금을 해제하지 않는다");
    std::fs::remove_dir(&lock).unwrap();
    provision_claude(&home, &profile).unwrap();
    assert!(!lock.exists());
    let actual: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"userID":"own", "mcpServers":{"local":{"command":"example"}}})
    );
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn atomic_mcp_publication_preserves_newer_state_and_removes_staging_file() {
    let home = root("mcp-atomic");
    let file = home.join(".claude.json");
    std::fs::write(&file, b"newer state").unwrap();
    assert_eq!(
        publish_mcp(&file, b"replacement", Some(b"old state"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"newer state");
    assert_eq!(
        publish_mcp(&file, b"replacement", None).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"newer state");
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        1,
        "임시 파일은 남기지 않는다"
    );
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn legacy_disabled_flag_still_shares_claude_but_keeps_other_providers_off() {
    let home = root("legacy-disabled");
    let host = host_fixture(&home);
    let claude = home.join("claude");
    let codex = home.join("codex");
    std::fs::create_dir(&claude).unwrap();
    std::fs::create_dir(&codex).unwrap();
    let state = home.join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::write(
        state.join("accounts.toml"),
        format!(
            "provisioning = false\n\
             [[accounts]]\nid = \"a\"\nprovider = \"claude\"\nlabel = \"a\"\nprofile_dir = \"{}\"\n\
             [[accounts]]\nid = \"b\"\nprovider = \"codex\"\nlabel = \"b\"\nprofile_dir = \"{}\"\n",
            claude.display(),
            codex.display()
        ),
    )
    .unwrap();
    provision_if_enabled(
        &home,
        Some(&state),
        ProviderKind::Claude,
        claude.to_str().unwrap(),
    );
    assert_eq!(
        std::fs::read_link(claude.join("plugins")).unwrap(),
        host.join("plugins")
    );
    assert_eq!(
        std::fs::read_link(claude.join("settings.json")).unwrap(),
        host.join("settings.json")
    );
    provision_if_enabled(
        &home,
        Some(&state),
        ProviderKind::Codex,
        codex.to_str().unwrap(),
    );
    assert_eq!(std::fs::read_dir(&codex).unwrap().count(), 0);
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn orca_managed_profile_also_shares_host_claude() {
    let home = root("orca-managed");
    let host = host_fixture(&home);
    let profile = home.join("claude-profiles/dae9286b-d011-4443-9ddb-7ada705db2c1/home");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("settings.json"), "{\"orca\":1}").unwrap();
    let state = home.join("state");
    std::fs::create_dir(&state).unwrap();
    let dir = profile.to_str().unwrap();
    std::fs::write(
        state.join("accounts.toml"),
        format!("[[accounts]]\nid = \"a\"\nprovider = \"claude\"\nlabel = \"a\"\nprofile_dir = \"{dir}\"\n"),
    )
    .unwrap();
    provision_if_enabled(&home, Some(&state), ProviderKind::Claude, dir);
    assert_eq!(
        std::fs::read_link(profile.join("settings.json")).unwrap(),
        host.join("settings.json")
    );
    assert_eq!(
        std::fs::read_link(profile.join("skills")).unwrap(),
        host.join("skills")
    );
    assert_eq!(
        std::fs::read_link(profile.join("rules")).unwrap(),
        host.join("rules")
    );
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn lists_never_overlap() {
    for name in HISTORY {
        assert!(!EXCLUDED.contains(name), "{name}");
    }
}

#[test]
fn shares_history_even_when_host_has_not_created_it_yet() {
    let home = root("history-first-launch");
    let profile = home.join("profile");
    std::fs::create_dir(&profile).unwrap();
    provision_claude(&home, &profile).unwrap();
    assert_eq!(
        std::fs::read_link(profile.join("projects")).unwrap(),
        home.join(".claude/projects")
    );
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn merges_nonempty_projects_instead_of_leaving_them_isolated() {
    let home = root("history-existing");
    let profile = home.join("profile");
    std::fs::create_dir_all(profile.join("projects/repo")).unwrap();
    std::fs::write(profile.join("projects/repo/session.jsonl"), "conversation").unwrap();
    provision_claude(&home, &profile).unwrap();
    assert_eq!(
        std::fs::read_link(profile.join("projects")).unwrap(),
        home.join(".claude/projects")
    );
    assert_eq!(
        std::fs::read(home.join(".claude/projects/repo/session.jsonl")).unwrap(),
        b"conversation"
    );
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn provisions_idempotently_without_touching_host() {
    let home = root("idem");
    let host = host_fixture(&home);
    let before = snapshot(&host);
    let profile = home.join("profile");
    std::fs::create_dir_all(&profile).unwrap();

    let first = provision_claude(&home, &profile).unwrap();
    assert_eq!(
        first.changed,
        ["skills", "commands", "agents", "plugins", "rules"]
            .into_iter()
            .chain(["settings.json", "CLAUDE.md"])
            .chain(HISTORY.iter().copied())
            .chain(["history.jsonl"])
            .collect::<Vec<_>>()
    );
    assert!(first.conflicts.is_empty());
    assert_eq!(
        std::fs::read_link(profile.join("projects")).unwrap(),
        host.join("projects")
    );
    assert!(profile.join("projects/-tmp-work/s1.jsonl").is_file());
    assert_eq!(
        std::fs::read_to_string(profile.join("CLAUDE.md")).unwrap(),
        MEMORY_IMPORT
    );
    assert!(
        !profile.join(".credentials.json").exists(),
        "로그인은 공유하지 않는다"
    );
    assert!(!profile.join(".claude.json").exists());
    assert!(
        profile.join("todos").is_symlink(),
        "첫 실행부터 공용 경로를 쓴다"
    );

    let again = provision_claude(&home, &profile).unwrap();
    assert_eq!(
        again,
        Report::default(),
        "두 번째 실행은 아무것도 바꾸지 않는다"
    );
    assert_eq!(snapshot(&host), before, "사용자 ~/.claude 원본 불변");
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn migrates_existing_profile_to_reference_the_host_and_archives_its_copy() {
    let home = root("migrate");
    let host = host_fixture(&home);
    let before = snapshot(&host);
    let profile = home.join("profile");
    std::fs::create_dir_all(profile.join("projects")).unwrap();
    std::fs::create_dir_all(profile.join("skills/mine")).unwrap();
    std::fs::write(profile.join("settings.json"), "{\"mine\":1}").unwrap();
    std::fs::write(profile.join("CLAUDE.md"), "내 메모\n").unwrap();

    let report = provision_claude(&home, &profile).unwrap();
    assert!(report.changed.contains(&"projects"));
    assert!(report.changed.contains(&"skills"));
    assert!(report.conflicts.is_empty());
    assert!(profile.join("projects").is_symlink());
    assert_eq!(
        std::fs::read_link(profile.join("skills")).unwrap(),
        host.join("skills")
    );
    assert!(
        host.join("skills/mine").is_dir(),
        "프로필 전용 스킬도 기본 폴더로 옮긴다"
    );
    assert_eq!(
        std::fs::read_link(profile.join("settings.json")).unwrap(),
        host.join("settings.json")
    );
    assert_eq!(
        std::fs::read_to_string(profile.join("CLAUDE.md")).unwrap(),
        MEMORY_IMPORT
    );
    let archive = history_store::archive_root(&host, &profile, "projects-merged-backup");
    assert_eq!(
        std::fs::read_to_string(archive.join("settings.json/original")).unwrap(),
        "{\"mine\":1}"
    );
    assert_eq!(
        std::fs::read_to_string(archive.join("CLAUDE.md/original")).unwrap(),
        "내 메모\n"
    );
    for (path, bytes) in before {
        if path.is_file() {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn refuses_host_folder_and_its_children() {
    let home = root("refuse");
    let host = host_fixture(&home);
    // Orca 감지 계정 경로(`~/.claude/profiles/<slug>`)처럼 원본 안에 있는 프로필.
    let inner = host.join("profiles").join("x");
    std::fs::create_dir_all(&inner).unwrap();
    let before = snapshot(&host);
    assert!(provision_claude(&home, &host).is_err());
    assert!(provision_claude(&home, &inner).is_err());
    assert!(
        provision_claude(&home, &home).is_err(),
        "원본의 조상(홈 자체)도 거부한다"
    );
    assert_eq!(snapshot(&host), before);
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn registered_only_and_trashing_profile_keeps_host_history() {
    let home = root("gate");
    let state = home.join("state");
    let host = host_fixture(&home);
    let before = snapshot(&host);
    let profile = home.join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let dir = profile.to_str().unwrap();

    // 설정 없음(기본 켜짐)이어도 등록된 계정이 없으면 아무것도 하지 않는다.
    provision_if_enabled(&home, Some(&state), ProviderKind::Claude, dir);
    assert_eq!(std::fs::read_dir(&profile).unwrap().count(), 0);

    std::fs::write(state.join("accounts.toml"), "provisioning = true\n").unwrap();
    provision_if_enabled(&home, Some(&state), ProviderKind::Claude, dir);
    assert_eq!(
        std::fs::read_dir(&profile).unwrap().count(),
        0,
        "등록 안 된 폴더(예: 데몬 env의 실제 CLAUDE_CONFIG_DIR)는 건드리지 않는다"
    );
    std::fs::write(
        state.join("accounts.toml"),
        format!(
            "provisioning = true\n[[accounts]]\nid = \"a\"\nprovider = \"claude\"\nlabel = \"a\"\nprofile_dir = \"{dir}\"\n"
        ),
    )
    .unwrap();
    provision_if_enabled(&home, Some(&state), ProviderKind::Codex, dir);
    assert_eq!(std::fs::read_dir(&profile).unwrap().count(), 0, "Claude만");
    provision_if_enabled(&home, Some(&state), ProviderKind::Claude, dir);
    assert!(profile.join("projects").exists());

    // 계정 삭제는 프로필을 휴지통으로 옮긴다. 링크만 옮겨지고 원본 대화는 남아야 한다.
    std::fs::remove_dir_all(&profile).unwrap();
    assert_eq!(snapshot(&host), before);
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn real_home_is_never_provisioned_from_a_test_process() {
    let home = krill_proto::os::home_dir().unwrap();
    let state = root("real-home-guard");
    let profile = state.join("profile");
    std::fs::create_dir(&profile).unwrap();
    let dir = profile.to_str().unwrap();
    std::fs::write(
        state.join("accounts.toml"),
        format!("[[accounts]]\nid = \"a\"\nprovider = \"claude\"\nlabel = \"a\"\nprofile_dir = \"{dir}\"\n"),
    )
    .unwrap();
    provision_if_enabled(&home, Some(&state), ProviderKind::Claude, dir);
    assert_eq!(std::fs::read_dir(&profile).unwrap().count(), 0);
    std::fs::remove_dir_all(state).ok();
}
