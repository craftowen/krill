// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Comprehensive unit tests for `krill bot` CLI parsing, offline catalog,
//! mock RPC execution, secret redaction, and zip export/import.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::SessionId;
use krill_proto::agent::Permission;
use krill_proto::bots::package::{BotPackageExportOptions, export_bot_zip, import_bot_zip};
use krill_proto::bots::{
    ActsFor, BotAccessRequest, BotAccessRequestStatus, BotAvatarData, BotConfig,
    BotConnectorStatus, BotConnectorTestResult, BotGroupChat, BotGroupMessage, BotGroupReplyMode,
    BotGroupThread, BotGroupView, BotId, BotItem, BotItemActor, BotItemDecision, BotItemEvent,
    BotItemEventKind, BotItemLink, BotItemStatus, BotItemsPage, BotMcpAuthKind, BotMcpOAuthState,
    BotMember, BotMemoryHistoryEntry, BotMemoryTidyStatus, BotMemoryTopicInfo, BotRpcRequest,
    BotRpcResponse, BotScheduleConfig, BotSessionSummary, BotSuggestion, BotToolRelayStatus,
    BotToolServerStatus, ComposioConnectionInfo, ConnectorHealthState, DefaultTrackerBots,
    ExternalBotCandidate, ExternalBotSourceApp, ExternalSource, FieldPatch,
};
use krill_proto::control::FEAT_CONTROL;
use krill_proto::ipc::FEAT_BOTS;

use super::exec::{
    build_secret_ref, execute_online, parse_snooze_until, today_iso_utc, try_execute_offline,
};
use super::parse::{
    AppsKind, BotCommand, ConnectorPatch, GoogleServiceKind, MailKind, SecretInputSource,
    TextInputSource, parse_command,
};
use super::{BotClient, CliError, USAGE, ensure_bots_feature, run_with_client};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_dir(label: &str) -> std::path::PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "krill-bot-cli-{label}-{}-{seq}",
        std::process::id()
    ));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn s(args: &[&str]) -> Vec<String> {
    args.iter().map(|v| (*v).to_owned()).collect()
}

fn sample_bot() -> BotConfig {
    let mut cfg = BotConfig::new(
        BotId::new("bot-pr-1").unwrap(),
        "PR Reviewer",
        "claude",
        1_700_000_000_000,
    )
    .unwrap();
    cfg.description = "Reviews PRs".to_owned();
    cfg.template_id = Some("pr-review".to_owned());
    cfg
}

fn sample_item() -> BotItem {
    BotItem {
        uid: "item-uid-1".to_owned(),
        number: 1,
        owner_bot_id: BotId::new("bot-pr-1").unwrap(),
        title: "Review #42".to_owned(),
        description: Some("Check edge cases".to_owned()),
        done_means: Some("Approved".to_owned()),
        status: BotItemStatus::NeedsYou,
        close_note: None,
        links: vec![BotItemLink::Pr("craftowen/krill#42".to_owned())],
        events: Vec::new(),
        snoozed_until_ms: None,
        version: 1,
        last_activity_at_ms: 1_700_000_000_000,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_000_000,
        closed_at_ms: None,
    }
}

struct MockClient {
    recorded: Vec<BotRpcRequest>,
    replies: VecDeque<Result<BotRpcResponse, CliError>>,
}

impl MockClient {
    fn new(replies: impl IntoIterator<Item = BotRpcResponse>) -> Self {
        Self {
            recorded: Vec::new(),
            replies: replies.into_iter().map(Ok).collect(),
        }
    }

    fn with_results(replies: impl IntoIterator<Item = Result<BotRpcResponse, CliError>>) -> Self {
        Self {
            recorded: Vec::new(),
            replies: replies.into_iter().collect(),
        }
    }
}

impl BotClient for MockClient {
    fn call(&mut self, request: BotRpcRequest) -> Result<BotRpcResponse, CliError> {
        self.recorded.push(request);
        self.replies
            .pop_front()
            .unwrap_or_else(|| Err(CliError::fail("unexpected extra mock call")))
    }
}

// ---------- Argument Parsing Across All Subcommands ----------

#[test]
fn parses_lifecycle_subcommands_and_flags() {
    assert_eq!(parse_command(&[], None).unwrap(), BotCommand::Help(None));
    assert_eq!(
        parse_command(&s(&["--help"]), None).unwrap(),
        BotCommand::Help(None)
    );
    assert_eq!(
        parse_command(&s(&["items", "--help"]), None).unwrap(),
        BotCommand::Help(Some("items".to_owned()))
    );
    assert_eq!(
        parse_command(&s(&["ls", "--json"]), None).unwrap(),
        BotCommand::List { json: true }
    );

    let cmd = parse_command(
        &s(&[
            "new",
            "PR Guard",
            "--template",
            "pr-review",
            "--agent",
            "codex",
            "--model",
            "gpt-5",
            "--folder",
            "/tmp/repo",
            "--slug",
            "pr-guard",
            "--permission",
            "yolo",
            "--json",
        ]),
        None,
    )
    .unwrap();
    let BotCommand::New(n) = cmd else {
        panic!("expected New");
    };
    assert_eq!(n.name.as_deref(), Some("PR Guard"));
    assert_eq!(n.template_id.as_deref(), Some("pr-review"));
    assert_eq!(n.agent_id, "codex");
    assert_eq!(n.model.as_deref(), Some("gpt-5"));
    assert_eq!(n.folder.as_deref(), Some("/tmp/repo"));
    assert_eq!(n.slug.as_deref(), Some("pr-guard"));
    assert_eq!(n.permission_mode, Some(Permission::Yolo));
    assert!(n.json);

    // Invalid Windows reserved slug rejected at parse time
    assert!(parse_command(&s(&["new", "Bad", "--slug", "con"]), None).is_err());

    let upd = parse_command(
        &s(&[
            "update",
            "pr-reviewer",
            "--name",
            "Reviewer 2",
            "--clear-model",
            "--pause",
            "--look-around",
            "off",
            "--memory-tidy-interval",
            "3600",
            "--notify-error",
            "false",
        ]),
        None,
    )
    .unwrap();
    let BotCommand::Update(u) = upd else {
        panic!("expected Update");
    };
    assert_eq!(u.bot, "pr-reviewer");
    assert_eq!(u.name.as_deref(), Some("Reviewer 2"));
    assert_eq!(u.model, FieldPatch::Clear);
    assert_eq!(u.paused, Some(true));
    assert_eq!(u.look_around_enabled, Some(false));
    assert_eq!(u.memory_tidy_interval_secs, Some(3600));
    assert_eq!(u.notify_error, Some(false));

    assert_eq!(
        parse_command(&s(&["rm", "pr-reviewer", "--delete-home"]), None).unwrap(),
        BotCommand::Remove {
            bot: "pr-reviewer".to_owned(),
            delete_home: true,
            json: false,
        }
    );
    assert_eq!(
        parse_command(&s(&["chat", "pr-reviewer", "--prompt", "hi"]), None).unwrap(),
        BotCommand::Start {
            bot: "pr-reviewer".to_owned(),
            prompt: Some("hi".to_owned()),
            cwd: None,
            json: false,
        }
    );
}

#[test]
fn parses_soul_memory_items_suggest_reply_send_and_groups() {
    assert_eq!(
        parse_command(&s(&["soul", "mybot", "set", "--text", "# Soul\n"]), None).unwrap(),
        BotCommand::SoulSet {
            bot: "mybot".to_owned(),
            source: TextInputSource::Inline("# Soul\n".to_owned()),
            json: false,
        }
    );
    assert_eq!(
        parse_command(
            &s(&["memory", "forget", "--fact", "stale note"]),
            Some("bot-env-1")
        )
        .unwrap(),
        BotCommand::MemoryForget {
            bot: "bot-env-1".to_owned(),
            fact: "stale note".to_owned(),
            json: false,
        }
    );
    assert_eq!(
        parse_command(&s(&["memory", "mybot", "tidy", "--json"]), None).unwrap(),
        BotCommand::MemoryTidy {
            bot: "mybot".to_owned(),
            json: true,
        }
    );

    // Items: both brief style and preamble style
    assert_eq!(
        parse_command(
            &s(&["items", "mybot", "ls", "--status", "needs-you", "--all"]),
            None
        )
        .unwrap(),
        BotCommand::ItemsList {
            bot: Some("mybot".to_owned()),
            status: Some(BotItemStatus::NeedsYou),
            all: true,
            include_snoozed: false,
            limit: None,
            cursor: None,
            page: None,
            json: false,
        }
    );
    let BotCommand::ItemsAdd(added) = parse_command(
        &s(&[
            "items",
            "mybot",
            "add",
            "Ship v1",
            "--body",
            "Checklist",
            "--done-means",
            "CI green",
            "--status",
            "working",
            "--link",
            "pr:org/repo#9",
        ]),
        None,
    )
    .unwrap() else {
        panic!("expected ItemsAdd");
    };
    assert_eq!(added.title, "Ship v1");
    assert_eq!(added.status, Some(BotItemStatus::Working));
    assert_eq!(added.links, vec![BotItemLink::Pr("org/repo#9".to_owned())]);

    assert_eq!(
        parse_command(
            &s(&["items", "mybot", "link", "42", "pr", "org/repo#99"]),
            None
        )
        .unwrap(),
        BotCommand::ItemsLink {
            bot: Some("mybot".to_owned()),
            item_id: "42".to_owned(),
            link: BotItemLink::Pr("org/repo#99".to_owned()),
            json: false,
        }
    );
    assert!(
        parse_command(
            &s(&["items", "mybot", "link", "42", "pr", "x", "--label", "y"]),
            None
        )
        .is_err()
    );
    assert_eq!(
        parse_command(
            &s(&["items", "link", "42", "url:https://example.com"]),
            Some("bot-1")
        )
        .unwrap(),
        BotCommand::ItemsLink {
            bot: Some("bot-1".to_owned()),
            item_id: "42".to_owned(),
            link: BotItemLink::Url("https://example.com".to_owned()),
            json: false,
        }
    );
    assert_eq!(
        parse_command(&s(&["items", "mybot", "done", "42"]), None).unwrap(),
        BotCommand::ItemsClose {
            bot: Some("mybot".to_owned()),
            item_id: "42".to_owned(),
            status: BotItemStatus::Done,
            note: None,
            json: false,
        }
    );

    assert_eq!(
        parse_command(&s(&["suggest", "mybot", "--run"]), None).unwrap(),
        BotCommand::SuggestRun {
            bot: "mybot".to_owned(),
            json: false,
        }
    );
    assert_eq!(
        parse_command(
            &s(&["reply", "mybot", "All green!", "--link", "ext-7"]),
            None
        )
        .unwrap(),
        BotCommand::Reply {
            bot: "mybot".to_owned(),
            text: "All green!".to_owned(),
            link_id: Some("ext-7".to_owned()),
            files: Vec::new(),
            reaction: None,
            json: false,
        }
    );
    assert_eq!(
        parse_command(
            &s(&[
                "send",
                "--to",
                "bot:release",
                "--type",
                "handoff",
                "Ready to tag"
            ]),
            Some("pr-review")
        )
        .unwrap(),
        BotCommand::SendMail {
            from: Some("bot:pr-review".to_owned()),
            to_bot: "release".to_owned(),
            kind: Some(MailKind::Handoff),
            subject: None,
            body: "Ready to tag".to_owned(),
            thread_id: None,
            json: false,
        }
    );
    assert_eq!(
        parse_command(
            &s(&[
                "groups",
                "create",
                "Release Squad",
                "--members",
                "pr,release"
            ]),
            None
        )
        .unwrap(),
        BotCommand::GroupsCreate {
            name: "Release Squad".to_owned(),
            members: vec!["pr".to_owned(), "release".to_owned()],
            description: String::new(),
            json: false,
        }
    );
}

#[test]
fn parses_phase2_and_phase3_connectors_access_secrets_tools_and_packaging() {
    let tg = parse_command(
        &s(&[
            "connectors",
            "mybot",
            "telegram",
            "--enable",
            "--allowed-chats",
            "-100123456,789",
            "--allowed-users",
            "42",
            "--acts-for",
            "member:42",
        ]),
        None,
    )
    .unwrap();
    let BotCommand::ConnectorConfigure(cfg) = tg else {
        panic!("expected ConnectorConfigure");
    };
    assert_eq!(
        cfg.patch,
        ConnectorPatch::Telegram {
            enabled: Some(true),
            secret_ref: None,
            allowed_chats: Some(vec![-100_123_456, 789]),
            allowed_users: Some(vec![42]),
            acts_for: Some(ActsFor::Member("42".to_owned())),
        }
    );

    let sl = parse_command(
        &s(&[
            "connectors",
            "mybot",
            "slack",
            "--enable",
            "--allowed-channels",
            "C1,C2",
            "--mirror",
            "--acts-for",
            "anyone",
        ]),
        None,
    )
    .unwrap();
    let BotCommand::ConnectorConfigure(sl_cfg) = sl else {
        panic!("expected slack");
    };
    assert!(matches!(
        sl_cfg.patch,
        ConnectorPatch::Slack {
            enabled: Some(true),
            mirror_desktop: Some(true),
            acts_for: Some(ActsFor::Anyone),
            ..
        }
    ));

    let dc = parse_command(
        &s(&[
            "connectors",
            "mybot",
            "discord",
            "--enable",
            "--allowed-guilds",
            "G1",
            "--allowed-channels",
            "CH1",
            "--allowed-users",
            "U1",
        ]),
        None,
    )
    .unwrap();
    assert!(matches!(
        dc,
        BotCommand::ConnectorConfigure(c) if matches!(c.patch, ConnectorPatch::Discord { enabled: Some(true), .. })
    ));

    let gg = parse_command(
        &s(&[
            "connectors",
            "mybot",
            "google-setup",
            "--enable",
            "--poll-secs",
            "120",
            "--services",
            "gmail,calendar,drive",
            "--start-oauth",
        ]),
        None,
    )
    .unwrap();
    let BotCommand::ConnectorConfigure(gg_cfg) = gg else {
        panic!("expected google");
    };
    assert_eq!(
        gg_cfg.patch,
        ConnectorPatch::Google {
            enabled: Some(true),
            poll_secs: Some(120),
            services: Some(vec![
                GoogleServiceKind::Gmail,
                GoogleServiceKind::Calendar,
                GoogleServiceKind::Drive,
            ]),
            client_secret_ref: None,
            token_secret_ref: None,
            start_oauth: true,
        }
    );

    let gh = parse_command(
        &s(&[
            "connectors",
            "mybot",
            "github",
            "--enable",
            "--repos",
            "craftowen/krill",
            "--use-gh-cli",
            "--poll-secs",
            "90",
        ]),
        None,
    )
    .unwrap();
    assert!(matches!(
        gh,
        BotCommand::ConnectorConfigure(c) if matches!(c.patch, ConnectorPatch::GitHub { enabled: Some(true), use_gh_cli: Some(true), poll_secs: Some(90), .. })
    ));

    let ln = parse_command(
        &s(&[
            "connectors",
            "mybot",
            "linear",
            "--enable",
            "--teams",
            "KRL,OPS",
            "--poll-secs",
            "60",
        ]),
        None,
    )
    .unwrap();
    assert!(matches!(
        ln,
        BotCommand::ConnectorConfigure(c) if matches!(c.patch, ConnectorPatch::Linear { enabled: Some(true), .. })
    ));

    let cp = parse_command(
        &s(&[
            "connectors",
            "mybot",
            "composio",
            "--enable",
            "--apps",
            "notion,jira",
        ]),
        None,
    )
    .unwrap();
    assert!(matches!(
        cp,
        BotCommand::ConnectorConfigure(c) if matches!(c.patch, ConnectorPatch::Composio { enabled: Some(true), .. })
    ));

    assert_eq!(
        parse_command(&s(&["access", "mybot", "approve", "tok-99"]), None).unwrap(),
        BotCommand::AccessDecide {
            token: "tok-99".to_owned(),
            approve: true,
            json: false,
        }
    );
    assert_eq!(
        parse_command(&s(&["access", "decline", "tok-88"]), None).unwrap(),
        BotCommand::AccessDecide {
            token: "tok-88".to_owned(),
            approve: false,
            json: false,
        }
    );

    assert_eq!(
        parse_command(
            &s(&["secret", "mybot", "set", "telegram", "--env", "TG_TOKEN"]),
            None
        )
        .unwrap(),
        BotCommand::SecretSet {
            bot: "mybot".to_owned(),
            key: "telegram".to_owned(),
            source: SecretInputSource::Env("TG_TOKEN".to_owned()),
            json: false,
        }
    );
    // Rejects raw secret values passed on argv
    assert!(
        parse_command(
            &s(&["secret", "mybot", "set", "telegram", "raw-secret"]),
            None
        )
        .is_err()
    );

    assert_eq!(
        parse_command(&s(&["tools", "mybot", "relay-status", "--json"]), None).unwrap(),
        BotCommand::ToolsRelayStatus { json: true }
    );
    assert_eq!(
        parse_command(
            &s(&["export", "mybot", "/tmp/mybot.zip", "--include-memory"]),
            None
        )
        .unwrap(),
        BotCommand::ExportZip {
            bot: "mybot".to_owned(),
            out_path: "/tmp/mybot.zip".to_owned(),
            include_memory: true,
            include_avatar: true,
            json: false,
        }
    );
    assert_eq!(
        parse_command(
            &s(&["import", "/tmp/mybot.zip", "--slug", "custom-slug"]),
            None
        )
        .unwrap(),
        BotCommand::ImportZip {
            in_path: "/tmp/mybot.zip".to_owned(),
            slug: Some("custom-slug".to_owned()),
            name: None,
            agent: None,
            json: false,
        }
    );
    assert_eq!(
        parse_command(
            &s(&[
                "import-external",
                "--source",
                "openclaw",
                "--apply",
                "--json"
            ]),
            None
        )
        .unwrap(),
        BotCommand::ImportExternal {
            source: Some(ExternalBotSourceApp::OpenClaw),
            path: None,
            apply: true,
            json: true,
        }
    );
}

// ---------- Offline Catalog & Slack Manifest ----------

#[test]
fn offline_catalog_templates_skills_apps_and_slack_manifest() {
    // 24 templates total, 6 coding
    let all_tpl = try_execute_offline(&parse_command(&s(&["templates", "--json"]), None).unwrap())
        .unwrap()
        .unwrap();
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&all_tpl).unwrap();
    assert_eq!(parsed.len(), 24);

    let coding = try_execute_offline(
        &parse_command(&s(&["templates", "--category", "coding", "--json"]), None).unwrap(),
    )
    .unwrap()
    .unwrap();
    let coding_rows: Vec<serde_json::Value> = serde_json::from_str(&coding).unwrap();
    assert_eq!(coding_rows.len(), 6);

    // 13 bundled skills
    let skills = try_execute_offline(&parse_command(&s(&["skills", "--json"]), None).unwrap())
        .unwrap()
        .unwrap();
    let skill_rows: Vec<serde_json::Value> = serde_json::from_str(&skills).unwrap();
    assert_eq!(skill_rows.len(), 13);

    // 13 MCP apps + 24 Composio apps
    let apps = try_execute_offline(
        &parse_command(&s(&["apps", "--kind", "all", "--json"]), None).unwrap(),
    )
    .unwrap()
    .unwrap();
    let apps_val: serde_json::Value = serde_json::from_str(&apps).unwrap();
    assert_eq!(apps_val["mcp"].as_array().unwrap().len(), 13);
    assert_eq!(apps_val["composio"].as_array().unwrap().len(), 24);

    let mcp_only = parse_command(&s(&["apps", "--kind", "mcp"]), None).unwrap();
    assert!(matches!(
        mcp_only,
        BotCommand::Apps {
            kind: AppsKind::Mcp,
            ..
        }
    ));
    let mcp_human = try_execute_offline(&mcp_only).unwrap().unwrap();
    assert!(mcp_human.contains("MCP Apps (13):"));
    assert!(!mcp_human.contains("Composio Curated Apps"));

    // Slack Socket Mode manifest YAML offline via both top-level and connectors subcommand
    let yaml_a = try_execute_offline(
        &parse_command(&s(&["connectors", "slack-manifest", "Release Bot"]), None).unwrap(),
    )
    .unwrap()
    .unwrap();
    let yaml_b =
        try_execute_offline(&parse_command(&s(&["slack-manifest", "Release Bot"]), None).unwrap())
            .unwrap()
            .unwrap();
    assert_eq!(yaml_a, yaml_b);
    assert!(yaml_a.contains("socket_mode_enabled: true"));
    assert!(yaml_a.contains("display_name: \"Release Bot\""));
}

// ---------- Snooze & Date Helpers ----------

#[test]
fn snooze_and_date_parsers_handle_relative_iso_epoch_and_clear() {
    let now_ms = 1_700_000_000_000u64;
    assert_eq!(
        parse_snooze_until("30m", now_ms).unwrap(),
        FieldPatch::Set(now_ms + 30 * 60 * 1000)
    );
    assert_eq!(
        parse_snooze_until("2h", now_ms).unwrap(),
        FieldPatch::Set(now_ms + 2 * 3600 * 1000)
    );
    assert_eq!(
        parse_snooze_until("1d", now_ms).unwrap(),
        FieldPatch::Set(now_ms + 86_400 * 1000)
    );
    assert_eq!(
        parse_snooze_until("clear", now_ms).unwrap(),
        FieldPatch::Clear
    );
    assert_eq!(
        parse_snooze_until("1970-01-02", now_ms).unwrap(),
        FieldPatch::Set(86_400_000)
    );
    assert_eq!(
        parse_snooze_until("1970-01-02T01:02:03Z", now_ms).unwrap(),
        FieldPatch::Set((86_400 + 3600 + 120 + 3) * 1000)
    );
    assert!(parse_snooze_until("bogus", now_ms).is_err());

    assert_eq!(today_iso_utc(0), "1970-01-01");
    assert_eq!(today_iso_utc(1_700_000_000), "2023-11-14");
}

// ---------- Mock RPC End-to-End Execution & Secret Redaction ----------

#[test]
fn secret_set_never_echoes_secret_and_supports_stdin_and_env() {
    let bot = sample_bot();
    let secret_value = "xoxb-SUPER-SECRET-TOKEN-99999";

    // Via --stdin
    let mut mock = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Ok,
    ]);
    let cmd = parse_command(
        &s(&[
            "secret",
            "bot-pr-1",
            "set",
            "slack-bot",
            "--stdin",
            "--json",
        ]),
        None,
    )
    .unwrap();
    let mut stdin = Cursor::new(format!("{secret_value}\n"));
    let out = execute_online(cmd, &mut mock, &mut stdin, 0, |_| None).unwrap();
    assert!(
        !out.contains(secret_value),
        "secret leaked into output: {out}"
    );
    assert!(out.contains("bot-pr-1:slack-bot"));
    assert_eq!(
        mock.recorded.get(1),
        Some(&BotRpcRequest::SetSecret {
            secret_ref: build_secret_ref(&bot.id, "slack-bot"),
            value: secret_value.to_owned(),
        })
    );

    // Via --env
    let mut mock2 = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Ok,
    ]);
    let cmd2 = parse_command(
        &s(&[
            "secret",
            "bot-pr-1",
            "set",
            "telegram",
            "--env",
            "TG_BOT_TOKEN",
        ]),
        None,
    )
    .unwrap();
    let mut empty = Cursor::new(Vec::<u8>::new());
    let out2 = execute_online(cmd2, &mut mock2, &mut empty, 0, |k| {
        (k == "TG_BOT_TOKEN").then(|| secret_value.to_owned())
    })
    .unwrap();
    assert!(!out2.contains(secret_value));
    assert!(out2.contains("stored secret bot-pr-1:telegram"));
}

#[test]
fn mock_rpc_covers_memory_items_connectors_tools_and_zip_roundtrip() {
    let bot = sample_bot();
    let item = sample_item();
    let mut empty = Cursor::new(Vec::<u8>::new());

    // 1. Memory add resolves bot by slug via ListBots fallback, appends dated fact
    let mut mock_mem = MockClient::with_results([
        Err(CliError::fail("not found")),
        Ok(BotRpcResponse::Bots(vec![bot.clone()])),
        Ok(BotRpcResponse::Text("# Memory\n".to_owned())),
        Ok(BotRpcResponse::MemoryUpdated {
            content: "# Memory\n\n- 2023-11-14 Run clippy\n".to_owned(),
            restored_facts: Vec::new(),
            removed_count: 0,
        }),
    ]);
    let out = execute_online(
        parse_command(&s(&["memory", "pr-reviewer", "add", "Run clippy"]), None).unwrap(),
        &mut mock_mem,
        &mut empty,
        1_700_000_000_000,
        |_| None,
    )
    .unwrap();
    assert!(out.contains("updated MEMORY.md for bot-pr-1"));
    assert!(matches!(
        mock_mem.recorded.last(),
        Some(BotRpcRequest::UpdateMemory { content, allow_deletions: false, .. })
            if content.contains("- 2023-11-14 Run clippy")
    ));

    // 2. Google setup with --start-oauth updates connector config then initiates OAuth
    let mut mock_google = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::GoogleOAuthStarted {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth?state=abc".to_owned(),
            state: "abc".to_owned(),
        },
    ]);
    let out_g = execute_online(
        parse_command(
            &s(&[
                "connectors",
                "bot-pr-1",
                "google-setup",
                "--enable",
                "--services",
                "gmail,calendar",
                "--start-oauth",
                "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_google,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    let g_val: serde_json::Value = serde_json::from_str(&out_g).unwrap();
    assert_eq!(g_val["state"], "abc");
    assert!(matches!(
        mock_google.recorded.get(1),
        Some(BotRpcRequest::SetConnectorConfig { connectors, .. })
            if connectors.google.enabled && connectors.google.services.gmail && connectors.google.services.calendar
    ));
    assert!(matches!(
        mock_google.recorded.get(2),
        Some(BotRpcRequest::StartGoogleOAuth { .. })
    ));

    // 3. Tools add curated MCP app (`sentry`)
    let mut updated_with_tool = bot.clone();
    updated_with_tool
        .mcp_tools
        .push(krill_proto::bots::BotMcpToolConfig {
            id: "sentry".to_owned(),
            name: "Sentry".to_owned(),
            endpoint_url: Some("https://mcp.sentry.dev/mcp".to_owned()),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::BrowserOAuth,
            header_names: Vec::new(),
            secret_ref: Some("bot-pr-1:mcp:sentry".to_owned()),
            enabled: true,
        });
    let mut mock_tools = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(updated_with_tool)),
    ]);
    let out_t = execute_online(
        parse_command(&s(&["tools", "bot-pr-1", "add", "sentry"]), None).unwrap(),
        &mut mock_tools,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(out_t.contains("sentry"));
    assert!(out_t.contains("https://mcp.sentry.dev/mcp"));

    // 4. Export zip to disk, then Import zip with `--slug` rewrite
    let dir = temp_dir("zip");
    let zip_path = dir.join("pr-reviewer.zip");
    let raw_zip = export_bot_zip(
        &bot,
        "# PR Reviewer\n\nChecks diffs.\n",
        &BTreeMap::new(),
        None,
        &BTreeMap::new(),
        None,
        BotPackageExportOptions {
            include_memory: false,
            include_avatar: false,
        },
    )
    .unwrap();
    let mut mock_exp = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::ExportedZip {
            filename: "pr-reviewer.zip".to_owned(),
            zip_bytes: raw_zip,
        },
    ]);
    execute_online(
        parse_command(
            &s(&["export", "bot-pr-1", zip_path.to_str().unwrap()]),
            None,
        )
        .unwrap(),
        &mut mock_exp,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(zip_path.is_file());

    let mut mock_imp = MockClient::new([BotRpcResponse::Bot(Box::new(bot.clone()))]);
    execute_online(
        parse_command(
            &s(&[
                "import",
                zip_path.to_str().unwrap(),
                "--slug",
                "imported-pr",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_imp,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    let Some(BotRpcRequest::ImportBotZipV2 {
        zip_bytes,
        name_override,
        slug_override,
        ..
    }) = mock_imp.recorded.first()
    else {
        panic!("expected ImportBotZipV2");
    };
    assert_eq!(name_override.as_deref(), None);
    assert_eq!(
        slug_override
            .as_ref()
            .map(krill_proto::bots::BotSlug::as_str),
        Some("imported-pr")
    );
    let parsed_pkg = import_bot_zip(zip_bytes).unwrap();
    assert_eq!(parsed_pkg.manifest.slug.as_str(), "pr-reviewer");

    // 5. Additional RPC formatting smoke checks (Start, Items, Groups, Relay, Access, LookAround, External)
    let mut mock_misc = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::SessionStarted {
            bot_id: bot.id.clone(),
            session_id: SessionId(7),
            pid: 4242,
        },
        BotRpcResponse::Items(vec![item]),
        BotRpcResponse::Groups(vec![BotGroupChat {
            id: "grp-1".to_owned(),
            name: "Ops".to_owned(),
            bot_ids: vec![bot.id.clone()],
            description: String::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }]),
        BotRpcResponse::ToolRelayStatus(BotToolRelayStatus {
            running: true,
            listen_addr: Some("127.0.0.1:41234".to_owned()),
            endpoint_url: Some("http://127.0.0.1:41234/bot-tools".to_owned()),
            connected_tools: vec![BotToolServerStatus {
                id: "sentry".to_owned(),
                name: "Sentry".to_owned(),
                endpoint_url: "https://mcp.sentry.dev/mcp".to_owned(),
                connected: true,
                tool_count: 5,
                last_error: None,
            }],
            last_error: None,
        }),
        BotRpcResponse::ConnectorStatuses(vec![BotConnectorStatus {
            bot_id: bot.id.clone(),
            source: ExternalSource::Telegram,
            enabled: true,
            connected: true,
            state: ConnectorHealthState::Connected,
            account_label: Some("@krillbot".to_owned()),
            last_sync_ms: None,
            last_error: None,
            error_category: None,
        }]),
        BotRpcResponse::AccessRequests(vec![BotAccessRequest {
            token: "tok-1".to_owned(),
            bot_id: bot.id.clone(),
            source: ExternalSource::Slack,
            sender_id: "U123".to_owned(),
            sender_name: "Alice".to_owned(),
            channel_id: "C123".to_owned(),
            status: BotAccessRequestStatus::Pending,
            created_at: 1,
            decided_at: None,
        }]),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::LookAroundResult {
            bot_id: bot.id.clone(),
            suggestions: vec![BotSuggestion {
                id: "sug-1".to_owned(),
                bot_id: bot.id.clone(),
                title: "Check stale PR".to_owned(),
                why: "Open 5 days".to_owned(),
                ask: "Ping reviewer?".to_owned(),
                link: None,
                source: Some(ExternalSource::GitHub),
                dismissed: false,
                created_at_ms: 1,
            }],
        },
        BotRpcResponse::ImportedExternalBots {
            candidates: vec![ExternalBotCandidate {
                app: ExternalBotSourceApp::Orca,
                agent: "claude".to_owned(),
                name: "Support".to_owned(),
                path: "~/.orca/bots/support".to_owned(),
                soul_preview: Some("Triages questions".to_owned()),
            }],
            imported: Vec::new(),
        },
    ]);

    assert!(
        execute_online(
            parse_command(&s(&["start", "bot-pr-1"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("session s7")
    );
    assert!(
        execute_online(
            parse_command(&s(&["items", "ls"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("Review #42")
    );
    assert!(
        execute_online(
            parse_command(&s(&["groups", "ls"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("Ops")
    );
    assert!(
        execute_online(
            parse_command(&s(&["tools", "relay-status"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("127.0.0.1:41234")
    );
    assert!(
        execute_online(
            parse_command(&s(&["connectors", "status"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("@krillbot")
    );
    assert!(
        execute_online(
            parse_command(&s(&["access", "ls"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("Alice")
    );
    assert!(
        execute_online(
            parse_command(&s(&["suggest", "bot-pr-1", "--run"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("Check stale PR")
    );
    assert!(
        execute_online(
            parse_command(&s(&["import-external"]), None).unwrap(),
            &mut mock_misc,
            &mut empty,
            0,
            |_| None,
        )
        .unwrap()
        .contains("DRY-RUN")
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn parses_and_executes_new_v2_and_import_v2_preserving_slug_and_name() {
    let bot = sample_bot();
    let mut empty = Cursor::new(Vec::<u8>::new());

    let mut mock_new = MockClient::new([BotRpcResponse::Bot(Box::new(bot.clone()))]);
    execute_online(
        parse_command(
            &s(&[
                "new",
                "Helper",
                "--slug",
                "helper-custom",
                "--avatar-color",
                "#3b82f6",
                "--skills",
                "code-review,triage",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_new,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    let Some(BotRpcRequest::CreateBotV2(params)) = mock_new.recorded.first() else {
        panic!("expected CreateBotV2");
    };
    assert_eq!(
        params.slug.as_ref().map(krill_proto::bots::BotSlug::as_str),
        Some("helper-custom")
    );
    assert_eq!(params.avatar_color.as_deref(), Some("#3b82f6"));
    assert_eq!(params.skills, vec!["code-review", "triage"]);

    let dir = temp_dir("import-v2");
    let zip_path = dir.join("sample.zip");
    let raw_zip = export_bot_zip(
        &bot,
        "# PR Reviewer\n",
        &BTreeMap::new(),
        None,
        &BTreeMap::new(),
        None,
        BotPackageExportOptions {
            include_memory: false,
            include_avatar: false,
        },
    )
    .unwrap();
    fs::write(&zip_path, raw_zip).unwrap();

    let mut mock_imp = MockClient::new([BotRpcResponse::Bot(Box::new(bot))]);
    execute_online(
        parse_command(
            &s(&[
                "import",
                zip_path.to_str().unwrap(),
                "--slug",
                "custom-slug",
                "--name",
                "Custom Display Name",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_imp,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    let Some(BotRpcRequest::ImportBotZipV2 {
        name_override,
        slug_override,
        ..
    }) = mock_imp.recorded.first()
    else {
        panic!("expected ImportBotZipV2");
    };
    assert_eq!(name_override.as_deref(), Some("Custom Display Name"));
    assert_eq!(
        slug_override
            .as_ref()
            .map(krill_proto::bots::BotSlug::as_str),
        Some("custom-slug")
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn split_bot_and_action_skipping_flags_works_across_all_subcommands() {
    // Leading --bot / --bot= / --json before action across all 7 subcommands + new v2 commands
    assert!(matches!(
        parse_command(&s(&["items", "--json", "--bot", "b1", "ls"]), None).unwrap(),
        BotCommand::ItemsList {
            bot: Some(ref b),
            json: true,
            ..
        } if b == "b1"
    ));
    assert!(matches!(
        parse_command(&s(&["soul", "--bot=b1", "set", "--text", "hi"]), None).unwrap(),
        BotCommand::SoulSet { ref bot, .. } if bot == "b1"
    ));
    assert!(matches!(
        parse_command(&s(&["memory", "--json", "--bot", "b1", "history"]), None).unwrap(),
        BotCommand::MemoryHistory {
            ref bot,
            json: true,
            ..
        } if bot == "b1"
    ));
    assert!(matches!(
        parse_command(&s(&["connectors", "--bot=b1", "test", "github"]), None).unwrap(),
        BotCommand::ConnectorTest {
            ref bot,
            source: ExternalSource::GitHub,
            ..
        } if bot == "b1"
    ));
    assert!(matches!(
        parse_command(&s(&["access", "--bot", "b1", "ls"]), None).unwrap(),
        BotCommand::AccessList {
            bot: Some(ref b),
            ..
        } if b == "b1"
    ));
    assert!(matches!(
        parse_command(&s(&["secret", "--bot=b1", "rm", "github"]), None).unwrap(),
        BotCommand::SecretRemove {
            ref bot,
            ref key,
            ..
        } if bot == "b1" && key == "github"
    ));
    assert!(matches!(
        parse_command(&s(&["tools", "--bot", "add", "add", "sentry"]), None).unwrap(),
        BotCommand::ToolsAdd(ref a) if a.bot == "add" && a.app_id == "sentry"
    ));

    // Disambiguating bot named like action keyword vs positional value matching keyword
    assert!(matches!(
        parse_command(&s(&["memory", "ls", "add", "Fact one"]), None).unwrap(),
        BotCommand::MemoryAdd { ref bot, ref fact, .. } if bot == "ls" && fact == "Fact one"
    ));
    assert!(matches!(
        parse_command(&s(&["items", "add", "done"]), Some("b1")).unwrap(),
        BotCommand::ItemsAdd(ref a) if a.bot == "b1" && a.title == "done"
    ));
    assert!(matches!(
        parse_command(&s(&["connectors", "status", "github"]), None).unwrap(),
        BotCommand::ConnectorConfigure(ref c)
            if c.bot == "status" && matches!(c.patch, ConnectorPatch::GitHub { .. })
    ));

    // Reject duplicate --text/--file/--stdin in memory set
    assert!(parse_command(&s(&["memory", "b1", "set", "--text", "a", "--stdin"]), None).is_err());
    assert!(
        parse_command(
            &s(&["memory", "b1", "set", "--file", "a.md", "--text", "b"]),
            None
        )
        .is_err()
    );
}

#[test]
fn parses_and_executes_v2_items_memory_schedules_tools_groups_sessions_avatar_composio() {
    let mut bot = sample_bot();
    bot.emoji = Some("🤖".to_owned());
    bot.avatar = Some("mascot:O1".to_owned());
    bot.avatar_color = Some("#3b82f6".to_owned());
    bot.first_run_completed = true;
    bot.skills = vec!["code-review".to_owned()];
    bot.members = vec![BotMember {
        id: "U1".to_owned(),
        name: "Alice".to_owned(),
        source: ExternalSource::Slack,
        workspace_id: Some("T1".to_owned()),
    }];
    bot.schedules = vec![BotScheduleConfig {
        id: "sched-daily".to_owned(),
        name: "Daily".to_owned(),
        cron: "0 9 * * 1-5".to_owned(),
        prompt: "Check".to_owned(),
        enabled: true,
        timezone: Some("UTC".to_owned()),
    }];

    // format_bot_detail prints all v2 metadata
    let bot_detail = super::format::format_bot_detail(&bot, false).unwrap();
    for expected in [
        "emoji",
        "avatar_color #3b82f6",
        "first_run_completed true",
        "skills      code-review",
        "members     Alice (slack:U1)",
        "notifications permission=true complete=true error=true",
        "schedules   sched-daily: Daily (0 9 * * 1-5) [enabled]",
    ] {
        assert!(
            bot_detail.contains(expected),
            "missing {expected} in:\n{bot_detail}"
        );
    }

    // Items v2: show, answer, paginated ls, update decisions, --bot qualification
    let mut item_with_events = sample_item();
    item_with_events.events.push(BotItemEvent {
        id: 1,
        item_uid: item_with_events.uid.clone(),
        kind: BotItemEventKind::Commented,
        actor: BotItemActor::Bot {
            bot_id: bot.id.clone(),
            session_id: None,
        },
        text: "Ready to ship?".to_owned(),
        decisions: vec![BotItemDecision {
            question: "Deploy?".to_owned(),
            options: vec!["Ship".to_owned(), "Hold".to_owned()],
            recommended: Some(0),
            multi: false,
        }],
        created_at_ms: 10,
    });

    // Reject out-of-range --recommended
    assert!(
        parse_command(
            &s(&[
                "items",
                "update",
                "42",
                "--ask",
                "Deploy?",
                "--option",
                "Ship",
                "--recommended",
                "3",
            ]),
            None,
        )
        .is_err()
    );

    let mut empty = Cursor::new(Vec::<u8>::new());
    let mut mock_items = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Item(Box::new(item_with_events.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Item(Box::new(item_with_events.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Item(Box::new(item_with_events.clone())),
        BotRpcResponse::ItemsPage(BotItemsPage {
            items: vec![item_with_events.clone()],
            next_cursor: Some("cur-2".to_owned()),
            total_count: 10,
            unseen_count: 3,
        }),
        BotRpcResponse::ItemsPage(BotItemsPage {
            items: vec![item_with_events],
            next_cursor: None,
            total_count: 10,
            unseen_count: 3,
        }),
    ]);

    let show_out = execute_online(
        parse_command(&s(&["items", "bot-pr-1", "show", "42"]), None).unwrap(),
        &mut mock_items,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(show_out.contains("Ready to ship?"));
    assert!(show_out.contains("question: Deploy?"));
    assert!(show_out.contains("[0] Ship (recommended)"));
    assert_eq!(
        mock_items.recorded.get(1),
        Some(&BotRpcRequest::GetBotItem {
            item_id: "bot-pr-1#42".to_owned()
        })
    );

    execute_online(
        parse_command(
            &s(&[
                "items",
                "bot-pr-1",
                "update",
                "42",
                "--ask",
                "Deploy?",
                "--option",
                "Ship",
                "--option",
                "Hold",
                "--recommended",
                "0",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_items,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(matches!(
        mock_items.recorded.get(3),
        Some(BotRpcRequest::UpdateItem(p))
            if p.item_id == "bot-pr-1#42" && p.decisions.len() == 1 && p.decisions[0].recommended == Some(0)
    ));

    execute_online(
        parse_command(
            &s(&[
                "items", "bot-pr-1", "answer", "42", "--option", "0", "--text", "Go ahead",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_items,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(matches!(
        mock_items.recorded.get(5),
        Some(BotRpcRequest::AnswerBotItem {
            item_id,
            option_index: Some(0),
            answer_text: Some(t),
            ..
        }) if item_id == "bot-pr-1#42" && t == "Go ahead"
    ));

    let page_out = execute_online(
        parse_command(
            &s(&[
                "items",
                "ls",
                "--page",
                "2",
                "--limit",
                "5",
                "--include-snoozed",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_items,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(page_out.contains("total=10 unseen=3"));
    assert!(matches!(
        mock_items.recorded.last(),
        Some(BotRpcRequest::ListItemsPage {
            include_snoozed: true,
            limit: Some(5),
            cursor: Some(c),
            ..
        }) if c == "cur-2"
    ));

    // Update --first-run-completed + --avatar-color -> UpdateBot + UpdateBotExtras
    let mut mock_upd = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
    ]);
    execute_online(
        parse_command(
            &s(&[
                "update",
                "bot-pr-1",
                "--first-run-completed",
                "on",
                "--avatar-color",
                "#10b981",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_upd,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(matches!(
        mock_upd.recorded.get(1),
        Some(BotRpcRequest::UpdateBot(p)) if p.first_run_completed == Some(true)
    ));
    assert!(matches!(
        mock_upd.recorded.get(2),
        Some(BotRpcRequest::UpdateBotExtras(p)) if p.avatar_color == FieldPatch::Set("#10b981".to_owned())
    ));

    // Schedules: invalid cron rejected; add -> enable -> disable -> rm
    assert!(
        parse_command(
            &s(&[
                "schedules",
                "bot-pr-1",
                "add",
                "--name",
                "Bad",
                "--cron",
                "not a cron",
                "--prompt",
                "hi",
            ]),
            None,
        )
        .is_err()
    );
    let mut mock_sched = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
    ]);
    execute_online(
        parse_command(
            &s(&[
                "schedules",
                "bot-pr-1",
                "add",
                "--id",
                "hourly",
                "--name",
                "Hourly",
                "--cron",
                "0 * * * *",
                "--prompt",
                "Scan",
                "--disabled",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_sched,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(matches!(
        mock_sched.recorded.get(1),
        Some(BotRpcRequest::UpdateBot(p))
            if p.schedules.as_ref().is_some_and(|s| s.len() == 2 && !s[1].enabled)
    ));
    execute_online(
        parse_command(
            &s(&["schedules", "bot-pr-1", "disable", "sched-daily"]),
            None,
        )
        .unwrap(),
        &mut mock_sched,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(matches!(
        mock_sched.recorded.get(3),
        Some(BotRpcRequest::UpdateBot(p))
            if p.schedules.as_ref().is_some_and(|s| !s[0].enabled)
    ));

    // Tools enable/disable/oauth + ENABLED column in tools ls
    let mut bot_with_tool = bot.clone();
    bot_with_tool
        .mcp_tools
        .push(krill_proto::bots::BotMcpToolConfig {
            id: "sentry".to_owned(),
            name: "Sentry".to_owned(),
            endpoint_url: Some("https://mcp.sentry.dev/mcp".to_owned()),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::BrowserOAuth,
            header_names: Vec::new(),
            secret_ref: None,
            enabled: true,
        });
    let mut mock_tools = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot_with_tool.clone())),
        BotRpcResponse::Bot(Box::new(bot_with_tool.clone())),
        BotRpcResponse::Bot(Box::new(bot_with_tool.clone())),
        BotRpcResponse::Bot(Box::new(bot_with_tool.clone())),
        BotRpcResponse::McpOAuth(BotMcpOAuthState {
            bot_id: bot.id.clone(),
            tool_id: "sentry".to_owned(),
            status: "pending".to_owned(),
            auth_url: Some("https://auth.example.com".to_owned()),
            error: None,
            updated_at_ms: 1,
        }),
    ]);
    let tools_out = execute_online(
        parse_command(&s(&["tools", "bot-pr-1", "ls"]), None).unwrap(),
        &mut mock_tools,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(tools_out.contains("ENABLED"));
    execute_online(
        parse_command(&s(&["tools", "bot-pr-1", "disable", "sentry"]), None).unwrap(),
        &mut mock_tools,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(matches!(
        mock_tools.recorded.get(2),
        Some(BotRpcRequest::UpdateBot(p))
            if p.mcp_tools.as_ref().is_some_and(|t| !t[0].enabled)
    ));
    let oauth_out = execute_online(
        parse_command(
            &s(&["tools", "bot-pr-1", "oauth", "sentry", "--status"]),
            None,
        )
        .unwrap(),
        &mut mock_tools,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(oauth_out.contains("status=pending"));

    // Remaining v2 RPC routing smoke checks (memory, tidy, suggestions, up-next, groups, sessions, avatar, tracker-default, access redeem, composio)
    let avatar_dir = temp_dir("avatar-v2");
    let png_in = avatar_dir.join("in.png");
    let png_out = avatar_dir.join("out/saved.png");
    fs::write(&png_in, b"\x89PNG\r\n\x1a\nfakepng").unwrap();
    let mut mock_v2 = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::MemoryHistory(vec![BotMemoryHistoryEntry {
            entry_id: "e1".to_owned(),
            bot_id: bot.id.clone(),
            timestamp_ms: 1,
            actor: "owner".to_owned(),
            action: "edit".to_owned(),
            added: vec!["x".to_owned()],
            removed: Vec::new(),
            before_content: String::new(),
            after_content: String::new(),
            undoable: true,
            undone: false,
        }]),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::MemoryUpdated {
            content: "updated".to_owned(),
            restored_facts: Vec::new(),
            removed_count: 0,
        },
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::MemoryTidyStatus(BotMemoryTidyStatus::default()),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::MemoryTopics(vec![BotMemoryTopicInfo {
            topic: "repos".to_owned(),
            bytes: 12,
            updated_at_ms: 1,
        }]),
        BotRpcResponse::Suggestions(vec![]),
        BotRpcResponse::Ok,
        BotRpcResponse::BotSessions(vec![BotSessionSummary {
            session_id: SessionId(9),
            bot_id: bot.id.clone(),
            link_id: None,
            pid: 99,
            live: true,
            working: false,
            label: "Triage".to_owned(),
            created_at_ms: 1,
        }]),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Ok,
        BotRpcResponse::GroupView(BotGroupView {
            group: BotGroupChat {
                id: "g1".to_owned(),
                name: "Ops".to_owned(),
                bot_ids: vec![bot.id.clone()],
                description: String::new(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            threads: vec![BotGroupThread {
                thread_id: "t1".to_owned(),
                group_id: "g1".to_owned(),
                title: "Incident".to_owned(),
                created_at_ms: 1,
                updated_at_ms: 1,
            }],
            messages: vec![],
            active_run: false,
        }),
        BotRpcResponse::GroupMessagePosted {
            message: BotGroupMessage {
                message_id: "m1".to_owned(),
                group_id: "g1".to_owned(),
                thread_id: "t1".to_owned(),
                sender_id: "owner".to_owned(),
                sender_name: "owner".to_owned(),
                body: "status?".to_owned(),
                mentions: vec![bot.id.clone()],
                created_at_ms: 2,
                in_progress: false,
            },
            dispatched_bots: vec![bot.id.clone()],
        },
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Ok,
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::BotAvatar(BotAvatarData {
            bot_id: bot.id.clone(),
            mime: "image/png".to_owned(),
            png_bytes: b"\x89PNG\r\n\x1a\ndata".to_vec(),
        }),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::ConnectorTest(BotConnectorTestResult {
            source: ExternalSource::GitHub,
            ok: true,
            account_label: Some("octocat".to_owned()),
            detail: "ok".to_owned(),
        }),
        BotRpcResponse::DefaultTrackerBots(DefaultTrackerBots {
            github: Some(bot.id.clone()),
            linear: None,
        }),
        BotRpcResponse::Ok,
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::ComposioConnections(vec![ComposioConnectionInfo {
            id: "c1".to_owned(),
            app_id: "notion".to_owned(),
            status: "ACTIVE".to_owned(),
            account_label: Some("me@example.com".to_owned()),
            created_at_ms: None,
        }]),
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::ComposioConnectStarted {
            connection_id: "c2".to_owned(),
            redirect_url: "https://composio.example/auth".to_owned(),
        },
    ]);

    assert!(
        execute_online(
            parse_command(&s(&["memory", "bot-pr-1", "history"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("e1")
    );
    assert!(
        execute_online(
            parse_command(
                &s(&["memory", "bot-pr-1", "edit", "--old", "a", "--new", "b"]),
                None
            )
            .unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("updated")
    );
    assert!(
        execute_online(
            parse_command(&s(&["tidy", "bot-pr-1", "status"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("memory tidy for")
    );
    assert!(
        execute_online(
            parse_command(&s(&["memory", "bot-pr-1", "topics"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("repos")
    );
    assert!(
        execute_online(
            parse_command(&s(&["suggestions", "ls", "--include-dismissed"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("No suggestions")
    );
    assert!(
        execute_online(
            parse_command(&s(&["up-next", "seen"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("seen")
    );
    assert!(
        execute_online(
            parse_command(&s(&["sessions", "ls"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("s9")
    );
    assert!(
        execute_online(
            parse_command(&s(&["sessions", "bot-pr-1", "stop", "s9"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("stopped session s9")
    );
    assert!(
        execute_online(
            parse_command(&s(&["groups", "read", "g1"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("Incident")
    );
    let send_g = execute_online(
        parse_command(
            &s(&[
                "groups",
                "send",
                "g1",
                "--thread",
                "t1",
                "--reply-mode",
                "all",
                "status?",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_v2,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(send_g.contains("m1"));
    assert!(matches!(
        mock_v2.recorded.last(),
        Some(BotRpcRequest::SendGroupMessage {
            reply_mode: Some(BotGroupReplyMode::All),
            ..
        })
    ));
    execute_online(
        parse_command(
            &s(&["avatar", "set", "bot-pr-1", png_in.to_str().unwrap()]),
            None,
        )
        .unwrap(),
        &mut mock_v2,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    execute_online(
        parse_command(
            &s(&["avatar", "get", "bot-pr-1", png_out.to_str().unwrap()]),
            None,
        )
        .unwrap(),
        &mut mock_v2,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(png_out.is_file());
    assert!(
        execute_online(
            parse_command(&s(&["connectors", "bot-pr-1", "test", "github"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("ok=true")
    );
    assert!(
        execute_online(
            parse_command(&s(&["tracker-default"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("github: bot-pr-1")
    );
    assert!(
        execute_online(
            parse_command(&s(&["access", "redeem", "PAIR-123"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("redeemed")
    );
    assert!(
        execute_online(
            parse_command(&s(&["composio", "bot-pr-1", "connections"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("notion")
    );
    assert!(
        execute_online(
            parse_command(&s(&["composio", "bot-pr-1", "connect", "linear"]), None).unwrap(),
            &mut mock_v2,
            &mut empty,
            0,
            |_| None
        )
        .unwrap()
        .contains("https://composio.example/auth")
    );

    fs::remove_dir_all(&avatar_dir).ok();
}

#[test]
fn connector_noop_google_defaults_resolve_bot_json_errors_and_p2_fixes() {
    let bot = sample_bot();
    let mut empty = Cursor::new(Vec::<u8>::new());

    // 1. Zero modifying flags -> never calls SetConnectorConfig
    let mut mock_noop = MockClient::new([BotRpcResponse::Bot(Box::new(bot.clone()))]);
    execute_online(
        parse_command(&s(&["connectors", "bot-pr-1", "telegram"]), None).unwrap(),
        &mut mock_noop,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert_eq!(mock_noop.recorded.len(), 1);
    assert!(matches!(
        mock_noop.recorded[0],
        BotRpcRequest::GetBot { .. }
    ));

    // 2. google-setup --enable without --services -> gmail = true, calendar = true
    let mut mock_google = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot.clone())),
    ]);
    execute_online(
        parse_command(
            &s(&["connectors", "bot-pr-1", "google-setup", "--enable"]),
            None,
        )
        .unwrap(),
        &mut mock_google,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(matches!(
        mock_google.recorded.get(1),
        Some(BotRpcRequest::SetConnectorConfig { connectors, .. })
            if connectors.google.enabled && connectors.google.services.gmail && connectors.google.services.calendar
    ));

    // 3. resolve_bot: falls back to ListBots on not-found, propagates transport/permission errors without ListBots
    let mut mock_nf = MockClient::with_results([
        Err(CliError::fail("bot not found: pr-reviewer")),
        Ok(BotRpcResponse::Bots(vec![bot.clone()])),
    ]);
    execute_online(
        parse_command(&s(&["show", "pr-reviewer"]), None).unwrap(),
        &mut mock_nf,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert_eq!(mock_nf.recorded.len(), 2);

    let mut mock_perm = MockClient::with_results([Err(CliError::fail("permission denied"))]);
    let perm_err = execute_online(
        parse_command(&s(&["show", "pr-reviewer"]), None).unwrap(),
        &mut mock_perm,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap_err();
    assert!(perm_err.message.contains("permission denied"));
    assert_eq!(mock_perm.recorded.len(), 1);

    // 4. Export to non-existent trailing-slash directory
    let dir = temp_dir("trailing-slash");
    let trailing = format!("{}/nested-dir/", dir.display());
    let raw_zip = export_bot_zip(
        &bot,
        "# PR Reviewer\n",
        &BTreeMap::new(),
        None,
        &BTreeMap::new(),
        None,
        BotPackageExportOptions {
            include_memory: false,
            include_avatar: false,
        },
    )
    .unwrap();
    let mut mock_exp = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::ExportedZip {
            filename: "pr-reviewer.zip".to_owned(),
            zip_bytes: raw_zip,
        },
    ]);
    execute_online(
        parse_command(&s(&["export", "bot-pr-1", &trailing]), None).unwrap(),
        &mut mock_exp,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert!(dir.join("nested-dir/pr-reviewer.zip").is_file());
    fs::remove_dir_all(&dir).ok();

    // 5. tools add accepts http://[::1]
    let mut mock_v6 = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Bot(Box::new(bot)),
    ]);
    execute_online(
        parse_command(
            &s(&[
                "tools",
                "bot-pr-1",
                "add",
                "local",
                "--url",
                "http://[::1]:8080/mcp",
            ]),
            None,
        )
        .unwrap(),
        &mut mock_v6,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();

    // 6. parse_snooze_until validates real calendar month lengths & leap years
    assert!(parse_snooze_until("2026-02-29", 0).is_err());
    assert!(parse_snooze_until("2026-02-31", 0).is_err());
    assert!(parse_snooze_until("2026-04-31", 0).is_err());
    assert!(parse_snooze_until("2024-02-29", 0).is_ok());

    // 7. --json error formatting across parse failures, FEAT_BOTS unnegotiated, and BotRpcResponse::Error
    let (parse_err, json_flag) = run_with_client(
        &[OsString::from("unknown-sub"), OsString::from("--json")],
        None,
        |_| -> Result<MockClient, CliError> { panic!("should not connect") },
        &mut empty,
        0,
        |_| None,
    )
    .unwrap_err();
    assert!(json_flag);
    let parsed_json: serde_json::Value =
        serde_json::from_str(&super::format_cli_error(&parse_err, true)).unwrap();
    assert_eq!(parsed_json["ok"], false);
    assert_eq!(parsed_json["code"], 2);

    let feat_err = ensure_bots_feature(FEAT_CONTROL).unwrap_err();
    let feat_json: serde_json::Value =
        serde_json::from_str(&super::format_cli_error(&feat_err, true)).unwrap();
    assert_eq!(feat_json["ok"], false);
    assert!(feat_json["error"].as_str().unwrap().contains("FEAT_BOTS"));
    assert!(ensure_bots_feature(FEAT_CONTROL | FEAT_BOTS).is_ok());

    let (rpc_err, rpc_json_flag) = run_with_client(
        &[OsString::from("ls"), OsString::from("--json")],
        None,
        |_| Ok(MockClient::with_results([Err(CliError::fail("boom"))])),
        &mut empty,
        0,
        |_| None,
    )
    .unwrap_err();
    assert!(rpc_json_flag);
    let rpc_json: serde_json::Value =
        serde_json::from_str(&super::format_cli_error(&rpc_err, true)).unwrap();
    assert_eq!(rpc_json["ok"], false);
    assert_eq!(rpc_json["error"], "boom");

    // 8. Secret keys & connector *-ref flags documented in USAGE
    for flag in [
        "--secret-ref",
        "--app-token-ref",
        "--bot-token-ref",
        "--client-secret-ref",
        "--token-secret-ref",
        "--token-ref",
        "--api-key-ref",
        "google-oauth",
        "google-token",
        "slack-app",
        "slack-bot",
    ] {
        assert!(USAGE.contains(flag), "missing {flag} in bot USAGE");
    }
}

#[test]
fn offer_proposes_a_bot_from_the_bots_own_chat() {
    assert_eq!(
        parse_command(
            &s(&[
                "offer",
                "--name",
                "Ledger",
                "--purpose",
                "Keeps the spend sheet."
            ]),
            Some("shop")
        )
        .unwrap(),
        BotCommand::Offer {
            bot: "shop".to_owned(),
            name: "Ledger".to_owned(),
            purpose: "Keeps the spend sheet.".to_owned(),
            json: false,
        }
    );
    assert_eq!(
        parse_command(
            &s(&[
                "offer",
                "--bot",
                "ops",
                "--name",
                "Ledger",
                "--purpose",
                "Keeps the spend sheet.",
                "--json",
            ]),
            None
        )
        .unwrap(),
        BotCommand::Offer {
            bot: "ops".to_owned(),
            name: "Ledger".to_owned(),
            purpose: "Keeps the spend sheet.".to_owned(),
            json: true,
        }
    );
    assert!(parse_command(&s(&["offer", "--name", "Ledger"]), Some("shop")).is_err());
    assert!(USAGE.contains("offer "));
    assert!(crate::USAGE.contains("/offer/"));

    let bot = sample_bot();
    let mut empty = Cursor::new(Vec::<u8>::new());
    let mut client = MockClient::new([
        BotRpcResponse::Bot(Box::new(bot.clone())),
        BotRpcResponse::Text("Offered Ledger\n".to_owned()),
        BotRpcResponse::Bot(Box::new(bot)),
        BotRpcResponse::Text("Offered Ledger\n".to_owned()),
    ]);
    let text_out = execute_online(
        parse_command(
            &s(&[
                "offer",
                "bot-pr-1",
                "--name",
                "Ledger",
                "--purpose",
                "Tracks spend",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    assert_eq!(text_out, "Offered Ledger");

    let json_out = execute_online(
        parse_command(
            &s(&[
                "offer",
                "--bot",
                "bot-pr-1",
                "--name",
                "Ledger",
                "--purpose",
                "Tracks spend",
                "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        0,
        |_| None,
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["message"], "Offered Ledger");
}

#[test]
fn session_refusal_uses_remote_host_name_when_present() {
    let refusal = krill_proto::bots::BotSessionRefusal::cli_not_installed("claude");
    let remote = super::format_session_refusal(&refusal, Some("user@remote"));
    assert!(remote.contains("user@remote"), "{remote}");
    assert!(!remote.contains("this computer"), "{remote}");
    let local = super::format_session_refusal(&refusal, None);
    assert!(local.contains("this computer"), "{local}");
}

#[test]
fn items_ls_with_status_filter_and_pagination_computes_accurate_counts_and_cursors() {
    use krill_proto::bots::BotUpcoming;
    let bot_id = BotId::new("bot-pr-1").unwrap();
    let mk_item = |num: u64, status: BotItemStatus, ts: u64, snooze: Option<u64>, ev_id: u64| {
        let mut item = sample_item();
        item.uid = format!("item-uid-{num}");
        item.number = num;
        item.owner_bot_id = bot_id.clone();
        item.title = format!("Item {num}");
        item.status = status;
        item.last_activity_at_ms = ts;
        item.snoozed_until_ms = snooze;
        item.events = vec![BotItemEvent {
            id: ev_id,
            item_uid: item.uid.clone(),
            kind: BotItemEventKind::Commented,
            actor: BotItemActor::Bot {
                bot_id: bot_id.clone(),
                session_id: None,
            },
            text: "hi".to_owned(),
            decisions: Vec::new(),
            created_at_ms: ts,
        }];
        item
    };
    let all_items = vec![
        mk_item(1, BotItemStatus::NeedsYou, 100, None, 10),
        mk_item(2, BotItemStatus::Working, 250, None, 20),
        mk_item(3, BotItemStatus::NeedsYou, 200, None, 30),
        mk_item(4, BotItemStatus::NeedsYou, 300, None, 40),
        mk_item(5, BotItemStatus::NeedsYou, 400, Some(9_999), 50),
    ];
    let upcoming = BotUpcoming {
        items_seen_event_id: 25,
        ..BotUpcoming::default()
    };

    let mut empty = Cursor::new(Vec::<u8>::new());
    let mut client = MockClient::new([
        // Call 1: --status needs_you --limit 2 --page 1 --json
        BotRpcResponse::Items(all_items.clone()),
        BotRpcResponse::Upcoming(upcoming.clone()),
        // Call 2: --status needs_you --limit 2 --page 2 --json
        BotRpcResponse::Items(all_items.clone()),
        BotRpcResponse::Upcoming(upcoming.clone()),
        // Call 3: --status needs_you --limit 2 --page 3 --json (exhausted)
        BotRpcResponse::Items(all_items.clone()),
        BotRpcResponse::Upcoming(upcoming.clone()),
        // Call 4: --status needs_you --limit 2 --cursor <page1_cursor> --json
        BotRpcResponse::Items(all_items.clone()),
        BotRpcResponse::Upcoming(upcoming.clone()),
        // Call 5: --status needs_you --limit 2 --include-snoozed --json
        BotRpcResponse::Items(all_items.clone()),
        BotRpcResponse::Upcoming(upcoming),
        // Call 6: --status planned --limit 2 --json (zero matches; no Upcoming needed)
        BotRpcResponse::Items(all_items),
    ]);

    let p1_raw = execute_online(
        parse_command(
            &s(&[
                "items",
                "ls",
                "--status",
                "needs_you",
                "--limit",
                "2",
                "--page",
                "1",
                "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        1_000,
        |_| None,
    )
    .unwrap();
    let p1: BotItemsPage = serde_json::from_str(&p1_raw).unwrap();
    assert_eq!(p1.total_count, 3);
    assert_eq!(p1.unseen_count, 2); // events 30, 40 > 25 (item 1 has ev 10 <= 25; item 5 is snoozed)
    assert_eq!(
        p1.items.iter().map(|i| i.number).collect::<Vec<_>>(),
        vec![4, 3]
    );
    let cursor1 = p1
        .next_cursor
        .clone()
        .expect("page 1 must have next_cursor");

    let p2_raw = execute_online(
        parse_command(
            &s(&[
                "items",
                "ls",
                "--status",
                "needs_you",
                "--limit",
                "2",
                "--page",
                "2",
                "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        1_000,
        |_| None,
    )
    .unwrap();
    let p2: BotItemsPage = serde_json::from_str(&p2_raw).unwrap();
    assert_eq!(p2.total_count, 3);
    assert_eq!(p2.unseen_count, 2);
    assert_eq!(
        p2.items.iter().map(|i| i.number).collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(p2.next_cursor, None);

    let p3_raw = execute_online(
        parse_command(
            &s(&[
                "items",
                "ls",
                "--status",
                "needs_you",
                "--limit",
                "2",
                "--page",
                "3",
                "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        1_000,
        |_| None,
    )
    .unwrap();
    let p3: BotItemsPage = serde_json::from_str(&p3_raw).unwrap();
    assert_eq!(p3.total_count, 3);
    assert!(p3.items.is_empty());
    assert_eq!(p3.next_cursor, None);

    let cur_raw = execute_online(
        parse_command(
            &s(&[
                "items",
                "ls",
                "--status",
                "needs_you",
                "--limit",
                "2",
                "--cursor",
                &cursor1,
                "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        1_000,
        |_| None,
    )
    .unwrap();
    let cur_page: BotItemsPage = serde_json::from_str(&cur_raw).unwrap();
    assert_eq!(
        cur_page.items.iter().map(|i| i.number).collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(cur_page.next_cursor, None);

    let snoozed_raw = execute_online(
        parse_command(
            &s(&[
                "items",
                "ls",
                "--status",
                "needs_you",
                "--limit",
                "2",
                "--include-snoozed",
                "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        1_000,
        |_| None,
    )
    .unwrap();
    let snoozed_page: BotItemsPage = serde_json::from_str(&snoozed_raw).unwrap();
    assert_eq!(snoozed_page.total_count, 4);
    assert_eq!(snoozed_page.unseen_count, 3);
    assert_eq!(
        snoozed_page
            .items
            .iter()
            .map(|i| i.number)
            .collect::<Vec<_>>(),
        vec![5, 4]
    );

    let zero_raw = execute_online(
        parse_command(
            &s(&[
                "items", "ls", "--status", "planned", "--limit", "2", "--json",
            ]),
            None,
        )
        .unwrap(),
        &mut client,
        &mut empty,
        1_000,
        |_| None,
    )
    .unwrap();
    let zero_page: BotItemsPage = serde_json::from_str(&zero_raw).unwrap();
    assert_eq!(zero_page.total_count, 0);
    assert_eq!(zero_page.unseen_count, 0);
    assert!(zero_page.items.is_empty());
    assert_eq!(zero_page.next_cursor, None);

    assert!(
        client
            .recorded
            .iter()
            .all(|req| !matches!(req, BotRpcRequest::ListItemsPage { .. }))
    );
}

#[test]
fn groups_send_defaults_sender_to_env_bot_id_and_avatar_rpc_fits_frame() {
    let cmd = parse_command(
        &s(&["groups", "send", "grp-1", "hello team"]),
        Some("alpha"),
    )
    .unwrap();
    match cmd {
        BotCommand::GroupsSend {
            group_id,
            sender,
            text,
            ..
        } => {
            assert_eq!(group_id, "grp-1");
            assert_eq!(sender.as_deref(), Some("alpha"));
            assert_eq!(text, "hello team");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // S1-08: MAX_RPC_AVATAR_PNG_BYTES must fit inside a single ipc::MAX_FRAME.
    let req = BotRpcRequest::UploadBotAvatar {
        bot_id: BotId::new("alpha").unwrap(),
        png_bytes: vec![0u8; krill_proto::bots::MAX_RPC_AVATAR_PNG_BYTES],
    };
    let encoded = krill_proto::screen::Msg::encode(&req)
        .expect("max RPC avatar must fit inside ipc::MAX_FRAME");
    assert!(
        encoded.len() <= krill_proto::ipc::MAX_FRAME + 4,
        "encoded avatar frame ({}) must fit inside ipc::MAX_FRAME + 4 ({})",
        encoded.len(),
        krill_proto::ipc::MAX_FRAME + 4
    );
}
