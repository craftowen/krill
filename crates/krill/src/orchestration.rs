//! `krill orchestration` (`krill orch`) CLI — Multi-Agent Orchestration (MK-114 / OWE-624).
//!
//! Coordinates multi-agent runs, task DAGs, worker dispatches, threaded messages,
//! blocking ask/reply, and decision gates backed by `state_dir/orchestration.json`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use std::io::Write;

use krill_proto::control::{FEAT_CLI, FEAT_CONTROL, Failure, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::orchestration::{
    DEFAULT_MAX_WORKER_DEPTH, Dispatch, GateStatus, MessageType, OrchAction, OrchResponse,
    OrchestrationStore, Run, SendParams, TaskStatus, WorkerStartSpec, worker_preamble,
};
use krill_proto::screen::Msg;

use crate::remote::Target;

const USAGE: &str = "\
usage: krill [--host <user@host>] orchestration <subcommand> [options]
  run-create --title <title> [--coordinator <session-id>] [--json]
  run-use <run-id|--clear> [--json]
  run-current [--json]
  run-list [--json]
  task-create [--run <run-id>] --title <title> [--desc <text>] [--deps <t1,t2>] [--json]
  task-list [--run <run-id>] [--json]
  task-update <task-id> --status <pending|ready|dispatched|completed|failed|blocked> [--summary <text>] [--json]
  worker-start [--spec <json>] [--run <run-id>] [--task <task-id>] --worker <name> [--agent <agent>] [--account <id>] [--session <id>] [--cwd <dir>] [--depth <N>] [--max-depth <N>] --prompt <text> [--json]
  send --from <name> --to <target> [--type <type>] --summary <text> [--body <text>] [--run <id>] [--task <id>] [--dispatch <id>] [--outcome <succeeded|failed>] [--json]
  check [--to <name>] [--types <t1,t2>] [--unread] [--ack] [--wait] [--timeout-ms <N>] [--json]
  ask --from <name> --to <target> --summary <question> [--body <text>] [--wait] [--timeout-ms <N>] [--json]
  reply <msg-id> [--from <name>] --body <answer> [--json]
  gate-create --from <name> --question <text> [--options <a,b>] [--run <id>] [--task <id>] [--json]
  gate-list [--pending] [--json]
  gate-resolve <gate-id> --decision <choice> [--rationale <text>] [--by <name>] [--json]
  worker-list [--run <run-id>] [--json]
  worker-show <worker-or-dispatch> [--json]
  worker-read <worker-or-dispatch> [--lines <N>] [--json]
  worker-stop <worker-or-dispatch> [--json]
  worker-release <worker-or-dispatch> [--json]
  worker-abandon <worker-or-dispatch> [--json]
  reset [--run <run-id>] [--json]";

enum TargetRef<'a> {
    Borrowed(&'a Target),
    Owned(Target),
}

impl std::ops::Deref for TargetRef<'_> {
    type Target = Target;
    fn deref(&self) -> &Target {
        match self {
            Self::Borrowed(t) => t,
            Self::Owned(t) => t,
        }
    }
}

struct OrchCtx<'a> {
    target: Option<&'a Target>,
    state_dir: &'a Path,
}

impl<'a> OrchCtx<'a> {
    #[cfg(test)]
    fn local(state_dir: &'a Path) -> Self {
        Self {
            target: None,
            state_dir,
        }
    }

    fn is_remote(&self) -> bool {
        self.target.is_some_and(Target::is_remote)
    }

    fn effective_target(&self) -> io::Result<TargetRef<'a>> {
        match self.target {
            Some(t) => Ok(TargetRef::Borrowed(t)),
            None => Target::establish(None).map(TargetRef::Owned),
        }
    }

    /// Calls `krilld` via `Request::Orchestration` when remote (`--host`) or when a local
    /// `krilld` socket is active for `daemon::state_dir()`. Returns `Ok(None)` when local
    /// and no daemon is listening so unit tests and offline invocations can use disk fallback.
    fn try_ipc(&self, action: &OrchAction, timeout: Duration) -> io::Result<Option<OrchResponse>> {
        let is_remote = self.is_remote();
        if !is_remote && daemon::state_dir().as_deref() != Some(self.state_dir) {
            return Ok(None);
        }
        let target = match self.target {
            Some(t) => TargetRef::Borrowed(t),
            None => match Target::establish(None) {
                Ok(t) => TargetRef::Owned(t),
                Err(_) => return Ok(None),
            },
        };
        let hello = daemon::client_hello(ClientRole::Cli, "krill");
        let mut conn = if is_remote {
            target
                .connect(&hello)
                .map_err(|e| io::Error::other(e.to_string()))?
        } else {
            match target.dial(&hello, Duration::from_secs(2)) {
                Ok(c) => c,
                Err(_) => return Ok(None),
            }
        };
        let required = FEAT_CONTROL | FEAT_CLI;
        if conn.negotiated.features & required != required {
            if is_remote {
                return Err(io::Error::other(
                    "remote krilld does not support orchestration IPC; update the daemon",
                ));
            }
            return Ok(None);
        }
        let io_wait = timeout.saturating_add(Duration::from_secs(5));
        conn.stream.set_read_timeout(Some(io_wait))?;
        conn.stream
            .set_write_timeout(Some(Duration::from_secs(5)))?;
        let encoded = Request::Orchestration(action.clone())
            .encode()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        if conn.stream.write_all(&encoded).is_err() {
            if is_remote {
                return Err(io::Error::other("remote krilld disconnected"));
            }
            return Ok(None);
        }
        let Some((kind, body)) = ipc::read_frame(&mut conn.stream)? else {
            if is_remote {
                return Err(io::Error::other("remote krilld disconnected"));
            }
            return Ok(None);
        };
        let reply = Reply::decode(kind, &body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        match reply {
            Reply::Orchestration(resp) => Ok(Some(resp)),
            Reply::Failed(Failure::BadRequest) if !is_remote => Ok(None),
            Reply::Failed(f) => Err(classify_daemon_failure(f)),
            _ => Err(io::Error::other("unexpected orchestration reply")),
        }
    }

    fn load_store(&self) -> io::Result<OrchestrationStore> {
        if let Some(OrchResponse::Snapshot(store)) =
            self.try_ipc(&OrchAction::Snapshot, Duration::from_secs(2))?
        {
            return Ok(store);
        }
        OrchestrationStore::load(self.state_dir)
    }
}

fn classify_daemon_failure(f: Failure) -> io::Error {
    let msg = f.to_string();
    if msg.contains("not found") || msg.contains("no active run") {
        io::Error::new(io::ErrorKind::NotFound, msg)
    } else if msg.contains("must not be empty")
        || msg.contains("not in gate options")
        || msg.contains("unknown ")
        || msg.contains("invalid ")
        || msg.contains("max_depth")
    {
        io::Error::new(io::ErrorKind::InvalidInput, msg)
    } else {
        io::Error::other(msg)
    }
}

pub fn run(raw: &[OsString]) -> ExitCode {
    let (host, rest_os) = match crate::remote::extract_host(raw) {
        Ok(parsed) => parsed,
        Err(text) => {
            eprintln!("orchestration: {text}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Ok(args) = rest_os
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("orchestration: arguments must be UTF-8");
        return ExitCode::from(2);
    };
    let Some((sub, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let target = match Target::establish(host) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("orchestration: remote: {e}");
            return ExitCode::FAILURE;
        }
    };
    let state_dir = match daemon::state_dir() {
        Some(dir) => dir,
        None if target.is_remote() => PathBuf::from("/tmp"),
        None => {
            eprintln!("orchestration: HOME is not set");
            return ExitCode::FAILURE;
        }
    };
    let ctx = OrchCtx {
        target: Some(&target),
        state_dir: &state_dir,
    };

    let res = match sub.as_str() {
        "run-create" => cmd_run_create_ctx(&ctx, rest),
        "run-use" => cmd_run_use_ctx(&ctx, rest),
        "run-current" => cmd_run_current_ctx(&ctx, rest),
        "run-list" | "runs" => cmd_run_list_ctx(&ctx, rest),
        "task-create" => cmd_task_create_ctx(&ctx, rest),
        "task-list" | "tasks" => cmd_task_list_ctx(&ctx, rest),
        "task-update" => cmd_task_update_ctx(&ctx, rest),
        "worker-start" => cmd_worker_start_ctx(&ctx, rest),
        "send" => cmd_send_ctx(&ctx, rest),
        "check" => cmd_check_ctx(&ctx, rest),
        "ask" => cmd_ask_ctx(&ctx, rest),
        "reply" => cmd_reply_ctx(&ctx, rest),
        "gate-create" => cmd_gate_create_ctx(&ctx, rest),
        "gate-list" | "gates" => cmd_gate_list_ctx(&ctx, rest),
        "gate-resolve" => cmd_gate_resolve_ctx(&ctx, rest),
        "worker-list" | "workers" => cmd_worker_list_ctx(&ctx, rest),
        "worker-show" => cmd_worker_show_ctx(&ctx, rest),
        "worker-read" => cmd_worker_read_ctx(&ctx, rest),
        "worker-stop" => cmd_worker_stop_ctx(&ctx, rest, false),
        "worker-release" => cmd_worker_stop_ctx(&ctx, rest, true),
        "worker-abandon" => cmd_worker_abandon_ctx(&ctx, rest),
        "reset" => cmd_reset_ctx(&ctx, rest),
        _ => {
            eprintln!("unknown orchestration subcommand: {sub}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("orchestration: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("orchestration: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
fn cmd_run_create(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_run_create_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_run_create_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut title = None;
    let mut coordinator = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--title" => title = it.next().cloned(),
            "--coordinator" => {
                let v = it.next().ok_or_else(|| bad_arg("missing --coordinator"))?;
                coordinator = Some(
                    v.parse::<u64>()
                        .map_err(|_| bad_arg("invalid session id"))?,
                );
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let title = title.ok_or_else(|| bad_arg("missing --title"))?;
    let created = match ctx.try_ipc(
        &OrchAction::CreateRun {
            title: title.clone(),
            coordinator_session_id: coordinator,
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Run(r)) => r,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| s.create_run(&title, coordinator))?,
    };
    if json {
        print_json(&created)?;
    } else {
        println!(
            "{}\t{}\t{}",
            created.id,
            created.status.as_str(),
            created.title
        );
    }
    Ok(())
}

fn cmd_run_use_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut target_run: Option<Option<String>> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--clear" => target_run = Some(None),
            "--run" => {
                let id = it.next().ok_or_else(|| bad_arg("missing --run"))?;
                target_run = Some(Some(id.clone()));
            }
            "--json" => json = true,
            s if !s.starts_with('-') && target_run.is_none() => {
                target_run = Some(Some(s.to_owned()));
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let run_id = target_run
        .ok_or_else(|| bad_arg("usage: krill orchestration run-use <run-id|--clear> [--json]"))?;
    let cur_id = match ctx.try_ipc(
        &OrchAction::SetCurrentRun {
            run_id: run_id.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::CurrentRun(c)) => c,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| s.set_current_run(run_id.as_deref()))?,
    };
    let active: Option<Run> =
        cur_id.and_then(|id| ctx.load_store().ok()?.runs.into_iter().find(|r| r.id == id));
    if json {
        print_json(&serde_json::json!({
            "active_run_id": active.as_ref().map(|r| r.id.as_str()),
            "run": active,
        }))?;
    } else if let Some(r) = active {
        println!("{}\t{}\t{}", r.id, r.status.as_str(), r.title);
    } else {
        println!("cleared");
    }
    Ok(())
}

fn cmd_run_current_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let json = has_flag(args, "--json");
    if args.iter().any(|a| a != "--json") {
        return Err(bad_arg("usage: krill orchestration run-current [--json]"));
    }
    let store = ctx.load_store()?;
    let current: Option<Run> = store
        .resolve_run_id(None)
        .ok()
        .and_then(|id| store.runs.into_iter().find(|r| r.id == id));
    let Some(r) = current else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no active run; pass --run <id> or set one with `krill orchestration run-use <id>`",
        ));
    };
    if json {
        print_json(&r)?;
    } else {
        println!("{}\t{}\t{}", r.id, r.status.as_str(), r.title);
    }
    Ok(())
}

#[cfg(test)]
fn cmd_run_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_run_list_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_run_list_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let json = has_flag(args, "--json");
    let store = ctx.load_store()?;
    if json {
        print_json(&store.runs)?;
    } else {
        println!("ID\tSTATUS\tTASKS\tTITLE");
        for r in &store.runs {
            let count = store.tasks.iter().filter(|t| t.run_id == r.id).count();
            println!("{}\t{}\t{}\t{}", r.id, r.status.as_str(), count, r.title);
        }
    }
    Ok(())
}

#[cfg(test)]
fn cmd_task_create(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_task_create_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_task_create_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut run_id = None;
    let mut title = None;
    let mut desc = String::new();
    let mut deps = Vec::new();
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--run" => run_id = it.next().cloned(),
            "--title" => title = it.next().cloned(),
            "--desc" | "--description" => {
                desc = it.next().cloned().unwrap_or_default();
            }
            "--deps" => {
                if let Some(raw) = it.next() {
                    deps.extend(
                        raw.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let title = title.ok_or_else(|| bad_arg("missing --title"))?;
    let resolved_run_id = match run_id {
        Some(id) => id,
        None => ctx.load_store()?.resolve_run_id(None)?,
    };
    let task = match ctx.try_ipc(
        &OrchAction::CreateTask {
            run_id: resolved_run_id.clone(),
            title: title.clone(),
            description: desc.clone(),
            deps: deps.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Task(t)) => t,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| {
            s.create_task(&resolved_run_id, &title, &desc, deps)
        })?,
    };
    if json {
        print_json(&task)?;
    } else {
        println!(
            "{}\t{}\t{}\t{}",
            task.id,
            task.run_id,
            task.status.as_str(),
            task.title
        );
    }
    Ok(())
}

#[cfg(test)]
fn cmd_task_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_task_list_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_task_list_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut run_filter = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--run" => run_filter = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let store = ctx.load_store()?;
    let tasks: Vec<_> = store
        .tasks
        .into_iter()
        .filter(|t| run_filter.as_ref().is_none_or(|r| &t.run_id == r))
        .collect();
    if json {
        print_json(&tasks)?;
    } else {
        println!("ID\tRUN\tSTATUS\tWORKER\tDEPS\tTITLE");
        for t in tasks {
            let worker = t.assigned_worker.as_deref().unwrap_or("-");
            let deps = if t.deps.is_empty() {
                "-".to_owned()
            } else {
                t.deps.join(",")
            };
            println!(
                "{}\t{}\t{}\t{worker}\t{deps}\t{}",
                t.id,
                t.run_id,
                t.status.as_str(),
                t.title
            );
        }
    }
    Ok(())
}

fn cmd_task_update_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let Some((task_id, rest)) = args.split_first() else {
        return Err(bad_arg("missing <task-id>"));
    };
    let mut status = None;
    let mut summary = None;
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--status" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --status"))?;
                status = Some(TaskStatus::parse(raw)?);
            }
            "--summary" => summary = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let status = status.ok_or_else(|| bad_arg("missing --status"))?;
    let updated = match ctx.try_ipc(
        &OrchAction::UpdateTask {
            task_id: task_id.clone(),
            status,
            summary: summary.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Task(t)) => t,
        _ => {
            OrchestrationStore::mutate(ctx.state_dir, |s| s.update_task(task_id, status, summary))?
        }
    };
    if json {
        print_json(&updated)?;
    } else {
        println!(
            "{}\t{}\t{}",
            updated.id,
            updated.status.as_str(),
            updated.title
        );
    }
    Ok(())
}

#[cfg(test)]
fn cmd_worker_start(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_worker_start_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_worker_start_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut spec = WorkerStartSpec::default();
    let mut explicit_agent: Option<String> = None;
    let mut explicit_depth: Option<u32> = None;
    let mut explicit_max_depth: Option<u32> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--spec" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --spec"))?;
                let parsed: WorkerStartSpec = serde_json::from_str(raw)
                    .map_err(|e| bad_arg(format!("invalid --spec JSON: {e}")))?;
                if spec.run_id.is_none() {
                    spec.run_id = parsed.run_id;
                }
                if spec.task_id.is_none() {
                    spec.task_id = parsed.task_id;
                }
                if spec.worker.is_none() {
                    spec.worker = parsed.worker;
                }
                if explicit_agent.is_none() && parsed.agent.is_some() {
                    spec.agent = parsed.agent;
                }
                if spec.account.is_none() {
                    spec.account = parsed.account;
                }
                if spec.session_id.is_none() {
                    spec.session_id = parsed.session_id;
                }
                if spec.cwd.is_none() {
                    spec.cwd = parsed.cwd;
                }
                if spec.prompt.is_none() {
                    spec.prompt = parsed.prompt;
                }
                if explicit_depth.is_none() && parsed.depth.is_some() {
                    spec.depth = parsed.depth;
                }
                if explicit_max_depth.is_none() && parsed.max_depth.is_some() {
                    spec.max_depth = parsed.max_depth;
                }
                if parsed.no_spawn {
                    spec.no_spawn = true;
                }
            }
            "--run" => spec.run_id = it.next().cloned(),
            "--task" => spec.task_id = it.next().cloned(),
            "--worker" | "--name" => spec.worker = it.next().cloned(),
            "--agent" => {
                explicit_agent = Some(take_opt(&mut it, "--agent")?.clone());
            }
            "--account" => {
                spec.account = Some(take_opt(&mut it, "--account")?.clone());
            }
            "--session" => {
                let v = it.next().ok_or_else(|| bad_arg("missing --session"))?;
                spec.session_id = Some(v.parse::<u64>().map_err(|_| bad_arg("invalid --session"))?);
            }
            "--cwd" => spec.cwd = it.next().cloned(),
            "--prompt" => spec.prompt = it.next().cloned(),
            "--depth" => {
                let v = it.next().ok_or_else(|| bad_arg("missing --depth"))?;
                explicit_depth = Some(v.parse::<u32>().map_err(|_| bad_arg("invalid --depth"))?);
            }
            "--max-depth" => {
                let v = it.next().ok_or_else(|| bad_arg("missing --max-depth"))?;
                explicit_max_depth = Some(
                    v.parse::<u32>()
                        .map_err(|_| bad_arg("invalid --max-depth"))?,
                );
            }
            "--no-spawn" => spec.no_spawn = true,
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let worker_name = spec.worker.ok_or_else(|| bad_arg("missing --worker"))?;
    let prompt = spec.prompt.ok_or_else(|| bad_arg("missing --prompt"))?;
    let raw_agent = explicit_agent
        .or(spec.agent)
        .unwrap_or_else(|| "claude".to_owned());
    let agent = krill_proto::agent::canonical_id(&raw_agent).to_owned();
    let env_depth = std::env::var("KRILL_ORCH_DEPTH")
        .or_else(|_| std::env::var("MK_WORKER_DEPTH"))
        .ok()
        .and_then(|v| v.parse().ok());
    let max_depth = explicit_max_depth
        .or(spec.max_depth)
        .or_else(|| {
            std::env::var("KRILL_ORCH_MAX_DEPTH")
                .or_else(|_| std::env::var("MK_MAX_WORKER_DEPTH"))
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(DEFAULT_MAX_WORKER_DEPTH);

    let work_cwd = match spec.cwd {
        Some(c) => PathBuf::from(c),
        None if ctx.is_remote() => {
            return Err(bad_arg(
                "remote worker-start requires --cwd <absolute path on the remote host>",
            ));
        }
        None => std::env::current_dir()?,
    };
    let cwd_str = work_cwd.to_string_lossy().into_owned();

    // 스폰 전에 run/task 유효성과 상태를 먼저 검증해 잘못된 요청이 고아 PTY 세션을 남기지 않게 한다.
    let store = ctx.load_store()?;
    let current_pane = std::env::var("MK_PANE_ID")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let depth = explicit_depth
        .or(spec.depth)
        .or(env_depth)
        .or_else(|| store.inferred_child_depth(current_pane))
        .unwrap_or(0);
    let run_id = match spec.run_id {
        Some(id) => id,
        None => store
            .resolve_run_id(None)
            .map_err(|_| bad_arg("missing --run"))?,
    };
    store.validate_worker_start_with_depth(
        &run_id,
        spec.task_id.as_deref(),
        &worker_name,
        depth,
        max_depth,
    )?;

    let target = ctx.effective_target()?;
    let mut session_id = spec.session_id;
    // 존재하지 않는 세션을 지정하면 dispatch가 Active 고스트로 남고 stop/release가 영구 실패한다(C1).
    // mutate 전에 세션 존재를 확인해 고스트 기록 자체를 만들지 않는다.
    if let Some(sid) = session_id {
        ensure_worker_session_exists_on_target(&target, &worker_name, sid)?;
    }

    let newly_spawned = session_id.is_none() && !spec.no_spawn;
    if newly_spawned {
        let spawned = crate::session::spawn_agent_session_on_target(
            &target,
            &agent,
            spec.account.clone(),
            &work_cwd,
        )
        .map_err(|e| io::Error::other(format!("spawn {agent} worker session: {e}")))?;
        session_id = Some(spawned);
    }

    let dispatch_res: io::Result<Dispatch> = match ctx.try_ipc(
        &OrchAction::StartWorker {
            run_id: run_id.clone(),
            task_id: spec.task_id.clone(),
            worker_name: worker_name.clone(),
            agent: agent.clone(),
            session_id,
            cwd: cwd_str.clone(),
            prompt: prompt.clone(),
            depth,
            max_depth,
        },
        Duration::from_secs(2),
    ) {
        Ok(Some(OrchResponse::Dispatch(d))) => Ok(d),
        Ok(_) => OrchestrationStore::mutate(ctx.state_dir, |s| {
            s.start_worker_with_depth(
                &run_id,
                spec.task_id.as_deref(),
                &worker_name,
                &agent,
                session_id,
                &cwd_str,
                &prompt,
                depth,
                max_depth,
            )
        }),
        Err(e) => Err(e),
    };
    let dispatch = match dispatch_res {
        Ok(d) => d,
        Err(e) => {
            if newly_spawned && let Some(sid) = session_id {
                crate::session::close_session_on_target(&target, sid).ok();
            }
            return Err(e);
        }
    };

    let delivered = dispatch.session_id.map_or(Ok(()), |sid| {
        let preamble = worker_preamble(&dispatch);
        if newly_spawned {
            crate::session::wait_for_spawned_agent_ready(
                &target,
                sid,
                crate::session::BOOT_TIMEOUT,
            )?;
        }
        crate::session::send_session_line(&target, sid, &preamble)
    });

    if json {
        print_json(&dispatch)?;
    } else {
        println!(
            "{}\t{}\t{}\t{}\tsession={}",
            dispatch.id,
            dispatch.worker_name,
            dispatch.agent,
            dispatch.status.as_str(),
            dispatch
                .session_id
                .map_or_else(|| "-".to_owned(), |n| n.to_string())
        );
    }
    // 디스패치는 기록됐으니 출력은 남기고, 프롬프트를 못 넣었으면 실패로 끝낸다(`krill send`로 다시 넣을 수 있다).
    delivered.map_err(|e| io::Error::other(format!("worker {}: {e}", dispatch.worker_name)))
}

#[cfg(test)]
fn cmd_send(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_send_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_send_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut from = None;
    let mut to = None;
    let mut msg_type = MessageType::Status;
    let mut summary = None;
    let mut body = String::new();
    let mut run_id = None;
    let mut task_id = None;
    let mut dispatch_id = None;
    let mut outcome = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" => from = it.next().cloned(),
            "--to" => to = it.next().cloned(),
            "--type" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --type"))?;
                msg_type = MessageType::parse(raw)?;
            }
            "--summary" => summary = it.next().cloned(),
            "--body" => body = it.next().cloned().unwrap_or_default(),
            "--run" => run_id = it.next().cloned(),
            "--task" => task_id = it.next().cloned(),
            "--dispatch" => dispatch_id = it.next().cloned(),
            "--outcome" | "--status" => outcome = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let from = from.ok_or_else(|| bad_arg("missing --from"))?;
    let to = to.ok_or_else(|| bad_arg("missing --to"))?;
    let summary = summary.ok_or_else(|| bad_arg("missing --summary"))?;
    let params = SendParams {
        from,
        to,
        msg_type,
        summary,
        body,
        run_id,
        task_id,
        dispatch_id,
        outcome,
        reply_to: None,
    };
    let created = match ctx.try_ipc(
        &OrchAction::SendMessage(params.clone()),
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Messages(m)) => m,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| s.send_message(&params))?,
    };
    if json {
        print_json(&created)?;
    } else {
        for m in created {
            println!(
                "{}\t{}\t->{}\t{}\t{}",
                m.id,
                m.from,
                m.to,
                m.msg_type.as_str(),
                m.summary
            );
        }
    }
    Ok(())
}

#[cfg(test)]
fn cmd_check(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_check_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_check_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut to = None;
    let mut types = Vec::new();
    let mut unread_only = false;
    let mut ack = false;
    let mut wait = false;
    let mut timeout_ms = 5_000_u64;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--to" | "--for" => to = it.next().cloned(),
            "--types" | "--type" => {
                if let Some(raw) = it.next() {
                    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        types.push(MessageType::parse(part)?);
                    }
                }
            }
            "--unread" => unread_only = true,
            "--ack" => ack = true,
            "--wait" => wait = true,
            "--timeout-ms" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --timeout-ms"))?;
                timeout_ms = raw
                    .parse::<u64>()
                    .map_err(|_| bad_arg("invalid --timeout-ms"))?;
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let wait_ms = if wait { timeout_ms } else { 0 };
    let deadline = Duration::from_millis(wait_ms);
    let messages = if let Some(OrchResponse::Messages(m)) = ctx.try_ipc(
        &OrchAction::CheckMessages {
            recipient: to.clone(),
            types: types.clone(),
            unread_only,
            ack,
            wait_ms,
        },
        deadline,
    )? {
        m
    } else {
        let start = Instant::now();
        loop {
            let mut preview_store = OrchestrationStore::load(ctx.state_dir)?;
            let preview = preview_store.check_messages(to.as_deref(), &types, unread_only, false);
            if !preview.is_empty() {
                if ack {
                    break OrchestrationStore::mutate(ctx.state_dir, |s| {
                        Ok(s.check_messages(to.as_deref(), &types, unread_only, true))
                    })?;
                }
                break preview;
            }
            if !wait || start.elapsed() >= deadline {
                break Vec::new();
            }
            std::thread::sleep(Duration::from_millis(80));
        }
    };
    if json {
        print_json(&messages)?;
    } else {
        println!("ID\tFROM\tTO\tTYPE\tACK\tSUMMARY");
        for m in messages {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                m.id,
                m.from,
                m.to,
                m.msg_type.as_str(),
                if m.acked { "acked" } else { "new" },
                m.summary
            );
        }
    }
    Ok(())
}

#[cfg(test)]
fn cmd_ask(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_ask_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_ask_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut from = None;
    let mut to = None;
    let mut summary = None;
    let mut body = String::new();
    let mut run_id = None;
    let mut task_id = None;
    let mut wait = false;
    let mut timeout_ms = 5_000_u64;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" => from = it.next().cloned(),
            "--to" => to = it.next().cloned(),
            "--summary" | "--question" => summary = it.next().cloned(),
            "--body" => body = it.next().cloned().unwrap_or_default(),
            "--run" => run_id = it.next().cloned(),
            "--task" => task_id = it.next().cloned(),
            "--wait" => wait = true,
            "--timeout-ms" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --timeout-ms"))?;
                timeout_ms = raw
                    .parse::<u64>()
                    .map_err(|_| bad_arg("invalid --timeout-ms"))?;
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let from = from.ok_or_else(|| bad_arg("missing --from"))?;
    let to = to.ok_or_else(|| bad_arg("missing --to"))?;
    let summary = summary.ok_or_else(|| bad_arg("missing --summary"))?;
    let params = SendParams {
        from: from.clone(),
        to,
        msg_type: MessageType::Question,
        summary,
        body,
        run_id,
        task_id,
        dispatch_id: None,
        outcome: None,
        reply_to: None,
    };
    let sent = match ctx.try_ipc(
        &OrchAction::SendMessage(params.clone()),
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Messages(m)) => m,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| s.send_message(&params))?,
    };
    let question = sent
        .into_iter()
        .next()
        .ok_or_else(|| bad_arg("failed to create question"))?;

    if !wait {
        if json {
            print_json(&question)?;
        } else {
            println!("{}\tquestion\t{}", question.id, question.summary);
        }
        return Ok(());
    }

    let deadline = Duration::from_millis(timeout_ms);
    if let Some(OrchResponse::Answer(ans_opt)) = ctx.try_ipc(
        &OrchAction::WaitAnswer {
            question_id: question.id.clone(),
            wait_ms: timeout_ms,
        },
        deadline,
    )? {
        if let Some(ans) = ans_opt {
            if json {
                print_json(&ans)?;
            } else {
                println!("{}\tanswer\t{}", ans.id, ans.body);
            }
        } else if json {
            print_json(&question)?;
        } else {
            println!("{}\tpending\t{}", question.id, question.summary);
        }
        return Ok(());
    }

    let start = Instant::now();
    loop {
        let store = OrchestrationStore::load(ctx.state_dir)?;
        if let Some(ans) = store
            .messages
            .iter()
            .find(|m| m.reply_to.as_deref() == Some(question.id.as_str()))
        {
            if json {
                print_json(ans)?;
            } else {
                println!("{}\tanswer\t{}", ans.id, ans.body);
            }
            return Ok(());
        }
        if start.elapsed() >= deadline {
            if json {
                print_json(&question)?;
            } else {
                println!("{}\tpending\t{}", question.id, question.summary);
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

#[cfg(test)]
fn cmd_reply(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_reply_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_reply_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let Some((msg_id, rest)) = args.split_first() else {
        return Err(bad_arg("missing <msg-id>"));
    };
    let mut from = "coordinator".to_owned();
    let mut body = None;
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" => {
                from.clone_from(it.next().ok_or_else(|| bad_arg("missing --from"))?);
            }
            "--body" | "--answer" => body = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let body = body.ok_or_else(|| bad_arg("missing --body"))?;
    let reply = match ctx.try_ipc(
        &OrchAction::ReplyMessage {
            question_id: msg_id.clone(),
            from: from.clone(),
            body: body.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Message(m)) => m,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| s.reply_message(msg_id, &from, &body))?,
    };
    if json {
        print_json(&reply)?;
    } else {
        println!("{}\treply->{}\t{}", reply.id, reply.to, reply.body);
    }
    Ok(())
}

#[cfg(test)]
fn cmd_gate_create(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_gate_create_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_gate_create_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut from = "worker".to_owned();
    let mut question = None;
    let mut options = Vec::new();
    let mut run_id = None;
    let mut task_id = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" => {
                from.clone_from(it.next().ok_or_else(|| bad_arg("missing --from"))?);
            }
            "--question" => question = it.next().cloned(),
            "--options" => {
                if let Some(raw) = it.next() {
                    options.extend(
                        raw.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
            "--run" => run_id = it.next().cloned(),
            "--task" => task_id = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let question = question.ok_or_else(|| bad_arg("missing --question"))?;
    if options.is_empty() {
        options = vec!["approve".to_owned(), "reject".to_owned()];
    }
    let gate = match ctx.try_ipc(
        &OrchAction::CreateGate {
            from: from.clone(),
            question: question.clone(),
            options: options.clone(),
            run_id: run_id.clone(),
            task_id: task_id.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Gate(g)) => g,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| {
            s.create_gate(&from, &question, options, run_id, task_id)
        })?,
    };
    if json {
        print_json(&gate)?;
    } else {
        println!("{}\t{}\t{}", gate.id, gate.status.as_str(), gate.question);
    }
    Ok(())
}

#[cfg(test)]
fn cmd_gate_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_gate_list_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_gate_list_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let pending_only = args.iter().any(|a| a == "--pending");
    let json = args.iter().any(|a| a == "--json");
    let store = ctx.load_store()?;
    let gates: Vec<_> = store
        .gates
        .into_iter()
        .filter(|g| !pending_only || g.status == GateStatus::Pending)
        .collect();
    if json {
        print_json(&gates)?;
    } else {
        println!("ID\tSTATUS\tFROM\tDECISION\tQUESTION");
        for g in gates {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                g.id,
                g.status.as_str(),
                g.from,
                g.decision.as_deref().unwrap_or("-"),
                g.question
            );
        }
    }
    Ok(())
}

#[cfg(test)]
fn cmd_gate_resolve(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_gate_resolve_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_gate_resolve_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let Some((gate_id, rest)) = args.split_first() else {
        return Err(bad_arg("missing <gate-id>"));
    };
    let mut decision = None;
    let mut rationale = None;
    let mut by = "coordinator".to_owned();
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--decision" => decision = it.next().cloned(),
            "--rationale" => rationale = it.next().cloned(),
            "--by" => {
                by.clone_from(it.next().ok_or_else(|| bad_arg("missing --by"))?);
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let decision = decision.ok_or_else(|| bad_arg("missing --decision"))?;
    let gate = match ctx.try_ipc(
        &OrchAction::ResolveGate {
            gate_id: gate_id.clone(),
            decision: decision.clone(),
            rationale: rationale.clone(),
            resolved_by: by.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Gate(g)) => g,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| {
            s.resolve_gate(gate_id, &decision, rationale, &by)
        })?,
    };
    if json {
        print_json(&gate)?;
    } else {
        println!(
            "{}\t{}\t{}",
            gate.id,
            gate.status.as_str(),
            gate.decision.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

#[cfg(test)]
fn cmd_worker_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_worker_list_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_worker_list_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut run_filter = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--run" => run_filter = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let store = ctx.load_store()?;
    let workers: Vec<_> = store
        .dispatches
        .into_iter()
        .filter(|d| run_filter.as_ref().is_none_or(|r| &d.run_id == r))
        .collect();
    if json {
        print_json(&workers)?;
    } else {
        println!("DISPATCH\tWORKER\tAGENT\tSTATUS\tSESSION\tTASK");
        for w in workers {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                w.id,
                w.worker_name,
                w.agent,
                w.status.as_str(),
                w.session_id
                    .map_or_else(|| "-".to_owned(), |n| n.to_string()),
                w.task_id.as_deref().unwrap_or("-")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
fn cmd_worker_show(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_worker_show_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_worker_show_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let Some((target, rest)) = args.split_first() else {
        return Err(bad_arg("missing <worker-or-dispatch>"));
    };
    let json = rest.iter().any(|a| a == "--json");
    let store = ctx.load_store()?;
    let Some(disp) = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == *target || d.worker_name == *target)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("worker not found: {target}"),
        ));
    };
    if json {
        print_json(disp)?;
    } else {
        println!(
            "dispatch: {}\nworker:   {}\nagent:    {}\nstatus:   {}\nrun:      {}\ntask:     {}\nsession:  {}\ncwd:      {}\nprompt:   {}",
            disp.id,
            disp.worker_name,
            disp.agent,
            disp.status.as_str(),
            disp.run_id,
            disp.task_id.as_deref().unwrap_or("-"),
            disp.session_id
                .map_or_else(|| "-".to_owned(), |n| n.to_string()),
            disp.cwd,
            disp.prompt
        );
    }
    Ok(())
}

fn cmd_worker_read_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let Some((target_name, rest)) = args.split_first() else {
        return Err(bad_arg("missing <worker-or-dispatch>"));
    };
    let mut lines = 40_usize;
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--lines" => {
                let raw = take_opt(&mut it, "--lines")?;
                lines = raw
                    .parse::<usize>()
                    .map_err(|_| bad_arg("invalid --lines"))?;
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let store = ctx.load_store()?;
    let Some(disp) = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == *target_name || d.worker_name == *target_name)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("worker not found: {target_name}"),
        ));
    };
    let target = ctx.effective_target()?;
    let mut screen_text = match (disp.status, disp.session_id) {
        (krill_proto::orchestration::DispatchStatus::Released, _) | (_, None) => {
            disp.archived_output.clone().unwrap_or_default()
        }
        (_, Some(sid)) => crate::session::capture_session_text_on_target(&target, sid)
            .ok()
            .or_else(|| disp.archived_output.clone())
            .unwrap_or_default(),
    };
    if screen_text.trim().is_empty() {
        screen_text = disp.summary.clone().unwrap_or_default();
    }
    let all_lines: Vec<&str> = screen_text.lines().collect();
    let start = all_lines.len().saturating_sub(lines);
    let tail = all_lines.get(start..).unwrap_or_default().join("\n");
    if json {
        print_json(&serde_json::json!({
            "dispatch_id": disp.id,
            "worker": disp.worker_name,
            "session_id": disp.session_id,
            "output": tail,
        }))?;
    } else {
        println!("{tail}");
    }
    Ok(())
}

#[cfg(test)]
fn cmd_worker_stop(state_dir: &Path, args: &[String], release: bool) -> io::Result<()> {
    cmd_worker_stop_ctx(&OrchCtx::local(state_dir), args, release)
}

fn cmd_worker_stop_ctx(ctx: &OrchCtx<'_>, args: &[String], release: bool) -> io::Result<()> {
    let Some((target_name, rest)) = args.split_first() else {
        return Err(bad_arg("missing <worker-or-dispatch>"));
    };
    if let Some(arg) = rest.iter().find(|a| *a != "--json") {
        return Err(bad_arg(format!("unexpected argument {arg}")));
    }
    let json = rest.iter().any(|a| a == "--json");
    let store = ctx.load_store()?;
    let selected = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == *target_name || d.worker_name == *target_name)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("worker not found: {target_name}"),
            )
        })?;
    let target = ctx.effective_target()?;
    let captured = match selected.session_id {
        Some(sid) if selected.status != krill_proto::orchestration::DispatchStatus::Released => {
            match crate::session::capture_session_text_on_target(&target, sid) {
                Ok(text) => Some(text),
                Err(capture_err) => settle_missing_session_capture(
                    &selected.worker_name,
                    sid,
                    capture_err,
                    selected.archived_output.clone(),
                    session_present_on_target(&target, sid),
                )?,
            }
        }
        _ => selected.archived_output.clone(),
    };
    // Persist terminal output before closing when using local store fallback.
    // Failure to close must leave the dispatch status unchanged.
    if let Some(ref text) = captured
        && !ctx.is_remote()
        && ctx.state_dir.join("orchestration.json").exists()
    {
        OrchestrationStore::mutate(ctx.state_dir, |s| {
            let stored = s
                .dispatches
                .iter_mut()
                .find(|d| d.id == selected.id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "dispatch missing"))?;
            stored.archived_output = Some(text.clone());
            Ok(())
        })?;
    }
    let disp = complete_worker_stop_ctx(ctx, &selected.id, release, captured, || {
        if selected.status != krill_proto::orchestration::DispatchStatus::Released
            && let Some(sid) = selected.session_id
            && crate::session::list_session_ids_on_target(&target)
                .map_err(io::Error::other)?
                .contains(&sid)
        {
            crate::session::close_session_on_target(&target, sid).map_err(io::Error::other)?;
        }
        Ok(())
    })?;
    if json {
        print_json(&disp)?;
    } else {
        println!(
            "{}\t{}\t{}",
            disp.id,
            disp.worker_name,
            disp.status.as_str()
        );
    }
    Ok(())
}

fn cmd_worker_abandon_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let Some((target_name, rest)) = args.split_first() else {
        return Err(bad_arg("missing <worker-or-dispatch>"));
    };
    if let Some(arg) = rest.iter().find(|a| *a != "--json") {
        return Err(bad_arg(format!("unexpected argument {arg}")));
    }
    let json = rest.iter().any(|a| a == "--json");
    let store = ctx.load_store()?;
    let selected = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == *target_name || d.worker_name == *target_name)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("worker not found: {target_name}"),
            )
        })?;
    let target = ctx.effective_target()?;
    if let Some(sid) = selected.session_id
        && let Ok(ids) = crate::session::list_session_ids_on_target(&target)
        && ids.contains(&sid)
    {
        crate::session::close_session_on_target(&target, sid).ok();
    }
    let disp = match ctx.try_ipc(
        &OrchAction::AbandonWorker {
            worker_or_dispatch: selected.id.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Dispatch(d)) => d,
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| s.abandon_worker(&selected.id))?,
    };
    if json {
        print_json(&disp)?;
    } else {
        println!(
            "{}\t{}\t{}",
            disp.id,
            disp.worker_name,
            disp.status.as_str()
        );
    }
    Ok(())
}

#[cfg(test)]
fn complete_worker_stop(
    state_dir: &Path,
    dispatch_id: &str,
    release: bool,
    close: impl FnOnce() -> io::Result<()>,
) -> io::Result<krill_proto::orchestration::Dispatch> {
    complete_worker_stop_ctx(
        &OrchCtx::local(state_dir),
        dispatch_id,
        release,
        None,
        close,
    )
}

fn complete_worker_stop_ctx(
    ctx: &OrchCtx<'_>,
    dispatch_id: &str,
    release: bool,
    archived_output: Option<String>,
    close: impl FnOnce() -> io::Result<()>,
) -> io::Result<krill_proto::orchestration::Dispatch> {
    close()?;
    match ctx.try_ipc(
        &OrchAction::StopOrReleaseWorker {
            worker_or_dispatch: dispatch_id.to_owned(),
            release,
            archived_output: archived_output.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Dispatch(d)) => Ok(d),
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| {
            if let Some(text) = archived_output
                && let Some(stored) = s.dispatches.iter_mut().find(|d| d.id == dispatch_id)
            {
                stored.archived_output = Some(text);
            }
            s.stop_or_release_worker(dispatch_id, release)
        }),
    }
}

/// `worker-start --session <id>`용 세션 존재 확인. 데몬에 닿지 않으면 그 오류를 그대로 돌려준다.
fn ensure_worker_session_exists_on_target(
    target: &Target,
    worker: &str,
    sid: u64,
) -> io::Result<()> {
    match crate::session::list_session_ids_on_target(target) {
        Ok(ids) => reject_missing_worker_session(worker, sid, &ids),
        Err(e) => Err(io::Error::other(format!("worker {worker}: {e}"))),
    }
}

/// 순수 판정 부분(테스트 가능): 목록에 없는 세션이면 `NotFound`(종료 1)로 거부한다.
fn reject_missing_worker_session(worker: &str, sid: u64, sessions: &[u64]) -> io::Result<()> {
    if sessions.contains(&sid) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("worker {worker}: session {sid} not found"),
        ))
    }
}

/// 데몬 세션 목록에서 존재 여부를 확인한다. 데몬에 닿지 않으면 `None`(모름 → 기존 실패 유지).
fn session_present_on_target(target: &Target, sid: u64) -> Option<bool> {
    crate::session::list_session_ids_on_target(target)
        .ok()
        .map(|ids| ids.contains(&sid))
}

/// `worker-stop`/`worker-release`의 capture 실패 시 정산 판단(C1).
/// 세션이 이미 없고 아카이브도 없으면 `Ok(None)`(출력 없이 종료 상태로 전이)한다.
/// 살아 있는 세션은 절대 닫지 않는다 — 닫기는 호출자의 close 클로저가 존재 확인 후 수행한다.
fn settle_missing_session_capture(
    worker: &str,
    sid: u64,
    capture_err: String,
    archived: Option<String>,
    present: Option<bool>,
) -> io::Result<Option<String>> {
    if let Some(text) = archived {
        return Ok(Some(text));
    }
    if present == Some(false) {
        eprintln!(
            "orchestration: worker {worker}: session {sid} already closed; settling without captured output"
        );
        return Ok(None);
    }
    Err(io::Error::other(capture_err))
}

#[cfg(test)]
fn cmd_reset(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_reset_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_reset_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut run_id = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--run" => run_id = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    if ctx
        .try_ipc(
            &OrchAction::Reset {
                run_id: run_id.clone(),
            },
            Duration::from_secs(2),
        )?
        .is_none()
    {
        OrchestrationStore::mutate(ctx.state_dir, |s| {
            s.reset(run_id.as_deref());
            Ok(())
        })?;
    }
    if json {
        print_json(&serde_json::json!({ "reset": true, "run": run_id }))?;
    } else {
        println!("orchestration state reset");
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(val: &T) -> io::Result<()> {
    let text = serde_json::to_string_pretty(val).map_err(io::Error::other)?;
    println!("{text}");
    Ok(())
}

fn bad_arg(msg: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

fn take_opt<'a>(args: &mut std::slice::Iter<'a, String>, name: &str) -> io::Result<&'a String> {
    args.next()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| bad_arg(format!("missing {name}")))
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::orchestration::DispatchStatus;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn option_and_flag_helpers() -> io::Result<()> {
        let raw = args(&["codex", "--json"]);
        let mut it = raw.iter();
        assert_eq!(take_opt(&mut it, "--agent")?, "codex");
        assert_eq!(it.next().map(String::as_str), Some("--json"));
        assert_eq!(
            take_opt(&mut it, "--agent").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(has_flag(&raw, "--json"));
        assert!(!has_flag(&raw, "--wait"));
        assert!(!has_flag(&[], "--json"));
        assert!(take_opt(&mut args(&["--json"]).iter(), "--agent").is_err());
        Ok(())
    }

    #[test]
    fn cli_dispatch_completion_promotes_dependent_task() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-cli-orch-dispatch-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        cmd_run_create(&dir, &args(&["--title", "CLI flow", "--json"]))?;
        let run = OrchestrationStore::load(&dir)?
            .runs
            .first()
            .unwrap()
            .clone();
        cmd_task_create(
            &dir,
            &args(&["--run", &run.id, "--title", "Implement", "--json"]),
        )?;
        let first = OrchestrationStore::load(&dir)?
            .tasks
            .first()
            .unwrap()
            .clone();
        cmd_task_create(
            &dir,
            &args(&["--run", &run.id, "--title", "Verify", "--deps", &first.id]),
        )?;
        assert_eq!(
            OrchestrationStore::load(&dir)?.tasks.get(1).unwrap().status,
            TaskStatus::Pending
        );
        cmd_worker_start(
            &dir,
            &args(&[
                "--run",
                &run.id,
                "--task",
                &first.id,
                "--worker",
                "cli-worker",
                "--agent",
                "codex",
                "--cwd",
                &dir.to_string_lossy(),
                "--prompt",
                "Implement",
                "--no-spawn",
                "--json",
            ]),
        )?;
        let dispatch = OrchestrationStore::load(&dir)?
            .dispatches
            .first()
            .unwrap()
            .clone();
        assert_eq!(dispatch.session_id, None);
        assert_eq!(dispatch.status, DispatchStatus::Active);
        cmd_send(
            &dir,
            &args(&[
                "--from",
                "cli-worker",
                "--to",
                "coordinator",
                "--type",
                "worker_done",
                "--summary",
                "Implementation complete",
                "--body",
                "Ready to verify",
                "--run",
                &run.id,
                "--task",
                &first.id,
                "--dispatch",
                &dispatch.id,
                "--outcome",
                "succeeded",
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        assert_eq!(store.tasks.first().unwrap().status, TaskStatus::Completed);
        assert_eq!(store.tasks.get(1).unwrap().status, TaskStatus::Ready);
        assert_eq!(
            store.dispatches.first().unwrap().status,
            DispatchStatus::Succeeded
        );
        cmd_check(
            &dir,
            &args(&[
                "--to",
                "coordinator",
                "--types",
                "worker_done",
                "--unread",
                "--ack",
                "--json",
            ]),
        )?;
        assert!(
            OrchestrationStore::load(&dir)?
                .messages
                .iter()
                .any(|message| { message.msg_type == MessageType::WorkerDone && message.acked })
        );
        cmd_run_list(&dir, &args(&["--json"]))?;
        cmd_task_list(&dir, &args(&["--run", &run.id, "--json"]))?;
        cmd_worker_list(&dir, &args(&["--run", &run.id, "--json"]))?;
        cmd_worker_show(&dir, &args(&["cli-worker", "--json"]))?;
        cmd_worker_stop(&dir, &args(&["cli-worker", "--json"]), true)?;
        assert_eq!(
            OrchestrationStore::load(&dir)?
                .dispatches
                .first()
                .unwrap()
                .status,
            DispatchStatus::Released
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn cli_questions_gates_and_reset_are_isolated() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-cli-orch-messages-{}", std::process::id()));
        let other = dir.join("other-state");
        std::fs::remove_dir_all(&dir).ok();
        cmd_ask(
            &dir,
            &args(&[
                "--from",
                "cli-worker",
                "--to",
                "coordinator",
                "--question",
                "Proceed?",
                "--json",
            ]),
        )?;
        let question = OrchestrationStore::load(&dir)?
            .messages
            .first()
            .unwrap()
            .clone();
        cmd_reply(
            &dir,
            &args(&[&question.id, "--answer", "Proceed", "--json"]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        let answer = store
            .messages
            .iter()
            .find(|message| message.reply_to.as_deref() == Some(question.id.as_str()))
            .unwrap();
        assert_eq!(answer.to, "cli-worker");
        assert_eq!(answer.body, "Proceed");
        assert_eq!(answer.msg_type, MessageType::Answer);
        cmd_gate_create(
            &dir,
            &args(&[
                "--from",
                "cli-worker",
                "--question",
                "Ship?",
                "--options",
                "approve,reject",
                "--json",
            ]),
        )?;
        let gate = OrchestrationStore::load(&dir)?
            .gates
            .first()
            .unwrap()
            .clone();
        assert!(cmd_gate_resolve(&dir, &args(&[&gate.id, "--decision", ""])).is_err());
        assert_eq!(
            OrchestrationStore::load(&dir)?
                .gates
                .first()
                .unwrap()
                .status,
            GateStatus::Pending
        );
        cmd_gate_resolve(
            &dir,
            &args(&[
                &gate.id,
                "--decision",
                "approve",
                "--rationale",
                "Reviewed",
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        let resolved = store.gates.first().unwrap();
        assert_eq!(resolved.status, GateStatus::Resolved);
        assert_eq!(resolved.decision.as_deref(), Some("approve"));
        assert_eq!(resolved.rationale.as_deref(), Some("Reviewed"));
        cmd_gate_list(&dir, &args(&["--pending", "--json"]))?;
        assert_eq!(
            OrchestrationStore::load(&other)?,
            OrchestrationStore::default()
        );
        let next_seq = store.next_seq;
        cmd_reset(&dir, &args(&["--json"]))?;
        let reset = OrchestrationStore::load(&dir)?;
        assert!(reset.messages.is_empty());
        assert!(reset.gates.is_empty());
        assert_eq!(reset.next_seq, next_seq);
        std::fs::remove_dir_all(dir)
    }
    #[test]
    fn worker_start_rejects_unknown_session_before_mutate() {
        assert!(reject_missing_worker_session("w1", 7, &[7, 8]).is_ok());
        let err = reject_missing_worker_session("w1", 999, &[7, 8]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("999"), "{err}");
        assert!(err.to_string().contains("w1"), "{err}");
    }

    #[test]
    fn worker_stop_settles_already_closed_session_without_output() -> io::Result<()> {
        // 아카이브가 있으면 세션 소실에도 그것을 쓴다.
        assert_eq!(
            settle_missing_session_capture(
                "w",
                1,
                "no such session".to_owned(),
                Some("out".to_owned()),
                Some(false)
            )?,
            Some("out".to_owned())
        );
        // 세션 소실 + 아카이브 없음 → 출력 없이 정산(종료 상태 전이 허용).
        assert_eq!(
            settle_missing_session_capture(
                "w",
                1,
                "no such session".to_owned(),
                None,
                Some(false)
            )?,
            None
        );
        // 세션이 살아 있으면 원래 오류를 유지한다(섣불리 성공하지 않음).
        assert!(
            settle_missing_session_capture("w", 1, "boom".to_owned(), None, Some(true)).is_err()
        );
        // 데몬 목록을 못 읽으면(모름) 원래 오류를 유지한다.
        assert!(settle_missing_session_capture("w", 1, "boom".to_owned(), None, None).is_err());
        Ok(())
    }

    #[test]
    fn ghost_dispatch_without_output_still_reaches_terminal_status() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-ghost-settle-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let stopped_target = OrchestrationStore::mutate(&dir, |store| {
            let run = store.create_run("Ghost settle", None)?;
            let task = store.create_task(&run.id, "Work", "", Vec::new())?;
            store.start_worker(
                &run.id,
                Some(&task.id),
                "ghost",
                "codex",
                Some(999),
                "/tmp",
                "p",
            )
        })?;
        // 세션이 이미 gone이라 close할 것이 없다는 상황을 no-op close로 재현한다.
        let stopped = complete_worker_stop(&dir, &stopped_target.id, false, || Ok(()))?;
        assert_eq!(stopped.status, DispatchStatus::Stopped);
        let released_target = OrchestrationStore::mutate(&dir, |store| {
            store.start_worker(
                &stopped.run_id,
                None,
                "ghost-release",
                "codex",
                Some(999),
                "/tmp",
                "p",
            )
        })?;
        let released = complete_worker_stop(&dir, &released_target.id, true, || Ok(()))?;
        assert_eq!(released.status, DispatchStatus::Released);
        // task에 묶인 dispatch가 stop되면 task 바인딩이 풀린다(영구 Active 방지).
        let store = OrchestrationStore::load(&dir)?;
        let task_id = stopped_target.task_id.clone().unwrap_or_default();
        let task = store.tasks.iter().find(|t| t.id == task_id).unwrap();
        assert_eq!(task.active_dispatch_id, None);
        assert_eq!(task.status, TaskStatus::Ready);
        std::fs::remove_dir_all(dir)
    }
    #[test]
    fn close_failure_preserves_dispatch_and_task_status() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-close-failure-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let dispatch = OrchestrationStore::mutate(&dir, |store| {
            let run = store.create_run("Close failure", None)?;
            let task = store.create_task(&run.id, "Work", "", Vec::new())?;
            store.start_worker(
                &run.id,
                Some(&task.id),
                "worker",
                "codex",
                Some(1),
                "/tmp",
                "p",
            )
        })?;
        let before = OrchestrationStore::load(&dir)?;
        for release in [false, true] {
            let result = complete_worker_stop(&dir, &dispatch.id, release, || {
                Err(io::Error::other("injected close failure"))
            });
            assert_eq!(result.unwrap_err().to_string(), "injected close failure");
            assert_eq!(OrchestrationStore::load(&dir)?, before);
        }
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn run_use_spec_abandon_and_depth_guard_work() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-cli-orch-spec-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let ctx = OrchCtx::local(&dir);
        cmd_run_create_ctx(&ctx, &args(&["--title", "Run One", "--json"]))?;
        cmd_run_create_ctx(&ctx, &args(&["--title", "Run Two", "--json"]))?;
        let runs = OrchestrationStore::load(&dir)?.runs;
        let run1 = &runs[0].id;
        let run2 = &runs[1].id;

        cmd_run_use_ctx(&ctx, &args(&[run1, "--json"]))?;
        cmd_run_current_ctx(&ctx, &args(&["--json"]))?;
        assert_eq!(
            OrchestrationStore::load(&dir)?.current_run_id.as_deref(),
            Some(run1.as_str())
        );

        // task-create without --run uses current_run_id (run1)
        cmd_task_create_ctx(&ctx, &args(&["--title", "Task via active run", "--json"]))?;
        let task = OrchestrationStore::load(&dir)?
            .tasks
            .first()
            .unwrap()
            .clone();
        assert_eq!(&task.run_id, run1);

        // worker-start with --spec JSON and depth guard
        let spec_json = serde_json::json!({
            "task_id": task.id,
            "worker": "spec-w1",
            "agent": "codex",
            "cwd": dir.to_string_lossy(),
            "prompt": "Line 1\nLine 2",
            "depth": 1,
            "max_depth": 2,
            "no_spawn": true,
        })
        .to_string();
        cmd_worker_start_ctx(&ctx, &args(&["--spec", &spec_json, "--json"]))?;
        let store_after_d1 = OrchestrationStore::load(&dir)?;
        let d1 = store_after_d1.dispatches.first().unwrap().clone();
        assert_eq!(d1.worker_name, "spec-w1");
        assert_eq!(d1.depth, 1);
        assert_eq!(d1.status, DispatchStatus::Active);

        // Exceeding max_depth is rejected
        assert!(
            cmd_worker_start_ctx(
                &ctx,
                &args(&[
                    "--worker",
                    "too-deep",
                    "--prompt",
                    "p",
                    "--depth",
                    "3",
                    "--max-depth",
                    "2",
                    "--no-spawn"
                ])
            )
            .is_err()
        );

        // Abandoning d1 frees the task for replacement
        cmd_worker_abandon_ctx(&ctx, &args(&["spec-w1", "--json"]))?;
        cmd_worker_start_ctx(
            &ctx,
            &args(&[
                "--task",
                &task.id,
                "--worker",
                "spec-w2",
                "--cwd",
                &dir.to_string_lossy(),
                "--prompt",
                "Retry",
                "--no-spawn",
                "--json",
            ]),
        )?;
        let d2 = OrchestrationStore::load(&dir)?
            .dispatches
            .get(1)
            .unwrap()
            .clone();
        assert_eq!(d2.worker_name, "spec-w2");
        assert_eq!(d2.status, DispatchStatus::Active);

        cmd_run_use_ctx(&ctx, &args(&[run2]))?;
        assert_eq!(
            OrchestrationStore::load(&dir)?.current_run_id.as_deref(),
            Some(run2.as_str())
        );
        cmd_run_use_ctx(&ctx, &args(&["--clear"]))?;
        std::fs::remove_dir_all(dir)
    }
}
