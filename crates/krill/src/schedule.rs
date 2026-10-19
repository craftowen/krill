//! MK-101: atomically edit daemon schedule configuration and inspect launch history.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::process::ExitCode;

use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::ClientRole;
use krill_proto::schedule::{
    MAX_RUNS, PREVIOUS_RUNS, RUNS, Schedule, Schedules, WorkspaceMode, request_run_now,
};

const USAGE: &str = "usage: krill schedule add <id> --cron '<minute hour day month weekday>' --prompt <text> [--cwd|--workdir <path>] [--precheck <cmd>] [--isolate|--new-worktree|--no-new-worktree] [--workspace-mode <reuse|isolate>] [--enable|--enabled|--disable|--disabled] [--agent <profile> | -- <executable> [args...]] [--json] | krill schedule edit <id> [--cron <expr>] [--prompt <text>] [--cwd|--workdir <path>] [--precheck <cmd>|--clear-precheck] [--isolate|--new-worktree|--no-new-worktree] [--workspace-mode <reuse|isolate>] [--enable|--enabled|--disable|--disabled] [--json] | krill schedule show <id> [--json] | krill schedule enable <id> [--json] | krill schedule disable <id> [--json] | krill schedule run <id> [--json] | krill schedule rm <id> [--json] | krill schedule list [--json] | krill schedule runs [--json] (cron is UTC; prompt is the last command argument)";

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
    match args {
        ["list"] => {
            let schedules = Schedules::load(dir)?;
            println!("{}", toml::to_string(&schedules).map_err(error)?);
        }
        ["list", "--json"] => {
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
        ["runs"] => {
            for name in [PREVIOUS_RUNS, RUNS] {
                match File::open(dir.join(name)) {
                    Ok(file) => {
                        let mut bytes = Vec::new();
                        file.take(MAX_RUNS.saturating_add(4096))
                            .read_to_end(&mut bytes)?;
                        for line in bytes.split_inclusive(|b| *b == b'\n') {
                            if line.last() == Some(&b'\n') {
                                io::stdout().lock().write_all(line)?;
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        ["runs", "--json"] => {
            let runs = krill_proto::schedule::load_coalesced_runs(dir)?;
            println!("{}", serde_json::to_string_pretty(&runs).map_err(error)?);
        }
        ["add", id, rest @ ..] => {
            let (entry, json) = add(id, rest)?;
            let _guard = Schedules::exclusive_edit(dir)?;
            let mut schedules = Schedules::load(dir)?;
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
        ["edit", id, rest @ ..] => {
            let _guard = Schedules::exclusive_edit(dir)?;
            let mut schedules = Schedules::load(dir)?;
            let Some(target) = schedules.schedule.iter_mut().find(|s| s.id == *id) else {
                return Err(error("unknown schedule ID"));
            };
            let json = apply_edit(target, rest)?;
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
        ["rm", id] | ["rm", id, "--json"] | ["rm", "--json", id] if *id != "--json" => {
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

fn add(id: &str, args: &[&str]) -> io::Result<(Schedule, bool)> {
    let mut entry = Schedule {
        id: id.into(),
        cron: String::new(),
        cwd: std::env::current_dir()?
            .into_os_string()
            .into_string()
            .map_err(|_| error("cwd must be UTF-8"))?,
        command: Vec::new(),
        prompt: String::new(),
        precheck: None,
        workspace_mode: WorkspaceMode::Reuse,
        enabled: true,
    };
    let mut json = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "--" => {
                entry.command = args.map(|s| (*s).to_owned()).collect();
                break;
            }
            "--json" => json = true,
            "--isolate" | "--new-worktree" => {
                entry.workspace_mode = WorkspaceMode::Isolate;
            }
            "--no-new-worktree" | "--reuse" => {
                entry.workspace_mode = WorkspaceMode::Reuse;
            }
            "--enable" | "--enabled" => entry.enabled = true,
            "--disable" | "--disabled" => entry.enabled = false,
            flag @ ("--cron" | "--cwd" | "--workdir" | "--prompt" | "--precheck"
            | "--workspace-mode" | "--agent") => {
                let value = args.next().ok_or_else(|| usage_error(USAGE))?;
                match flag {
                    "--cron" => entry.cron = (*value).into(),
                    "--cwd" | "--workdir" => {
                        entry.cwd = fs::canonicalize(value)?
                            .into_os_string()
                            .into_string()
                            .map_err(|_| error("cwd must be UTF-8"))?;
                    }
                    "--precheck" => {
                        let trimmed = value.trim();
                        if trimmed.is_empty() {
                            return Err(usage_error(USAGE));
                        }
                        entry.precheck = Some(trimmed.to_owned());
                    }
                    "--workspace-mode" => {
                        entry.workspace_mode = match *value {
                            "reuse" | "existing" => WorkspaceMode::Reuse,
                            "isolate" | "isolated" | "new_worktree" => WorkspaceMode::Isolate,
                            _ => return Err(usage_error(USAGE)),
                        };
                    }
                    "--agent" => {
                        let trimmed = value.trim();
                        if trimmed.is_empty() {
                            return Err(usage_error(USAGE));
                        }
                        entry.command = vec![trimmed.to_owned()];
                    }
                    _ => entry.prompt = (*value).into(),
                }
            }
            _ => return Err(usage_error(USAGE)),
        }
    }
    if entry.cron.is_empty() || entry.command.is_empty() {
        return Err(usage_error(USAGE));
    }
    Ok((entry, json))
}

fn apply_edit(entry: &mut Schedule, args: &[&str]) -> io::Result<bool> {
    if args.is_empty() {
        return Err(usage_error(USAGE));
    }
    let mut json = false;
    let mut changed = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "--" => {
                let cmd: Vec<String> = args.map(|s| (*s).to_owned()).collect();
                if cmd.is_empty() {
                    return Err(usage_error(USAGE));
                }
                entry.command = cmd;
                changed = true;
                break;
            }
            "--json" => json = true,
            "--isolate" | "--new-worktree" => {
                entry.workspace_mode = WorkspaceMode::Isolate;
                changed = true;
            }
            "--no-new-worktree" | "--reuse" => {
                entry.workspace_mode = WorkspaceMode::Reuse;
                changed = true;
            }
            "--clear-precheck" => {
                entry.precheck = None;
                changed = true;
            }
            "--enable" | "--enabled" => {
                entry.enabled = true;
                changed = true;
            }
            "--disable" | "--disabled" => {
                entry.enabled = false;
                changed = true;
            }
            flag @ ("--cron" | "--cwd" | "--workdir" | "--prompt" | "--precheck"
            | "--workspace-mode" | "--agent") => {
                let value = args.next().ok_or_else(|| usage_error(USAGE))?;
                match flag {
                    "--cron" => entry.cron = (*value).into(),
                    "--cwd" | "--workdir" => {
                        entry.cwd = fs::canonicalize(value)?
                            .into_os_string()
                            .into_string()
                            .map_err(|_| error("cwd must be UTF-8"))?;
                    }
                    "--precheck" => {
                        let trimmed = value.trim();
                        if trimmed.is_empty() {
                            return Err(usage_error(USAGE));
                        }
                        entry.precheck = Some(trimmed.to_owned());
                    }
                    "--workspace-mode" => {
                        entry.workspace_mode = match *value {
                            "reuse" | "existing" => WorkspaceMode::Reuse,
                            "isolate" | "isolated" | "new_worktree" => WorkspaceMode::Isolate,
                            _ => return Err(usage_error(USAGE)),
                        };
                    }
                    "--agent" => {
                        let trimmed = value.trim();
                        if trimmed.is_empty() {
                            return Err(usage_error(USAGE));
                        }
                        entry.command = vec![trimmed.to_owned()];
                    }
                    _ => entry.prompt = (*value).into(),
                }
                changed = true;
            }
            _ => return Err(usage_error(USAGE)),
        }
    }
    if !changed {
        return Err(usage_error(USAGE));
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
