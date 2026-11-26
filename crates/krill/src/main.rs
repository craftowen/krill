#![forbid(unsafe_code)]
//! `krill` CLI: daemon-backed session commands (MK-045), doctor (MK-025, MK-067),
//! agent profiles (MK-037), hooks (MK-038), worktrees (MK-042/043), git changes (MK-053).

/// 파이프가 일찍 닫혀(`krill doctor | head -1`) `BrokenPipe`가 나도 panic(종료 101)하지 않는다.
macro_rules! println {
    () => {{
        <std::io::Stdout as std::io::Write>::write_all(&mut std::io::stdout(), b"\n").ok();
    }};
    ($($arg:tt)*) => {{
        <std::io::Stdout as std::io::Write>::write_fmt(
            &mut std::io::stdout(),
            format_args!("{}\n", format_args!($($arg)*)),
        )
        .ok();
    }};
}

mod account;
mod artifacts;
mod bot;
mod changes;
mod doctor;
mod drop;
mod errors;
mod file;
mod history;
#[cfg(unix)]
mod hook;
#[cfg(windows)]
#[path = "hook_windows.rs"]
mod hook;
mod hook_install;
mod host;
mod import;
mod mobile;
mod orca_import;
mod orchestration;
mod remote;
mod schedule;
mod search;
mod session;
mod set;
mod skills;
mod statusline;
mod telemetry;
mod update;
mod worktree;

use std::process::ExitCode;

use krill_proto::agent::{self, Agents, Permission};
use krill_proto::daemon;

const USAGE: &str = "\
usage: krill [--host <user@host>] <command> [options]
  (--host <user@host> applies to session, worktree, bot, and orchestration commands)

Sessions:
  krill ls/new/fork/send/read/wait/attach | krill kill|close <id>
  krill search <query> [--agent <id>] [--path <worktree>] [--limit <n>] [--json]
  krill handoff --from <id>
  krill file open|diff <path>
  krill history clear
  krill ports

Worktrees & Git:
  krill worktree list/show/current/ps
  krill worktree new <name> [--parent <path>]
  krill worktree rm <path>
  krill worktree cleanup [--pr merged|closed] [--idle-days <N>]
  krill worktree space [--repo <path>]
  krill worktree set [--parent <path>|--clear-parent]
  krill worktree symlink <list|add|rm> [<path>]
  krill worktree hooks [--setup <cmd>] [--archive <cmd>]
  krill set --comment <text> [--parent <path>|--clear-parent]
  krill changes [--repo <path>]
  krill reference|ref list/add/remove/find

Bots, Orchestration & Automations:
  krill bot|bots ls/new/show/update/rm/start/templates/skills/apps/soul/memory/tidy/items/suggest/suggestions/up-next/schedules/sessions/offer/reply/send/groups/connectors/tracker-default/composio/access/secret/tools/avatar/export/import/import-external
  krill orchestration|orch <cmd>
  krill schedule|automations add/edit/run/rm/list/runs
  krill skills list/get/install
  krill artifacts|artifact init|lint|share|update|retention|unshare|status|list|delete

Mobile, Hosts & Drop:
  krill mobile pair/devices/revoke/serve/login/logout/status
  krill host status [<host>] [--json]
  krill drop <file>...|ls|rm <id> [--json]

Accounts, Agents & Hooks:
  krill account [list|switch|add|rm]
  krill agent list
  krill hook <agent>
  krill hook install|uninstall <claude|codex|agy|cursor|opencode|pi>
  krill statusline [--chain <command>] | krill statusline install|uninstall
  krill import orca [--dry-run] [--json] [--only accounts,ssh,projects,settings,sessions,bots]

Diagnostics & Maintenance:
  krill doctor [--bundle|--json [--no-start]]
  krill update [--check] [--app <path>] [--json]
  krill telemetry status|enable|disable|flush [--json]
  krill errors status|enable|disable|flush [--json]
  krill --version";

fn main() -> ExitCode {
    krill_proto::init_build_info(krill_version::VERSION, krill_version::BUILD);
    run(std::env::args_os().skip(1).collect())
}

pub(crate) fn normalize_host_flag(raw: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(raw.len() + 1);
    let mut after_double_dash = false;
    for arg in raw {
        if after_double_dash {
            out.push(arg);
            continue;
        }
        if arg == "--" {
            after_double_dash = true;
            out.push(arg);
            continue;
        }
        if let Some(s) = arg.to_str()
            && let Some(rest) = s.strip_prefix("--host=")
        {
            out.push(std::ffi::OsString::from("--host"));
            out.push(std::ffi::OsString::from(rest));
            continue;
        }
        out.push(arg);
    }
    out
}

fn run(raw: Vec<std::ffi::OsString>) -> ExitCode {
    let raw = normalize_host_flag(raw);
    if matches!(
        raw.as_slice(),
        [flag] if matches!(flag.to_str(), Some("-h" | "--help" | "help"))
    ) || matches!(
        raw.as_slice(),
        [host, _, flag]
            if host == "--host" && matches!(flag.to_str(), Some("-h" | "--help" | "help"))
    ) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if matches!(
        raw.as_slice(),
        [flag] if matches!(flag.to_str(), Some("-V" | "--version" | "version"))
    ) {
        println!("krill {}", krill_version::BUILD);
        return ExitCode::SUCCESS;
    }
    if raw.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    // 원격 SSH 중계: stdout이 IPC 바이트라 로그 초기화·스킬 동기화보다 먼저 처리한다.
    if raw.first().is_some_and(|s| s == "__bridge") {
        return remote::bridge_main(&raw);
    }
    // Hook stdout and its 50 ms budget take precedence over diagnostics.
    // statusline run mode is also tick-hot: no log init, like hooks.
    let hook_run = raw.first().is_some_and(|s| s == "hook")
        && !raw
            .get(1)
            .is_some_and(|s| s == "install" || s == "uninstall");
    let statusline_run = raw.first().is_some_and(|s| s == "statusline")
        && !raw
            .get(1)
            .is_some_and(|s| s == "install" || s == "uninstall");
    if !hook_run && !statusline_run {
        krill_log::init(krill_log::Component::Cli).ok();
        // 앱의 60초 `errors flush`, 에이전트의 `read` 폴링, 읽기 전용 `account list` 같은 호출마다 줄을 남기면
        // 5개 회전 파일이 몇 분 만에 밀려나 실제 진단이 사라진다(2026-11-05). 사람이 드물게 실행해 되짚을 가치가
        // 있는 명령만 이름과 함께 남긴다.
        if let Some(command) = logged_cli_command(&raw) {
            tracing::info!(command, "cli starting");
        }
        // Hook/statusline hot paths, read-only polling commands, and isolated runtimes must not
        // rescan all 8 agent skill roots on every invocation.
        if !cfg!(test)
            && daemon::state_dir_override().is_none()
            && should_sync_skills_for_command(&raw)
            && let Some(home) = krill_proto::os::home_dir()
        {
            let report = krill_proto::orchestration::sync_builtin_skills_at(&home);
            log_skill_sync_failures(&report.failures);
        }
    }
    if raw.first().is_some_and(|s| s == "__forward") {
        return remote::forward_main(&raw);
    }
    if raw.first().is_some_and(|s| s == "hook")
        && !raw
            .get(1)
            .is_some_and(|s| s == "install" || s == "uninstall")
    {
        return hook::run(
            raw.get(1).and_then(|s| s.to_str()),
            raw.get(2).and_then(|s| s.to_str()),
        );
    }
    if raw.first().is_some_and(|s| s == "statusline") {
        return statusline::run(&raw[1..]);
    }
    if raw.first().and_then(|s| s.to_str()).is_some_and(|s| {
        matches!(
            s,
            "ls" | "new" | "fork" | "send" | "read" | "wait" | "attach" | "kill" | "close"
        )
    }) || raw.first().is_some_and(|s| s == "--host")
        && raw.get(2).and_then(|s| s.to_str()).is_some_and(|s| {
            matches!(
                s,
                "ls" | "new" | "fork" | "send" | "read" | "wait" | "attach" | "kill" | "close"
            )
        })
    {
        return session::run(&raw);
    }
    if raw.first().is_some_and(|s| s == "--host") && raw.get(2).is_some_and(|s| s == "worktree") {
        let mut forwarded = vec![raw[0].clone(), raw[1].clone()];
        forwarded.extend_from_slice(&raw[3..]);
        return worktree::run(&forwarded);
    }
    if raw.first().is_some_and(|s| s == "--host")
        && raw
            .get(2)
            .is_some_and(|s| s == "orchestration" || s == "orch")
    {
        let mut forwarded = vec![raw[0].clone(), raw[1].clone()];
        forwarded.extend_from_slice(&raw[3..]);
        return orchestration::run(&forwarded);
    }
    if raw.first().is_some_and(|s| s == "--host")
        && raw.get(2).is_some_and(|s| s == "bot" || s == "bots")
    {
        let mut forwarded = vec![raw[0].clone(), raw[1].clone()];
        forwarded.extend_from_slice(&raw[3..]);
        return bot::run(&forwarded);
    }
    if raw.first().is_some_and(|s| s == "bot" || s == "bots") {
        return bot::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "mobile") {
        return mobile::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "host" || s == "hosts") {
        return host::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "drop") {
        return drop::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "search") {
        return search::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "history") {
        return history::run(&raw[1..]);
    }
    if raw
        .first()
        .is_some_and(|s| s == "schedule" || s == "automations" || s == "automation")
    {
        return schedule::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "changes") {
        return changes::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "file") {
        return file::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "worktree") {
        return worktree::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "set") {
        return set::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "telemetry") {
        return telemetry::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "errors") {
        return errors::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "update") {
        return update::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "account") {
        return account::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "import") {
        return import::run(&raw[1..]);
    }
    if raw
        .first()
        .is_some_and(|s| s == "orchestration" || s == "orch")
    {
        return orchestration::run(&raw[1..]);
    }
    if raw
        .first()
        .is_some_and(|s| s == "artifacts" || s == "artifact")
    {
        return artifacts::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "skills") {
        return skills::run_skills(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "handoff") {
        return skills::run_handoff(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "reference" || s == "ref") {
        return skills::run_reference(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "ports") {
        return skills::run_ports(&raw[1..]);
    }
    let Ok(args) = raw
        .into_iter()
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("arguments must be UTF-8");
        return ExitCode::from(2);
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["doctor"] => doctor::run(),
        ["doctor", "--bundle"] => doctor::bundle(),
        ["doctor", "--json"] => doctor::run_json(false),
        ["doctor", "--json", "--no-start"] | ["doctor", "--no-start", "--json"] => {
            doctor::run_json(true)
        }
        ["doctor", "-h" | "--help" | "help"]
        | ["doctor", _, "-h" | "--help" | "help"]
        | ["doctor", _, _, "-h" | "--help" | "help"] => {
            println!("usage: krill doctor [--bundle|--json [--no-start]]");
            ExitCode::SUCCESS
        }
        ["agent", "list"] => agent_list(),
        ["agent", "-h" | "--help" | "help"] | ["agent", _, "-h" | "--help" | "help"] => {
            println!("usage: krill agent list");
            ExitCode::SUCCESS
        }
        ["hook", action @ ("install" | "uninstall"), ref rest @ ..] => {
            hook_install::run(action, rest)
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// 프로필마다 PATH 감지·버전을 보이고, 없으면 설치 명령을 안내한다. 미설치는 실패가 아니다.
fn agent_list() -> ExitCode {
    let file = daemon::state_dir().map(|d| d.join(agent::FILE_NAME));
    let user = match file.as_deref().map(std::fs::read_to_string) {
        Some(Ok(text)) => Some(text),
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
            println!("profiles   FAIL: {e}");
            return ExitCode::FAILURE;
        }
        _ => None,
    };
    let agents = match Agents::load(user.as_deref()) {
        Ok(a) => a,
        Err(e) => {
            println!("profiles   FAIL: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(f) = &file {
        let state = if user.is_some() {
            ""
        } else {
            " (none, built-in only)"
        };
        println!("profiles   {}{state}", f.display());
    }
    let yolo = agents.permission == Permission::Yolo;
    println!(
        "permission {}",
        if yolo {
            "[YOLO] permission checks bypassed where supported"
        } else {
            "manual (agent default)"
        }
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    #[cfg(windows)]
    let path = krill_proto::os::augment_windows_path(Some(&path)).unwrap_or(path);
    let all_profiles = agents.all_profiles_with_presets();
    let mut rows: Vec<_> = all_profiles.iter().collect();
    rows.sort_by(|(a, _), (b, _)| {
        agent::profile_rank(a)
            .cmp(&agent::profile_rank(b))
            .then_with(|| a.cmp(b))
    });
    for (id, p) in rows {
        let exe = p.program().and_then(|prog| agent::which(prog, &path));
        let status = match &exe {
            Some(exe) => format!(
                "{}  {}",
                agent::version(exe).as_deref().unwrap_or("version unknown"),
                exe.display()
            ),
            None => {
                if !p.install_command().trim().is_empty() {
                    format!("not installed. install: {}", p.install_command())
                } else if let Some(url) = agent::agent_homepage(id) {
                    format!("not installed. docs: {url}")
                } else {
                    "not installed".to_owned()
                }
            }
        };
        let badge = match (yolo, p.yolo_args.is_empty()) {
            (false, _) => "",
            (true, false) => " [YOLO]",
            (true, true) => " (no YOLO flag)",
        };
        println!("{id:<10} {:<12} {status}{badge}", p.name);
    }
    ExitCode::SUCCESS
}

/// Return the command name to log for rare/mutating CLI invocations.
/// Read-only `account` queries (`account`, `account list`, `account history`, `account provisioning status`)
/// are omitted so periodic polling never rotates out diagnostic logs.
fn logged_cli_command(raw: &[std::ffi::OsString]) -> Option<&'static str> {
    let first = raw.first()?.to_str()?;
    match first {
        "account" => {
            let sub = raw.get(1).and_then(|s| s.to_str());
            let sub2 = raw.get(2).and_then(|s| s.to_str());
            match (sub, sub2) {
                (None | Some("list" | "history" | "--help" | "-h"), _)
                | (Some("provisioning"), Some("status" | "--help" | "-h") | None) => None,
                _ => Some("account"),
            }
        }
        "bot" => Some("bot"),
        "doctor" => Some("doctor"),
        "hook" => Some("hook"),
        "import" => Some("import"),
        "mobile" => Some("mobile"),
        "statusline" => Some("statusline"),
        "update" => Some("update"),
        _ => None,
    }
}

/// Only run host-wide built-in skill synchronization on setup/lifecycle CLI commands,
/// avoiding redundant 8-root filesystem scans on every `ls`/`read`/`send`/`account` invocation.
fn should_sync_skills_for_command(raw: &[std::ffi::OsString]) -> bool {
    raw.first().and_then(|s| s.to_str()).is_some_and(|cmd| {
        matches!(
            cmd,
            "skills" | "doctor" | "update" | "hook" | "import" | "new"
        )
    })
}

/// Emit at most one privacy-safe summary warning when built-in skill sync skips targets.
/// `krill-log` forbids filesystem paths and error `Display` strings at `tracing` call sites.
fn log_skill_sync_failures(failures: &[String]) {
    let skipped_count = failures.len();
    if skipped_count > 0 {
        tracing::warn!(
            skipped_count,
            "built-in skill synchronization skipped target(s)"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fmt;
    use std::process::ExitCode;
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    use super::{USAGE, log_skill_sync_failures, run};

    #[derive(Default)]
    struct RecordedEvents {
        lines: Vec<String>,
    }

    struct CaptureVisitor<'a> {
        out: &'a mut String,
    }

    impl Visit for CaptureVisitor<'_> {
        fn record_u64(&mut self, field: &Field, value: u64) {
            fmt::Write::write_fmt(self.out, format_args!("{}={value} ", field.name())).ok();
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            fmt::Write::write_fmt(self.out, format_args!("{}={value:?} ", field.name())).ok();
        }
    }

    struct CaptureSubscriber(Arc<Mutex<RecordedEvents>>);

    impl Subscriber for CaptureSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut line = String::new();
            let mut visitor = CaptureVisitor { out: &mut line };
            event.record(&mut visitor);
            if let Ok(mut guard) = self.0.lock() {
                guard.lines.push(line);
            }
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    #[test]
    fn skill_sync_warning_logs_count_without_paths_or_error_display() {
        let recorded = Arc::new(Mutex::new(RecordedEvents::default()));
        let subscriber = CaptureSubscriber(Arc::clone(&recorded));
        let failures = vec![
            "artifacts (/Users/alice/.claude/skills): Operation not permitted (os error 1)"
                .to_owned(),
            "krill (/Users/alice/.codex/skills): Permission denied (os error 13)".to_owned(),
        ];
        tracing::subscriber::with_default(subscriber, || {
            log_skill_sync_failures(&[]);
            log_skill_sync_failures(&failures);
        });
        let lines = recorded.lock().unwrap().lines.clone();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("skipped_count=2"));
        assert!(!lines[0].contains("/Users/alice"));
        assert!(!lines[0].contains("Operation not permitted"));
        assert!(!lines[0].contains("Permission denied"));
    }

    #[test]
    fn top_level_help_exits_zero_and_empty_args_exits_two() {
        for flag in ["-h", "--help", "help"] {
            assert_eq!(run(vec![OsString::from(flag)]), ExitCode::SUCCESS, "{flag}");
            assert_eq!(
                run(vec![
                    OsString::from("--host"),
                    OsString::from("prod"),
                    OsString::from(flag),
                ]),
                ExitCode::SUCCESS,
                "--host prod {flag}"
            );
        }
        assert_eq!(run(Vec::new()), ExitCode::from(2));
        for flag in ["-V", "--version", "version"] {
            assert_eq!(run(vec![OsString::from(flag)]), ExitCode::SUCCESS, "{flag}");
        }
    }

    #[test]
    fn subcommand_help_delegates_to_modules() {
        for argv in [
            vec!["doctor", "--help"],
            vec!["agent", "list", "--help"],
            vec!["history", "--help"],
            vec!["update", "--help"],
            vec!["search", "--help"],
            vec!["mobile", "pair", "--help"],
            vec!["host", "--help"],
            vec!["drop", "--help"],
            vec!["ls", "--help"],
            vec!["new", "--help"],
            vec!["send", "--help"],
            vec!["read", "--help"],
            vec!["wait", "--help"],
            vec!["kill", "--help"],
            vec!["orchestration", "--help"],
            vec!["statusline", "--help"],
            vec!["telemetry", "--help"],
            vec!["errors", "--help"],
            vec!["account", "--help"],
            vec!["artifacts", "--help"],
            vec!["schedule", "--help"],
            vec!["changes", "--help"],
            vec!["file", "--help"],
            vec!["worktree", "--help"],
            vec!["set", "--help"],
            vec!["import", "--help"],
            vec!["bot", "--help"],
            vec!["skills", "--help"],
            vec!["handoff", "--help"],
            vec!["reference", "--help"],
            vec!["ports", "--help"],
        ] {
            let raw = argv.into_iter().map(OsString::from).collect::<Vec<_>>();
            assert_eq!(run(raw.clone()), ExitCode::SUCCESS, "{raw:?}");
        }
        // send -- --help는 리터럴이므로 도움말이 아니다(세션 모듈 단위 테스트에서 보장).
    }

    #[test]
    fn usage_lists_m10_worktree_and_set_subcommands() {
        for expected in [
            "krill set --comment <text> [--parent <path>|--clear-parent]",
            "krill worktree new <name>",
            "krill worktree rm <path>",
            "krill worktree cleanup",
            "krill worktree space",
            "krill worktree set",
            "krill worktree symlink",
            "krill worktree hooks",
        ] {
            assert!(
                USAGE.contains(expected),
                "missing {expected:?} in main USAGE: {USAGE}"
            );
        }
    }

    #[test]
    fn host_equals_form_is_normalized_for_help_and_bot() {
        assert_eq!(
            run(vec![
                OsString::from("--host=prod"),
                OsString::from("--help")
            ]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run(vec![
                OsString::from("--host=prod"),
                OsString::from("bot"),
                OsString::from("--help"),
            ]),
            ExitCode::SUCCESS
        );
        let normalized = super::normalize_host_flag(vec![
            OsString::from("--host=alice@box"),
            OsString::from("send"),
            OsString::from("--"),
            OsString::from("--host=literal"),
        ]);
        assert_eq!(
            normalized,
            vec![
                OsString::from("--host"),
                OsString::from("alice@box"),
                OsString::from("send"),
                OsString::from("--"),
                OsString::from("--host=literal"),
            ]
        );
    }

    #[test]
    fn readonly_account_queries_skip_cli_start_log_and_skill_sync() {
        let args = |xs: &[&str]| xs.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(super::logged_cli_command(&args(&["account"])), None);
        assert_eq!(
            super::logged_cli_command(&args(&["account", "list", "--json"])),
            None
        );
        assert_eq!(
            super::logged_cli_command(&args(&["account", "history"])),
            None
        );
        assert_eq!(
            super::logged_cli_command(&args(&["account", "provisioning", "status"])),
            None
        );
        assert_eq!(
            super::logged_cli_command(&args(&["account", "switch", "claude", "work"])),
            Some("account")
        );
        assert_eq!(
            super::logged_cli_command(&args(&["doctor", "--json"])),
            Some("doctor")
        );

        assert!(!super::should_sync_skills_for_command(&args(&[
            "account", "list"
        ])));
        assert!(!super::should_sync_skills_for_command(&args(&["ls"])));
        assert!(super::should_sync_skills_for_command(&args(&["doctor"])));
        assert!(super::should_sync_skills_for_command(&args(&["skills"])));
        assert!(super::should_sync_skills_for_command(&args(&["new"])));
    }
}
