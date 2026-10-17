//! AS-12: older writers must retain fields introduced by newer versions.
use krill_proto::orchestration::{COMMENTS_FILE, ORCH_FILE, OrchestrationStore, WorktreeStore};

#[test]
fn as12_orchestration_preserves_future_root_and_nested_fields() {
    let dir = std::env::temp_dir().join(format!("as12-orch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let raw = r#"{"future":{"mode":"v2"},"runs":[{"id":"r1","title":"old","status":"active","created_at_ms":1,"updated_at_ms":1,"future":{"flag":true}}]}"#;
    std::fs::write(dir.join(ORCH_FILE), raw).unwrap();
    let mut store = OrchestrationStore::load(&dir).unwrap();
    store.runs[0].title = "edited".into();
    store.save(&dir).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(ORCH_FILE)).unwrap()).unwrap();
    assert_eq!(value["future"]["mode"], "v2");
    assert_eq!(value["runs"][0]["future"]["flag"], true);
    assert_eq!(value["runs"][0]["title"], "edited");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn as12_worktree_preserves_future_fields_and_comment_source() {
    let dir = std::env::temp_dir().join(format!("as12-worktree-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let raw = "# future writer\nfuture = { flag = true } # keep this\n[comments]\n'/a' = 'old'\n'/b' = 'delete'\n";
    std::fs::write(dir.join(COMMENTS_FILE), raw).unwrap();
    let mut store = WorktreeStore::load(&dir).unwrap();
    store.comments.insert("/a".into(), "edited".into());
    store.comments.remove("/b");
    store.save(&dir).unwrap();
    let body = std::fs::read_to_string(dir.join(COMMENTS_FILE)).unwrap();
    assert!(body.contains("future = { flag = true } # keep this"));
    assert!(body.contains("# future writer"));
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert_eq!(value["comments"]["/a"].as_str(), Some("edited"));
    assert!(value["comments"].get("/b").is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn as12_agent_profile_edit_preserves_future_fields_and_source() {
    let raw = "# newer agents\nfuture = { flag = true } # root\n[agent.codex]\nname = 'Codex'\nlaunch = ['codex']\nfuture = { mode = 'v2' } # profile\n[agent.codex.env]\nOLD = 'remove'\n";
    let agents = krill_proto::agent::Agents::load(Some(raw)).unwrap();
    let mut profile = agents.profiles["codex"].clone();
    profile.launch.push("--new".into());
    profile.env.clear();
    let body = krill_proto::agent::set_user_profile(Some(raw), "codex", &profile).unwrap();
    assert!(body.contains("future = { mode = 'v2' } # profile"));
    assert!(body.contains("future = { flag = true } # root"));
    assert!(body.contains("# newer agents"));
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert!(value["agent"]["codex"].get("env").is_none());
}

#[test]
fn as12_hook_receipt_rejects_unknown_rollback_operations() {
    let raw = r#"{"agent":"claude","complete":true,"changes":[],"codex":null,"future_changes":[{"path":"secret"}]}"#;
    assert!(serde_json::from_str::<krill_proto::hook_install::Receipt>(raw).is_err());
}

#[test]
fn as12_named_entry_rename_keeps_its_future_fields() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Commands {
        commands: Vec<Command>,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Command {
        name: String,
        text: String,
    }
    let raw = "commands = [{name = 'old', text = 'echo', future = 'keep'}]\n";
    let mut commands: Commands = toml::from_str(raw).unwrap();
    commands.commands[0].name = "new".into();
    let body = krill_proto::config_document::edit_toml(raw, &commands).unwrap();
    assert!(body.contains("future = 'keep'"));
}

#[test]
fn as12_auto_switch_rule_edit_and_remove_keeps_future_policy() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Rules {
        rules: Vec<krill_proto::usage::AutoSwitchRule>,
    }
    let raw = "[[rules]]\nprovider = 'Claude'\nfrom_account_id = 'one'\nenabled = true\nthreshold_percent = 90\nfuture = 'keep-one'\n[[rules]]\nprovider = 'Codex'\nfrom_account_id = 'two'\nenabled = true\nthreshold_percent = 90\nfuture = 'delete-two'\n";
    let mut rules: Rules = toml::from_str(raw).unwrap();
    rules.rules.pop();
    rules.rules[0].threshold_percent = 80;
    let body = krill_proto::config_document::edit_toml(raw, &rules).unwrap();
    assert!(body.contains("future = 'keep-one'"));
    assert!(!body.contains("delete-two"));
}
