use super::*;
fn stored(profile: &str, account: Option<LaunchAccount>) -> StoredLaunch {
    StoredLaunch {
        profile: profile.into(),
        permission: krill_proto::agent::Permission::Manual,
        session_id: None,
        transcript_path: None,
        cwd: Vec::new(),
        hibernation_blocked: true,
        account,
    }
}
#[test]
fn canonical_hook_keeps_custom_profile_account_and_cwd_relative_scope() {
    let agents=Agents::load(Some("[agent.work]\nname = 'Work'\nlaunch = ['codex']\n[agent.work.env]\nCODEX_HOME = 'profile'\n")).unwrap();
    let mut launch = stored("work", None);
    let ctx = resolve(
        Path::new("/host"),
        Path::new("/repo"),
        "codex",
        Some(&launch),
        &agents,
        |_| Some("/wrong".into()),
    );
    assert_eq!(ctx.agent, "codex");
    assert_eq!(ctx.config, Some(PathBuf::from("/repo/profile")));
    launch.account = Some(LaunchAccount {
        id: "account".into(),
        env_key: "CODEX_HOME".into(),
        dir: "/account".into(),
    });
    assert_eq!(
        resolve(
            Path::new("/host"),
            Path::new("/repo"),
            "codex",
            Some(&launch),
            &agents,
            |_| None
        )
        .config,
        Some(PathBuf::from("/account"))
    );
    launch.account.as_mut().unwrap().dir.clear();
    assert_eq!(
        resolve(
            Path::new("/host"),
            Path::new("/repo"),
            "codex",
            Some(&launch),
            &agents,
            |_| Some("/wrong".into())
        )
        .config,
        None
    );
}
#[test]
fn agy_config_env_and_changed_provider_do_not_reuse_wrong_account() {
    let agents = Agents::load(None).unwrap();
    let launch = stored(
        "agy",
        Some(LaunchAccount {
            id: "agy".into(),
            env_key: "ANTIGRAVITY_CLI_HOME".into(),
            dir: "/agy".into(),
        }),
    );
    assert_eq!(
        resolve(
            Path::new("/host"),
            Path::new("/repo"),
            "antigravity",
            Some(&launch),
            &agents,
            |_| None
        )
        .config,
        Some(PathBuf::from("/agy"))
    );
    let changed = resolve(
        Path::new("/host"),
        Path::new("/repo"),
        "claude",
        Some(&launch),
        &agents,
        |key| (key == "CLAUDE_CONFIG_DIR").then(|| "/claude".into()),
    );
    assert_eq!(changed.agent, "claude");
    assert_eq!(changed.config, Some(PathBuf::from("/claude")));
}

#[test]
fn observed_known_provider_wins_over_shell_wrapper_and_version_metadata_is_bound() {
    let agents = Agents::load(Some(
        "[agent.claude]\nname='Wrapped Claude'\nlaunch=['/bin/sh']\n",
    ))
    .unwrap();
    let ctx = resolve(
        Path::new("/home"),
        Path::new("/repo"),
        "claude",
        None,
        &agents,
        |_| None,
    );
    assert_eq!(ctx.agent, "claude");
    let path = std::env::temp_dir().join(format!(
        "krill-composer-version-{}.jsonl",
        std::process::id()
    ));
    fs::write(
        &path,
        r#"{"type":"session_meta","payload":{"id":"bound-session","cli_version":"0.164.0"}}
{"type":"message","content":"private body"}"#,
    )
    .unwrap();
    let mut launch = stored("codex", None);
    launch.transcript_path = Some(path.to_str().unwrap().into());
    launch.session_id = Some("bound-session".into());
    assert!(verified_codex_transcript(&launch));
    launch.session_id = Some("different-session".into());
    assert!(!verified_codex_transcript(&launch));
    fs::remove_file(path).unwrap();
}

#[test]
fn command_context_changes_for_real_account_profile_binding_without_exposing_paths() {
    let agents = Agents::load(None).unwrap();
    let mut launch = stored(
        "codex",
        Some(LaunchAccount {
            id: "same-account-id".into(),
            env_key: "CODEX_HOME".into(),
            dir: "/private/account-a".into(),
        }),
    );
    let before = command_context(Some(&launch), &agents, "codex");
    launch.account.as_mut().unwrap().dir = "/private/account-b".into();
    let after = command_context(Some(&launch), &agents, "codex");
    assert_ne!(before, after);
    assert_eq!(before.len(), 64);
    assert!(before.chars().all(|c| c.is_ascii_hexdigit()));
    launch.profile = "claude".into();
    assert_ne!(after, command_context(Some(&launch), &agents, "claude"));
    assert!(!before.contains("private"));
}
#[test]
fn model_discovery_uses_account_and_never_claims_config_is_current_or_applied() {
    let root = std::env::temp_dir().join(format!("krill-composer-models-{}", std::process::id()));
    fs::create_dir_all(root.join("account")).unwrap();
    fs::write(root.join("account/models_cache.json"),r#"{"models":[{"slug":"actual-model","display_name":"Actual"},{"slug":"hidden-model","visibility":"hide"},{"slug":"bad\n/model"}]}"#).unwrap();
    fs::write(
        root.join("account/config.toml"),
        "model = 'configured-model'\nsecret='private'\n",
    )
    .unwrap();
    let ctx = Context {
        home: root.clone(),
        cwd: root.clone(),
        agent: "codex".into(),
        config: Some(root.join("account")),
        env: BTreeMap::new(),
        verified_codex: false,
    };
    let value = ctx.models();
    assert_eq!(value["supported"], false);
    assert!(value["current"].is_null());
    assert_eq!(value["items"].as_array().unwrap().len(), 2);
    assert!(!value.to_string().contains("private"));
    assert_eq!(ctx.set_model("actual-model").unwrap()["applied"], false);
    for id in ["forged", "actual-model\r/model", "../config", ""] {
        assert!(ctx.set_model(id).is_err());
    }
    assert_eq!(
        fs::read_to_string(root.join("account/config.toml")).unwrap(),
        "model = 'configured-model'\nsecret='private'\n"
    );
    fs::remove_dir_all(root).unwrap();
}
