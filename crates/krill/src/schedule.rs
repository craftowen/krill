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

const USAGE: &str = "usage: krill schedule add <id> --cron '<minute hour day month weekday>' --prompt <text> [--cwd <path>] [--precheck <cmd>] [--isolate] [--workspace-mode <reuse|isolate>] -- <executable> [args...] | krill schedule run <id> | krill schedule rm <id> | krill schedule list | krill schedule runs (cron is UTC; prompt is the last command argument)";

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
    match args.as_slice() {
        ["list"] => {
            let schedules = Schedules::load(&dir)?;
            println!("{}", toml::to_string(&schedules).map_err(error)?);
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
        ["add", id, rest @ ..] => {
            let entry = add(id, rest)?;
            let _guard = Schedules::exclusive_edit(&dir)?;
            let mut schedules = Schedules::load(&dir)?;
            if schedules.schedule.iter().any(|s| s.id == *id) {
                return Err(error(
                    "schedule ID already exists; remove it before replacing",
                ));
            }
            schedules.schedule.push(entry);
            schedules.save(&dir)?;
            // Activate the daemon so reservations work with the app closed.
            let paths = Paths::dev().map_err(error)?;
            daemon::connect(
                &paths,
                &daemon::client_hello(ClientRole::Cli, "mk-schedule"),
            )
            .map_err(|e| error(format!("schedule saved, daemon startup failed: {e}")))?;
            println!("saved {id} (UTC)");
        }
        ["run", id] => {
            let schedules = Schedules::load(&dir)?;
            if !schedules.schedule.iter().any(|s| s.id == *id) {
                return Err(usage_error("unknown schedule ID"));
            }
            let paths = Paths::dev().map_err(|e| error(format!("daemon required: {e}")))?;
            daemon::connect(
                &paths,
                &daemon::client_hello(ClientRole::Cli, "mk-schedule"),
            )
            .map_err(|e| error(format!("daemon required: {e}")))?;
            request_run_now(&dir, id)?;
            println!("triggered {id}");
        }
        ["rm", id] => {
            let _guard = Schedules::exclusive_edit(&dir)?;
            let mut schedules = Schedules::load(&dir)?;
            let before = schedules.schedule.len();
            schedules.schedule.retain(|s| s.id != *id);
            if before == schedules.schedule.len() {
                return Err(error("unknown schedule ID"));
            }
            schedules.save(&dir)?;
            println!("removed {id}");
        }
        _ => return Err(usage_error(USAGE)),
    }
    Ok(())
}

fn add(id: &str, args: &[&str]) -> io::Result<Schedule> {
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
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "--" => {
                entry.command = args.map(|s| (*s).to_owned()).collect();
                break;
            }
            "--isolate" => {
                entry.workspace_mode = WorkspaceMode::Isolate;
            }
            flag @ ("--cron" | "--cwd" | "--prompt" | "--precheck" | "--workspace-mode") => {
                let value = args.next().ok_or_else(|| usage_error(USAGE))?;
                match flag {
                    "--cron" => entry.cron = (*value).into(),
                    "--cwd" => {
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
                    _ => entry.prompt = (*value).into(),
                }
            }
            _ => return Err(usage_error(USAGE)),
        }
    }
    if entry.cron.is_empty() || entry.command.is_empty() {
        return Err(usage_error(USAGE));
    }
    Ok(entry)
}

fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn usage_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
}
