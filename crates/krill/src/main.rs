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
mod changes;
mod computer;
mod doctor;
mod file;
mod history;
mod hook;
mod hook_install;
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

const USAGE: &str = "usage: krill mobile pair/devices/revoke/serve | krill ls/new/fork/send/read/wait/attach | krill kill <id> | krill file open|diff <path> | krill search <query> [--agent <id>] [--path <worktree>] [--limit <n>] [--json] | krill import orca [--dry-run] [--json] [--only accounts,ssh,projects,settings] | krill orchestration <cmd> | krill skills list/get/install | krill artifacts share|update|unshare|list|delete | krill computer <cmd> | krill handoff --from <id> | krill reference list/add/remove/find | krill ports | krill schedule add/rm/list/runs | krill account [list|switch|add|rm|auto-switch] | krill set --comment <text> | krill telemetry status|enable|disable | krill update [--check] [--app <path>] [--json] | krill history clear | krill doctor [--bundle|--json [--no-start]] | krill changes [--repo <path>] | krill agent list | krill hook <agent> | krill hook install|uninstall <claude|codex|agy|cursor|opencode> | krill statusline [--chain <command>] | krill statusline install|uninstall | krill worktree new <name> | krill worktree rm <path>";

fn main() -> ExitCode {
    let raw: Vec<_> = std::env::args_os().skip(1).collect();
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
        // MK-170: 경로를 정하기 전에 옛 번들 id 데이터를 옮긴다(옛 holder가 살아 있으면 그대로 둔다).
        let migrated = daemon::migrate::run();
        krill_log::init(krill_log::Component::Cli).ok();
        tracing::info!(?migrated, "cli starting");
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
    if raw.first().is_some_and(|s| s == "mobile") {
        return mobile::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "search") {
        return search::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "history") {
        return history::run(&raw[1..]);
    }
    if raw.first().is_some_and(|s| s == "schedule") {
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
    if raw.first().is_some_and(|s| s == "computer") {
        return computer::run(&raw[1..]);
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
        ["agent", "list"] => agent_list(),
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
    let mut rows: Vec<_> = agents.profiles.iter().collect();
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
            None => format!("not installed. install: {}", p.install),
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
