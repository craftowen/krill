// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Pure argument parser for `krill bot` subcommands (Phases 1-3).

use krill_proto::agent::Permission;
use krill_proto::bots::{
    ActsFor, BotGroupReplyMode, BotItemDecision, BotItemLink, BotItemStatus, BotMcpAppCategory,
    BotMcpAuthKind, BotTemplateCategory, ExternalBotSourceApp, ExternalSource, FieldPatch,
    validate_bot_slug,
};
use krill_proto::schedule::Cron;

use super::{CliError, USAGE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BotCommand {
    Help(Option<String>),
    // Lifecycle
    List {
        json: bool,
    },
    New(NewBotArgs),
    Show {
        bot: String,
        json: bool,
    },
    Update(UpdateBotArgs),
    Remove {
        bot: String,
        delete_home: bool,
        json: bool,
    },
    Start {
        bot: String,
        prompt: Option<String>,
        cwd: Option<String>,
        json: bool,
    },
    // Offline Catalog
    Templates {
        category: Option<BotTemplateCategory>,
        show_id: Option<String>,
        json: bool,
    },
    Skills {
        show_name: Option<String>,
        json: bool,
    },
    Apps {
        kind: AppsKind,
        category: Option<BotMcpAppCategory>,
        json: bool,
    },
    SlackManifest {
        bot_name: String,
        json: bool,
    },
    // Soul & Memory
    SoulGet {
        bot: String,
        json: bool,
    },
    SoulSet {
        bot: String,
        source: TextInputSource,
        json: bool,
    },
    MemoryList {
        bot: String,
        json: bool,
    },
    MemoryAdd {
        bot: String,
        fact: String,
        date: Option<String>,
        source: Option<String>,
        json: bool,
    },
    MemoryForget {
        bot: String,
        fact: String,
        json: bool,
    },
    MemoryTidy {
        bot: String,
        json: bool,
    },
    MemoryTidyStatus {
        bot: String,
        json: bool,
    },
    MemoryTidyUndo {
        bot: String,
        tidy_id: Option<String>,
        json: bool,
    },
    MemoryHistory {
        bot: String,
        limit: Option<u32>,
        json: bool,
    },
    MemoryEdit {
        bot: String,
        old_fact: String,
        new_fact: String,
        json: bool,
    },
    MemoryUndo {
        bot: String,
        entry_id: String,
        json: bool,
    },
    MemoryTopics {
        bot: String,
        json: bool,
    },
    MemoryTopicGet {
        bot: String,
        topic: String,
        json: bool,
    },
    MemoryTopicSet {
        bot: String,
        topic: String,
        source: TextInputSource,
        json: bool,
    },
    MemorySet {
        bot: String,
        source: TextInputSource,
        allow_deletions: bool,
        json: bool,
    },
    // Items Inbox
    ItemsList {
        bot: Option<String>,
        status: Option<BotItemStatus>,
        all: bool,
        include_snoozed: bool,
        limit: Option<u32>,
        cursor: Option<String>,
        page: Option<u32>,
        json: bool,
    },
    ItemsShow {
        bot: Option<String>,
        item_id: String,
        json: bool,
    },
    ItemsAdd(ItemsAddArgs),
    ItemsUpdate(ItemsUpdateArgs),
    ItemsAnswer {
        bot: Option<String>,
        item_id: String,
        text: Option<String>,
        option_index: Option<u32>,
        decision_id: Option<String>,
        request_id: Option<String>,
        json: bool,
    },
    ItemsComment {
        bot: Option<String>,
        item_id: String,
        author: Option<String>,
        text: String,
        json: bool,
    },
    ItemsSnooze {
        bot: Option<String>,
        item_id: String,
        until: String,
        json: bool,
    },
    ItemsUnsnooze {
        bot: Option<String>,
        item_id: String,
        json: bool,
    },
    ItemsLink {
        bot: Option<String>,
        item_id: String,
        link: BotItemLink,
        json: bool,
    },
    ItemsClose {
        bot: Option<String>,
        item_id: String,
        status: BotItemStatus,
        note: Option<String>,
        json: bool,
    },
    // Look Around, Schedules, Sessions & Messaging
    SuggestRun {
        bot: String,
        json: bool,
    },
    SuggestPropose {
        bot: String,
        title: String,
        why: String,
        ask: String,
        link: Option<String>,
        json: bool,
    },
    SuggestionsList {
        bot: Option<String>,
        include_dismissed: bool,
        json: bool,
    },
    SuggestionsDismiss {
        bot: String,
        suggestion_id: String,
        json: bool,
    },
    SuggestionsStart {
        bot: String,
        suggestion_id: String,
        json: bool,
    },
    UpNextSeen {
        bot: Option<String>,
        json: bool,
    },
    SchedulesList {
        bot: String,
        json: bool,
    },
    SchedulesAdd {
        bot: String,
        id: Option<String>,
        name: String,
        cron: String,
        prompt: String,
        timezone: Option<String>,
        enabled: bool,
        json: bool,
    },
    SchedulesSetEnabled {
        bot: String,
        id: String,
        enabled: bool,
        json: bool,
    },
    SchedulesRemove {
        bot: String,
        id: String,
        json: bool,
    },
    SessionsList {
        bot: Option<String>,
        json: bool,
    },
    SessionsReset {
        bot: String,
        session_id: Option<u64>,
        json: bool,
    },
    SessionsStop {
        bot: String,
        session_id: u64,
        json: bool,
    },
    SessionsRetryKickoff {
        bot: String,
        json: bool,
    },
    SessionsTakeOver {
        bot: String,
        session_id: u64,
        json: bool,
    },
    SessionsRunAgain {
        bot: String,
        session_id: u64,
        json: bool,
    },
    Offer {
        bot: String,
        name: String,
        purpose: String,
        json: bool,
    },
    Reply {
        bot: String,
        text: String,
        link_id: Option<String>,
        files: Vec<String>,
        reaction: Option<String>,
        json: bool,
    },
    SendMail {
        from: Option<String>,
        to_bot: String,
        kind: Option<MailKind>,
        subject: Option<String>,
        body: String,
        thread_id: Option<String>,
        json: bool,
    },
    GroupsList {
        json: bool,
    },
    GroupsCreate {
        name: String,
        members: Vec<String>,
        description: String,
        json: bool,
    },
    GroupsUpdate {
        group_id: String,
        name: Option<String>,
        description: Option<String>,
        members: Option<Vec<String>>,
        json: bool,
    },
    GroupsRemove {
        group_id: String,
        json: bool,
    },
    GroupsReorder {
        group_ids: Vec<String>,
        json: bool,
    },
    GroupsNewThread {
        group_id: String,
        title: Option<String>,
        json: bool,
    },
    GroupsRead {
        group_id: String,
        thread_id: Option<String>,
        limit: Option<u32>,
        json: bool,
    },
    GroupsSend {
        group_id: String,
        thread_id: Option<String>,
        sender: Option<String>,
        text: String,
        reply_mode: Option<BotGroupReplyMode>,
        json: bool,
    },
    GroupsStop {
        group_id: String,
        thread_id: Option<String>,
        json: bool,
    },
    // Connectors & Tracker Routing
    ConnectorStatus {
        bot: Option<String>,
        json: bool,
    },
    ConnectorReload {
        bot: Option<String>,
        json: bool,
    },
    ConnectorSync {
        bot: String,
        source: Option<ExternalSource>,
        json: bool,
    },
    ConnectorTest {
        bot: String,
        source: ExternalSource,
        json: bool,
    },
    ConnectorConfigure(ConnectorPatchArgs),
    TrackerDefaultGet {
        json: bool,
    },
    TrackerDefaultSet {
        source: ExternalSource,
        bot: Option<String>,
        json: bool,
    },
    ComposioConnections {
        bot: String,
        json: bool,
    },
    ComposioConnect {
        bot: String,
        app_id: String,
        json: bool,
    },
    ComposioDisconnect {
        bot: String,
        connection_id: String,
        json: bool,
    },
    // Access Control
    AccessList {
        bot: Option<String>,
        json: bool,
    },
    AccessDecide {
        token: String,
        approve: bool,
        json: bool,
    },
    AccessRedeem {
        code: String,
        json: bool,
    },
    // Secrets
    SecretSet {
        bot: String,
        key: String,
        source: SecretInputSource,
        json: bool,
    },
    SecretRemove {
        bot: String,
        key: String,
        json: bool,
    },
    // Tools & Relay
    ToolsList {
        bot: String,
        json: bool,
    },
    ToolsAdd(ToolsAddArgs),
    ToolsSetEnabled {
        bot: String,
        app_id: String,
        enabled: bool,
        json: bool,
    },
    ToolsOAuth {
        bot: String,
        tool_id: String,
        mode: ToolsOAuthMode,
        json: bool,
    },
    ToolsRemove {
        bot: String,
        app_id: String,
        json: bool,
    },
    ToolsRelayStatus {
        json: bool,
    },
    // Avatar
    AvatarSet {
        bot: String,
        png_path: String,
        json: bool,
    },
    AvatarGet {
        bot: String,
        out_path: String,
        json: bool,
    },
    AvatarColor {
        bot: String,
        color: String,
        json: bool,
    },
    // Packaging & External Import
    ExportZip {
        bot: String,
        out_path: String,
        include_memory: bool,
        include_avatar: bool,
        json: bool,
    },
    ImportZip {
        in_path: String,
        slug: Option<String>,
        name: Option<String>,
        agent: Option<String>,
        json: bool,
    },
    ImportExternal {
        source: Option<ExternalBotSourceApp>,
        path: Option<String>,
        apply: bool,
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppsKind {
    Mcp,
    Composio,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailKind {
    Status,
    Question,
    Handoff,
}

impl MailKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Question => "question",
            Self::Handoff => "handoff",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextInputSource {
    Inline(String),
    File(String),
    Stdin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SecretInputSource {
    Stdin,
    Env(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolsOAuthMode {
    Start,
    Status,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewBotArgs {
    pub name: Option<String>,
    pub template_id: Option<String>,
    pub agent_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<Permission>,
    pub description: Option<String>,
    pub emoji: Option<String>,
    pub avatar: Option<String>,
    pub avatar_color: Option<String>,
    pub skills: Vec<String>,
    pub folder: Option<String>,
    pub slug: Option<String>,
    pub soul: Option<TextInputSource>,
    pub json: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UpdateBotArgs {
    pub bot: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub agent_id: Option<String>,
    pub model: FieldPatch<String>,
    pub effort: FieldPatch<String>,
    pub emoji: FieldPatch<String>,
    pub avatar: FieldPatch<String>,
    pub avatar_color: FieldPatch<String>,
    pub workspace: FieldPatch<String>,
    pub permission_mode: Option<Permission>,
    pub paused: Option<bool>,
    pub first_run_completed: Option<bool>,
    pub look_around_enabled: Option<bool>,
    pub look_around_interval_secs: Option<u64>,
    pub look_around_max_suggestions: Option<u32>,
    pub look_around_weekdays_only: Option<bool>,
    pub look_around_focus: Option<String>,
    pub memory_tidy_enabled: Option<bool>,
    pub memory_tidy_interval_secs: Option<u64>,
    pub notify_permission: Option<bool>,
    pub notify_complete: Option<bool>,
    pub notify_error: Option<bool>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemsAddArgs {
    pub bot: String,
    pub title: String,
    pub body: Option<String>,
    pub done_means: Option<String>,
    pub status: Option<BotItemStatus>,
    pub links: Vec<BotItemLink>,
    pub json: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ItemsUpdateArgs {
    pub bot: Option<String>,
    pub item_id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub done_means: Option<String>,
    pub status: Option<BotItemStatus>,
    pub comment: Option<String>,
    pub decisions: Vec<BotItemDecision>,
    pub close_note: Option<String>,
    pub snooze: Option<String>,
    pub unsnooze: bool,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectorPatchArgs {
    pub bot: String,
    pub patch: ConnectorPatch,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectorPatch {
    Telegram {
        enabled: Option<bool>,
        secret_ref: Option<String>,
        allowed_chats: Option<Vec<i64>>,
        allowed_users: Option<Vec<i64>>,
        acts_for: Option<ActsFor>,
    },
    Slack {
        enabled: Option<bool>,
        app_token_ref: Option<String>,
        bot_token_ref: Option<String>,
        allowed_channels: Option<Vec<String>>,
        allowed_users: Option<Vec<String>>,
        mirror_desktop: Option<bool>,
        acts_for: Option<ActsFor>,
    },
    Discord {
        enabled: Option<bool>,
        bot_token_ref: Option<String>,
        allowed_guilds: Option<Vec<String>>,
        allowed_channels: Option<Vec<String>>,
        allowed_users: Option<Vec<String>>,
        acts_for: Option<ActsFor>,
    },
    Google {
        enabled: Option<bool>,
        poll_secs: Option<u64>,
        services: Option<Vec<GoogleServiceKind>>,
        client_secret_ref: Option<String>,
        token_secret_ref: Option<String>,
        start_oauth: bool,
    },
    GitHub {
        enabled: Option<bool>,
        repos: Option<Vec<String>>,
        use_gh_cli: Option<bool>,
        token_ref: Option<String>,
        poll_secs: Option<u64>,
    },
    Linear {
        enabled: Option<bool>,
        teams: Option<Vec<String>>,
        api_key_ref: Option<String>,
        poll_secs: Option<u64>,
    },
    Composio {
        enabled: Option<bool>,
        apps: Option<Vec<String>>,
        api_key_ref: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoogleServiceKind {
    Gmail,
    Calendar,
    Drive,
    Docs,
    Sheets,
    Contacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolsAddArgs {
    pub bot: String,
    pub app_id: String,
    pub name: Option<String>,
    pub url: Option<String>,
    pub headers: Vec<String>,
    pub auth_kind: Option<BotMcpAuthKind>,
    pub secret_ref: Option<String>,
    pub command: Vec<String>,
    pub json: bool,
}

pub(crate) fn help_text(topic: Option<&str>) -> &'static str {
    match topic {
        Some("new" | "create") => {
            "usage: krill bot new <name> [--template <id>] [--agent <id>] [--model <m>] [--effort <lvl>] [--permission <manual|yolo>] [--description <text>] [--emoji <str>] [--avatar <str>] [--avatar-color <hex>] [--skills <a,b>] [--folder <path>] [--slug <slug>] [--soul <text> | --soul-file <path>] [--json]"
        }
        Some("update" | "edit") => {
            "usage: krill bot update <bot> [--name <n>] [--description <d>] [--agent <id>] [--model <m>|--clear-model] [--effort <e>|--clear-effort] [--emoji <e>|--clear-emoji] [--avatar <a>|--clear-avatar] [--avatar-color <hex>|--clear-avatar-color] [--folder <p>|--clear-workspace] [--permission <manual|yolo>] [--first-run-completed <on|off>] [--pause|--resume] [--look-around <on|off>] [--look-around-interval <secs>] [--look-around-max <n>] [--look-around-weekdays <on|off>] [--look-around-focus <text>] [--memory-tidy <on|off>] [--memory-tidy-interval <secs>] [--notify-permission <on|off>] [--notify-complete <on|off>] [--notify-error <on|off>] [--json]"
        }
        Some("offer") => {
            "usage: krill bot offer [<bot> | --bot <bot>] --name <name> --purpose <text> [--json]  (propose a new bot; your owner sees a card in this chat and nothing is created until they press Create bot)"
        }
        Some("soul") => {
            "usage: krill bot soul [<bot>] [get | set (--text <str> | --file <path> | --stdin)] [--json]"
        }
        Some("memory") => {
            "usage: krill bot memory [<bot>] [ls | add <fact> [--date YYYY-MM-DD] [--source <src>] | forget --fact <fact> | history [--limit <N>] | edit --old <fact> --new <fact> | undo <entry-id> | tidy [status | undo [<tidy-id>]] | topics | topic <name> [--set (--text <str> | --file <path> | --stdin)] | set (--text <str> | --file <path> | --stdin) [--allow-deletions]] [--json]"
        }
        Some("tidy") => "usage: krill bot tidy [<bot>] [run | status | undo [<tidy-id>]] [--json]",
        Some("items" | "item") => {
            "usage: krill bot items [<bot>] [ls [--status <st>] [--all] [--include-snoozed] [--limit <N>] [--cursor <c> | --page <N>] | show <item-id> | add <title> [--body <text>] [--done-means <text>] [--status <st>] [--link <kind:target>...] | update <item-id> [--status <st>] [--title <t>] [--body <b>] [--done-means <d>] [--comment <c>] [--close-note <n>] [--snooze <until>|--unsnooze] [--ask <q> | --question <q> [--option <opt>... [--recommended <idx>]]] | answer <item-id> [--text <str> | --option <idx>] [--decision-id <id>] [--request-id <id>] | comment <item-id> <text> [--author <name>] | snooze <item-id> <until> | unsnooze <item-id> | link <item-id> (<dispatch|pr|url|file> <target> | <kind>:<target>) | done <item-id> [--note <text>] | cancel <item-id> [--note <text>]] [--json]"
        }
        Some("suggestions" | "suggestion") => {
            "usage: krill bot suggestions [<bot>] [ls [--include-dismissed] | dismiss <id> | start <id>] [--json]"
        }
        Some("up-next") => "usage: krill bot up-next seen [--bot <bot>] [--json]",
        Some("schedules" | "schedule") => {
            "usage: krill bot schedules [<bot>] [ls | add --name <n> --cron <expr> --prompt <text> [--id <id>] [--timezone <tz>] [--disabled] | enable <id> | disable <id> | rm <id>] [--json]"
        }
        Some("sessions" | "session") => {
            "usage: krill bot sessions [<bot>] [ls | reset [--session <id>] | stop <session-id> | retry-kickoff | take-over <session-id> | run-again <session-id>] [--json]"
        }
        Some("connectors" | "connector") => {
            "usage: krill bot connectors [<bot>] [status | reload | sync [--source <src>] | test <source> | slack-manifest <bot-name> | telegram [--enable|--disable] [--secret-ref <ref>] [--allowed-chats <ids>] [--allowed-users <ids>] [--acts-for ...] | slack [--enable|--disable] [--app-token-ref <ref>] [--bot-token-ref <ref>] [--allowed-channels <ids>] [--allowed-users <ids>] [--mirror|--no-mirror] [--acts-for ...] | discord [--enable|--disable] [--bot-token-ref <ref>] [--allowed-guilds <ids>] [--allowed-channels <ids>] [--allowed-users <ids>] [--acts-for ...] | google-setup [--enable|--disable] [--poll-secs <N>] [--services ...] [--client-secret-ref <ref>] [--token-secret-ref <ref>] [--start-oauth] | github [--enable|--disable] [--repos owner/repo,...] [--use-gh-cli|--no-gh-cli] [--token-ref <ref>] [--poll-secs <N>] | linear [--enable|--disable] [--teams KEY,...] [--api-key-ref <ref>] [--poll-secs <N>] | composio [--enable|--disable] [--apps app1,app2] [--api-key-ref <ref>]] [--json]"
        }
        Some("tracker-default") => {
            "usage: krill bot tracker-default [github|linear] [--bot <bot> | --clear] [--json]"
        }
        Some("composio") => {
            "usage: krill bot composio [<bot>] [connections | connect <app-id> | disconnect <connection-id>] [--json]"
        }
        Some("secret" | "secrets") => {
            "usage: krill bot secret [<bot>] [set <key> (--stdin | --env <VAR>) | rm <key>] [--json]\nkeys: telegram, slack-app, slack-bot, discord, google-oauth, google-token, github, linear, composio, mcp:<app-id>"
        }
        Some("tools" | "tool") => {
            "usage: krill bot tools [<bot>] [ls | add <app-id> [--url <url>] [--name <name>] [--header <name>...] [--auth oauth|bearer|none] [--secret-ref <ref>] [--cmd <bin>...] | enable <app-id> | disable <app-id> | oauth <tool-id> [--status | --cancel] | rm <app-id> | relay-status] [--json]"
        }
        Some("access") => {
            "usage: krill bot access [<bot>] [ls | approve <token> | decline <token> | redeem <code>] [--json]"
        }
        Some("groups" | "group") => {
            "usage: krill bot groups [ls | create <name> --members <a,b> [--description <text>] | update <group-id> [--name <n>] [--description <d>] [--members <a,b>] | rm <group-id> | reorder <id1,id2,...> | new-thread <group-id> [--title <t>] | read <group-id> [--thread <id>] [--limit <N>] | send <group-id> [--thread <id>] [--from <sender>] [--reply-mode mentions|all|selected] <message> | stop <group-id> [--thread <id>]] [--json]"
        }
        Some("avatar") => {
            "usage: krill bot avatar [set <bot> <png-path> | get <bot> <out-path> | color <bot> <hex>] [--json]"
        }
        _ => USAGE,
    }
}

pub(crate) fn parse_command(
    args: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let Some((first, rest)) = args.split_first() else {
        return Ok(BotCommand::Help(None));
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return Ok(BotCommand::Help(rest.first().cloned()));
    }
    if matches!(rest.first().map(String::as_str), Some("-h" | "--help")) {
        return Ok(BotCommand::Help(Some(first.clone())));
    }

    match first.as_str() {
        "ls" | "list" => parse_list(rest),
        "new" | "create" => parse_new(rest),
        "show" | "get" | "info" => parse_show(rest, default_bot),
        "update" | "edit" => parse_update(rest, default_bot),
        "rm" | "remove" | "delete" => parse_remove(rest, default_bot),
        "start" | "chat" => parse_start(rest, default_bot),
        "templates" | "template" => parse_templates(rest),
        "skills" | "skill" => parse_skills(rest),
        "apps" | "app" => parse_apps(rest),
        "slack-manifest" => parse_slack_manifest(rest),
        "soul" => parse_soul(rest, default_bot),
        "memory" => parse_memory(rest, default_bot),
        "tidy" => parse_tidy(rest, default_bot),
        "items" | "item" => parse_items(rest, default_bot),
        "suggest" => parse_suggest(rest, default_bot),
        "suggestions" | "suggestion" => parse_suggestions(rest, default_bot),
        "up-next" => parse_up_next(rest, default_bot),
        "schedules" | "schedule" => parse_schedules(rest, default_bot),
        "sessions" | "session" => parse_sessions(rest, default_bot),
        "reply" => parse_reply(rest, default_bot),
        "offer" => parse_offer(rest, default_bot),
        "send" => parse_send(rest, default_bot),
        "groups" | "group" => parse_groups(rest),
        "connectors" | "connector" => parse_connectors(rest, default_bot),
        "tracker-default" => parse_tracker_default(rest),
        "composio" => parse_composio(rest, default_bot),
        "access" => parse_access(rest, default_bot),
        "secret" | "secrets" => parse_secret(rest, default_bot),
        "tools" | "tool" => parse_tools(rest, default_bot),
        "avatar" => parse_avatar(rest, default_bot),
        "export" => parse_export(rest, default_bot),
        "import" => parse_import(rest),
        "import-external" => parse_import_external(rest),
        other => Err(CliError::usage(format!(
            "unknown bot subcommand '{other}'\n{USAGE}"
        ))),
    }
}

fn take_opt<'a>(
    args: &'a [String],
    idx: &mut usize,
    flag: &str,
) -> Result<Option<&'a str>, CliError> {
    let Some(cur) = args.get(*idx) else {
        return Ok(None);
    };
    if cur == flag {
        *idx = idx.saturating_add(1);
        let val = args
            .get(*idx)
            .ok_or_else(|| CliError::usage(format!("{flag} requires a value")))?;
        return Ok(Some(val.as_str()));
    }
    if let Some(rest) = cur.strip_prefix(flag).and_then(|s| s.strip_prefix('=')) {
        return Ok(Some(rest));
    }
    Ok(None)
}

fn require_bot(
    explicit: Option<String>,
    default_bot: Option<&str>,
    ctx: &str,
) -> Result<String, CliError> {
    explicit
        .or_else(|| default_bot.map(str::to_owned))
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            CliError::usage(format!(
                "{ctx} requires <bot> (or set KRILL_BOT_ID / --bot <bot>)"
            ))
        })
}

fn parse_bool_flag(raw: &str, flag: &str) -> Result<bool, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" | "enable" | "enabled" => Ok(true),
        "false" | "0" | "off" | "no" | "disable" | "disabled" => Ok(false),
        other => Err(CliError::usage(format!(
            "invalid boolean value '{other}' for {flag} (expected on|off|true|false)"
        ))),
    }
}

fn parse_permission(raw: &str) -> Result<Permission, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "manual" | "default" => Ok(Permission::Manual),
        "yolo" | "bypass" | "auto" => Ok(Permission::Yolo),
        other => Err(CliError::usage(format!(
            "invalid permission '{other}' (expected manual|yolo)"
        ))),
    }
}

pub(crate) fn parse_item_status(raw: &str) -> Result<BotItemStatus, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "planned" => Ok(BotItemStatus::Planned),
        "working" | "in_progress" | "in-progress" => Ok(BotItemStatus::Working),
        "needs_you" | "needs-you" | "waiting" => Ok(BotItemStatus::NeedsYou),
        "done" | "completed" => Ok(BotItemStatus::Done),
        "canceled" | "cancelled" => Ok(BotItemStatus::Canceled),
        other => Err(CliError::usage(format!(
            "invalid item status '{other}' (expected planned|working|needs_you|done|canceled)"
        ))),
    }
}

fn parse_template_category(raw: &str) -> Result<BotTemplateCategory, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "coding" => Ok(BotTemplateCategory::Coding),
        "releases" | "release" => Ok(BotTemplateCategory::Releases),
        "support" => Ok(BotTemplateCategory::Support),
        "planning" => Ok(BotTemplateCategory::Planning),
        "ops" => Ok(BotTemplateCategory::Ops),
        "personal" => Ok(BotTemplateCategory::Personal),
        other => Err(CliError::usage(format!(
            "invalid template category '{other}' (expected coding|releases|support|planning|ops|personal)"
        ))),
    }
}

fn parse_mcp_category(raw: &str) -> Result<BotMcpAppCategory, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "coding" => Ok(BotMcpAppCategory::Coding),
        "chat" => Ok(BotMcpAppCategory::Chat),
        "monitoring" => Ok(BotMcpAppCategory::Monitoring),
        "docs" => Ok(BotMcpAppCategory::Docs),
        "mail" => Ok(BotMcpAppCategory::Mail),
        "sales" => Ok(BotMcpAppCategory::Sales),
        "design" => Ok(BotMcpAppCategory::Design),
        other => Err(CliError::usage(format!(
            "invalid app category '{other}' (expected coding|chat|monitoring|docs|mail|sales|design)"
        ))),
    }
}

pub(crate) fn parse_external_source(raw: &str) -> Result<ExternalSource, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "telegram" => Ok(ExternalSource::Telegram),
        "slack" => Ok(ExternalSource::Slack),
        "discord" => Ok(ExternalSource::Discord),
        "google" => Ok(ExternalSource::Google),
        "github" => Ok(ExternalSource::GitHub),
        "linear" => Ok(ExternalSource::Linear),
        "composio" => Ok(ExternalSource::Composio),
        other => Err(CliError::usage(format!(
            "invalid connector source '{other}' (expected telegram|slack|discord|google|github|linear|composio)"
        ))),
    }
}

fn parse_acts_for(raw: &str) -> Result<ActsFor, CliError> {
    let trimmed = raw.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "owner" => Ok(ActsFor::Owner),
        "anyone" => Ok(ActsFor::Anyone),
        _ => {
            if let Some(member_id) = trimmed
                .strip_prefix("member:")
                .or_else(|| trimmed.strip_prefix("MEMBER:"))
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Ok(ActsFor::Member(member_id.to_owned()))
            } else {
                Err(CliError::usage(format!(
                    "invalid --acts-for value '{trimmed}' (expected owner|anyone|member:<id>)"
                )))
            }
        }
    }
}

fn parse_csv_strings(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_csv_i64(raw: &str, flag: &str) -> Result<Vec<i64>, CliError> {
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let parsed = part
            .parse::<i64>()
            .map_err(|_| CliError::usage(format!("invalid integer ID '{part}' for {flag}")))?;
        out.push(parsed);
    }
    Ok(out)
}

fn parse_link_pair(kind: &str, target: &str) -> Result<BotItemLink, CliError> {
    let target = target.trim();
    if target.is_empty() {
        return Err(CliError::usage("link target must not be empty"));
    }
    match kind.trim().to_ascii_lowercase().as_str() {
        "dispatch" => Ok(BotItemLink::Dispatch(target.to_owned())),
        "pr" => Ok(BotItemLink::Pr(target.to_owned())),
        "url" => Ok(BotItemLink::Url(target.to_owned())),
        "file" => Ok(BotItemLink::File(target.to_owned())),
        other => Err(CliError::usage(format!(
            "invalid link kind '{other}' (expected dispatch|pr|url|file)"
        ))),
    }
}

fn parse_link_spec(raw: &str) -> Result<BotItemLink, CliError> {
    let Some((kind, target)) = raw.split_once(':') else {
        return Err(CliError::usage(format!(
            "invalid --link '{raw}' (expected <dispatch|pr|url|file>:<target>)"
        )));
    };
    parse_link_pair(kind, target)
}

fn parse_list(args: &[String]) -> Result<BotCommand, CliError> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => {
                return Err(CliError::usage(format!(
                    "unexpected argument '{other}' for ls"
                )));
            }
        }
    }
    Ok(BotCommand::List { json })
}

fn parse_new(args: &[String]) -> Result<BotCommand, CliError> {
    let mut name: Option<String> = None;
    let mut template_id: Option<String> = None;
    let mut agent_id: Option<String> = None;
    let mut model: Option<String> = None;
    let mut effort: Option<String> = None;
    let mut permission_mode: Option<Permission> = None;
    let mut description: Option<String> = None;
    let mut emoji: Option<String> = None;
    let mut avatar: Option<String> = None;
    let mut avatar_color: Option<String> = None;
    let mut skills: Vec<String> = Vec::new();
    let mut folder: Option<String> = None;
    let mut slug: Option<String> = None;
    let mut soul: Option<TextInputSource> = None;
    let mut json = false;

    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--name")? {
            name = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--template")? {
            template_id = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--agent")? {
            agent_id = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--model")? {
            model = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--effort")? {
            effort = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--permission")? {
            permission_mode = Some(parse_permission(val)?);
        } else if let Some(val) = take_opt(args, &mut idx, "--description")? {
            description = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--emoji")? {
            emoji = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--avatar")? {
            avatar = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--avatar-color")? {
            let trimmed = val.trim();
            if trimmed.is_empty() {
                return Err(CliError::usage("--avatar-color must not be empty"));
            }
            avatar_color = Some(trimmed.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--skills")? {
            skills.extend(parse_csv_strings(val));
        } else if let Some(val) = take_opt(args, &mut idx, "--skill")? {
            let trimmed = val.trim();
            if trimmed.is_empty() {
                return Err(CliError::usage("--skill must not be empty"));
            }
            skills.push(trimmed.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--folder")? {
            folder = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--workspace")? {
            folder = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--slug")? {
            validate_bot_slug(val).map_err(|e| CliError::usage(e.to_string()))?;
            slug = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--soul")? {
            if soul.is_some() {
                return Err(CliError::usage("specify only one of --soul or --soul-file"));
            }
            soul = Some(TextInputSource::Inline(val.to_owned()));
        } else if let Some(val) = take_opt(args, &mut idx, "--soul-file")? {
            if soul.is_some() {
                return Err(CliError::usage("specify only one of --soul or --soul-file"));
            }
            soul = Some(TextInputSource::File(val.to_owned()));
        } else if !arg.starts_with('-') && name.is_none() {
            name = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for new"
            )));
        }
        idx = idx.saturating_add(1);
    }

    if name.is_none() && template_id.is_none() {
        return Err(CliError::usage("new requires <name> (or --template <id>)"));
    }

    Ok(BotCommand::New(NewBotArgs {
        name,
        template_id,
        agent_id: agent_id.unwrap_or_else(|| "claude".to_owned()),
        model,
        effort,
        permission_mode,
        description,
        emoji,
        avatar,
        avatar_color,
        skills,
        folder,
        slug,
        soul,
        json,
    }))
}

fn parse_show(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut bot: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if !arg.starts_with('-') && bot.is_none() {
            bot = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for show"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::Show {
        bot: require_bot(bot, default_bot, "show")?,
        json,
    })
}

fn parse_update(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut out = UpdateBotArgs::default();
    let mut bot: Option<String> = None;
    let mut any_field = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            out.json = true;
        } else if arg == "--clear-model" {
            out.model = FieldPatch::Clear;
            any_field = true;
        } else if arg == "--clear-effort" {
            out.effort = FieldPatch::Clear;
            any_field = true;
        } else if arg == "--clear-emoji" {
            out.emoji = FieldPatch::Clear;
            any_field = true;
        } else if arg == "--clear-avatar" {
            out.avatar = FieldPatch::Clear;
            any_field = true;
        } else if arg == "--clear-avatar-color" {
            out.avatar_color = FieldPatch::Clear;
            any_field = true;
        } else if arg == "--clear-workspace" || arg == "--clear-folder" {
            out.workspace = FieldPatch::Clear;
            any_field = true;
        } else if arg == "--pause" {
            out.paused = Some(true);
            any_field = true;
        } else if arg == "--resume" || arg == "--unpause" {
            out.paused = Some(false);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--name")? {
            out.name = Some(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--description")? {
            out.description = Some(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--agent")? {
            out.agent_id = Some(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--model")? {
            out.model = FieldPatch::Set(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--effort")? {
            out.effort = FieldPatch::Set(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--emoji")? {
            out.emoji = FieldPatch::Set(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--avatar")? {
            out.avatar = FieldPatch::Set(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--avatar-color")? {
            let trimmed = val.trim();
            if trimmed.is_empty() {
                return Err(CliError::usage("--avatar-color must not be empty"));
            }
            out.avatar_color = FieldPatch::Set(trimmed.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--first-run-completed")? {
            out.first_run_completed = Some(parse_bool_flag(val, "--first-run-completed")?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--folder")? {
            out.workspace = FieldPatch::Set(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--workspace")? {
            out.workspace = FieldPatch::Set(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--permission")? {
            out.permission_mode = Some(parse_permission(val)?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--paused")? {
            out.paused = Some(parse_bool_flag(val, "--paused")?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--look-around")? {
            out.look_around_enabled = Some(parse_bool_flag(val, "--look-around")?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--look-around-interval")? {
            let secs = val
                .parse::<u64>()
                .map_err(|_| CliError::usage("invalid --look-around-interval seconds"))?;
            out.look_around_interval_secs = Some(secs);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--look-around-max")? {
            let max = val
                .parse::<u32>()
                .map_err(|_| CliError::usage("invalid --look-around-max value"))?;
            out.look_around_max_suggestions = Some(max);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--look-around-weekdays")? {
            out.look_around_weekdays_only = Some(parse_bool_flag(val, "--look-around-weekdays")?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--look-around-focus")? {
            out.look_around_focus = Some(val.to_owned());
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--memory-tidy")? {
            out.memory_tidy_enabled = Some(parse_bool_flag(val, "--memory-tidy")?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--memory-tidy-interval")? {
            let secs = val
                .parse::<u64>()
                .map_err(|_| CliError::usage("invalid --memory-tidy-interval seconds"))?;
            out.memory_tidy_interval_secs = Some(secs);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--notify-permission")? {
            out.notify_permission = Some(parse_bool_flag(val, "--notify-permission")?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--notify-complete")? {
            out.notify_complete = Some(parse_bool_flag(val, "--notify-complete")?);
            any_field = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--notify-error")? {
            out.notify_error = Some(parse_bool_flag(val, "--notify-error")?);
            any_field = true;
        } else if !arg.starts_with('-') && bot.is_none() {
            bot = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for update"
            )));
        }
        idx = idx.saturating_add(1);
    }

    if !any_field {
        return Err(CliError::usage("update requires at least one field flag"));
    }
    out.bot = require_bot(bot, default_bot, "update")?;
    Ok(BotCommand::Update(out))
}

fn parse_remove(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut bot: Option<String> = None;
    let mut delete_home = false;
    let mut keep_home = false;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--delete-home" {
            delete_home = true;
        } else if arg == "--keep-home" {
            keep_home = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if !arg.starts_with('-') && bot.is_none() {
            bot = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for rm"
            )));
        }
        idx = idx.saturating_add(1);
    }
    if delete_home && keep_home {
        return Err(CliError::usage(
            "--delete-home and --keep-home are mutually exclusive",
        ));
    }
    Ok(BotCommand::Remove {
        bot: require_bot(bot, default_bot, "rm")?,
        delete_home,
        json,
    })
}

fn parse_start(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut bot: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--prompt")? {
            prompt = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--cwd")? {
            cwd = Some(val.to_owned());
        } else if !arg.starts_with('-') && bot.is_none() {
            bot = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for start"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::Start {
        bot: require_bot(bot, default_bot, "start")?,
        prompt,
        cwd,
        json,
    })
}

fn parse_templates(args: &[String]) -> Result<BotCommand, CliError> {
    let mut category: Option<BotTemplateCategory> = None;
    let mut show_id: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--category")? {
            category = Some(parse_template_category(val)?);
        } else if let Some(val) = take_opt(args, &mut idx, "--show")? {
            show_id = Some(val.to_owned());
        } else if !arg.starts_with('-') && show_id.is_none() {
            show_id = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for templates"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::Templates {
        category,
        show_id,
        json,
    })
}

fn parse_skills(args: &[String]) -> Result<BotCommand, CliError> {
    let mut show_name: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--show")? {
            show_name = Some(val.to_owned());
        } else if !arg.starts_with('-') && show_name.is_none() {
            show_name = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for skills"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::Skills { show_name, json })
}

fn parse_apps(args: &[String]) -> Result<BotCommand, CliError> {
    let mut kind = AppsKind::All;
    let mut category: Option<BotMcpAppCategory> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--kind")? {
            kind = match val.trim().to_ascii_lowercase().as_str() {
                "mcp" => AppsKind::Mcp,
                "composio" => AppsKind::Composio,
                "all" => AppsKind::All,
                other => {
                    return Err(CliError::usage(format!(
                        "invalid --kind '{other}' (expected mcp|composio|all)"
                    )));
                }
            };
        } else if let Some(val) = take_opt(args, &mut idx, "--category")? {
            category = Some(parse_mcp_category(val)?);
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for apps"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::Apps {
        kind,
        category,
        json,
    })
}

fn parse_slack_manifest(args: &[String]) -> Result<BotCommand, CliError> {
    let mut bot_name: Option<String> = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if !arg.starts_with('-') && bot_name.is_none() {
            bot_name = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for slack-manifest"
            )));
        }
    }
    let bot_name = bot_name.ok_or_else(|| CliError::usage("slack-manifest requires <bot-name>"))?;
    Ok(BotCommand::SlackManifest { bot_name, json })
}

type SplitBotAction<'a> = (Option<String>, bool, Option<&'a str>, &'a [String]);

/// Disambiguates `[<bot>] <action> ...` vs `<action> ...`, skipping leading `--bot`/`--json` flags
/// and preserving bot names that coincide with action keywords.
fn split_bot_and_action_skipping_flags<'a>(
    args: &'a [String],
    actions: &[&str],
    zero_pos_actions: &[&str],
) -> Result<SplitBotAction<'a>, CliError> {
    let mut bot: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(cur) = args.get(idx) {
        if cur == "--json" {
            json = true;
            idx = idx.saturating_add(1);
        } else if cur == "--bot" {
            idx = idx.saturating_add(1);
            let val = args
                .get(idx)
                .ok_or_else(|| CliError::usage("--bot requires a value"))?;
            bot = Some(val.clone());
            idx = idx.saturating_add(1);
        } else if let Some(val) = cur.strip_prefix("--bot=") {
            bot = Some(val.to_owned());
            idx = idx.saturating_add(1);
        } else {
            break;
        }
    }

    let rest = args.get(idx..).unwrap_or(&[]);
    let Some(first) = rest.first() else {
        return Ok((bot, json, None, rest));
    };
    if first.starts_with('-') {
        return Ok((bot, json, None, rest));
    }

    if bot.is_some() {
        if actions.contains(&first.as_str()) {
            return Ok((
                bot,
                json,
                Some(first.as_str()),
                rest.get(1..).unwrap_or(&[]),
            ));
        }
        return Ok((bot, json, None, rest));
    }

    if let Some(second) = rest.get(1)
        && !second.starts_with('-')
        && actions.contains(&second.as_str())
        && (!actions.contains(&first.as_str()) || zero_pos_actions.contains(&first.as_str()))
    {
        return Ok((
            Some(first.clone()),
            json,
            Some(second.as_str()),
            rest.get(2..).unwrap_or(&[]),
        ));
    }

    if actions.contains(&first.as_str()) {
        return Ok((
            None,
            json,
            Some(first.as_str()),
            rest.get(1..).unwrap_or(&[]),
        ));
    }

    Ok((
        Some(first.clone()),
        json,
        None,
        rest.get(1..).unwrap_or(&[]),
    ))
}

fn parse_soul(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &["get", "show", "set"],
        &["get", "show", "set"],
    )?;
    let action = action.unwrap_or("get");
    let mut source: Option<TextInputSource> = None;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--stdin" && action == "set" {
            if source.is_some() {
                return Err(CliError::usage(
                    "specify only one of --text, --file, or --stdin",
                ));
            }
            source = Some(TextInputSource::Stdin);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if action == "set"
            && let Some(val) = take_opt(tail, &mut idx, "--text")?
        {
            if source.is_some() {
                return Err(CliError::usage(
                    "specify only one of --text, --file, or --stdin",
                ));
            }
            source = Some(TextInputSource::Inline(val.to_owned()));
        } else if action == "set"
            && let Some(val) = take_opt(tail, &mut idx, "--file")?
        {
            if source.is_some() {
                return Err(CliError::usage(
                    "specify only one of --text, --file, or --stdin",
                ));
            }
            source = Some(TextInputSource::File(val.to_owned()));
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for soul {action}"
            )));
        }
        idx = idx.saturating_add(1);
    }
    let bot = require_bot(bot, default_bot, "soul")?;
    match action {
        "get" | "show" => Ok(BotCommand::SoulGet { bot, json }),
        "set" => {
            let source = source.ok_or_else(|| {
                CliError::usage("soul set requires --text <str>, --file <path>, or --stdin")
            })?;
            Ok(BotCommand::SoulSet { bot, source, json })
        }
        _ => Err(CliError::usage(help_text(Some("soul")))),
    }
}

fn parse_text_source_flag(
    arg: &str,
    tail: &[String],
    idx: &mut usize,
    target: &mut Option<TextInputSource>,
) -> Result<bool, CliError> {
    if arg == "--stdin" {
        if target.is_some() {
            return Err(CliError::usage(
                "specify only one of --text, --file, or --stdin",
            ));
        }
        *target = Some(TextInputSource::Stdin);
        return Ok(true);
    }
    if let Some(val) = take_opt(tail, idx, "--text")? {
        if target.is_some() {
            return Err(CliError::usage(
                "specify only one of --text, --file, or --stdin",
            ));
        }
        *target = Some(TextInputSource::Inline(val.to_owned()));
        return Ok(true);
    }
    if let Some(val) = take_opt(tail, idx, "--file")? {
        if target.is_some() {
            return Err(CliError::usage(
                "specify only one of --text, --file, or --stdin",
            ));
        }
        *target = Some(TextInputSource::File(val.to_owned()));
        return Ok(true);
    }
    Ok(false)
}

fn parse_memory(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &[
            "ls", "list", "show", "add", "forget", "tidy", "set", "history", "edit", "undo",
            "topics", "topic",
        ],
        &["ls", "list", "show", "set", "history", "edit", "topics"],
    )?;
    let action = action.unwrap_or("ls");
    let mut fact: Option<String> = None;
    let mut date: Option<String> = None;
    let mut fact_source: Option<String> = None;
    let mut text_source: Option<TextInputSource> = None;
    let mut allow_deletions = false;
    let mut limit: Option<u32> = None;
    let mut old_fact: Option<String> = None;
    let mut new_fact: Option<String> = None;
    let mut entry_id: Option<String> = None;
    let mut topic_name: Option<String> = None;
    let mut topic_set_flag = false;
    let mut tidy_sub: Option<String> = None;
    let mut tidy_id: Option<String> = None;
    let mut idx = 0usize;

    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--allow-deletions" && action == "set" {
            allow_deletions = true;
        } else if arg == "--set" && action == "topic" {
            topic_set_flag = true;
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if matches!(action, "set" | "topic")
            && parse_text_source_flag(arg, tail, &mut idx, &mut text_source)?
        {
        } else if matches!(action, "add" | "forget")
            && let Some(val) = take_opt(tail, &mut idx, "--fact")?
        {
            fact = Some(val.to_owned());
        } else if action == "add"
            && let Some(val) = take_opt(tail, &mut idx, "--date")?
        {
            date = Some(val.to_owned());
        } else if action == "add"
            && let Some(val) = take_opt(tail, &mut idx, "--source")?
        {
            fact_source = Some(val.to_owned());
        } else if action == "history"
            && let Some(val) = take_opt(tail, &mut idx, "--limit")?
        {
            let n = val
                .parse::<u32>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| CliError::usage("--limit must be a positive integer"))?;
            limit = Some(n);
        } else if action == "edit"
            && let Some(val) = take_opt(tail, &mut idx, "--old")?
        {
            old_fact = Some(val.to_owned());
        } else if action == "edit"
            && let Some(val) = take_opt(tail, &mut idx, "--new")?
        {
            new_fact = Some(val.to_owned());
        } else if action == "undo"
            && let Some(val) = take_opt(tail, &mut idx, "--entry-id")?
        {
            entry_id = Some(val.to_owned());
        } else if action == "topic"
            && let Some(val) = take_opt(tail, &mut idx, "--name")?
        {
            topic_name = Some(val.to_owned());
        } else if action == "tidy"
            && let Some(val) = take_opt(tail, &mut idx, "--tidy-id")?
        {
            tidy_id = Some(val.to_owned());
        } else if matches!(action, "add" | "forget") && !arg.starts_with('-') && fact.is_none() {
            fact = Some(arg.clone());
        } else if action == "edit" && !arg.starts_with('-') && old_fact.is_none() {
            old_fact = Some(arg.clone());
        } else if action == "edit" && !arg.starts_with('-') && new_fact.is_none() {
            new_fact = Some(arg.clone());
        } else if action == "undo" && !arg.starts_with('-') && entry_id.is_none() {
            entry_id = Some(arg.clone());
        } else if action == "topic" && !arg.starts_with('-') && topic_name.is_none() {
            topic_name = Some(arg.clone());
        } else if action == "tidy" && !arg.starts_with('-') && tidy_sub.is_none() {
            tidy_sub = Some(arg.clone());
        } else if action == "tidy"
            && tidy_sub.as_deref() == Some("undo")
            && !arg.starts_with('-')
            && tidy_id.is_none()
        {
            tidy_id = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for memory {action}"
            )));
        }
        idx = idx.saturating_add(1);
    }

    let bot = require_bot(bot, default_bot, "memory")?;
    match action {
        "ls" | "list" | "show" => Ok(BotCommand::MemoryList { bot, json }),
        "add" => {
            let fact = fact
                .filter(|f| !f.trim().is_empty())
                .ok_or_else(|| CliError::usage("memory add requires <fact>"))?;
            Ok(BotCommand::MemoryAdd {
                bot,
                fact,
                date,
                source: fact_source,
                json,
            })
        }
        "forget" => {
            let fact = fact
                .filter(|f| !f.trim().is_empty())
                .ok_or_else(|| CliError::usage("memory forget requires --fact <fact>"))?;
            Ok(BotCommand::MemoryForget { bot, fact, json })
        }
        "history" => Ok(BotCommand::MemoryHistory { bot, limit, json }),
        "edit" => {
            let old_fact = old_fact
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("memory edit requires --old <fact>"))?;
            let new_fact = new_fact
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("memory edit requires --new <fact>"))?;
            Ok(BotCommand::MemoryEdit {
                bot,
                old_fact,
                new_fact,
                json,
            })
        }
        "undo" => {
            let entry_id = entry_id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("memory undo requires <entry-id>"))?;
            Ok(BotCommand::MemoryUndo {
                bot,
                entry_id,
                json,
            })
        }
        "topics" => Ok(BotCommand::MemoryTopics { bot, json }),
        "topic" => {
            let topic = topic_name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("memory topic requires <name>"))?;
            if topic_set_flag || text_source.is_some() {
                let source = text_source.ok_or_else(|| {
                    CliError::usage(
                        "memory topic --set requires --text <str>, --file <path>, or --stdin",
                    )
                })?;
                Ok(BotCommand::MemoryTopicSet {
                    bot,
                    topic,
                    source,
                    json,
                })
            } else {
                Ok(BotCommand::MemoryTopicGet { bot, topic, json })
            }
        }
        "tidy" => match tidy_sub.as_deref() {
            None | Some("run") => Ok(BotCommand::MemoryTidy { bot, json }),
            Some("status") => Ok(BotCommand::MemoryTidyStatus { bot, json }),
            Some("undo") => Ok(BotCommand::MemoryTidyUndo { bot, tidy_id, json }),
            Some(other) => Err(CliError::usage(format!(
                "unknown memory tidy subcommand '{other}' (expected status|undo)"
            ))),
        },
        "set" => {
            let source = text_source.ok_or_else(|| {
                CliError::usage("memory set requires --text <str>, --file <path>, or --stdin")
            })?;
            Ok(BotCommand::MemorySet {
                bot,
                source,
                allow_deletions,
                json,
            })
        }
        _ => Err(CliError::usage(help_text(Some("memory")))),
    }
}

fn parse_tidy(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) =
        split_bot_and_action_skipping_flags(args, &["run", "status", "undo"], &["run", "status"])?;
    let action = action.unwrap_or("run");
    let mut tidy_id: Option<String> = None;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if action == "undo" && !arg.starts_with('-') && tidy_id.is_none() {
            tidy_id = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for tidy {action}"
            )));
        }
        idx = idx.saturating_add(1);
    }
    let bot = require_bot(bot, default_bot, "tidy")?;
    match action {
        "run" => Ok(BotCommand::MemoryTidy { bot, json }),
        "status" => Ok(BotCommand::MemoryTidyStatus { bot, json }),
        "undo" => Ok(BotCommand::MemoryTidyUndo { bot, tidy_id, json }),
        _ => Err(CliError::usage(help_text(Some("tidy")))),
    }
}

fn finalize_decision(
    decision: Option<BotItemDecision>,
    out: &mut Vec<BotItemDecision>,
) -> Result<(), CliError> {
    if let Some(d) = decision {
        if let Some(rec) = d.recommended
            && rec >= d.options.len()
        {
            return Err(CliError::usage(format!(
                "--recommended {rec} is out of range for {} option(s)",
                d.options.len()
            )));
        }
        out.push(d);
    }
    Ok(())
}

fn parse_items(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &[
            "ls", "list", "show", "add", "create", "update", "answer", "comment", "snooze",
            "unsnooze", "link", "done", "cancel",
        ],
        &["ls", "list"],
    )?;
    let action = action.unwrap_or("ls");

    match action {
        "ls" | "list" => {
            let mut status: Option<BotItemStatus> = None;
            let mut all = false;
            let mut include_snoozed = false;
            let mut limit: Option<u32> = None;
            let mut cursor: Option<String> = None;
            let mut page: Option<u32> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if arg == "--all" || arg == "--include-closed" {
                    all = true;
                } else if arg == "--include-snoozed" {
                    include_snoozed = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--status")? {
                    status = Some(parse_item_status(val)?);
                } else if let Some(val) = take_opt(tail, &mut idx, "--limit")? {
                    let n = val
                        .parse::<u32>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| CliError::usage("--limit must be a positive integer"))?;
                    limit = Some(n);
                } else if let Some(val) = take_opt(tail, &mut idx, "--cursor")? {
                    cursor = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--page")? {
                    let p = val
                        .parse::<u32>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| CliError::usage("--page must be >= 1"))?;
                    page = Some(p);
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items ls"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            if cursor.is_some() && page.is_some() {
                return Err(CliError::usage(
                    "--cursor and --page are mutually exclusive",
                ));
            }
            Ok(BotCommand::ItemsList {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                status,
                all,
                include_snoozed,
                limit,
                cursor,
                page,
                json,
            })
        }
        "show" => {
            let mut item_id: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && item_id.is_none() {
                    item_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items show"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let item_id = item_id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("items show requires <item-id>"))?;
            Ok(BotCommand::ItemsShow {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                item_id,
                json,
            })
        }
        "add" | "create" => {
            let mut title: Option<String> = None;
            let mut body: Option<String> = None;
            let mut done_means: Option<String> = None;
            let mut status: Option<BotItemStatus> = None;
            let mut links = Vec::new();
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--title")? {
                    title = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--body")? {
                    body = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--description")? {
                    body = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--done-means")? {
                    done_means = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--status")? {
                    status = Some(parse_item_status(val)?);
                } else if let Some(val) = take_opt(tail, &mut idx, "--link")? {
                    links.push(parse_link_spec(val)?);
                } else if !arg.starts_with('-') && title.is_none() {
                    title = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items add"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let title = title
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| CliError::usage("items add requires <title> or --title <title>"))?;
            Ok(BotCommand::ItemsAdd(ItemsAddArgs {
                bot: require_bot(bot, default_bot, "items add")?,
                title,
                body,
                done_means,
                status,
                links,
                json,
            }))
        }
        "update" => {
            let mut out = ItemsUpdateArgs {
                json,
                ..ItemsUpdateArgs::default()
            };
            let mut item_id: Option<String> = None;
            let mut any_field = false;
            let mut current_decision: Option<BotItemDecision> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    out.json = true;
                } else if arg == "--unsnooze" {
                    out.unsnooze = true;
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--status")? {
                    out.status = Some(parse_item_status(val)?);
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--title")? {
                    out.title = Some(val.to_owned());
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--body")? {
                    out.body = Some(val.to_owned());
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--description")? {
                    out.body = Some(val.to_owned());
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--done-means")? {
                    out.done_means = Some(val.to_owned());
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--comment")? {
                    out.comment = Some(val.to_owned());
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--close-note")? {
                    out.close_note = Some(val.to_owned());
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--snooze")? {
                    out.snooze = Some(val.to_owned());
                    any_field = true;
                } else if let Some(val) =
                    take_opt(tail, &mut idx, "--ask")?.or(take_opt(tail, &mut idx, "--question")?)
                {
                    let trimmed = val.trim();
                    if trimmed.is_empty() {
                        return Err(CliError::usage(
                            "--ask/--question requires a non-empty question",
                        ));
                    }
                    finalize_decision(current_decision.take(), &mut out.decisions)?;
                    current_decision = Some(BotItemDecision {
                        question: trimmed.to_owned(),
                        options: Vec::new(),
                        recommended: None,
                    });
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--option")? {
                    let Some(ref mut d) = current_decision else {
                        return Err(CliError::usage(
                            "--option requires a preceding --ask or --question",
                        ));
                    };
                    let trimmed = val.trim();
                    if trimmed.is_empty() {
                        return Err(CliError::usage("--option requires a non-empty value"));
                    }
                    d.options.push(trimmed.to_owned());
                    any_field = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--recommended")? {
                    let Some(ref mut d) = current_decision else {
                        return Err(CliError::usage(
                            "--recommended requires a preceding --ask or --question",
                        ));
                    };
                    let rec = val
                        .parse::<usize>()
                        .map_err(|_| CliError::usage("--recommended must be a 0-based index"))?;
                    d.recommended = Some(rec);
                    any_field = true;
                } else if !arg.starts_with('-') && item_id.is_none() {
                    item_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items update"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            finalize_decision(current_decision.take(), &mut out.decisions)?;
            if out.snooze.is_some() && out.unsnooze {
                return Err(CliError::usage(
                    "--snooze and --unsnooze are mutually exclusive",
                ));
            }
            if !any_field {
                return Err(CliError::usage(
                    "items update requires at least one field flag",
                ));
            }
            out.item_id =
                item_id.ok_or_else(|| CliError::usage("items update requires <item-id>"))?;
            out.bot = bot.or_else(|| default_bot.map(str::to_owned));
            Ok(BotCommand::ItemsUpdate(out))
        }
        "answer" => {
            let mut item_id: Option<String> = None;
            let mut text: Option<String> = None;
            let mut option_index: Option<u32> = None;
            let mut decision_id: Option<String> = None;
            let mut request_id: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--text")? {
                    text = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--option")?.or(take_opt(
                    tail,
                    &mut idx,
                    "--option-index",
                )?) {
                    let n = val
                        .parse::<u32>()
                        .map_err(|_| CliError::usage("--option must be a non-negative integer"))?;
                    option_index = Some(n);
                } else if let Some(val) = take_opt(tail, &mut idx, "--decision-id")? {
                    decision_id = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--request-id")? {
                    request_id = Some(val.to_owned());
                } else if !arg.starts_with('-') && item_id.is_none() {
                    item_id = Some(arg.clone());
                } else if !arg.starts_with('-') && text.is_none() {
                    text = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items answer"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let item_id =
                item_id.ok_or_else(|| CliError::usage("items answer requires <item-id>"))?;
            if text.as_ref().is_none_or(|t| t.trim().is_empty()) && option_index.is_none() {
                return Err(CliError::usage(
                    "items answer requires --text <str> (or <text>) or --option <idx>",
                ));
            }
            Ok(BotCommand::ItemsAnswer {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                item_id,
                text,
                option_index,
                decision_id,
                request_id,
                json,
            })
        }
        "comment" => {
            let mut item_id: Option<String> = None;
            let mut text: Option<String> = None;
            let mut author: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--text")? {
                    text = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--author")? {
                    author = Some(val.to_owned());
                } else if !arg.starts_with('-') && item_id.is_none() {
                    item_id = Some(arg.clone());
                } else if !arg.starts_with('-') && text.is_none() {
                    text = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items comment"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let item_id = item_id
                .ok_or_else(|| CliError::usage("items comment requires <item-id> <text>"))?;
            let text = text
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| CliError::usage("items comment requires <text> or --text <text>"))?;
            Ok(BotCommand::ItemsComment {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                item_id,
                author,
                text,
                json,
            })
        }
        "snooze" => {
            let mut item_id: Option<String> = None;
            let mut until: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && item_id.is_none() {
                    item_id = Some(arg.clone());
                } else if !arg.starts_with('-') && until.is_none() {
                    until = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items snooze"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let item_id = item_id
                .ok_or_else(|| CliError::usage("items snooze requires <item-id> <until>"))?;
            let until = until.ok_or_else(|| {
                CliError::usage("items snooze requires <until> (e.g. 2h, 1d, 2026-11-25)")
            })?;
            Ok(BotCommand::ItemsSnooze {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                item_id,
                until,
                json,
            })
        }
        "unsnooze" => {
            let mut item_id: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && item_id.is_none() {
                    item_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items unsnooze"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let item_id =
                item_id.ok_or_else(|| CliError::usage("items unsnooze requires <item-id>"))?;
            Ok(BotCommand::ItemsUnsnooze {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                item_id,
                json,
            })
        }
        "link" => {
            let mut positionals: Vec<String> = Vec::new();
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') {
                    positionals.push(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items link"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let (item_id, link) = match positionals.as_slice() {
                [item_id, spec] => (item_id.clone(), parse_link_spec(spec)?),
                [item_id, kind, target] => (item_id.clone(), parse_link_pair(kind, target)?),
                _ => {
                    return Err(CliError::usage(
                        "items link requires <item-id> (<dispatch|pr|url|file> <target> | <kind>:<target>)",
                    ));
                }
            };
            Ok(BotCommand::ItemsLink {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                item_id,
                link,
                json,
            })
        }
        "done" | "cancel" => {
            let status = if action == "done" {
                BotItemStatus::Done
            } else {
                BotItemStatus::Canceled
            };
            let mut item_id: Option<String> = None;
            let mut note: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--note")? {
                    note = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--close-note")? {
                    note = Some(val.to_owned());
                } else if !arg.starts_with('-') && item_id.is_none() {
                    item_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for items {action}"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let item_id = item_id
                .ok_or_else(|| CliError::usage(format!("items {action} requires <item-id>")))?;
            Ok(BotCommand::ItemsClose {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                item_id,
                status,
                note,
                json,
            })
        }
        _ => Err(CliError::usage(help_text(Some("items")))),
    }
}

fn parse_suggest(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut bot: Option<String> = None;
    let mut run_flag = false;
    let mut title: Option<String> = None;
    let mut why: Option<String> = None;
    let mut ask: Option<String> = None;
    let mut link: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--run" {
            run_flag = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--title")? {
            title = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--why")? {
            why = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--ask")? {
            ask = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--link")? {
            link = Some(val.to_owned());
        } else if !arg.starts_with('-') && bot.is_none() {
            bot = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for suggest"
            )));
        }
        idx = idx.saturating_add(1);
    }

    let bot = require_bot(bot, default_bot, "suggest")?;
    let has_proposal = title.is_some() || why.is_some() || ask.is_some() || link.is_some();
    if run_flag && has_proposal {
        return Err(CliError::usage(
            "suggest --run cannot be combined with --title/--why/--ask",
        ));
    }
    if has_proposal {
        let title = title
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| CliError::usage("suggest proposal requires --title"))?;
        let why = why.unwrap_or_default();
        let ask = ask.unwrap_or_default();
        Ok(BotCommand::SuggestPropose {
            bot,
            title,
            why,
            ask,
            link,
            json,
        })
    } else {
        Ok(BotCommand::SuggestRun { bot, json })
    }
}

/// `krill bot offer [<bot> | --bot <bot>] --name <n> --purpose <p> [--json]`: a bot proposes a new bot (Orca `bot create --offer`).
fn parse_offer(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut bot: Option<String> = None;
    let mut name: Option<String> = None;
    let mut purpose: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--name")? {
            name = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--purpose")? {
            purpose = Some(val.to_owned());
        } else if !arg.starts_with('-') && bot.is_none() {
            bot = Some(arg.clone());
        } else {
            return Err(CliError::usage(help_text(Some("offer"))));
        }
        idx = idx.saturating_add(1);
    }
    let (Some(name), Some(purpose)) = (name, purpose) else {
        return Err(CliError::usage(help_text(Some("offer"))));
    };
    Ok(BotCommand::Offer {
        bot: require_bot(bot, default_bot, "offer")?,
        name,
        purpose,
        json,
    })
}

fn parse_reply(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut bot: Option<String> = None;
    let mut text: Option<String> = None;
    let mut link_id: Option<String> = None;
    let mut files = Vec::new();
    let mut reaction: Option<String> = None;
    let mut positionals = Vec::new();
    let mut json = false;
    let mut idx = 0usize;

    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--text")? {
            text = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--link")? {
            link_id = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--file")? {
            files.push(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--react")? {
            reaction = Some(val.to_owned());
        } else if !arg.starts_with('-') {
            positionals.push(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for reply"
            )));
        }
        idx = idx.saturating_add(1);
    }

    match (bot.is_some(), text.is_some(), positionals.as_slice()) {
        (_, true, []) => {}
        (false, true, [b]) => bot = Some(b.clone()),
        (true, false, [t]) => text = Some(t.clone()),
        (false, false, [b, t]) => {
            bot = Some(b.clone());
            text = Some(t.clone());
        }
        (false, false, [t]) if default_bot.is_some() => {
            text = Some(t.clone());
        }
        _ => {
            return Err(CliError::usage(
                "reply requires <bot> <text> (or --text <text> with KRILL_BOT_ID)",
            ));
        }
    }

    let bot = require_bot(bot, default_bot, "reply")?;
    let text = text.unwrap_or_default();
    if text.trim().is_empty() && files.is_empty() && reaction.is_none() {
        return Err(CliError::usage(
            "reply requires non-empty text, --file, or --react",
        ));
    }
    Ok(BotCommand::Reply {
        bot,
        text,
        link_id,
        files,
        reaction,
        json,
    })
}

fn parse_send(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut to_bot: Option<String> = None;
    let mut from: Option<String> = None;
    let mut kind: Option<MailKind> = None;
    let mut subject: Option<String> = None;
    let mut body: Option<String> = None;
    let mut thread_id: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;

    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--to")? {
            let trimmed = val.trim();
            let stripped = trimmed.strip_prefix("bot:").unwrap_or(trimmed);
            if stripped.is_empty() {
                return Err(CliError::usage("--to requires bot:<slug>"));
            }
            to_bot = Some(stripped.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--from")? {
            from = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--type")? {
            kind = Some(match val.trim().to_ascii_lowercase().as_str() {
                "status" => MailKind::Status,
                "question" => MailKind::Question,
                "handoff" => MailKind::Handoff,
                other => {
                    return Err(CliError::usage(format!(
                        "invalid --type '{other}' (expected status|question|handoff)"
                    )));
                }
            });
        } else if let Some(val) = take_opt(args, &mut idx, "--subject")? {
            subject = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--body")? {
            body = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--thread")? {
            thread_id = Some(val.to_owned());
        } else if !arg.starts_with('-') && body.is_none() {
            body = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for send"
            )));
        }
        idx = idx.saturating_add(1);
    }

    let to_bot = to_bot.ok_or_else(|| CliError::usage("send requires --to bot:<slug>"))?;
    let body = body
        .filter(|b| !b.trim().is_empty())
        .ok_or_else(|| CliError::usage("send requires <message> or --body <text>"))?;
    let from = from.or_else(|| default_bot.map(|b| format!("bot:{b}")));
    Ok(BotCommand::SendMail {
        from,
        to_bot,
        kind,
        subject,
        body,
        thread_id,
        json,
    })
}

fn parse_reply_mode(raw: &str) -> Result<BotGroupReplyMode, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mentions" | "mention" => Ok(BotGroupReplyMode::Mentions),
        "all" => Ok(BotGroupReplyMode::All),
        "selected" => Ok(BotGroupReplyMode::Selected),
        other => Err(CliError::usage(format!(
            "invalid --reply-mode '{other}' (expected mentions|all|selected)"
        ))),
    }
}

fn parse_groups(args: &[String]) -> Result<BotCommand, CliError> {
    let Some((first, tail)) = args.split_first() else {
        return Ok(BotCommand::GroupsList { json: false });
    };
    if first == "--json" && tail.is_empty() {
        return Ok(BotCommand::GroupsList { json: true });
    }
    match first.as_str() {
        "ls" | "list" => {
            let mut json = false;
            for arg in tail {
                if arg == "--json" {
                    json = true;
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups ls"
                    )));
                }
            }
            Ok(BotCommand::GroupsList { json })
        }
        "create" => {
            let mut name: Option<String> = None;
            let mut members: Vec<String> = Vec::new();
            let mut description = String::new();
            let mut json = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--members")? {
                    members.extend(parse_csv_strings(val));
                } else if let Some(val) = take_opt(tail, &mut idx, "--description")? {
                    val.clone_into(&mut description);
                } else if !arg.starts_with('-') && name.is_none() {
                    name = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups create"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let name = name
                .filter(|n| !n.trim().is_empty())
                .ok_or_else(|| CliError::usage("groups create requires <name>"))?;
            if members.is_empty() {
                return Err(CliError::usage(
                    "groups create requires --members <bot1,bot2>",
                ));
            }
            Ok(BotCommand::GroupsCreate {
                name,
                members,
                description,
                json,
            })
        }
        "update" => {
            let mut group_id: Option<String> = None;
            let mut name: Option<String> = None;
            let mut description: Option<String> = None;
            let mut members: Option<Vec<String>> = None;
            let mut json = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--name")? {
                    name = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--description")? {
                    description = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--members")? {
                    let parsed = parse_csv_strings(val);
                    if parsed.is_empty() {
                        return Err(CliError::usage("--members requires at least one bot"));
                    }
                    members = Some(parsed);
                } else if !arg.starts_with('-') && group_id.is_none() {
                    group_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups update"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let group_id =
                group_id.ok_or_else(|| CliError::usage("groups update requires <group-id>"))?;
            if name.is_none() && description.is_none() && members.is_none() {
                return Err(CliError::usage(
                    "groups update requires at least one of --name, --description, or --members",
                ));
            }
            Ok(BotCommand::GroupsUpdate {
                group_id,
                name,
                description,
                members,
                json,
            })
        }
        "rm" | "remove" | "delete" => {
            let mut group_id: Option<String> = None;
            let mut json = false;
            for arg in tail {
                if arg == "--json" {
                    json = true;
                } else if !arg.starts_with('-') && group_id.is_none() {
                    group_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups rm"
                    )));
                }
            }
            let group_id =
                group_id.ok_or_else(|| CliError::usage("groups rm requires <group-id>"))?;
            Ok(BotCommand::GroupsRemove { group_id, json })
        }
        "reorder" => {
            let mut group_ids: Vec<String> = Vec::new();
            let mut json = false;
            for arg in tail {
                if arg == "--json" {
                    json = true;
                } else if !arg.starts_with('-') {
                    group_ids.extend(parse_csv_strings(arg));
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups reorder"
                    )));
                }
            }
            if group_ids.is_empty() {
                return Err(CliError::usage(
                    "groups reorder requires one or more group IDs",
                ));
            }
            Ok(BotCommand::GroupsReorder { group_ids, json })
        }
        "new-thread" => {
            let mut group_id: Option<String> = None;
            let mut title: Option<String> = None;
            let mut json = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--title")? {
                    title = Some(val.to_owned());
                } else if !arg.starts_with('-') && group_id.is_none() {
                    group_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups new-thread"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let group_id =
                group_id.ok_or_else(|| CliError::usage("groups new-thread requires <group-id>"))?;
            Ok(BotCommand::GroupsNewThread {
                group_id,
                title,
                json,
            })
        }
        "read" => {
            let mut group_id: Option<String> = None;
            let mut thread_id: Option<String> = None;
            let mut limit: Option<u32> = None;
            let mut json = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--thread")? {
                    thread_id = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--limit")? {
                    let n = val
                        .parse::<u32>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| CliError::usage("--limit must be a positive integer"))?;
                    limit = Some(n);
                } else if !arg.starts_with('-') && group_id.is_none() {
                    group_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups read"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let group_id =
                group_id.ok_or_else(|| CliError::usage("groups read requires <group-id>"))?;
            Ok(BotCommand::GroupsRead {
                group_id,
                thread_id,
                limit,
                json,
            })
        }
        "send" => {
            let mut group_id: Option<String> = None;
            let mut thread_id: Option<String> = None;
            let mut sender: Option<String> = None;
            let mut text: Option<String> = None;
            let mut reply_mode: Option<BotGroupReplyMode> = None;
            let mut json = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--thread")? {
                    thread_id = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--from")? {
                    sender = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--text")? {
                    text = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--reply-mode")? {
                    reply_mode = Some(parse_reply_mode(val)?);
                } else if !arg.starts_with('-') && group_id.is_none() {
                    group_id = Some(arg.clone());
                } else if !arg.starts_with('-') && text.is_none() {
                    text = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups send"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let group_id =
                group_id.ok_or_else(|| CliError::usage("groups send requires <group-id>"))?;
            let text = text
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| CliError::usage("groups send requires <message> or --text <str>"))?;
            Ok(BotCommand::GroupsSend {
                group_id,
                thread_id,
                sender,
                text,
                reply_mode,
                json,
            })
        }
        "stop" => {
            let mut group_id: Option<String> = None;
            let mut thread_id: Option<String> = None;
            let mut json = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--thread")? {
                    thread_id = Some(val.to_owned());
                } else if !arg.starts_with('-') && group_id.is_none() {
                    group_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for groups stop"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let group_id =
                group_id.ok_or_else(|| CliError::usage("groups stop requires <group-id>"))?;
            Ok(BotCommand::GroupsStop {
                group_id,
                thread_id,
                json,
            })
        }
        other => Err(CliError::usage(format!(
            "unknown groups action '{other}' (expected ls|create|update|rm|reorder|new-thread|read|send|stop)"
        ))),
    }
}

fn parse_connectors(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    const ACTIONS: &[&str] = &[
        "status",
        "reload",
        "sync",
        "test",
        "slack-manifest",
        "telegram",
        "slack",
        "discord",
        "google-setup",
        "google",
        "github",
        "linear",
        "composio",
    ];
    const ZERO_POS_ACTIONS: &[&str] = &[
        "status",
        "reload",
        "sync",
        "telegram",
        "slack",
        "discord",
        "google-setup",
        "google",
        "github",
        "linear",
        "composio",
    ];
    let (mut bot, mut json, action, tail) =
        split_bot_and_action_skipping_flags(args, ACTIONS, ZERO_POS_ACTIONS)?;
    let action = action.unwrap_or("status");

    match action {
        "slack-manifest" => {
            let mut bot_name: Option<String> = None;
            for arg in tail {
                if arg == "--json" {
                    json = true;
                } else if !arg.starts_with('-') && bot_name.is_none() {
                    bot_name = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for connectors slack-manifest"
                    )));
                }
            }
            let bot_name = bot_name
                .or(bot)
                .ok_or_else(|| CliError::usage("connectors slack-manifest requires <bot-name>"))?;
            Ok(BotCommand::SlackManifest { bot_name, json })
        }
        "status" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for connectors status"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::ConnectorStatus {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                json,
            })
        }
        "reload" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for connectors reload"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::ConnectorReload {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                json,
            })
        }
        "sync" => {
            let mut source: Option<ExternalSource> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--source")? {
                    source = Some(parse_external_source(val)?);
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for connectors sync"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::ConnectorSync {
                bot: require_bot(bot, default_bot, "connectors sync")?,
                source,
                json,
            })
        }
        "test" => {
            let mut source: Option<ExternalSource> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--source")? {
                    source = Some(parse_external_source(val)?);
                } else if !arg.starts_with('-') && source.is_none() {
                    source = Some(parse_external_source(arg)?);
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for connectors test"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let source =
                source.ok_or_else(|| CliError::usage("connectors test requires <source>"))?;
            Ok(BotCommand::ConnectorTest {
                bot: require_bot(bot, default_bot, "connectors test")?,
                source,
                json,
            })
        }
        "telegram" => Ok(with_json_override(
            parse_connector_telegram(bot, tail, default_bot)?,
            json,
        )),
        "slack" => Ok(with_json_override(
            parse_connector_slack(bot, tail, default_bot)?,
            json,
        )),
        "discord" => Ok(with_json_override(
            parse_connector_discord(bot, tail, default_bot)?,
            json,
        )),
        "google-setup" | "google" => Ok(with_json_override(
            parse_connector_google(bot, tail, default_bot)?,
            json,
        )),
        "github" => Ok(with_json_override(
            parse_connector_github(bot, tail, default_bot)?,
            json,
        )),
        "linear" => Ok(with_json_override(
            parse_connector_linear(bot, tail, default_bot)?,
            json,
        )),
        "composio" => Ok(with_json_override(
            parse_connector_composio(bot, tail, default_bot)?,
            json,
        )),
        _ => Err(CliError::usage(help_text(Some("connectors")))),
    }
}

fn with_json_override(mut cmd: BotCommand, json_prefix: bool) -> BotCommand {
    if json_prefix && let BotCommand::ConnectorConfigure(ref mut args) = cmd {
        args.json = true;
    }
    cmd
}

fn parse_connector_telegram(
    mut bot: Option<String>,
    tail: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let mut enabled = None;
    let mut secret_ref = None;
    let mut allowed_chats = None;
    let mut allowed_users = None;
    let mut acts_for = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--enable" {
            enabled = Some(true);
        } else if arg == "--disable" {
            enabled = Some(false);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--secret-ref")? {
            secret_ref = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--allowed-chats")? {
            allowed_chats = Some(parse_csv_i64(val, "--allowed-chats")?);
        } else if let Some(val) = take_opt(tail, &mut idx, "--allowed-users")? {
            allowed_users = Some(parse_csv_i64(val, "--allowed-users")?);
        } else if let Some(val) = take_opt(tail, &mut idx, "--acts-for")? {
            acts_for = Some(parse_acts_for(val)?);
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for connectors telegram"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::ConnectorConfigure(ConnectorPatchArgs {
        bot: require_bot(bot, default_bot, "connectors telegram")?,
        patch: ConnectorPatch::Telegram {
            enabled,
            secret_ref,
            allowed_chats,
            allowed_users,
            acts_for,
        },
        json,
    }))
}

fn parse_connector_slack(
    mut bot: Option<String>,
    tail: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let mut enabled = None;
    let mut app_token_ref = None;
    let mut bot_token_ref = None;
    let mut allowed_channels = None;
    let mut allowed_users = None;
    let mut mirror_desktop = None;
    let mut acts_for = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--enable" {
            enabled = Some(true);
        } else if arg == "--disable" {
            enabled = Some(false);
        } else if arg == "--mirror" {
            mirror_desktop = Some(true);
        } else if arg == "--no-mirror" {
            mirror_desktop = Some(false);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--app-token-ref")? {
            app_token_ref = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot-token-ref")? {
            bot_token_ref = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--allowed-channels")? {
            allowed_channels = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--allowed-users")? {
            allowed_users = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--acts-for")? {
            acts_for = Some(parse_acts_for(val)?);
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for connectors slack"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::ConnectorConfigure(ConnectorPatchArgs {
        bot: require_bot(bot, default_bot, "connectors slack")?,
        patch: ConnectorPatch::Slack {
            enabled,
            app_token_ref,
            bot_token_ref,
            allowed_channels,
            allowed_users,
            mirror_desktop,
            acts_for,
        },
        json,
    }))
}

fn parse_connector_discord(
    mut bot: Option<String>,
    tail: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let mut enabled = None;
    let mut bot_token_ref = None;
    let mut allowed_guilds = None;
    let mut allowed_channels = None;
    let mut allowed_users = None;
    let mut acts_for = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--enable" {
            enabled = Some(true);
        } else if arg == "--disable" {
            enabled = Some(false);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot-token-ref")? {
            bot_token_ref = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--allowed-guilds")? {
            allowed_guilds = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--allowed-channels")? {
            allowed_channels = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--allowed-users")? {
            allowed_users = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--acts-for")? {
            acts_for = Some(parse_acts_for(val)?);
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for connectors discord"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::ConnectorConfigure(ConnectorPatchArgs {
        bot: require_bot(bot, default_bot, "connectors discord")?,
        patch: ConnectorPatch::Discord {
            enabled,
            bot_token_ref,
            allowed_guilds,
            allowed_channels,
            allowed_users,
            acts_for,
        },
        json,
    }))
}

fn parse_google_services(raw: &str) -> Result<Vec<GoogleServiceKind>, CliError> {
    let mut out = Vec::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match item.to_ascii_lowercase().as_str() {
            "gmail" => out.push(GoogleServiceKind::Gmail),
            "calendar" => out.push(GoogleServiceKind::Calendar),
            "drive" => out.push(GoogleServiceKind::Drive),
            "docs" => out.push(GoogleServiceKind::Docs),
            "sheets" => out.push(GoogleServiceKind::Sheets),
            "contacts" => out.push(GoogleServiceKind::Contacts),
            "all" => {
                return Ok(vec![
                    GoogleServiceKind::Gmail,
                    GoogleServiceKind::Calendar,
                    GoogleServiceKind::Drive,
                    GoogleServiceKind::Docs,
                    GoogleServiceKind::Sheets,
                    GoogleServiceKind::Contacts,
                ]);
            }
            "none" => return Ok(Vec::new()),
            other => {
                return Err(CliError::usage(format!(
                    "invalid Google service '{other}' (expected gmail,calendar,drive,docs,sheets,contacts)"
                )));
            }
        }
    }
    Ok(out)
}

fn parse_connector_google(
    mut bot: Option<String>,
    tail: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let mut enabled = None;
    let mut poll_secs = None;
    let mut services = None;
    let mut client_secret_ref = None;
    let mut token_secret_ref = None;
    let mut start_oauth = false;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--enable" {
            enabled = Some(true);
        } else if arg == "--disable" {
            enabled = Some(false);
        } else if arg == "--start-oauth" {
            start_oauth = true;
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--poll-secs")? {
            let secs = val
                .parse::<u64>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| CliError::usage("--poll-secs must be a positive integer"))?;
            poll_secs = Some(secs);
        } else if let Some(val) = take_opt(tail, &mut idx, "--services")? {
            services = Some(parse_google_services(val)?);
        } else if let Some(val) = take_opt(tail, &mut idx, "--client-secret-ref")? {
            client_secret_ref = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--token-secret-ref")? {
            token_secret_ref = Some(val.to_owned());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for connectors google-setup"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::ConnectorConfigure(ConnectorPatchArgs {
        bot: require_bot(bot, default_bot, "connectors google-setup")?,
        patch: ConnectorPatch::Google {
            enabled,
            poll_secs,
            services,
            client_secret_ref,
            token_secret_ref,
            start_oauth,
        },
        json,
    }))
}

fn parse_connector_github(
    mut bot: Option<String>,
    tail: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let mut enabled = None;
    let mut repos = None;
    let mut use_gh_cli = None;
    let mut token_ref = None;
    let mut poll_secs = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--enable" {
            enabled = Some(true);
        } else if arg == "--disable" {
            enabled = Some(false);
        } else if arg == "--use-gh-cli" {
            use_gh_cli = Some(true);
        } else if arg == "--no-gh-cli" {
            use_gh_cli = Some(false);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--repos")? {
            repos = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--token-ref")? {
            token_ref = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--poll-secs")? {
            let secs = val
                .parse::<u64>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| CliError::usage("--poll-secs must be a positive integer"))?;
            poll_secs = Some(secs);
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for connectors github"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::ConnectorConfigure(ConnectorPatchArgs {
        bot: require_bot(bot, default_bot, "connectors github")?,
        patch: ConnectorPatch::GitHub {
            enabled,
            repos,
            use_gh_cli,
            token_ref,
            poll_secs,
        },
        json,
    }))
}

fn parse_connector_linear(
    mut bot: Option<String>,
    tail: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let mut enabled = None;
    let mut teams = None;
    let mut api_key_ref = None;
    let mut poll_secs = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--enable" {
            enabled = Some(true);
        } else if arg == "--disable" {
            enabled = Some(false);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--teams")? {
            teams = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--api-key-ref")? {
            api_key_ref = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--poll-secs")? {
            let secs = val
                .parse::<u64>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| CliError::usage("--poll-secs must be a positive integer"))?;
            poll_secs = Some(secs);
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for connectors linear"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::ConnectorConfigure(ConnectorPatchArgs {
        bot: require_bot(bot, default_bot, "connectors linear")?,
        patch: ConnectorPatch::Linear {
            enabled,
            teams,
            api_key_ref,
            poll_secs,
        },
        json,
    }))
}

fn parse_connector_composio(
    mut bot: Option<String>,
    tail: &[String],
    default_bot: Option<&str>,
) -> Result<BotCommand, CliError> {
    let mut enabled = None;
    let mut apps = None;
    let mut api_key_ref = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--enable" {
            enabled = Some(true);
        } else if arg == "--disable" {
            enabled = Some(false);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if let Some(val) = take_opt(tail, &mut idx, "--apps")? {
            apps = Some(parse_csv_strings(val));
        } else if let Some(val) = take_opt(tail, &mut idx, "--api-key-ref")? {
            api_key_ref = Some(val.to_owned());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for connectors composio"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::ConnectorConfigure(ConnectorPatchArgs {
        bot: require_bot(bot, default_bot, "connectors composio")?,
        patch: ConnectorPatch::Composio {
            enabled,
            apps,
            api_key_ref,
        },
        json,
    }))
}

fn parse_access(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &["ls", "list", "approve", "decline", "redeem"],
        &["ls", "list"],
    )?;
    let action = action.unwrap_or("ls");
    match action {
        "ls" | "list" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for access ls"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::AccessList {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                json,
            })
        }
        "approve" | "decline" => {
            let mut token: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if !arg.starts_with('-') && token.is_none() {
                    token = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for access {action}"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let token = token
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| CliError::usage(format!("access {action} requires <token>")))?;
            Ok(BotCommand::AccessDecide {
                token,
                approve: action == "approve",
                json,
            })
        }
        "redeem" => {
            let mut code: Option<String> = None;
            for arg in tail {
                if arg == "--json" {
                    json = true;
                } else if !arg.starts_with('-') && code.is_none() {
                    code = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for access redeem"
                    )));
                }
            }
            let code = code
                .filter(|c| !c.trim().is_empty())
                .ok_or_else(|| CliError::usage("access redeem requires <code>"))?;
            Ok(BotCommand::AccessRedeem { code, json })
        }
        _ => Err(CliError::usage(help_text(Some("access")))),
    }
}

fn parse_secret(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) =
        split_bot_and_action_skipping_flags(args, &["set", "rm", "remove", "delete"], &[])?;
    let Some(action) = action else {
        return Err(CliError::usage(help_text(Some("secret"))));
    };
    let mut key: Option<String> = None;
    let mut source: Option<SecretInputSource> = None;
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--stdin" && action == "set" {
            if source.is_some() {
                return Err(CliError::usage(
                    "specify only one of --stdin or --env <VAR>",
                ));
            }
            source = Some(SecretInputSource::Stdin);
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if action == "set"
            && let Some(val) = take_opt(tail, &mut idx, "--env")?
        {
            if source.is_some() {
                return Err(CliError::usage(
                    "specify only one of --stdin or --env <VAR>",
                ));
            }
            if val.trim().is_empty() {
                return Err(CliError::usage("--env requires a variable name"));
            }
            source = Some(SecretInputSource::Env(val.to_owned()));
        } else if !arg.starts_with('-') && key.is_none() {
            key = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for secret {action}"
            )));
        }
        idx = idx.saturating_add(1);
    }

    let bot = require_bot(bot, default_bot, "secret")?;
    let key = key
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| CliError::usage(format!("secret {action} requires <key>")))?;
    match action {
        "set" => {
            let source = source.ok_or_else(|| {
                CliError::usage(
                    "secret set requires --stdin or --env <VAR> (secrets are never accepted as CLI args)",
                )
            })?;
            Ok(BotCommand::SecretSet {
                bot,
                key,
                source,
                json,
            })
        }
        "rm" | "remove" | "delete" => Ok(BotCommand::SecretRemove { bot, key, json }),
        _ => Err(CliError::usage(help_text(Some("secret")))),
    }
}

fn parse_tools(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &[
            "ls",
            "list",
            "add",
            "enable",
            "disable",
            "oauth",
            "rm",
            "remove",
            "relay-status",
        ],
        &["ls", "list", "relay-status"],
    )?;
    let action = action.unwrap_or("ls");

    match action {
        "relay-status" => {
            for arg in tail {
                if arg == "--json" {
                    json = true;
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for tools relay-status"
                    )));
                }
            }
            Ok(BotCommand::ToolsRelayStatus { json })
        }
        "ls" | "list" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for tools ls"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::ToolsList {
                bot: require_bot(bot, default_bot, "tools ls")?,
                json,
            })
        }
        "add" => {
            let mut app_id: Option<String> = None;
            let mut name: Option<String> = None;
            let mut url: Option<String> = None;
            let mut headers: Vec<String> = Vec::new();
            let mut auth_kind: Option<BotMcpAuthKind> = None;
            let mut secret_ref: Option<String> = None;
            let mut command: Vec<String> = Vec::new();
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--url")? {
                    url = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--name")? {
                    name = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--header")? {
                    let hdr = val.trim();
                    if hdr.is_empty() {
                        return Err(CliError::usage("--header requires a non-empty header name"));
                    }
                    headers.push(hdr.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--auth")? {
                    auth_kind = Some(match val.trim().to_ascii_lowercase().as_str() {
                        "oauth" | "browser_oauth" | "browser-oauth" => BotMcpAuthKind::BrowserOAuth,
                        "bearer" | "bearer_header" | "header" => BotMcpAuthKind::BearerHeader,
                        "none" => BotMcpAuthKind::None,
                        other => {
                            return Err(CliError::usage(format!(
                                "invalid --auth '{other}' (expected oauth|bearer|none)"
                            )));
                        }
                    });
                } else if let Some(val) = take_opt(tail, &mut idx, "--secret-ref")? {
                    secret_ref = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--cmd")? {
                    command.push(val.to_owned());
                } else if !arg.starts_with('-') && app_id.is_none() {
                    app_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for tools add"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let app_id = app_id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("tools add requires <app-id>"))?;
            Ok(BotCommand::ToolsAdd(ToolsAddArgs {
                bot: require_bot(bot, default_bot, "tools add")?,
                app_id,
                name,
                url,
                headers,
                auth_kind,
                secret_ref,
                command,
                json,
            }))
        }
        "enable" | "disable" => {
            let mut app_id: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && app_id.is_none() {
                    app_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for tools {action}"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let app_id = app_id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage(format!("tools {action} requires <app-id>")))?;
            Ok(BotCommand::ToolsSetEnabled {
                bot: require_bot(bot, default_bot, &format!("tools {action}"))?,
                app_id,
                enabled: action == "enable",
                json,
            })
        }
        "oauth" => {
            let mut tool_id: Option<String> = None;
            let mut status_flag = false;
            let mut cancel_flag = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if arg == "--status" {
                    status_flag = true;
                } else if arg == "--cancel" {
                    cancel_flag = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && tool_id.is_none() {
                    tool_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for tools oauth"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            if status_flag && cancel_flag {
                return Err(CliError::usage(
                    "--status and --cancel are mutually exclusive",
                ));
            }
            let tool_id = tool_id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("tools oauth requires <tool-id>"))?;
            let mode = if cancel_flag {
                ToolsOAuthMode::Cancel
            } else if status_flag {
                ToolsOAuthMode::Status
            } else {
                ToolsOAuthMode::Start
            };
            Ok(BotCommand::ToolsOAuth {
                bot: require_bot(bot, default_bot, "tools oauth")?,
                tool_id,
                mode,
                json,
            })
        }
        "rm" | "remove" => {
            let mut app_id: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && app_id.is_none() {
                    app_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for tools rm"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let app_id = app_id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("tools rm requires <app-id>"))?;
            Ok(BotCommand::ToolsRemove {
                bot: require_bot(bot, default_bot, "tools rm")?,
                app_id,
                json,
            })
        }
        _ => Err(CliError::usage(help_text(Some("tools")))),
    }
}

fn parse_session_id(raw: &str, flag: &str) -> Result<u64, CliError> {
    let trimmed = raw.trim();
    let digits = trimmed
        .strip_prefix('s')
        .or_else(|| trimmed.strip_prefix('S'))
        .unwrap_or(trimmed);
    digits
        .parse::<u64>()
        .map_err(|_| CliError::usage(format!("invalid session ID '{trimmed}' for {flag}")))
}

fn parse_suggestions(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &["ls", "list", "dismiss", "start"],
        &["ls", "list"],
    )?;
    let action = action.unwrap_or("ls");
    match action {
        "ls" | "list" => {
            let mut include_dismissed = false;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if arg == "--include-dismissed" {
                    include_dismissed = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for suggestions ls"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::SuggestionsList {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                include_dismissed,
                json,
            })
        }
        "dismiss" | "start" => {
            let mut suggestion_id: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && suggestion_id.is_none() {
                    suggestion_id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for suggestions {action}"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let suggestion_id =
                suggestion_id
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        CliError::usage(format!("suggestions {action} requires <suggestion-id>"))
                    })?;
            let bot = require_bot(bot, default_bot, &format!("suggestions {action}"))?;
            if action == "dismiss" {
                Ok(BotCommand::SuggestionsDismiss {
                    bot,
                    suggestion_id,
                    json,
                })
            } else {
                Ok(BotCommand::SuggestionsStart {
                    bot,
                    suggestion_id,
                    json,
                })
            }
        }
        _ => Err(CliError::usage(help_text(Some("suggestions")))),
    }
}

fn parse_up_next(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) =
        split_bot_and_action_skipping_flags(args, &["seen"], &["seen"])?;
    let Some("seen") = action else {
        return Err(CliError::usage(help_text(Some("up-next"))));
    };
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for up-next seen"
            )));
        }
        idx = idx.saturating_add(1);
    }
    Ok(BotCommand::UpNextSeen {
        bot: bot.or_else(|| default_bot.map(str::to_owned)),
        json,
    })
}

fn parse_schedules(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &["ls", "list", "add", "enable", "disable", "rm", "remove"],
        &["ls", "list", "add"],
    )?;
    let action = action.unwrap_or("ls");
    match action {
        "ls" | "list" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for schedules ls"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::SchedulesList {
                bot: require_bot(bot, default_bot, "schedules ls")?,
                json,
            })
        }
        "add" => {
            let mut id: Option<String> = None;
            let mut name: Option<String> = None;
            let mut cron: Option<String> = None;
            let mut prompt: Option<String> = None;
            let mut timezone: Option<String> = None;
            let mut enabled = true;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if arg == "--disabled" {
                    enabled = false;
                } else if arg == "--enabled" {
                    // Support both `--enabled <bool>` and bare `--enabled`
                    if let Some(next) = tail.get(idx + 1)
                        && !next.starts_with('-')
                    {
                        idx = idx.saturating_add(1);
                        enabled = parse_bool_flag(next, "--enabled")?;
                    } else {
                        enabled = true;
                    }
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--id")? {
                    id = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--name")? {
                    name = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--cron")? {
                    cron = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--prompt")? {
                    prompt = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--timezone")? {
                    timezone = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for schedules add"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let name = name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("schedules add requires --name <name>"))?;
            let cron = cron
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("schedules add requires --cron <expr>"))?;
            Cron::parse(&cron)
                .map_err(|e| CliError::usage(format!("invalid cron expression '{cron}': {e}")))?;
            let prompt = prompt
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage("schedules add requires --prompt <text>"))?;
            Ok(BotCommand::SchedulesAdd {
                bot: require_bot(bot, default_bot, "schedules add")?,
                id,
                name,
                cron,
                prompt,
                timezone,
                enabled,
                json,
            })
        }
        "enable" | "disable" | "rm" | "remove" => {
            let mut id: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && id.is_none() {
                    id = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for schedules {action}"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let id = id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CliError::usage(format!("schedules {action} requires <id>")))?;
            let bot = require_bot(bot, default_bot, &format!("schedules {action}"))?;
            match action {
                "enable" => Ok(BotCommand::SchedulesSetEnabled {
                    bot,
                    id,
                    enabled: true,
                    json,
                }),
                "disable" => Ok(BotCommand::SchedulesSetEnabled {
                    bot,
                    id,
                    enabled: false,
                    json,
                }),
                _ => Ok(BotCommand::SchedulesRemove { bot, id, json }),
            }
        }
        _ => Err(CliError::usage(help_text(Some("schedules")))),
    }
}

fn parse_sessions(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &[
            "ls",
            "list",
            "reset",
            "stop",
            "retry-kickoff",
            "take-over",
            "run-again",
        ],
        &["ls", "list", "reset", "retry-kickoff"],
    )?;
    let action = action.unwrap_or("ls");
    match action {
        "ls" | "list" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for sessions ls"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::SessionsList {
                bot: bot.or_else(|| default_bot.map(str::to_owned)),
                json,
            })
        }
        "reset" => {
            let mut session_id: Option<u64> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if let Some(val) = take_opt(tail, &mut idx, "--session")? {
                    session_id = Some(parse_session_id(val, "--session")?);
                } else if !arg.starts_with('-') && session_id.is_none() {
                    session_id = Some(parse_session_id(arg, "sessions reset")?);
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for sessions reset"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::SessionsReset {
                bot: require_bot(bot, default_bot, "sessions reset")?,
                session_id,
                json,
            })
        }
        "retry-kickoff" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for sessions retry-kickoff"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::SessionsRetryKickoff {
                bot: require_bot(bot, default_bot, "sessions retry-kickoff")?,
                json,
            })
        }
        "stop" | "take-over" | "run-again" => {
            let mut session_id: Option<u64> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && session_id.is_none() {
                    session_id = Some(parse_session_id(arg, action)?);
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for sessions {action}"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let session_id = session_id.ok_or_else(|| {
                CliError::usage(format!("sessions {action} requires <session-id>"))
            })?;
            let bot = require_bot(bot, default_bot, &format!("sessions {action}"))?;
            match action {
                "stop" => Ok(BotCommand::SessionsStop {
                    bot,
                    session_id,
                    json,
                }),
                "take-over" => Ok(BotCommand::SessionsTakeOver {
                    bot,
                    session_id,
                    json,
                }),
                _ => Ok(BotCommand::SessionsRunAgain {
                    bot,
                    session_id,
                    json,
                }),
            }
        }
        _ => Err(CliError::usage(help_text(Some("sessions")))),
    }
}

fn parse_tracker_default(args: &[String]) -> Result<BotCommand, CliError> {
    let mut source: Option<ExternalSource> = None;
    let mut bot: Option<String> = None;
    let mut clear = false;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--clear" {
            clear = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if !arg.starts_with('-') && source.is_none() {
            let parsed = parse_external_source(arg)?;
            if !matches!(parsed, ExternalSource::GitHub | ExternalSource::Linear) {
                return Err(CliError::usage(
                    "tracker-default source must be github or linear",
                ));
            }
            source = Some(parsed);
        } else if !arg.starts_with('-') && bot.is_none() {
            bot = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for tracker-default"
            )));
        }
        idx = idx.saturating_add(1);
    }
    if bot.is_some() && clear {
        return Err(CliError::usage("--bot and --clear are mutually exclusive"));
    }
    match source {
        None => {
            if bot.is_some() || clear {
                return Err(CliError::usage(
                    "tracker-default requires <github|linear> before --bot or --clear",
                ));
            }
            Ok(BotCommand::TrackerDefaultGet { json })
        }
        Some(source) => {
            if !clear && bot.is_none() {
                return Err(CliError::usage(
                    "tracker-default <github|linear> requires --bot <bot> or --clear",
                ));
            }
            Ok(BotCommand::TrackerDefaultSet { source, bot, json })
        }
    }
}

fn parse_composio(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) = split_bot_and_action_skipping_flags(
        args,
        &["connections", "ls", "list", "connect", "disconnect"],
        &["connections", "ls", "list"],
    )?;
    let action = action.unwrap_or("connections");
    match action {
        "connections" | "ls" | "list" => {
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for composio connections"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            Ok(BotCommand::ComposioConnections {
                bot: require_bot(bot, default_bot, "composio connections")?,
                json,
            })
        }
        "connect" | "disconnect" => {
            let mut target: Option<String> = None;
            let mut idx = 0usize;
            while let Some(arg) = tail.get(idx) {
                if arg == "--json" {
                    json = true;
                } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
                    bot = Some(val.to_owned());
                } else if !arg.starts_with('-') && target.is_none() {
                    target = Some(arg.clone());
                } else {
                    return Err(CliError::usage(format!(
                        "unexpected argument '{arg}' for composio {action}"
                    )));
                }
                idx = idx.saturating_add(1);
            }
            let target = target.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                let arg_name = if action == "connect" {
                    "<app-id>"
                } else {
                    "<connection-id>"
                };
                CliError::usage(format!("composio {action} requires {arg_name}"))
            })?;
            let bot = require_bot(bot, default_bot, &format!("composio {action}"))?;
            if action == "connect" {
                Ok(BotCommand::ComposioConnect {
                    bot,
                    app_id: target,
                    json,
                })
            } else {
                Ok(BotCommand::ComposioDisconnect {
                    bot,
                    connection_id: target,
                    json,
                })
            }
        }
        _ => Err(CliError::usage(help_text(Some("composio")))),
    }
}

fn parse_avatar(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let (mut bot, mut json, action, tail) =
        split_bot_and_action_skipping_flags(args, &["set", "get", "color"], &[])?;
    let Some(action) = action else {
        return Err(CliError::usage(help_text(Some("avatar"))));
    };
    let mut positionals: Vec<String> = Vec::new();
    let mut idx = 0usize;
    while let Some(arg) = tail.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(tail, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if !arg.starts_with('-') {
            positionals.push(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for avatar {action}"
            )));
        }
        idx = idx.saturating_add(1);
    }
    let (resolved_bot, value) = match (bot, positionals.as_slice()) {
        (Some(b), [v]) => (b, v.clone()),
        (None, [b, v]) => (b.clone(), v.clone()),
        (None, [v]) if default_bot.is_some() => (
            require_bot(None, default_bot, &format!("avatar {action}"))?,
            v.clone(),
        ),
        _ => {
            let arg_name = match action {
                "set" => "<png-path>",
                "get" => "<out-path>",
                _ => "<hex>",
            };
            return Err(CliError::usage(format!(
                "avatar {action} requires <bot> {arg_name}"
            )));
        }
    };
    match action {
        "set" => Ok(BotCommand::AvatarSet {
            bot: resolved_bot,
            png_path: value,
            json,
        }),
        "get" => Ok(BotCommand::AvatarGet {
            bot: resolved_bot,
            out_path: value,
            json,
        }),
        "color" => Ok(BotCommand::AvatarColor {
            bot: resolved_bot,
            color: value,
            json,
        }),
        _ => Err(CliError::usage(help_text(Some("avatar")))),
    }
}

fn parse_export(args: &[String], default_bot: Option<&str>) -> Result<BotCommand, CliError> {
    let mut bot: Option<String> = None;
    let mut include_memory = false;
    let mut include_avatar = true;
    let mut json = false;
    let mut positionals = Vec::new();
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--include-memory" {
            include_memory = true;
        } else if arg == "--no-avatar" {
            include_avatar = false;
        } else if let Some(val) = take_opt(args, &mut idx, "--bot")? {
            bot = Some(val.to_owned());
        } else if !arg.starts_with('-') {
            positionals.push(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for export"
            )));
        }
        idx = idx.saturating_add(1);
    }

    let out_path = match (bot.is_some(), positionals.as_slice()) {
        (true, [out]) => out.clone(),
        (false, [b, out]) => {
            bot = Some(b.clone());
            out.clone()
        }
        (false, [out]) if default_bot.is_some() => out.clone(),
        _ => {
            return Err(CliError::usage("export requires <bot> <out.zip>"));
        }
    };

    Ok(BotCommand::ExportZip {
        bot: require_bot(bot, default_bot, "export")?,
        out_path,
        include_memory,
        include_avatar,
        json,
    })
}

fn parse_import(args: &[String]) -> Result<BotCommand, CliError> {
    let mut in_path: Option<String> = None;
    let mut slug: Option<String> = None;
    let mut name: Option<String> = None;
    let mut agent: Option<String> = None;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--slug")? {
            validate_bot_slug(val).map_err(|e| CliError::usage(e.to_string()))?;
            slug = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--name")? {
            name = Some(val.to_owned());
        } else if let Some(val) = take_opt(args, &mut idx, "--agent")? {
            agent = Some(val.to_owned());
        } else if !arg.starts_with('-') && in_path.is_none() {
            in_path = Some(arg.clone());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for import"
            )));
        }
        idx = idx.saturating_add(1);
    }
    let in_path = in_path.ok_or_else(|| CliError::usage("import requires <in.zip>"))?;
    Ok(BotCommand::ImportZip {
        in_path,
        slug,
        name,
        agent,
        json,
    })
}

fn parse_import_external(args: &[String]) -> Result<BotCommand, CliError> {
    let mut source: Option<ExternalBotSourceApp> = None;
    let mut path: Option<String> = None;
    let mut apply = false;
    let mut dry_run = false;
    let mut json = false;
    let mut idx = 0usize;
    while let Some(arg) = args.get(idx) {
        if arg == "--json" {
            json = true;
        } else if arg == "--apply" {
            apply = true;
        } else if arg == "--dry-run" {
            dry_run = true;
        } else if let Some(val) = take_opt(args, &mut idx, "--source")? {
            source = match val.trim().to_ascii_lowercase().as_str() {
                "all" => None,
                "orca" => Some(ExternalBotSourceApp::Orca),
                "openclaw" | "clawdbot" | "moltbot" => Some(ExternalBotSourceApp::OpenClaw),
                "hermes" => Some(ExternalBotSourceApp::Hermes),
                "openbot" => Some(ExternalBotSourceApp::OpenBot),
                other => {
                    return Err(CliError::usage(format!(
                        "invalid --source '{other}' (expected orca|openclaw|hermes|openbot|all)"
                    )));
                }
            };
        } else if let Some(val) = take_opt(args, &mut idx, "--path")? {
            path = Some(val.to_owned());
        } else {
            return Err(CliError::usage(format!(
                "unexpected argument '{arg}' for import-external"
            )));
        }
        idx = idx.saturating_add(1);
    }
    if apply && dry_run {
        return Err(CliError::usage(
            "--apply and --dry-run are mutually exclusive",
        ));
    }
    Ok(BotCommand::ImportExternal {
        source,
        path,
        apply,
        json,
    })
}
