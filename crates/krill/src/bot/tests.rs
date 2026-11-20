// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Comprehensive unit tests for `krill bot` CLI parsing, offline catalog,
//! mock RPC execution, secret redaction, and zip export/import.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::SessionId;
use krill_proto::agent::Permission;
use krill_proto::bots::package::{BotPackageExportOptions, export_bot_zip, import_bot_zip};
use krill_proto::bots::{
    ActsFor, BotAccessRequest, BotAccessRequestStatus, BotConfig, BotConnectorStatus, BotGroupChat,
    BotId, BotItem, BotItemLink, BotItemStatus, BotMcpAuthKind, BotRpcRequest, BotRpcResponse,
    BotSuggestion, BotToolRelayStatus, BotToolServerStatus, ConnectorHealthState,
    ExternalBotCandidate, ExternalBotSourceApp, ExternalSource, FieldPatch,
};

use super::exec::{
    build_secret_ref, execute_online, parse_snooze_until, today_iso_utc, try_execute_offline,
};
use super::parse::{
    AppsKind, BotCommand, ConnectorPatch, GoogleServiceKind, MailKind, SecretInputSource,
    TextInputSource, parse_command,
};
use super::{BotClient, CliError};

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
            &s(&[
                "items",
                "mybot",
                "link",
                "42",
                "pr",
                "org/repo#99",
                "--label",
                "Main PR",
            ]),
            None
        )
        .unwrap(),
        BotCommand::ItemsLink {
            bot: Some("mybot".to_owned()),
            item_id: "42".to_owned(),
            link: BotItemLink::Pr("org/repo#99".to_owned()),
            label: Some("Main PR".to_owned()),
            json: false,
        }
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
            label: None,
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
    let Some(BotRpcRequest::ImportBotZip {
        zip_bytes,
        name_override,
        ..
    }) = mock_imp.recorded.first()
    else {
        panic!("expected ImportBotZip");
    };
    assert_eq!(name_override.as_deref(), Some("imported-pr"));
    let parsed_pkg = import_bot_zip(zip_bytes).unwrap();
    assert_eq!(parsed_pkg.manifest.slug.as_str(), "imported-pr");

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
