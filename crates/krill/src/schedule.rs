// Automation design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
// Shared schedule/preset and automation control behavior; independently implemented for Krill.
//! MK-101: atomically edit daemon schedule configuration and inspect launch history.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::process::ExitCode;

use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::ClientRole;
use krill_proto::schedule::{
    Schedule, SchedulePreset, Schedules, WorkspaceMode, build_preset_cron, request_run_now,
    slugify_schedule_id,
};

const USAGE: &str = "usage: krill schedule|automations create|add [<id>|--id <id>] [--name <name>] --cron|--schedule <expr> | --trigger <hourly|daily|weekdays|weekly|cron|rrule> [--time HH:MM] [--day 0..6] [--cwd|--workdir|--repo|--workspace <path>] [--agent|--provider <profile> | -- <executable> [args...]] [--prompt <text>] [--extra-agent-args <args>|--clear-extra-agent-args] [--precheck <cmd>|--clear-precheck] [--precheck-timeout <seconds>] [--workspace-mode <reuse|isolate>] [--base-branch <branch>|--clear-base-branch] [--reuse-session|--fresh-session] [--missed-run-grace-minutes <minutes>] [--enabled|--disabled] [--json]; edit <id>|--id <id> accepts the same flags; show|enable|disable|run|remove|rm <id> [--json]; list|ls [--json]; runs [--id <id>] [--json]. Schedules use UTC.";

pub fn run(raw: &[OsString]) -> ExitCode {
    match execute(raw) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("schedule: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("schedule: {e}");
            ExitCode::FAILURE
        }
    }
}

fn execute(raw: &[OsString]) -> io::Result<()> {
    let args = raw
        .iter()
        .map(|s| {
            s.to_str()
                .ok_or_else(|| usage_error("arguments must be UTF-8"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let dir = daemon::state_dir().ok_or_else(|| error("state directory unavailable"))?;
    execute_in(&dir, &args, true)
}

fn execute_in(dir: &std::path::Path, args: &[&str], connect_daemon: bool) -> io::Result<()> {
    // `--` 뒤 `<executable> [args...]`의 `--help`는 검색어가 아니라 해당 명령의 도움말 요청이 아니라
    // 스케줄 실행 대상의 인자이므로 도움말로 삼키지 않는다. 첫 토큰(서브커맨드 자리)만 본다.
    let head = args.split(|a| *a == "--").next().unwrap_or(&[]);
    if head.len() <= 2 && head.iter().any(|a| matches!(*a, "-h" | "--help" | "help")) {
        println!("{USAGE}");
        return Ok(());
    }
    match args {
        ["list" | "ls"] => {
            let schedules = Schedules::load(dir)?;
            println!("{}", toml::to_string(&schedules).map_err(error)?);
        }
        ["list" | "ls", "--json"] => {
            let schedules = Schedules::load(dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&schedules.schedule).map_err(error)?
            );
        }
        ["show", id] | ["show", id, "--json"] | ["show", "--json", id] if *id != "--json" => {
            let json = args.contains(&"--json");
            let schedules = Schedules::load(dir)?;
            let Some(target) = schedules.schedule.iter().find(|s| s.id == *id) else {
                return Err(error("unknown schedule ID"));
            };
            if json {
                println!("{}", serde_json::to_string_pretty(target).map_err(error)?);
            } else {
                println!("{}", toml::to_string(target).map_err(error)?);
            }
        }
        ["enable", id] | ["enable", id, "--json"] | ["enable", "--json", id] if *id != "--json" => {
            let json = args.contains(&"--json");
            set_enabled(dir, id, true, json)?;
        }
        ["disable", id] | ["disable", id, "--json"] | ["disable", "--json", id]
            if *id != "--json" =>
        {
            let json = args.contains(&"--json");
            set_enabled(dir, id, false, json)?;
        }
        ["runs", rest @ ..] => {
            let mut id = None;
            let mut json = false;
            let mut flags = rest.iter();
            while let Some(flag) = flags.next() {
                match *flag {
                    "--id" => id = Some(*flags.next().ok_or_else(|| usage_error(USAGE))?),
                    "--json" => json = true,
                    _ => return Err(usage_error(USAGE)),
                }
            }
            let mut runs = if json {
                krill_proto::schedule::load_coalesced_runs(dir)?
            } else {
                krill_proto::schedule::load_runs(dir)?
            };
            runs.retain(|run| id.is_none_or(|id| run.id == id));
            if json {
                println!("{}", serde_json::to_string_pretty(&runs).map_err(error)?);
            } else {
                for run in runs {
                    println!("{}", serde_json::to_string(&run).map_err(error)?);
                }
            }
        }
        ["add" | "create", rest @ ..] => {
            let _guard = Schedules::exclusive_edit(dir)?;
            let mut schedules = Schedules::load(dir)?;
            let (id, flags) = extract_id(rest)?;
            let (mut entry, json) = add(id.unwrap_or("automation"), &flags)?;
            if id.is_none() {
                let name = entry
                    .name
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| usage_error("create requires an ID or --name"))?;
                entry.id = slugify_schedule_id(
                    name,
                    &schedules
                        .schedule
                        .iter()
                        .map(|s| s.id.clone())
                        .collect::<Vec<_>>(),
                );
            }
            let id = &entry.id;
            if schedules.schedule.iter().any(|s| s.id == *id) {
                return Err(error(
                    "schedule ID already exists; remove it before replacing",
                ));
            }
            schedules.schedule.push(entry.clone());
            schedules.save(dir)?;
            if connect_daemon {
                // Activate the daemon so reservations work with the app closed.
                let paths = Paths::dev().map_err(error)?;
                daemon::connect(
                    &paths,
                    &daemon::client_hello(ClientRole::Cli, "mk-schedule"),
                )
                .map_err(|e| error(format!("schedule saved, daemon startup failed: {e}")))?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&entry).map_err(error)?);
            } else {
                println!("saved {id} (UTC)");
            }
        }
        ["edit", rest @ ..] => {
            let (id, flags) = extract_id(rest)?;
            let id = id.ok_or_else(|| usage_error("edit requires an ID"))?;
            let _guard = Schedules::exclusive_edit(dir)?;
            let mut schedules = Schedules::load(dir)?;
            let Some(target) = schedules.schedule.iter_mut().find(|s| s.id == id) else {
                return Err(error("unknown schedule ID"));
            };
            let json = apply_edit(target, &flags)?;
            let updated = target.clone();
            schedules.save(dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&updated).map_err(error)?);
            } else {
                println!("updated {id} (UTC)");
            }
        }
        ["run", id] | ["run", id, "--json"] | ["run", "--json", id] if *id != "--json" => {
            let json = args.contains(&"--json");
            let schedules = Schedules::load(dir)?;
            if !schedules.schedule.iter().any(|s| s.id == *id) {
                return Err(usage_error("unknown schedule ID"));
            }
            if connect_daemon {
                let paths = Paths::dev().map_err(|e| error(format!("daemon required: {e}")))?;
                daemon::connect(
                    &paths,
                    &daemon::client_hello(ClientRole::Cli, "mk-schedule"),
                )
                .map_err(|e| error(format!("daemon required: {e}")))?;
            }
            request_run_now(dir, id)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "id": id,
                        "triggered": true,
                    }))
                    .map_err(error)?
                );
            } else {
                println!("triggered {id}");
            }
        }
        ["rm" | "remove", id]
        | ["rm" | "remove", id, "--json"]
        | ["rm" | "remove", "--json", id]
            if *id != "--json" =>
        {
            let json = args.contains(&"--json");
            let _guard = Schedules::exclusive_edit(dir)?;
            let mut schedules = Schedules::load(dir)?;
            let before = schedules.schedule.len();
            schedules.schedule.retain(|s| s.id != *id);
            if before == schedules.schedule.len() {
                return Err(error("unknown schedule ID"));
            }
            schedules.save(dir)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "id": id,
                        "removed": true,
                    }))
                    .map_err(error)?
                );
            } else {
                println!("removed {id}");
            }
        }
        _ => return Err(usage_error(USAGE)),
    }
    Ok(())
}

fn set_enabled(dir: &std::path::Path, id: &str, enabled: bool, json: bool) -> io::Result<()> {
    let _guard = Schedules::exclusive_edit(dir)?;
    let mut schedules = Schedules::load(dir)?;
    let Some(target) = schedules.schedule.iter_mut().find(|s| s.id == id) else {
        return Err(error("unknown schedule ID"));
    };
    target.enabled = enabled;
    let updated = target.clone();
    schedules.save(dir)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&updated).map_err(error)?);
    } else if enabled {
        println!("enabled {id}");
    } else {
        println!("disabled {id}");
    }
    Ok(())
}

fn extract_id<'a>(args: &[&'a str]) -> io::Result<(Option<&'a str>, Vec<&'a str>)> {
    let mut id = None;
    let mut flags = Vec::new();
    let mut args = args.iter().copied().peekable();
    if args.peek().is_some_and(|arg| !arg.starts_with('-')) {
        id = args.next();
    }
    while let Some(arg) = args.next() {
        if arg == "--" {
            flags.push(arg);
            flags.extend(args);
            break;
        }
        if arg == "--id" {
            if id.is_some() {
                return Err(usage_error("ID supplied more than once"));
            }
            id = Some(args.next().ok_or_else(|| usage_error(USAGE))?);
        } else {
            flags.push(arg);
            if matches!(
                arg,
                "--name"
                    | "--cron"
                    | "--schedule"
                    | "--trigger"
                    | "--time"
                    | "--day"
                    | "--cwd"
                    | "--workdir"
                    | "--repo"
                    | "--workspace"
                    | "--prompt"
                    | "--precheck"
                    | "--workspace-mode"
                    | "--agent"
                    | "--provider"
                    | "--extra-agent-args"
                    | "--precheck-timeout"
                    | "--base-branch"
                    | "--missed-run-grace-minutes"
            ) {
                flags.push(args.next().ok_or_else(|| usage_error(USAGE))?);
            }
        }
    }
    Ok((id, flags))
}

fn add(id: &str, args: &[&str]) -> io::Result<(Schedule, bool)> {
    let mut entry = Schedule {
        id: id.into(),
        cwd: std::env::current_dir()?
            .into_os_string()
            .into_string()
            .map_err(|_| error("cwd must be UTF-8"))?,
        ..Default::default()
    };
    let json = parse_entry_flags(&mut entry, args)?;
    entry.validate().map_err(usage_error)?;
    Ok((entry, json))
}

fn apply_edit(entry: &mut Schedule, args: &[&str]) -> io::Result<bool> {
    if args.is_empty() || args.iter().all(|a| *a == "--json") {
        return Err(usage_error(USAGE));
    }
    let json = parse_entry_flags(entry, args)?;
    entry.validate().map_err(usage_error)?;
    Ok(json)
}

fn parse_entry_flags(entry: &mut Schedule, args: &[&str]) -> io::Result<bool> {
    let mut json = false;
    let mut trigger = None;
    let mut time = None;
    let mut day = None;
    let mut expression = None;
    let mut args = args.iter().copied();
    while let Some(arg) = args.next() {
        match arg {
            "--" => {
                entry.command = args.map(str::to_owned).collect();
                entry.agent = None;
                break;
            }
            "--json" => json = true,
            "--isolate" | "--new-worktree" => entry.workspace_mode = WorkspaceMode::Isolate,
            "--no-new-worktree" | "--reuse" => entry.workspace_mode = WorkspaceMode::Reuse,
            "--enable" | "--enabled" => entry.enabled = true,
            "--disable" | "--disabled" => entry.enabled = false,
            "--reuse-session" => entry.reuse_session = true,
            "--fresh-session" => entry.reuse_session = false,
            "--clear-precheck" => entry.precheck = None,
            "--clear-extra-agent-args" => entry.extra_agent_args = None,
            "--clear-base-branch" => entry.base_branch = None,
            flag @ ("--name"
            | "--cron"
            | "--schedule"
            | "--trigger"
            | "--time"
            | "--day"
            | "--cwd"
            | "--workdir"
            | "--repo"
            | "--workspace"
            | "--prompt"
            | "--precheck"
            | "--workspace-mode"
            | "--agent"
            | "--provider"
            | "--extra-agent-args"
            | "--precheck-timeout"
            | "--base-branch"
            | "--missed-run-grace-minutes") => {
                let value = args.next().ok_or_else(|| usage_error(USAGE))?;
                let optional = || (!value.trim().is_empty()).then(|| value.to_owned());
                match flag {
                    "--name" => entry.name = optional(),
                    "--cron" | "--schedule" => expression = Some(value),
                    "--trigger" => trigger = Some(value),
                    "--time" => time = Some(value),
                    "--day" => day = Some(value),
                    "--cwd" | "--workdir" | "--repo" | "--workspace" => {
                        entry.cwd = fs::canonicalize(value)?
                            .into_os_string()
                            .into_string()
                            .map_err(|_| error("cwd must be UTF-8"))?;
                    }
                    "--prompt" => entry.prompt = value.into(),
                    "--precheck" => {
                        if value.trim().is_empty() {
                            return Err(usage_error("empty precheck; use --clear-precheck"));
                        }
                        entry.precheck = optional();
                    }
                    "--workspace-mode" => {
                        entry.workspace_mode = match value {
                            "reuse" | "existing" => WorkspaceMode::Reuse,
                            "isolate" | "isolated" | "new_worktree" | "new_per_run" => {
                                WorkspaceMode::Isolate
                            }
                            _ => return Err(usage_error(USAGE)),
                        }
                    }
                    "--agent" | "--provider" => {
                        entry.agent = Some(value.into());
                        entry.command = vec![value.into()];
                    }
                    "--extra-agent-args" => entry.extra_agent_args = optional(),
                    "--base-branch" => entry.base_branch = optional(),
                    "--precheck-timeout" => {
                        entry.precheck_timeout_secs = Some(value.parse().map_err(usage_error)?);
                    }
                    "--missed-run-grace-minutes" => {
                        entry.missed_run_grace_minutes = Some(value.parse().map_err(usage_error)?);
                    }
                    _ => return Err(usage_error(USAGE)),
                }
            }
            _ => return Err(usage_error(USAGE)),
        }
    }
    if let Some(trigger) = trigger {
        let preset = match trigger {
            "hourly" => SchedulePreset::Hourly,
            "daily" => SchedulePreset::Daily,
            "weekdays" => SchedulePreset::Weekdays,
            "weekly" => SchedulePreset::Weekly,
            "cron" | "rrule" => SchedulePreset::Custom,
            _ => return Err(usage_error("invalid trigger")),
        };
        if (time.is_some() && matches!(preset, SchedulePreset::Hourly | SchedulePreset::Custom))
            || (day.is_some() && preset != SchedulePreset::Weekly)
        {
            return Err(usage_error(
                "--time requires daily/weekdays/weekly; --day requires weekly",
            ));
        }
        if preset == SchedulePreset::Custom {
            entry.cron = expression
                .ok_or_else(|| usage_error("cron/rrule trigger requires --schedule"))?
                .into();
        } else {
            if expression.is_some() {
                return Err(usage_error(
                    "preset trigger and explicit schedule are mutually exclusive",
                ));
            }
            let (hour, minute) = time
                .unwrap_or("09:00")
                .split_once(':')
                .ok_or_else(|| usage_error("time must be HH:MM"))?;
            let hour = hour.parse::<u8>().map_err(usage_error)?;
            let minute = if preset == SchedulePreset::Hourly {
                0
            } else {
                minute.parse::<u8>().map_err(usage_error)?
            };
            let weekday = match day.unwrap_or("1").to_ascii_lowercase().as_str() {
                "sun" | "sunday" => 0,
                "mon" | "monday" => 1,
                "tue" | "tuesday" => 2,
                "wed" | "wednesday" => 3,
                "thu" | "thursday" => 4,
                "fri" | "friday" => 5,
                "sat" | "saturday" => 6,
                n => n.parse().map_err(usage_error)?,
            };
            entry.cron = build_preset_cron(preset, hour, minute, weekday).map_err(usage_error)?;
        }
    } else {
        if time.is_some() || day.is_some() {
            return Err(usage_error("--time/--day requires --trigger"));
        }
        if let Some(expr) = expression {
            entry.cron = expr.into();
        }
    }
    Ok(json)
}

fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn usage_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_aliases_name_ids_presets_and_new_flags_roundtrip() {
        let dir = std::env::temp_dir().join(format!("r504-cli-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let flags = [
            "create",
            "--name",
            "Daily Audit",
            "--provider",
            "claude",
            "--trigger",
            "weekly",
            "--time",
            "08:35",
            "--day",
            "friday",
            "--extra-agent-args",
            "--model 'two words'",
            "--base-branch",
            "dev",
            "--precheck-timeout",
            "123",
            "--missed-run-grace-minutes",
            "720",
            "--disabled",
            "--json",
        ];
        execute_in(&dir, &flags, false).unwrap();
        execute_in(&dir, &flags, false).unwrap();
        let saved = Schedules::load(&dir).unwrap();
        assert_eq!(saved.schedule[0].id, "daily-audit");
        assert_eq!(saved.schedule[1].id, "daily-audit-2");
        let entry = &saved.schedule[0];
        assert_eq!(entry.name.as_deref(), Some("Daily Audit"));
        assert_eq!(entry.cron, "35 8 * * 5");
        assert_eq!(entry.agent.as_deref(), Some("claude"));
        assert_eq!(entry.precheck_timeout_secs, Some(123));
        assert_eq!(entry.missed_run_grace_minutes, Some(720));
        assert!(!entry.enabled);
        execute_in(
            &dir,
            &[
                "edit",
                "--id",
                "daily-audit",
                "--clear-extra-agent-args",
                "--clear-base-branch",
                "--reuse-session",
            ],
            false,
        )
        .unwrap();
        let saved = Schedules::load(&dir).unwrap();
        assert!(saved.schedule[0].reuse_session);
        assert!(saved.schedule[0].extra_agent_args.is_none());
        assert!(saved.schedule[0].base_branch.is_none());
        execute_in(&dir, &["ls", "--json"], false).unwrap();
        execute_in(&dir, &["runs", "--id", "daily-audit", "--json"], false).unwrap();
        execute_in(&dir, &["runs", "--id", "daily-audit"], false).unwrap();
        execute_in(&dir, &["remove", "daily-audit-2"], false).unwrap();
        assert!(
            execute_in(
                &dir,
                &[
                    "create",
                    "--name",
                    "bad",
                    "--agent",
                    "claude",
                    "--trigger",
                    "hourly",
                    "--time",
                    "09:00"
                ],
                false
            )
            .is_err()
        );
        assert!(
            execute_in(
                &dir,
                &[
                    "create",
                    "--name",
                    "bad",
                    "--agent",
                    "claude",
                    "--trigger",
                    "daily",
                    "--schedule",
                    "0 9 * * *"
                ],
                false
            )
            .is_err()
        );
        assert!(
            execute_in(
                &dir,
                &[
                    "create",
                    "--id",
                    "explicit",
                    "--agent",
                    "codex",
                    "--trigger",
                    "rrule",
                    "--schedule",
                    "FREQ=DAILY;BYHOUR=9",
                    "--fresh-session"
                ],
                false
            )
            .is_ok()
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn help_returns_usage_without_daemon() {
        let dir = std::env::temp_dir().join(format!("krill-sched-help-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();
        assert!(execute_in(&dir, &["--help"], false).is_ok());
        assert!(execute_in(&dir, &["-h"], false).is_ok());
        drop(fs::remove_dir_all(&dir));
    }

    #[test]
    fn schedule_add_edit_show_enable_disable_run_and_rm_preserve_fields() {
        let dir = std::env::temp_dir().join(format!("krill-sched-cli-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();

        execute_in(
            &dir,
            &[
                "add",
                "nightly",
                "--cron",
                "0 2 * * *",
                "--workdir",
                dir.to_str().unwrap(),
                "--prompt",
                "check status",
                "--agent",
                "claude",
                "--json",
            ],
            false,
        )
        .unwrap();

        let loaded = Schedules::load(&dir).unwrap();
        assert_eq!(loaded.schedule.len(), 1);
        assert_eq!(loaded.schedule[0].id, "nightly");
        assert_eq!(loaded.schedule[0].command, vec!["claude"]);
        assert!(loaded.schedule[0].enabled);

        execute_in(&dir, &["show", "nightly", "--json"], false).unwrap();

        execute_in(
            &dir,
            &[
                "edit",
                "nightly",
                "--cron",
                "15 3 * * *",
                "--prompt",
                "updated prompt",
                "--precheck",
                "git status",
                "--new-worktree",
                "--disabled",
                "--json",
            ],
            false,
        )
        .unwrap();

        let edited = Schedules::load(&dir).unwrap();
        assert_eq!(edited.schedule[0].cron, "15 3 * * *");
        assert_eq!(edited.schedule[0].prompt, "updated prompt");
        assert_eq!(edited.schedule[0].precheck.as_deref(), Some("git status"));
        assert_eq!(edited.schedule[0].workspace_mode, WorkspaceMode::Isolate);
        assert!(!edited.schedule[0].enabled);

        execute_in(
            &dir,
            &[
                "edit",
                "nightly",
                "--clear-precheck",
                "--no-new-worktree",
                "--enabled",
            ],
            false,
        )
        .unwrap();
        let cleared = Schedules::load(&dir).unwrap();
        assert!(cleared.schedule[0].precheck.is_none());
        assert_eq!(cleared.schedule[0].workspace_mode, WorkspaceMode::Reuse);
        assert!(cleared.schedule[0].enabled);

        execute_in(&dir, &["disable", "nightly", "--json"], false).unwrap();
        assert!(!Schedules::load(&dir).unwrap().schedule[0].enabled);
        execute_in(&dir, &["enable", "nightly", "--json"], false).unwrap();
        assert!(Schedules::load(&dir).unwrap().schedule[0].enabled);

        execute_in(&dir, &["run", "nightly", "--json"], false).unwrap();
        assert_eq!(
            krill_proto::schedule::take_run_now(&dir).unwrap(),
            vec!["nightly"]
        );

        execute_in(&dir, &["rm", "nightly", "--json"], false).unwrap();
        assert!(Schedules::load(&dir).unwrap().schedule.is_empty());

        drop(fs::remove_dir_all(&dir));
    }
}
