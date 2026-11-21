// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! `krill bot`: Krill Bots CLI (Phases 1-3 lifecycle, catalog, soul/memory, items,
//! look around, messaging, connectors, access control, secrets, MCP tools, packaging,
//! and external bot import).

mod exec;
mod format;
mod parse;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::time::Duration;

use krill_proto::bots::{BotRpcRequest, BotRpcResponse};
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon::{self, Connection};
use krill_proto::ipc::{self, ClientRole, FEAT_BOTS};
use krill_proto::screen::Msg as _;

use crate::remote::Target;

const IO_READ_TIMEOUT: Duration = Duration::from_secs(30);
const IO_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const USAGE: &str = "\
usage: krill [--host <user@host>] bot <subcommand> [options]

Lifecycle:
  ls|list [--json]
  new|create <name> [--template <id>] [--agent <id>] [--model <m>] [--effort <lvl>]
             [--permission <manual|yolo>] [--description <text>] [--emoji <str>]
             [--avatar <str>] [--avatar-color <hex>] [--skills <a,b>] [--folder <path>]
             [--slug <slug>] [--soul <text> | --soul-file <path>] [--json]
  show <bot> [--json]
  update <bot> [--name <n>] [--description <d>] [--agent <id>] [--model <m>|--clear-model]
               [--effort <e>|--clear-effort] [--emoji <e>|--clear-emoji]
               [--avatar <a>|--clear-avatar] [--avatar-color <hex>|--clear-avatar-color]
               [--folder <p>|--clear-workspace] [--permission <manual|yolo>]
               [--first-run-completed <on|off>] [--pause|--resume]
               [--look-around <on|off>] [--look-around-interval <secs>]
               [--look-around-max <n>] [--look-around-weekdays <on|off>]
               [--look-around-focus <text>] [--memory-tidy <on|off>]
               [--memory-tidy-interval <secs>] [--notify-permission <on|off>]
               [--notify-complete <on|off>] [--notify-error <on|off>] [--json]
  rm <bot> [--delete-home|--keep-home] [--json]
  start|chat <bot> [--prompt <text>] [--cwd <path>] [--json]

Catalog (offline):
  templates [--category coding|releases|support|planning|ops|personal] [--show <id>] [--json]
  skills [--show <name>] [--json]
  apps [--kind mcp|composio|all] [--category coding|chat|monitoring|docs|mail|sales|design] [--json]
  slack-manifest <bot-name> [--json]

Soul, Memory & Tidy:
  soul [<bot>] [get | set (--text <str> | --file <path> | --stdin)] [--json]
  memory [<bot>] [ls | add <fact> [--date YYYY-MM-DD] [--source <src>]
                  | forget --fact <fact> | history [--limit <N>]
                  | edit --old <fact> --new <fact> | undo <entry-id>
                  | tidy [status | undo [<tidy-id>]]
                  | topics | topic <name> [--set (--text <str> | --file <path> | --stdin)]
                  | set (--text <str> | --file <path> | --stdin) [--allow-deletions]] [--json]
  tidy [<bot>] [run | status | undo [<tidy-id>]] [--json]

Items Inbox:
  items [<bot>] [ls [--status planned|working|needs_you|done|canceled] [--all]
                    [--include-snoozed] [--limit <N>] [--cursor <c> | --page <N>]
                 | show <item-id>
                 | add <title> [--body <text>] [--done-means <text>] [--status <st>] [--link <kind:target>...]
                 | update <item-id> [--status <st>] [--title <t>] [--body <b>] [--done-means <d>]
                          [--comment <c>] [--close-note <n>] [--snooze <until>|--unsnooze]
                          [--ask <q> | --question <q> [--option <opt>... [--recommended <idx>]]]
                 | answer <item-id> [--text <str> | --option <idx>] [--decision-id <id>] [--request-id <id>]
                 | comment <item-id> <text> [--author <name>]
                 | snooze <item-id> <until> | unsnooze <item-id>
                 | link <item-id> (<dispatch|pr|url|file> <target> | <kind>:<target>)
                 | done <item-id> [--note <text>] | cancel <item-id> [--note <text>]] [--json]

Look Around, Schedules, Sessions & Messaging:
  suggest [<bot>] [--run] [--title <t> --why <w> --ask <a> [--link <url>]] [--json]
  suggestions [<bot>] [ls [--include-dismissed] | dismiss <id> | start <id>] [--json]
  up-next seen [--bot <bot>] [--json]
  schedules [<bot>] [ls | add --name <n> --cron <expr> --prompt <text> [--id <id>] [--timezone <tz>] [--disabled]
                     | enable <id> | disable <id> | rm <id>] [--json]
  sessions [<bot>] [ls | reset [--session <id>] | stop <id> | retry-kickoff | take-over <id> | run-again <id>] [--json]
  reply [<bot>] (<text> | --text <text>) [--link <external-id>] [--file <path>...] [--react <emoji>] [--json]
  send --to bot:<slug> [--from <sender>] [--type status|question|handoff]
       [--subject <text>] [--thread <id>] <message> [--json]
  groups [ls | create <name> --members <a,b> [--description <text>]
          | update <id> [--name <n>] [--description <d>] [--members <a,b>]
          | rm <id> | reorder <id1,id2,...> | new-thread <id> [--title <t>]
          | read <id> [--thread <tid>] [--limit <N>]
          | send <id> [--thread <tid>] [--from <sender>] [--reply-mode mentions|all|selected] <msg>
          | stop <id> [--thread <tid>]] [--json]

Connectors, Tracker Routing, Composio & Access:
  connectors [<bot>] [status | reload | sync [--source <src>] | test <source> | slack-manifest <bot-name>
                      | telegram [--enable|--disable] [--secret-ref <ref>] [--allowed-chats <ids>] [--allowed-users <ids>] [--acts-for owner|anyone|member:<id>]
                      | slack [--enable|--disable] [--app-token-ref <ref>] [--bot-token-ref <ref>] [--allowed-channels <ids>] [--allowed-users <ids>] [--mirror|--no-mirror] [--acts-for ...]
                      | discord [--enable|--disable] [--bot-token-ref <ref>] [--allowed-guilds <ids>] [--allowed-channels <ids>] [--allowed-users <ids>] [--acts-for ...]
                      | google-setup [--enable|--disable] [--poll-secs <N>] [--services gmail,calendar,drive,docs,sheets,contacts] [--client-secret-ref <ref>] [--token-secret-ref <ref>] [--start-oauth]
                      | github [--enable|--disable] [--repos owner/repo,...] [--use-gh-cli|--no-gh-cli] [--token-ref <ref>] [--poll-secs <N>]
                      | linear [--enable|--disable] [--teams KEY,...] [--api-key-ref <ref>] [--poll-secs <N>]
                      | composio [--enable|--disable] [--apps app1,app2] [--api-key-ref <ref>]] [--json]
  tracker-default [github|linear] [--bot <bot> | --clear] [--json]
  composio [<bot>] [connections | connect <app-id> | disconnect <connection-id>] [--json]
  access [<bot>] [ls | approve <token> | decline <token> | redeem <code>] [--json]

Secrets, MCP Tools, Avatar & Packaging:
  secret [<bot>] [set <key> (--stdin | --env <VAR>) | rm <key>] [--json]
         keys: telegram, slack-app, slack-bot, discord, google-oauth, google-token, github, linear, composio, mcp:<app-id>
  tools [<bot>] [ls | add <app-id> [--url <url>] [--name <name>] [--header <name>...] [--auth oauth|bearer|none] [--secret-ref <ref>] [--cmd <bin>...]
                 | enable <app-id> | disable <app-id> | oauth <tool-id> [--status | --cancel] | rm <app-id> | relay-status] [--json]
  avatar [set <bot> <png-path> | get <bot> <out-path> | color <bot> <hex>] [--json]
  export <bot> <out.zip> [--include-memory] [--no-avatar] [--json]
  import <in.zip> [--slug <slug>] [--name <name>] [--agent <id>] [--json]
  import-external [--source orca|openclaw|hermes|openbot|all] [--path <dir>] [--apply|--dry-run] [--json]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliError {
    pub code: u8,
    pub message: String,
}

impl CliError {
    pub(crate) fn usage(msg: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: msg.into(),
        }
    }

    pub(crate) fn fail(msg: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: msg.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<io::Error> for CliError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::InvalidInput {
            Self::usage(err.to_string())
        } else {
            Self::fail(err.to_string())
        }
    }
}

pub(crate) trait BotClient {
    fn call(&mut self, request: BotRpcRequest) -> Result<BotRpcResponse, CliError>;
}

struct DaemonBotClient {
    target: Target,
    conn: Option<Connection>,
}

impl DaemonBotClient {
    fn new(target: Target) -> Self {
        Self { target, conn: None }
    }

    fn ensure_connected(&mut self) -> Result<&mut Connection, CliError> {
        if self.conn.is_none() {
            let hello = daemon::client_hello(ClientRole::Cli, "krill-bot");
            let conn = self
                .target
                .connect(&hello)
                .map_err(|e| CliError::fail(e.to_string()))?;
            ensure_bots_feature(conn.negotiated.features)?;
            conn.stream
                .set_read_timeout(Some(IO_READ_TIMEOUT))
                .map_err(|e| CliError::fail(e.to_string()))?;
            conn.stream
                .set_write_timeout(Some(IO_WRITE_TIMEOUT))
                .map_err(|e| CliError::fail(e.to_string()))?;
            self.conn = Some(conn);
        }
        self.conn
            .as_mut()
            .ok_or_else(|| CliError::fail("daemon connection unavailable"))
    }
}

pub(crate) fn ensure_bots_feature(features: u64) -> Result<(), CliError> {
    let required = FEAT_CONTROL | FEAT_BOTS;
    if features & required != required {
        return Err(CliError::fail(
            "krilld does not support bots (FEAT_BOTS); update or restart the daemon",
        ));
    }
    Ok(())
}

impl BotClient for DaemonBotClient {
    fn call(&mut self, request: BotRpcRequest) -> Result<BotRpcResponse, CliError> {
        let encoded = Request::Bot(request)
            .encode()
            .map_err(|e| CliError::fail(format!("encode bot request: {e}")))?;
        let conn = self.ensure_connected()?;
        if let Err(err) = conn.stream.write_all(&encoded) {
            self.conn = None;
            return Err(CliError::fail(format!("send bot request to krilld: {err}")));
        }
        let frame = match ipc::read_frame(&mut conn.stream) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                self.conn = None;
                return Err(CliError::fail("krilld closed connection before replying"));
            }
            Err(err) => {
                self.conn = None;
                return Err(CliError::fail(format!("read bot reply from krilld: {err}")));
            }
        };
        let reply = Reply::decode(frame.0, &frame.1)
            .map_err(|e| CliError::fail(format!("decode bot reply: {e}")))?;
        match reply {
            Reply::Bot(BotRpcResponse::Error(msg)) => Err(CliError::fail(msg)),
            Reply::Bot(resp) => Ok(resp),
            Reply::Failed(failure) => Err(CliError::fail(failure.to_string())),
            _ => Err(CliError::fail("unexpected non-bot reply from krilld")),
        }
    }
}

fn env_bot_id() -> Option<String> {
    std::env::var("KRILL_BOT_ID")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn raw_wants_json(raw: &[OsString]) -> bool {
    for arg in raw {
        if arg == "--" {
            break;
        }
        if arg == "--json" {
            return true;
        }
    }
    false
}

pub(crate) fn format_cli_error(err: &CliError, json_mode: bool) -> String {
    if json_mode {
        let payload = serde_json::json!({
            "ok": false,
            "error": err.message,
            "code": err.code,
        });
        match serde_json::to_string_pretty(&payload) {
            Ok(rendered) => rendered,
            Err(_) => format!(
                r#"{{"ok":false,"error":{},"code":{}}}"#,
                serde_json::json!(err.message),
                err.code
            ),
        }
    } else {
        format!("bot: {}", err.message)
    }
}

pub(crate) fn run_with_client<C, R, B, E>(
    raw: &[OsString],
    default_bot: Option<&str>,
    build_client: B,
    stdin: &mut R,
    now_ms: u64,
    get_env: E,
) -> Result<String, (CliError, bool)>
where
    C: BotClient,
    R: io::Read,
    B: FnOnce(Option<String>) -> Result<C, CliError>,
    E: Fn(&str) -> Option<String>,
{
    let normalized = crate::normalize_host_flag(raw.to_vec());
    let json_mode = raw_wants_json(&normalized);
    let (host, rest) = crate::remote::extract_host(&normalized)
        .map_err(|text| (CliError::usage(format!("{text}\n{USAGE}")), json_mode))?;
    let args = rest
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| (CliError::usage("arguments must be UTF-8"), json_mode))?;

    let cmd = parse::parse_command(&args, default_bot).map_err(|err| (err, json_mode))?;

    if let parse::BotCommand::Help(topic) = cmd {
        return Ok(parse::help_text(topic.as_deref()).to_owned());
    }

    if let Some(offline_res) = exec::try_execute_offline(&cmd) {
        return offline_res.map_err(|err| (err, json_mode));
    }

    let mut client = build_client(host).map_err(|err| (err, json_mode))?;
    exec::execute_online(cmd, &mut client, stdin, now_ms, get_env).map_err(|err| (err, json_mode))
}

pub fn run(raw: &[OsString]) -> ExitCode {
    let default_bot = env_bot_id();
    let mut stdin = io::stdin().lock();
    let now_ms = exec::now_unix_ms();
    match run_with_client(
        raw,
        default_bot.as_deref(),
        |host| {
            Target::establish(host)
                .map(DaemonBotClient::new)
                .map_err(|err| CliError::fail(format!("remote: {err}")))
        },
        &mut stdin,
        now_ms,
        |var| std::env::var(var).ok(),
    ) {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err((err, json_mode)) => {
            let rendered = format_cli_error(&err, json_mode);
            if json_mode {
                println!("{rendered}");
            } else {
                eprintln!("{rendered}");
            }
            ExitCode::from(err.code)
        }
    }
}
