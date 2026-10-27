//! `krill orchestration` (`krill orch`) CLI — Multi-Agent Orchestration (MK-114 / OWE-624).
//!
//! Coordinates multi-agent runs, task DAGs, worker dispatches, threaded messages,
//! blocking ask/reply, and decision gates backed by `state_dir/orchestration.json`.

use krill_proto::os::ffi::OsStrExt;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use std::io::Write;

use krill_proto::control::{FEAT_CLI, FEAT_CONTROL, Failure, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::orchestration::{
    DEFAULT_MAX_WORKER_DEPTH, Dispatch, DispatchSessionOrigin, DispatchStatus,
    FEAT_ORCH_SNAPSHOT_PAGE, FRAME_LIMIT_ERROR, GateStatus, MessageType, OrchAction, OrchResponse,
    OrchestrationStore, Run, SendParams, TaskStatus, WorkerOperation, WorkerStartSpec,
    worker_preamble,
};
use krill_proto::screen::Msg;

use crate::remote::Target;

const USAGE: &str = "\
usage: krill [--host <user@host>] orchestration <subcommand> [options]
  run-create --title <title> [--coordinator <session-id>] [--json]
  run-show [<run-id>] [--json]
  run-use <run-id|--clear> [--json]
  run-current [--json]
  run-list [--json]
  task-create [--run <run-id>] --title <title> [--desc <text>] [--deps <t1,t2>] [--json]
  task-list [--run <run-id>] [--json]
  task-update <task-id> --status <pending|ready|dispatched|completed|failed|blocked> [--summary <text>] [--json]
  worker-start [--spec <json|text>] [--run <run-id>] [--task <task-id>] --worker <name> [--agent <agent>] [--account <id>] [--session <id>] [--cwd <dir>] [--worktree current|<path>|new-child] [--name <name>] [--base <ref>] [--setup run|skip] [--trust-setup <hash>] [--model <str>] [--effort <minimal|low|medium|high|xhigh|max>] [--retry-of <dispatch-id>] [--retry-request <req-id>] [--depth <N>] [--max-depth <N>] [--preamble <full|minimal|none>|--no-preamble] --prompt <text> [--json]
    new-child requires --name, uses the enclosing worktree of --cwd (default: current directory) as parent, and does not support --host.
  send --from <name> [--to <target>] [--type <type>] --summary <text> [--body <text>] [--run <id>] [--task <id>] [--dispatch <id>] [--outcome <succeeded|failed>] [--json]
  check [--to <name>] [--run <id>] [--types <t1,t2>] [--unread] [--ack [<delivery-id>]|--ack-ids <id1,id2,...>] [--wait [<secs>]] [--timeout-ms <N>] [--json]
  inbox [--to <name>] [--run <id>] [--all] [--json]
  ask (--from <name> [--to <target>] --summary <question> [--body <text>] | --question-id <msg-id>) [--wait] [--timeout-ms <N>] [--json]
  reply <msg-id> [--from <name>] --body <answer> [--json]
  gate-create --from <name> --question <text> [--options <a,b>] [--run <id>] [--task <id>] [--json]
  gate-list [--pending] [--json]
  gate-resolve <gate-id> --decision <choice> [--rationale <text>] [--by <name>] [--json]
  worker-list [--run <run-id>] [--terminal-state <state[,state...]>] [--json]
  worker-show <worker-or-dispatch> [--json]
  dispatch-show [<dispatch-or-worker>] [--dispatch <id>] [--task <task-id>] [--preamble] [--json]
  request-show <req-id> [--json]
  worker-read <worker-or-dispatch> [--lines <N>] [--json]
  worker-stop <worker-or-dispatch> [--json]
  worker-retain <worker-or-dispatch> [--json]
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

    fn can_query_daemon(&self) -> bool {
        self.is_remote() || daemon::state_dir().as_deref() == Some(self.state_dir)
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
        let Some(mut conn) = self.connect_ipc()? else {
            return Ok(None);
        };
        self.request_ipc(&mut conn, action, timeout)
    }

    /// Opens one orchestration connection (see [`Self::try_ipc`]); `Ok(None)` means local without a
    /// listening daemon. The control connection serves several requests, so paged reads reuse it.
    fn connect_ipc(&self) -> io::Result<Option<krill_proto::daemon::Connection>> {
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
        let conn = if is_remote {
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
        Ok(Some(conn))
    }

    /// One request on an open connection. `Ok(None)`: an older local daemon rejected a legacy
    /// opcode, so the caller may apply the same change on disk (never for `requires_v2` opcodes).
    fn request_ipc(
        &self,
        conn: &mut krill_proto::daemon::Connection,
        action: &OrchAction,
        timeout: Duration,
    ) -> io::Result<Option<OrchResponse>> {
        let is_remote = self.is_remote();
        if matches!(action, OrchAction::RetainWorker { .. })
            && conn.negotiated.features & krill_proto::orchestration::FEAT_ORCH_RETAIN == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "update krilld to retain workers",
            ));
        }
        let io_wait = timeout.saturating_add(Duration::from_secs(5));
        conn.stream.set_read_timeout(Some(io_wait))?;
        conn.stream
            .set_write_timeout(Some(Duration::from_secs(5)))?;
        let encoded = Request::Orchestration(action.clone())
            .encode()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        // A partial write or missing reply cannot prove that the daemon did not mutate.
        conn.stream.write_all(&encoded)?;
        let (kind, body) =
            ipc::read_frame(&mut conn.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        let reply = Reply::decode(kind, &body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        match reply {
            Reply::Orchestration(OrchResponse::Error(msg)) => Err(classify_orch_error(msg)),
            Reply::Orchestration(resp) => Ok(Some(resp)),
            Reply::Failed(Failure::BadRequest) if action.requires_v2() => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "update krilld for orchestration v2",
            )),
            Reply::Failed(Failure::BadRequest) if !is_remote => Ok(None),
            Reply::Failed(f) => Err(classify_daemon_failure(f)),
            _ => Err(io::Error::other("unexpected orchestration reply")),
        }
    }

    /// Read-only snapshot. A daemon with `FEAT_ORCH_SNAPSHOT_PAGE` streams it in bounded pages so
    /// a valid store larger than one IPC frame is still read losslessly. An older daemon serves one
    /// `SnapshotV2` frame; if that frame cannot fit (EOF from a prior build, or the explicit frame-limit
    /// error), a local caller reads the same `orchestration.json` the daemon serves — read-only, never
    /// for mutations — while a remote caller gets an explicit error instead of any local state.
    fn load_store(&self) -> io::Result<OrchestrationStore> {
        let Some(mut conn) = self.connect_ipc()? else {
            return OrchestrationStore::load(self.state_dir);
        };
        let timeout = Duration::from_secs(2);
        let fetched = if conn.negotiated.features & FEAT_ORCH_SNAPSHOT_PAGE != 0 {
            OrchestrationStore::fetch_paged(|generation, offset| {
                self.request_ipc(
                    &mut conn,
                    &OrchAction::SnapshotPage { generation, offset },
                    timeout,
                )?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "update krilld for orchestration v2",
                    )
                })
            })
        } else {
            match self.request_ipc(&mut conn, &OrchAction::SnapshotV2, timeout) {
                Ok(Some(OrchResponse::Snapshot(store) | OrchResponse::SnapshotV2(store))) => {
                    Ok(store)
                }
                Ok(Some(_)) => Err(io::Error::other("unexpected orchestration reply")),
                Ok(None) => return OrchestrationStore::load(self.state_dir),
                Err(e) => Err(e),
            }
        };
        match fetched {
            Err(e) if snapshot_unavailable(&e) => {
                if self.is_remote() {
                    Err(io::Error::other(format!(
                        "remote orchestration state exceeds the IPC frame limit; update the remote krilld ({e})"
                    )))
                } else {
                    OrchestrationStore::load(self.state_dir)
                }
            }
            other => other,
        }
    }
}

/// A single-frame snapshot that an older daemon could not deliver: a prior build closes the stream
/// (EOF) and the current build answers with the frame-limit error. Nothing else qualifies.
fn snapshot_unavailable(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::UnexpectedEof || e.to_string().contains(FRAME_LIMIT_ERROR)
}

fn classify_orch_error(msg: String) -> io::Error {
    if msg.contains("not found") || msg.contains("no active run") {
        io::Error::new(io::ErrorKind::NotFound, msg)
    } else if msg.contains("must not")
        || msg.contains("not in gate options")
        || msg.contains("already resolved")
        || msg.contains("does not belong")
        || msg.contains("already acknowledged")
        || msg.contains("does not match")
        || msg.contains("ack_ids")
        || msg.contains("exceeds")
        || msg.contains("already has an active")
        || msg.contains("cannot dispatch")
        || msg.contains("unknown ")
        || msg.contains("invalid ")
        || msg.contains("max_depth")
    {
        io::Error::new(io::ErrorKind::InvalidInput, msg)
    } else {
        io::Error::other(msg)
    }
}

fn classify_daemon_failure(f: Failure) -> io::Error {
    classify_orch_error(f.to_string())
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
    // 서브커맨드 자리만 도움말로 본다. 하위 명령의 프롬프트 텍스트(`ask ... help`)는 건드리지 않는다.
    if matches!(
        args.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let Some((sub, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if matches!(rest.first().map(String::as_str), Some("--help" | "-h")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    // 원격에서 지원하지 않는 배치는 SSH 터널을 열기 전에 거부한다.
    if sub == "worker-start"
        && let Err(e) = parse_worker_start(rest, host.is_some())
    {
        eprintln!("orchestration: {e}\n{USAGE}");
        return ExitCode::from(2);
    }
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
        "run-show" => cmd_run_show_ctx(&ctx, rest),
        "run-use" => cmd_run_use_ctx(&ctx, rest),
        "run-current" => cmd_run_current_ctx(&ctx, rest),
        "run-list" | "runs" => cmd_run_list_ctx(&ctx, rest),
        "task-create" => cmd_task_create_ctx(&ctx, rest),
        "task-list" | "tasks" => cmd_task_list_ctx(&ctx, rest),
        "task-update" => cmd_task_update_ctx(&ctx, rest),
        "worker-start" => cmd_worker_start_ctx(&ctx, rest),
        "send" => cmd_send_ctx(&ctx, rest),
        "check" => cmd_check_ctx(&ctx, rest),
        "inbox" => cmd_inbox_ctx(&ctx, rest),
        "ask" => cmd_ask_ctx(&ctx, rest),
        "reply" => cmd_reply_ctx(&ctx, rest),
        "gate-create" => cmd_gate_create_ctx(&ctx, rest),
        "gate-list" | "gates" => cmd_gate_list_ctx(&ctx, rest),
        "gate-resolve" => cmd_gate_resolve_ctx(&ctx, rest),
        "worker-list" | "workers" => cmd_worker_list_ctx(&ctx, rest),
        "worker-show" => cmd_worker_show_ctx(&ctx, rest),
        "dispatch-show" => cmd_dispatch_show_ctx(&ctx, rest),
        "request-show" => cmd_request_show_ctx(&ctx, rest),
        "worker-read" => cmd_worker_read_ctx(&ctx, rest),
        "worker-stop" => cmd_worker_stop_ctx(&ctx, rest, false),
        "worker-retain" => cmd_worker_retain_ctx(&ctx, rest),
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
            "--title" | "--objective" => title = it.next().cloned(),
            "--worktree" | "--cwd" => {
                it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
            }
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

fn cmd_run_show_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut given = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--id" | "--run" => {
                let id = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                given = Some(id.clone());
            }
            "--json" => json = true,
            s if !s.starts_with('-') && given.is_none() => {
                given = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let store = ctx.load_store()?;
    let run_id = store.resolve_run_for_session(given.as_deref(), caller_session_id())?;
    let Some(run) = store.runs.iter().find(|r| r.id == run_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("run not found: {run_id}"),
        ));
    };
    let tasks_in_run: Vec<_> = store.tasks.iter().filter(|t| t.run_id == run_id).collect();
    let dispatches_in_run: Vec<_> = store
        .dispatches
        .iter()
        .filter(|d| d.run_id == run_id)
        .collect();
    if json {
        print_json(&serde_json::json!({
            "run": run,
            "tasks": tasks_in_run,
            "workers": dispatches_in_run,
        }))?;
    } else {
        println!(
            "{}\t{}\ttasks={}\tworkers={}\t{}",
            run.id,
            run.status.as_str(),
            tasks_in_run.len(),
            dispatches_in_run.len(),
            run.title
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
            "--run" | "--id" => {
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
            session_id: caller_session_id(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::CurrentRun(c)) => c,
        Some(OrchResponse::Error(error)) => return Err(bad_arg(error)),
        Some(_) => return Err(io::Error::other("unexpected run selection reply")),
        None => OrchestrationStore::mutate(ctx.state_dir, |s| {
            s.set_session_run(caller_session_id(), run_id.as_deref())
        })?,
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
        .resolve_run_for_session(None, caller_session_id())
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
            "--title" | "--task-title" => title = it.next().cloned(),
            "--desc" | "--description" | "--spec" => {
                desc = it.next().cloned().unwrap_or_default();
            }
            "--parent" => {
                let p = it.next().ok_or_else(|| bad_arg("missing --parent"))?;
                if !p.trim().is_empty() {
                    deps.push(p.trim().to_owned());
                }
            }
            "--deps" => {
                if let Some(raw) = it.next() {
                    if raw.trim_start().starts_with('[') {
                        let parsed = serde_json::from_str::<Vec<String>>(raw)
                            .map_err(|e| bad_arg(format!("invalid --deps JSON: {e}")))?;
                        deps.extend(
                            parsed
                                .into_iter()
                                .map(|s| s.trim().to_owned())
                                .filter(|s| !s.is_empty()),
                        );
                    } else {
                        deps.extend(
                            raw.split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned),
                        );
                    }
                }
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    if title.is_none() && !desc.trim().is_empty() {
        let first_line = desc.lines().next().unwrap_or_default().trim();
        let derived: String = first_line.chars().take(80).collect();
        if !derived.is_empty() {
            title = Some(derived);
        }
    }
    let title = title.ok_or_else(|| bad_arg("missing --title"))?;
    let resolved_run_id = match run_id {
        Some(id) => id,
        None => ctx
            .load_store()?
            .resolve_run_for_session(None, caller_session_id())?,
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
    let mut task_id = None;
    let mut status = None;
    let mut summary = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--id" | "--task" => {
                let id = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                task_id = Some(id.clone());
            }
            "--status" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --status"))?;
                status = Some(TaskStatus::parse(raw)?);
            }
            "--summary" | "--result" => summary = it.next().cloned(),
            "--json" => json = true,
            s if !s.starts_with('-') && task_id.is_none() => {
                task_id = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let task_id = task_id.ok_or_else(|| bad_arg("missing <task-id>"))?;
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
            OrchestrationStore::mutate(ctx.state_dir, |s| s.update_task(&task_id, status, summary))?
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PreambleMode {
    #[default]
    Full,
    Minimal,
    None,
}

impl PreambleMode {
    fn parse(raw: &str) -> io::Result<Self> {
        match raw {
            "full" => Ok(Self::Full),
            "minimal" => Ok(Self::Minimal),
            "none" => Ok(Self::None),
            _ => Err(bad_arg(format!(
                "invalid --preamble {raw} (expected full|minimal|none)"
            ))),
        }
    }
}

/// `--preamble full|minimal|none`: 워커 세션에 처음 보낼 지시문. `none`이면 아무것도 보내지 않는다.
fn format_worker_preamble(
    dispatch: &krill_proto::orchestration::Dispatch,
    mode: PreambleMode,
) -> Option<String> {
    match mode {
        PreambleMode::Full => Some(worker_preamble(dispatch)),
        PreambleMode::Minimal => Some(format!(
            "[Krill Worker] run={} task={} dispatch={} worker={}: {}\nRead `krill skills get orchestration --reference references/worker-contract.md` using KRILL_CLI when set; use Krill CLI reporting, not Orca or native subagents.",
            dispatch.run_id,
            dispatch.task_id.as_deref().unwrap_or("-"),
            dispatch.id,
            dispatch.worker_name,
            dispatch.prompt.replace('\n', " | ")
        )),
        PreambleMode::None => None,
    }
}

#[cfg(test)]
fn cmd_worker_start(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_worker_start_ctx(&OrchCtx::local(state_dir), args)
}

#[derive(Debug)]
struct WorkerStartArgs {
    spec: WorkerStartSpec,
    explicit_agent: Option<String>,
    explicit_depth: Option<u32>,
    explicit_max_depth: Option<u32>,
    preamble_mode: PreambleMode,
    auto_create_task: bool,
    json: bool,
    child_name: Option<String>,
    worktree_options: Vec<OsString>,
}

fn parse_worker_start(args: &[String], remote: bool) -> io::Result<WorkerStartArgs> {
    let mut spec = WorkerStartSpec::default();
    let mut new_child = false;
    let mut child_name: Option<String> = None;
    let mut worktree_options = Vec::new();
    let mut explicit_agent: Option<String> = None;
    let mut explicit_depth: Option<u32> = None;
    let mut explicit_max_depth: Option<u32> = None;
    let mut preamble_mode = PreambleMode::Full;
    let mut auto_create_task = false;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--spec" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --spec"))?;
                if raw.trim_start().starts_with('{') {
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
                    if spec.model.is_none() {
                        spec.model = parsed.model;
                    }
                    if spec.effort.is_none() {
                        spec.effort = parsed.effort;
                    }
                    if spec.retry_of.is_none() {
                        spec.retry_of = parsed.retry_of;
                    }
                    if spec.retry_request.is_none() {
                        spec.retry_request = parsed.retry_request;
                    }
                    if parsed.no_spawn {
                        spec.no_spawn = true;
                    }
                } else {
                    spec.prompt = Some(raw.clone());
                    auto_create_task = true;
                }
            }
            "--run" => spec.run_id = it.next().cloned(),
            "--task" => spec.task_id = it.next().cloned(),
            "--worker" => spec.worker = it.next().cloned(),
            "--name" => {
                let name = take_opt(&mut it, "--name")?.clone();
                spec.worker = Some(name.clone());
                child_name = Some(name);
            }
            "--base" | "--setup" | "--trust-setup" => {
                let value = take_opt(&mut it, arg)?;
                worktree_options.extend([OsString::from(arg), OsString::from(value)]);
            }
            "--agent" => {
                explicit_agent = Some(take_opt(&mut it, "--agent")?.clone());
            }
            "--account" => {
                spec.account = Some(take_opt(&mut it, "--account")?.clone());
            }
            "--session" | "--terminal" => {
                let v = it.next().ok_or_else(|| bad_arg("missing --session"))?;
                spec.session_id = Some(v.parse::<u64>().map_err(|_| bad_arg("invalid --session"))?);
            }
            "--cwd" => spec.cwd = Some(take_opt(&mut it, "--cwd")?.clone()),
            "--worktree" => {
                let val = it.next().ok_or_else(|| bad_arg("missing --worktree"))?;
                new_child = val == "new-child";
                if val != "current" && !new_child {
                    spec.cwd = Some(val.clone());
                }
            }
            "--model" => spec.model = Some(take_opt(&mut it, "--model")?.clone()),
            "--effort" => spec.effort = Some(take_opt(&mut it, "--effort")?.clone()),
            "--retry-of" => spec.retry_of = Some(take_opt(&mut it, "--retry-of")?.clone()),
            "--retry-request" => {
                spec.retry_request = Some(take_opt(&mut it, "--retry-request")?.clone());
            }
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
            "--preamble" => {
                let raw = take_opt(&mut it, "--preamble")?;
                preamble_mode = PreambleMode::parse(raw)?;
            }
            "--no-preamble" => preamble_mode = PreambleMode::None,
            "--no-spawn" => spec.no_spawn = true,
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    if spec.model.as_deref().is_some_and(|v| v.trim().is_empty()) {
        return Err(bad_arg("--model must not be empty"));
    }
    if new_child {
        if remote {
            return Err(bad_arg(
                "--worktree new-child does not support --host; create the remote worktree separately",
            ));
        }
        if child_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
        {
            return Err(bad_arg("--worktree new-child requires --name <name>"));
        }
        if spec.session_id.is_some() || spec.no_spawn {
            return Err(bad_arg(
                "--worktree new-child requires a new session; omit --session and --no-spawn",
            ));
        }
    } else {
        child_name = None;
        if !worktree_options.is_empty() {
            return Err(bad_arg(
                "--base, --setup and --trust-setup require --worktree new-child",
            ));
        }
    }
    if spec.model.is_some() || spec.effort.is_some() {
        if spec.session_id.is_some() {
            return Err(bad_arg(
                "--model and --effort configure a new agent session; omit --session",
            ));
        }
        let overrides = krill_proto::agent::LaunchOverrides {
            model: spec.model.clone(),
            effort: spec.effort.clone(),
        };
        krill_proto::agent::encode_agent_program_id("claude", &overrides).map_err(bad_arg)?;
    }
    Ok(WorkerStartArgs {
        spec,
        explicit_agent,
        explicit_depth,
        explicit_max_depth,
        preamble_mode,
        auto_create_task,
        json,
        child_name,
        worktree_options,
    })
}

fn cmd_worker_start_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let WorkerStartArgs {
        mut spec,
        explicit_agent,
        explicit_depth,
        explicit_max_depth,
        preamble_mode,
        auto_create_task,
        json,
        child_name,
        worktree_options,
    } = parse_worker_start(args, ctx.is_remote())?;
    let initial_store = ctx.load_store()?;
    if spec.retry_of.is_some() || spec.retry_request.is_some() {
        let previous = initial_store
            .dispatches
            .iter()
            .find(|d| {
                spec.retry_of.as_ref().is_none_or(|id| &d.id == id)
                    && spec
                        .retry_request
                        .as_ref()
                        .is_none_or(|id| d.request_id.as_ref() == Some(id))
            })
            .ok_or_else(|| bad_arg("retry dispatch/request not found or mismatched"))?;
        spec.retry_of = Some(previous.id.clone());
        spec.run_id.get_or_insert_with(|| previous.run_id.clone());
        if spec.task_id.is_none() {
            spec.task_id.clone_from(&previous.task_id);
        }
        spec.worker
            .get_or_insert_with(|| previous.worker_name.clone());
        spec.agent.get_or_insert_with(|| previous.agent.clone());
        spec.cwd.get_or_insert_with(|| previous.cwd.clone());
        spec.prompt.get_or_insert_with(|| previous.prompt.clone());
        if spec.model.is_none() {
            spec.model.clone_from(&previous.model);
        }
        if spec.effort.is_none() {
            spec.effort.clone_from(&previous.effort);
        }
    }
    let prompt = spec.prompt.ok_or_else(|| bad_arg("missing --prompt"))?;
    let raw_agent = explicit_agent
        .or(spec.agent)
        .unwrap_or_else(|| "claude".to_owned());
    let agent = krill_proto::agent::canonical_id(&raw_agent).to_owned();
    if (spec.model.is_some() || spec.effort.is_some()) && !ctx.is_remote() {
        let agents =
            crate::session::load_local_agents(Some(ctx.state_dir)).map_err(io::Error::other)?;
        let (canonical, profile) = agents
            .resolve(&raw_agent)
            .ok_or_else(|| bad_arg(format!("unknown agent profile '{raw_agent}'")))?;
        krill_proto::orchestration::worker_launch_profile(
            profile,
            canonical,
            spec.model.as_deref(),
            spec.effort.as_deref(),
        )?;
    }
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

    let mut work_cwd = resolve_worker_cwd(spec.cwd.as_deref(), ctx.is_remote())?;
    // setup 옵션 검증도 작업·워크트리 생성 전에 기존 worktree 파서를 통과시킨다.
    let child_request = child_name
        .as_deref()
        .map(|name| {
            crate::worktree::parse_new_child(&work_cwd, name, &worktree_options).map_err(bad_arg)
        })
        .transpose()?;

    // 스폰 전에 run/task 유효성과 상태를 먼저 검증해 잘못된 요청이 고아 PTY 세션을 남기지 않게 한다.
    let store = ctx.load_store()?;
    let worker_name = match spec.worker {
        Some(w) => w,
        None if auto_create_task => {
            format!("{agent}-{}", store.dispatches.len().saturating_add(1))
        }
        None => return Err(bad_arg("missing --worker")),
    };
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
            .resolve_run_for_session(None, caller_session_id())
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
    let session_id = spec.session_id;
    if let Some(sid) = session_id {
        ensure_worker_session_exists_on_target(&target, &worker_name, sid)?;
    }
    let created_child = if let Some(request) = child_request {
        let created =
            crate::worktree::create_new_on_target(&target, request).map_err(io::Error::other)?;
        work_cwd = PathBuf::from(OsStr::from_bytes(&created.path));
        Some(work_cwd.clone())
    } else {
        None
    };
    // 생성된 워크트리는 어느 후속 단계가 실패해도 보존하고 경로를 오류에 붙인다.
    with_created_worktree(created_child.as_deref(), || {
        let cwd_str = work_cwd.to_string_lossy().into_owned();
        let mut auto_created_task = None;
        if spec.task_id.is_none() && auto_create_task {
            let first_line = prompt.lines().next().unwrap_or_default().trim();
            let mut derived_title: String = first_line.chars().take(80).collect();
            if derived_title.is_empty() {
                derived_title.clone_from(&worker_name);
            }
            let created_task = match ctx.try_ipc(
                &OrchAction::CreateTask {
                    run_id: run_id.clone(),
                    title: derived_title.clone(),
                    description: prompt.clone(),
                    deps: Vec::new(),
                },
                Duration::from_secs(2),
            )? {
                Some(OrchResponse::Task(t)) => t,
                _ => OrchestrationStore::mutate(ctx.state_dir, |s| {
                    s.create_task(&run_id, &derived_title, &prompt, Vec::new())
                })?,
            };
            auto_created_task = Some(created_task.id.clone());
            spec.task_id = Some(created_task.id);
        }

        let newly_spawned = session_id.is_none() && !spec.no_spawn;
        let configured = WorkerStartSpec {
            run_id: Some(run_id.clone()),
            task_id: spec.task_id.clone(),
            worker: Some(worker_name.clone()),
            agent: Some(agent.clone()),
            session_id,
            cwd: Some(cwd_str),
            prompt: Some(prompt.clone()),
            depth: Some(depth),
            max_depth: Some(max_depth),
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            retry_of: spec.retry_of.clone(),
            retry_request: spec.retry_request.clone(),
            ..WorkerStartSpec::default()
        };
        let mut dispatch = fail_task_on_error(ctx, auto_created_task.as_deref(), || {
            match ctx.try_ipc(
                &OrchAction::StartWorkerConfigured {
                    spec: configured.clone(),
                },
                Duration::from_secs(2),
            )? {
                Some(OrchResponse::Dispatch(d) | OrchResponse::DispatchV2(d)) => Ok(*d),
                Some(OrchResponse::Error(e)) => Err(bad_arg(e)),
                None => OrchestrationStore::mutate(ctx.state_dir, |s| {
                    s.start_worker_configured(&configured)
                }),
                Some(_) => Err(io::Error::other("unexpected worker start reply")),
            }
        })?;
        if newly_spawned {
            let launched = ctx.try_ipc(
                &OrchAction::LaunchWorker {
                    dispatch_id: dispatch.id.clone(),
                    account: spec.account.clone(),
                },
                Duration::from_secs(30),
            );
            match launched {
                Ok(Some(OrchResponse::Dispatch(d) | OrchResponse::DispatchV2(d))) => {
                    dispatch = *d;
                }
                other => {
                    let error = match other {
                        Err(e) => e,
                        Ok(Some(OrchResponse::Error(e))) => io::Error::other(e),
                        _ => io::Error::other("worker launch requires an updated krilld"),
                    };
                    let error = io::Error::new(
                        error.kind(),
                        format!("spawn {agent} worker session: {error}"),
                    );
                    mark_worker_start_failed(ctx, &dispatch.id, &error.to_string())?;
                    return Err(error);
                }
            }
        }
        let delivered: Result<(), String> = dispatch.session_id.map_or(Ok(()), |sid| {
            let Some(preamble) = format_worker_preamble(&dispatch, preamble_mode) else {
                return Ok(());
            };
            if newly_spawned {
                crate::session::wait_for_spawned_agent_ready(
                    &target,
                    sid,
                    crate::session::BOOT_TIMEOUT,
                )?;
            }
            crate::session::send_session_line(&target, sid, &preamble)?;
            Ok(())
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
        if let Err(error) = delivered {
            mark_worker_start_failed(ctx, &dispatch.id, &error)?;
            return Err(io::Error::other(format!(
                "worker {}: {error}",
                dispatch.worker_name
            )));
        }
        Ok(())
    })
}

fn caller_session_id() -> Option<u32> {
    std::env::var("MK_PANE_ID").ok()?.parse().ok()
}

fn mark_worker_start_failed(ctx: &OrchCtx<'_>, dispatch_id: &str, reason: &str) -> io::Result<()> {
    let dispatch = match ctx.try_ipc(
        &OrchAction::WorkerStartFailed {
            dispatch_id: dispatch_id.to_owned(),
            reason: reason.to_owned(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Dispatch(d) | OrchResponse::DispatchV2(d)) => *d,
        Some(OrchResponse::Error(e)) => return Err(io::Error::other(e)),
        None => OrchestrationStore::mutate(ctx.state_dir, |s| {
            s.worker_start_failed(dispatch_id, reason)
        })?,
        Some(_) => return Err(io::Error::other("unexpected worker start failure reply")),
    };
    if let Some(summary) = dispatch.summary.filter(|s| s.contains("circuit breaker")) {
        return Err(io::Error::other(summary));
    }
    Ok(())
}

fn cmd_request_show_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let (id, json) = parse_worker_or_dispatch_args(args)?;
    let store = ctx.load_store()?;
    let dispatch = store
        .dispatches
        .iter()
        .find(|d| d.request_id.as_deref() == Some(id.as_str()))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("request not found: {id}"))
        })?;
    if json {
        print_json(dispatch)?;
    } else {
        println!(
            "{}\t{}\t{}\t{}",
            id,
            dispatch.id,
            dispatch.status.as_str(),
            dispatch.prompt
        );
    }
    Ok(())
}

/// 디스패치 전에 실패하면 CLI가 자동 생성한 작업만 실패로 정산한다. 기존 작업과 이미 배정된 작업은 바꾸지 않는다.
fn fail_task_on_error<T>(
    ctx: &OrchCtx<'_>,
    auto_created_task: Option<&str>,
    action: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    action().map_err(|error| {
        let Some(task_id) = auto_created_task else {
            return error;
        };
        match fail_unstarted_task(ctx, task_id, &error.to_string()) {
            Ok(()) => error,
            Err(cleanup) => io::Error::new(
                error.kind(),
                format!("{error}; could not mark auto-created task {task_id} failed: {cleanup}"),
            ),
        }
    })
}

fn fail_unstarted_task(ctx: &OrchCtx<'_>, task_id: &str, reason: &str) -> io::Result<()> {
    let summary = Some(format!("worker session start failed: {reason}"));
    match ctx.try_ipc(
        &OrchAction::UpdateTask {
            task_id: task_id.to_owned(),
            status: TaskStatus::Failed,
            summary: summary.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Task(_)) => Ok(()),
        None => OrchestrationStore::mutate(ctx.state_dir, |store| {
            store.update_task(task_id, TaskStatus::Failed, summary)
        })
        .map(|_| ()),
        Some(_) => Err(io::Error::other("unexpected task update reply")),
    }
}

fn with_created_worktree<T>(
    created: Option<&Path>,
    action: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    action().map_err(|error| match created {
        Some(path) => io::Error::new(
            error.kind(),
            format!("{error}; created worktree preserved at {}", path.display()),
        ),
        None => error,
    })
}

/// 로컬 cwd는 데몬에 넘기기 전에 절대 경로·디렉터리로 검증한다. 원격 경로는 로컬에서 읽지 않는다.
fn resolve_worker_cwd(cwd: Option<&str>, remote: bool) -> io::Result<PathBuf> {
    if remote {
        let cwd = cwd.ok_or_else(|| {
            bad_arg("remote worker-start requires --cwd <absolute path on the remote host>")
        })?;
        if !cwd.starts_with('/') {
            return Err(bad_arg(
                "remote --cwd must be an absolute path on the remote host",
            ));
        }
        return Ok(PathBuf::from(cwd));
    }
    let path = cwd.map_or_else(std::env::current_dir, |cwd| Ok(PathBuf::from(cwd)))?;
    let canonical = std::fs::canonicalize(&path).map_err(|e| {
        bad_arg(format!(
            "cannot resolve worker --cwd {}: {e}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(bad_arg(format!(
            "worker --cwd is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
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
    let mut files_modified = None;
    let mut report_path = None;
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
            "--summary" | "--subject" => summary = it.next().cloned(),
            "--body" => body = it.next().cloned().unwrap_or_default(),
            "--files-modified" => files_modified = it.next().cloned(),
            "--report-path" => report_path = it.next().cloned(),
            "--phase" | "--thread-id" | "--priority" => {
                it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
            }
            "--run" => run_id = it.next().cloned(),
            "--task" | "--task-id" => task_id = it.next().cloned(),
            "--dispatch" | "--dispatch-id" => dispatch_id = it.next().cloned(),
            "--outcome" | "--status" => outcome = it.next().cloned(),
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    if summary.is_none() && !body.trim().is_empty() {
        let first_line = body.lines().next().unwrap_or_default().trim();
        if !first_line.is_empty() {
            summary = Some(first_line.to_owned());
        }
    }
    if let Some(files) = files_modified.filter(|s| !s.trim().is_empty()) {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("files_modified: ");
        body.push_str(files.trim());
    }
    if let Some(rp) = report_path.filter(|s| !s.trim().is_empty()) {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("report_path: ");
        body.push_str(rp.trim());
    }
    let from = from.ok_or_else(|| bad_arg("missing --from"))?;
    let to = to.unwrap_or_else(|| "coordinator".to_owned());
    let summary = summary.ok_or_else(|| bad_arg("missing --summary"))?;
    if run_id.is_none()
        && let Some(sid) = caller_session_id()
    {
        run_id = ctx.load_store()?.session_runs.get(&sid).cloned();
    }
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
        Some(OrchResponse::Error(error)) => return Err(bad_arg(error)),
        Some(_) => return Err(io::Error::other("unexpected orchestration mutation reply")),
        None => OrchestrationStore::mutate(ctx.state_dir, |s| s.send_message(&params))?,
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
    use krill_proto::orchestration::{DEFAULT_CHECK_WAIT_MS, MAX_CHECK_WAIT_MS};
    let mut to = None;
    let mut types = Vec::new();
    let mut unread_only = false;
    let mut ack = false;
    let mut run_id = None;
    let mut delivery_id = None;
    let mut ack_ids = Vec::new();
    let mut ack_ids_flag = false;
    let mut wait = false;
    let mut timeout_ms: Option<u64> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--to" | "--for" | "--terminal" => {
                let val = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                to = Some(val.clone());
            }
            "--types" | "--type" => {
                if let Some(raw) = it.next() {
                    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        types.push(MessageType::parse(part)?);
                    }
                }
            }
            "--unread" => unread_only = true,
            "--all" => unread_only = false,
            "--run" => run_id = Some(take_opt(&mut it, "--run")?.clone()),
            "--ack" => {
                ack = true;
                if let Some(id) = it.as_slice().first()
                    && !id.starts_with('-')
                {
                    delivery_id = it.next().cloned();
                }
            }
            "--peek" => ack = false,
            "--ack-ids" => {
                ack_ids_flag = true;
                let raw = it.next().ok_or_else(|| bad_arg("missing --ack-ids"))?;
                for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    ack_ids.push(part.to_owned());
                }
            }
            "--wait" => {
                wait = true;
                if let Some(next_arg) = it.as_slice().first()
                    && let Ok(secs) = next_arg.parse::<u64>()
                    && secs > 0
                {
                    let _ = it.next();
                    timeout_ms = Some(secs.saturating_mul(1000));
                }
            }
            "--timeout-ms" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --timeout-ms"))?;
                timeout_ms = Some(
                    raw.parse::<u64>()
                        .map_err(|_| bad_arg("invalid --timeout-ms"))?,
                );
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    if ack_ids_flag {
        if ack_ids.is_empty() {
            return Err(bad_arg("--ack-ids must contain at least one message id"));
        }
        if wait || ack {
            return Err(bad_arg("--ack-ids cannot be combined with --wait or --ack"));
        }
    }
    let wait_ms = if wait {
        timeout_ms
            .unwrap_or(DEFAULT_CHECK_WAIT_MS)
            .min(MAX_CHECK_WAIT_MS)
    } else {
        0
    };
    let deadline = Duration::from_millis(wait_ms);
    let (delivery, messages) = if ack_ids_flag {
        let messages = match ctx.try_ipc(
            &OrchAction::AckMessageIds {
                recipient: to.clone(),
                run_id: run_id.clone(),
                types: types.clone(),
                unread_only,
                ack_ids: ack_ids.clone(),
            },
            Duration::from_secs(2),
        )? {
            Some(OrchResponse::Messages(messages)) => messages,
            Some(OrchResponse::Error(error)) => return Err(bad_arg(error)),
            Some(_) => return Err(io::Error::other("unexpected acknowledgment reply")),
            None => OrchestrationStore::mutate(ctx.state_dir, |s| {
                s.ack_message_ids(
                    to.as_deref(),
                    run_id.as_deref(),
                    &types,
                    unread_only,
                    &ack_ids,
                )
            })?,
        };
        (None, messages)
    } else {
        match ctx.try_ipc(
            &OrchAction::CheckMessages {
                recipient: to.clone(),
                types: types.clone(),
                unread_only,
                ack,
                wait_ms,
                run_id: run_id.clone(),
                delivery_id: delivery_id.clone(),
            },
            deadline,
        )? {
            Some(OrchResponse::Delivery { delivery, messages }) => (delivery, messages),
            Some(OrchResponse::Messages(messages)) => (None, messages),
            Some(OrchResponse::Error(e)) => return Err(bad_arg(e)),
            Some(_) => return Err(io::Error::other("unexpected mailbox reply")),
            None => {
                let start = Instant::now();
                loop {
                    let checked = OrchestrationStore::mutate(ctx.state_dir, |s| {
                        s.check_delivery(
                            to.as_deref(),
                            &types,
                            unread_only,
                            ack,
                            run_id.as_deref(),
                            delivery_id.as_deref(),
                        )
                    })?;
                    if !checked.1.is_empty() || !wait || start.elapsed() >= deadline {
                        break checked;
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
            }
        }
    };
    if json {
        if ack_ids_flag || (ack && delivery_id.is_none()) {
            print_json(&messages)?;
        } else {
            print_json(&serde_json::json!({ "delivery": delivery, "messages": messages }))?;
        }
    } else {
        if let Some(delivery) = delivery {
            println!(
                "Delivery: {} (ack with check --ack {})",
                delivery.id, delivery.id
            );
        }
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

fn cmd_inbox_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut rcpt = "coordinator".to_owned();
    let mut run_filter: Option<String> = None;
    let mut all = false;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--to" | "--for" | "--terminal" => {
                let val = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                rcpt.clone_from(val);
            }
            "--run" => {
                let val = it.next().ok_or_else(|| bad_arg("missing --run"))?;
                run_filter = Some(val.clone());
            }
            "--all" => all = true,
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let store = ctx.load_store()?;
    let messages: Vec<_> = store
        .messages
        .into_iter()
        .filter(|msg| {
            (msg.to == rcpt || msg.to == "@all")
                && (all || !msg.acked)
                && run_filter
                    .as_ref()
                    .is_none_or(|rid| msg.run_id.as_ref() == Some(rid))
        })
        .collect();
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
    let mut options = None;
    let mut run_id = None;
    let mut task_id = None;
    let mut question_id = None;
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
            "--options" => options = it.next().cloned(),
            "--run" => run_id = it.next().cloned(),
            "--task" => task_id = it.next().cloned(),
            "--question-id" | "--id" | "--resume" => {
                question_id = Some(take_opt(&mut it, "--question-id")?.clone());
            }
            "--wait" => wait = true,
            "--no-wait" => wait = false,
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
    if let Some(opts) = options.filter(|s| !s.trim().is_empty()) {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("Options: ");
        body.push_str(opts.trim());
    }
    let question = if let Some(qid) = question_id {
        // 타임아웃 뒤 같은 질문을 다시 기다린다. 새 Question을 만들지 않는다.
        ctx.load_store()?
            .messages
            .into_iter()
            .find(|m| m.id == qid && m.msg_type == MessageType::Question)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("question {qid} not found"))
            })?
    } else {
        let from = from.ok_or_else(|| bad_arg("missing --from"))?;
        let to = to.unwrap_or_else(|| "coordinator".to_owned());
        let summary = summary.ok_or_else(|| bad_arg("missing --summary"))?;
        let params = SendParams {
            from,
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
        sent.into_iter()
            .next()
            .ok_or_else(|| bad_arg("failed to create question"))?
    };

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
    let mut msg_id = None;
    let mut from = "coordinator".to_owned();
    let mut body = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--id" | "--message" => {
                let id = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                msg_id = Some(id.clone());
            }
            "--from" => {
                from.clone_from(it.next().ok_or_else(|| bad_arg("missing --from"))?);
            }
            "--body" | "--answer" => body = it.next().cloned(),
            "--json" => json = true,
            s if !s.starts_with('-') && msg_id.is_none() => {
                msg_id = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let msg_id = msg_id.ok_or_else(|| bad_arg("missing <msg-id>"))?;
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
        _ => OrchestrationStore::mutate(ctx.state_dir, |s| s.reply_message(&msg_id, &from, &body))?,
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
    let mut gate_id = None;
    let mut decision = None;
    let mut rationale = None;
    let mut by = "coordinator".to_owned();
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--id" | "--gate" => {
                let id = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                gate_id = Some(id.clone());
            }
            "--decision" | "--resolution" => decision = it.next().cloned(),
            "--rationale" => rationale = it.next().cloned(),
            "--by" => {
                by.clone_from(it.next().ok_or_else(|| bad_arg("missing --by"))?);
            }
            "--json" => json = true,
            s if !s.starts_with('-') && gate_id.is_none() => {
                gate_id = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let gate_id = gate_id.ok_or_else(|| bad_arg("missing <gate-id>"))?;
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
            s.resolve_gate(&gate_id, &decision, rationale, &by)
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct WorkerNextAction {
    action: &'static str,
    reason: &'static str,
    command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct WorkerReport {
    #[serde(flatten)]
    dispatch: Dispatch,
    session_state: &'static str,
    session_live: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_state: Option<&'static str>,
    terminal_state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_action: Option<WorkerNextAction>,
}

fn dispatch_owns_session(all_dispatches: &[Dispatch], disp: &Dispatch) -> bool {
    let Some(sid) = disp.session_id else {
        return false;
    };
    let owner = all_dispatches
        .iter()
        .rev()
        .find(|d| {
            d.session_id == Some(sid)
                && matches!(d.status, DispatchStatus::Active | DispatchStatus::Retained)
        })
        .or_else(|| {
            all_dispatches
                .iter()
                .rev()
                .find(|d| d.session_id == Some(sid))
        });
    owner.is_some_and(|o| o.id == disp.id)
}

fn build_worker_report(
    disp: &Dispatch,
    all_dispatches: &[Dispatch],
    observed: Option<&[crate::session::ObservedSession]>,
) -> WorkerReport {
    use krill_proto::control::AgentState;
    let owns_session = dispatch_owns_session(all_dispatches, disp);
    let (session_state, session_live, agent_state) = match (disp.session_id, observed) {
        (None, _) => ("unbound", None, None),
        (Some(_), None) => ("unknown", None, None),
        (Some(sid), Some(list)) => match list.iter().find(|s| s.id == sid) {
            None => ("missing", Some(false), None),
            Some(obs) => {
                let a_state = obs.agent_state.map(crate::session::state_name);
                if obs.exit_code.is_some() || obs.agent_state == Some(AgentState::Exited) {
                    ("exited", Some(false), a_state)
                } else {
                    ("live", Some(true), a_state)
                }
            }
        },
    };
    let terminal_state = if disp.is_released() {
        if disp.session_id.is_some() && !owns_session {
            "reassigned"
        } else {
            "released"
        }
    } else if disp.release_state == krill_proto::orchestration::DispatchReleaseState::Retained {
        if !owns_session {
            "reassigned"
        } else if session_live == Some(false) {
            "released"
        } else {
            "retained"
        }
    } else {
        match disp.status {
            DispatchStatus::Active => "active",
            DispatchStatus::Retained => {
                if !owns_session {
                    "reassigned"
                } else if session_live == Some(false) {
                    "released"
                } else {
                    "retained"
                }
            }
            DispatchStatus::Released | DispatchStatus::Abandoned => {
                if disp.session_id.is_some() && !owns_session {
                    "reassigned"
                } else {
                    "released"
                }
            }
            DispatchStatus::Succeeded | DispatchStatus::Failed | DispatchStatus::Stopped => {
                if disp.session_id.is_none() || session_live == Some(false) {
                    "released"
                } else if !owns_session {
                    "reassigned"
                } else {
                    "reclaimable"
                }
            }
        }
    };
    let next_action = if terminal_state == "active" && owns_session && session_live == Some(false) {
        Some(WorkerNextAction {
            action: "abandon_or_release",
            reason: "dispatch is still marked active, but its worker session is no longer running",
            command: format!("krill orchestration worker-abandon {}", disp.id),
        })
    } else if terminal_state == "reclaimable"
        && disp.session_origin == DispatchSessionOrigin::Borrowed
    {
        Some(WorkerNextAction {
            action: "abandon",
            reason: "the dispatch borrowed an existing session; fence it without closing the session",
            command: format!("krill orchestration worker-abandon {}", disp.id),
        })
    } else if terminal_state == "reclaimable" {
        Some(WorkerNextAction {
            action: "release_or_retain",
            reason: "dispatch has settled while its terminal session remains open",
            command: format!("krill orchestration worker-release {}", disp.id),
        })
    } else {
        None
    };
    WorkerReport {
        dispatch: disp.clone(),
        session_state,
        session_live,
        agent_state,
        terminal_state,
        next_action,
    }
}

fn observe_daemon_sessions(ctx: &OrchCtx<'_>) -> Option<Vec<crate::session::ObservedSession>> {
    if !ctx.can_query_daemon() {
        return None;
    }
    ctx.effective_target()
        .ok()
        .and_then(|t| crate::session::observe_sessions_on_target(&t))
}

#[cfg(test)]
fn cmd_worker_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
    cmd_worker_list_ctx(&OrchCtx::local(state_dir), args)
}

fn cmd_worker_list_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut run_filter = None;
    let mut terminal_filters: Vec<String> = Vec::new();
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--run" => {
                let val = it.next().ok_or_else(|| bad_arg("missing --run"))?;
                run_filter = Some(val.clone());
            }
            "--terminal-state" => {
                let raw = it
                    .next()
                    .ok_or_else(|| bad_arg("missing --terminal-state"))?;
                for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    if !matches!(
                        part,
                        "active" | "reclaimable" | "retained" | "reassigned" | "released"
                    ) {
                        return Err(bad_arg(format!(
                            "invalid --terminal-state '{part}' (expected active|reclaimable|retained|reassigned|released)"
                        )));
                    }
                    terminal_filters.push(part.to_owned());
                }
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let store = ctx.load_store()?;
    let observed = observe_daemon_sessions(ctx);
    let reports: Vec<WorkerReport> = store
        .dispatches
        .iter()
        .map(|d| build_worker_report(d, &store.dispatches, observed.as_deref()))
        .filter(|r| {
            run_filter
                .as_ref()
                .is_none_or(|rid| &r.dispatch.run_id == rid)
        })
        .filter(|r| {
            terminal_filters.is_empty() || terminal_filters.iter().any(|f| f == r.terminal_state)
        })
        .collect();
    if json {
        print_json(&reports)?;
    } else {
        println!("DISPATCH\tWORKER\tAGENT\tSTATUS\tTERMINAL\tSESSION\tTASK\tNEXT_ACTION");
        for r in reports {
            let w = &r.dispatch;
            let status_str = if w.status == DispatchStatus::Active && r.session_live == Some(false)
            {
                "active(exited)"
            } else {
                w.status.as_str()
            };
            let session_col = match w.session_id {
                Some(sid) => format!("{sid}({})", r.session_state),
                None => "-".to_owned(),
            };
            let next_col = r.next_action.as_ref().map_or("-", |na| na.action);
            println!(
                "{}\t{}\t{}\t{status_str}\t{}\t{session_col}\t{}\t{next_col}",
                w.id,
                w.worker_name,
                w.agent,
                r.terminal_state,
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
    let mut target = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dispatch" | "--worker" => {
                let val = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                target = Some(val.clone());
            }
            "--json" => json = true,
            s if !s.starts_with('-') && target.is_none() => {
                target = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let target = target.ok_or_else(|| bad_arg("missing <worker-or-dispatch>"))?;
    let store = ctx.load_store()?;
    let Some(disp) = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == target || d.worker_name == target)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("worker not found: {target}"),
        ));
    };
    let observed = observe_daemon_sessions(ctx);
    let report = build_worker_report(disp, &store.dispatches, observed.as_deref());
    if json {
        print_json(&report)?;
    } else {
        println!(
            "dispatch:       {}\nworker:         {}\nagent:          {}\nstatus:         {}\nterminal_state: {}\nrun:            {}\ntask:           {}\nsession:        {} ({})\ncwd:            {}\nprompt:         {}",
            disp.id,
            disp.worker_name,
            disp.agent,
            disp.status.as_str(),
            report.terminal_state,
            disp.run_id,
            disp.task_id.as_deref().unwrap_or("-"),
            disp.session_id
                .map_or_else(|| "-".to_owned(), |n| n.to_string()),
            report.session_state,
            disp.cwd,
            disp.prompt
        );
        if let Some(na) = &report.next_action {
            println!("next_action:    {} ({})", na.action, na.command);
        }
    }
    Ok(())
}

fn cmd_dispatch_show_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let mut target = None;
    let mut task_filter = None;
    let mut preamble = false;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dispatch" | "--worker" => {
                let val = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                target = Some(val.clone());
            }
            "--task" => {
                let val = it.next().ok_or_else(|| bad_arg("missing --task"))?;
                task_filter = Some(val.clone());
            }
            "--preamble" => preamble = true,
            "--json" => json = true,
            s if !s.starts_with('-') && target.is_none() => {
                target = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    if target.is_none() && task_filter.is_none() {
        return Err(bad_arg("missing <dispatch-or-worker> or --task <task-id>"));
    }
    let store = ctx.load_store()?;
    let disp = store
        .dispatches
        .iter()
        .rev()
        .find(|d| {
            let matches_target = target
                .as_ref()
                .is_none_or(|t| d.id == *t || d.worker_name == *t);
            let matches_task = task_filter
                .as_ref()
                .is_none_or(|tid| d.task_id.as_ref() == Some(tid));
            matches_target && matches_task
        })
        .ok_or_else(|| {
            let label = target.as_deref().or(task_filter.as_deref()).unwrap_or("?");
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("dispatch not found: {label}"),
            )
        })?;
    if json {
        if preamble {
            print_json(&serde_json::json!({
                "dispatch": disp,
                "preamble": worker_preamble(disp),
            }))?;
        } else {
            print_json(disp)?;
        }
    } else if preamble {
        println!("{}", worker_preamble(disp));
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
    let mut target_name = None;
    let mut lines = 40_usize;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dispatch" | "--worker" => {
                let val = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                target_name = Some(val.clone());
            }
            "--lines" | "--limit" => {
                let raw = take_opt(&mut it, "--lines")?;
                lines = raw
                    .parse::<usize>()
                    .map_err(|_| bad_arg("invalid --lines"))?;
            }
            "--json" => json = true,
            s if !s.starts_with('-') && target_name.is_none() => {
                target_name = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let target_name = target_name.ok_or_else(|| bad_arg("missing <worker-or-dispatch>"))?;
    let store = ctx.load_store()?;
    let Some(disp) = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == target_name || d.worker_name == target_name)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("worker not found: {target_name}"),
        ));
    };
    let target = ctx.effective_target()?;
    let mut screen_text = match (disp.is_released(), disp.session_id) {
        (true, _) | (_, None) => disp.archived_output.clone().unwrap_or_default(),
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

fn parse_worker_or_dispatch_args(args: &[String]) -> io::Result<(String, bool)> {
    let mut target_name = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dispatch" | "--worker" => {
                let val = it.next().ok_or_else(|| bad_arg(format!("missing {arg}")))?;
                target_name = Some(val.clone());
            }
            "--json" => json = true,
            s if !s.starts_with('-') && target_name.is_none() => {
                target_name = Some(s.to_owned());
            }
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let target_name = target_name.ok_or_else(|| bad_arg("missing <worker-or-dispatch>"))?;
    Ok((target_name, json))
}

fn cmd_worker_stop_ctx(ctx: &OrchCtx<'_>, args: &[String], release: bool) -> io::Result<()> {
    let (target_name, json) = parse_worker_or_dispatch_args(args)?;
    let store = ctx.load_store()?;
    let selected = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == target_name || d.worker_name == target_name)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("worker not found: {target_name}"),
            )
        })?;
    if release {
        selected.validate_release()?;
    }
    if selected.session_origin == DispatchSessionOrigin::Borrowed {
        return Err(bad_arg(
            "cannot close a borrowed session; use worker-abandon for non-destructive fencing",
        ));
    }
    let owns_session = dispatch_owns_session(&store.dispatches, selected);
    let target = ctx.effective_target()?;
    let captured = match selected.session_id {
        Some(sid) if owns_session && !selected.is_released() => {
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
        if owns_session
            && !selected.is_released()
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

fn cmd_worker_retain_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let (target_name, json) = parse_worker_or_dispatch_args(args)?;
    let disp = match ctx.try_ipc(
        &OrchAction::ManageWorker {
            worker_or_dispatch: target_name.clone(),
            operation: WorkerOperation::Retain,
            archived_output: None,
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Dispatch(d) | OrchResponse::DispatchV2(d)) => *d,
        Some(OrchResponse::Error(error)) => return Err(bad_arg(error)),
        Some(_) => return Err(io::Error::other("unexpected orchestration mutation reply")),
        None => OrchestrationStore::mutate(ctx.state_dir, |s| s.retain_worker(&target_name))?,
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

fn cmd_worker_abandon_ctx(ctx: &OrchCtx<'_>, args: &[String]) -> io::Result<()> {
    let (target_name, json) = parse_worker_or_dispatch_args(args)?;
    let store = ctx.load_store()?;
    let selected = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == target_name || d.worker_name == target_name)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("worker not found: {target_name}"),
            )
        })?;
    let disp = match ctx.try_ipc(
        &OrchAction::ManageWorker {
            worker_or_dispatch: selected.id.clone(),
            operation: WorkerOperation::Abandon,
            archived_output: None,
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Dispatch(d) | OrchResponse::DispatchV2(d)) => *d,
        Some(OrchResponse::Error(error)) => return Err(bad_arg(error)),
        Some(_) => return Err(io::Error::other("unexpected orchestration mutation reply")),
        None => OrchestrationStore::mutate(ctx.state_dir, |s| s.abandon_worker(&selected.id))?,
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
    if release {
        let store = ctx.load_store()?;
        let selected = store
            .dispatches
            .iter()
            .find(|d| d.id == dispatch_id)
            .ok_or_else(|| bad_arg("dispatch not found"))?;
        selected.validate_release()?;
    }
    close()?;
    match ctx.try_ipc(
        &OrchAction::ManageWorker {
            worker_or_dispatch: dispatch_id.to_owned(),
            operation: if release {
                WorkerOperation::Release
            } else {
                WorkerOperation::Stop
            },
            archived_output: archived_output.clone(),
        },
        Duration::from_secs(2),
    )? {
        Some(OrchResponse::Dispatch(d) | OrchResponse::DispatchV2(d)) => Ok(*d),
        Some(OrchResponse::Error(error)) => Err(bad_arg(error)),
        Some(_) => Err(io::Error::other("unexpected orchestration mutation reply")),
        None => OrchestrationStore::mutate(ctx.state_dir, |s| {
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
    fn help_returns_zero_at_top_level() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("-h")]), ExitCode::SUCCESS);
        assert_eq!(
            run(&[OsString::from("run-list"), OsString::from("--help")]),
            ExitCode::SUCCESS
        );
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
                .release_state,
            krill_proto::orchestration::DispatchReleaseState::Released
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
    fn launch_preferences_parse_and_reject_invalid_effort() -> io::Result<()> {
        let parsed = parse_worker_start(
            &args(&[
                "--model",
                "chosen",
                "--effort",
                "max",
                "--retry-of",
                "disp-1",
                "--retry-request",
                "req-1",
            ]),
            false,
        )?;
        assert_eq!(parsed.spec.model.as_deref(), Some("chosen"));
        assert_eq!(parsed.spec.effort.as_deref(), Some("max"));
        assert_eq!(parsed.spec.retry_of.as_deref(), Some("disp-1"));
        assert_eq!(parsed.spec.retry_request.as_deref(), Some("req-1"));
        assert!(parse_worker_start(&args(&["--effort", "ultra"]), false).is_err());
        assert!(parse_worker_start(&args(&["--model"]), false).is_err());
        assert!(parse_worker_start(&args(&["--model", ""]), false).is_err());
        Ok(())
    }

    #[test]
    fn spawn_failure_fails_only_auto_created_tasks_and_reports_cleanup_failure() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("krill-spawn-cleanup-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let (automatic, explicit, successful) = OrchestrationStore::mutate(&dir, |store| {
            let run = store.create_run("cleanup", None)?;
            Ok((
                store.create_task(&run.id, "automatic", "", Vec::new())?.id,
                store.create_task(&run.id, "explicit", "", Vec::new())?.id,
                store.create_task(&run.id, "successful", "", Vec::new())?.id,
            ))
        })?;
        let ctx = OrchCtx::local(&dir);
        let error = fail_task_on_error(&ctx, Some(&automatic), || {
            Err::<u64, _>(io::Error::other("injected spawn failure"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "injected spawn failure");
        assert!(
            fail_task_on_error(&ctx, None, || Err::<u64, _>(io::Error::other(
                "explicit task spawn failure"
            )))
            .is_err()
        );
        assert_eq!(fail_task_on_error(&ctx, Some(&successful), || Ok(42))?, 42);
        let store = OrchestrationStore::load(&dir)?;
        let task = store
            .tasks
            .iter()
            .find(|task| task.id == automatic)
            .unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.active_dispatch_id, None);
        assert!(
            task.result_summary
                .as_deref()
                .unwrap()
                .contains("injected spawn failure")
        );
        for id in [explicit, successful] {
            assert_eq!(
                store
                    .tasks
                    .iter()
                    .find(|task| task.id == id)
                    .unwrap()
                    .status,
                TaskStatus::Ready
            );
        }
        assert!(store.dispatches.is_empty());
        let error = fail_task_on_error(&ctx, Some("missing-task"), || {
            Err::<u64, _>(io::Error::other("original spawn failure"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("original spawn failure"));
        assert!(
            error
                .to_string()
                .contains("could not mark auto-created task missing-task failed")
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn worker_start_parses_child_name_setup_and_existing_placements() {
        let hash = "a".repeat(64);
        let parsed = parse_worker_start(
            &args(&[
                "--spec",
                "work",
                "--worktree",
                "new-child",
                "--name",
                "child",
                "--cwd",
                "/parent/nested",
                "--base",
                "feature",
                "--setup",
                "run",
                "--trust-setup",
                &hash,
            ]),
            false,
        )
        .unwrap();
        assert_eq!(parsed.child_name.as_deref(), Some("child"));
        assert_eq!(parsed.spec.worker.as_deref(), Some("child"));
        assert_eq!(parsed.spec.cwd.as_deref(), Some("/parent/nested"));
        assert_eq!(
            parsed.worktree_options,
            [
                "--base",
                "feature",
                "--setup",
                "run",
                "--trust-setup",
                &hash
            ]
            .map(OsString::from)
        );
        assert!(parsed.auto_create_task);
        let named = parse_worker_start(
            &args(&[
                "--name",
                "existing-worker",
                "--worktree",
                "current",
                "--cwd",
                "/explicit",
            ]),
            false,
        )
        .unwrap();
        assert!(named.child_name.is_none());
        assert_eq!(named.spec.worker.as_deref(), Some("existing-worker"));
        assert_eq!(named.spec.cwd.as_deref(), Some("/explicit"));
        let path = parse_worker_start(
            &args(&["--worktree", "/existing", "--worker", "worker"]),
            false,
        )
        .unwrap();
        assert!(path.child_name.is_none());
        assert_eq!(path.spec.cwd.as_deref(), Some("/existing"));
        let current = parse_worker_start(&args(&["--worktree", "current"]), false).unwrap();
        assert!(current.spec.cwd.is_none());
    }

    #[test]
    fn child_placement_rejects_missing_name_remote_host_and_existing_session() {
        assert_eq!(
            run(&[
                "--host",
                "invalid-remote",
                "worker-start",
                "--worktree",
                "new-child",
                "--name",
                "child"
            ]
            .map(OsString::from)),
            ExitCode::from(2)
        );
        for raw in [
            vec!["--worktree", "new-child"],
            vec!["--worktree", "new-child", "--worker", "worker"],
            vec!["--worktree", "new-child", "--name", " "],
        ] {
            let error = parse_worker_start(&args(&raw), false).unwrap_err();
            assert!(error.to_string().contains("requires --name"), "{error}");
        }
        let error =
            parse_worker_start(&args(&["--worktree", "new-child", "--name", "child"]), true)
                .unwrap_err();
        assert!(
            error.to_string().contains("does not support --host"),
            "{error}"
        );
        assert!(
            parse_worker_start(
                &args(&[
                    "--worktree",
                    "new-child",
                    "--name",
                    "child",
                    "--session",
                    "1"
                ]),
                false
            )
            .is_err()
        );
        assert!(
            parse_worker_start(
                &args(&["--worktree", "new-child", "--name", "child", "--no-spawn"]),
                false
            )
            .is_err()
        );
        assert!(parse_worker_start(&args(&["--setup", "skip"]), false).is_err());
    }

    #[test]
    fn failed_start_preserves_created_child_and_reports_its_path() -> io::Result<()> {
        let dir =
            std::env::temp_dir().join(format!("krill-preserved-child-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("work"), "preserve")?;
        let error = with_created_worktree(Some(&dir), || {
            Err::<(), _>(io::Error::other("spawn failed"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("spawn failed"));
        assert!(error.to_string().contains(&*dir.to_string_lossy()));
        assert_eq!(std::fs::read_to_string(dir.join("work"))?, "preserve");
        assert_eq!(
            with_created_worktree(None, || Err::<(), _>(io::Error::other("original")))
                .unwrap_err()
                .to_string(),
            "original"
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn worker_cwd_is_canonical_and_invalid_paths_do_not_create_tasks() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("krill-worker-cwd-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("nested"))?;
        std::fs::write(dir.join("file"), "file")?;
        assert_eq!(
            resolve_worker_cwd(Some(&dir.join("nested/..").to_string_lossy()), false)?,
            std::fs::canonicalize(&dir)?
        );
        assert_eq!(
            resolve_worker_cwd(Some("."), false)?,
            std::fs::canonicalize(".")?
        );
        assert_eq!(
            resolve_worker_cwd(None, false)?,
            std::fs::canonicalize(std::env::current_dir()?)?
        );
        for path in [dir.join("missing"), dir.join("file")] {
            let error = cmd_worker_start(
                &dir.join("state"),
                &args(&["--spec", "must not run", "--cwd", &path.to_string_lossy()]),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("--cwd"), "{error}");
            assert!(!dir.join("state/orchestration.json").exists());
        }
        assert!(resolve_worker_cwd(None, true).is_err());
        assert!(resolve_worker_cwd(Some("relative"), true).is_err());
        assert_eq!(
            resolve_worker_cwd(Some("/remote/does/not/exist"), true)?,
            PathBuf::from("/remote/does/not/exist")
        );
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
        complete_worker_stop(&dir, &released_target.id, false, || Ok(()))?;
        let released = complete_worker_stop(&dir, &released_target.id, true, || Ok(()))?;
        assert_eq!(released.status, DispatchStatus::Stopped);
        assert!(released.is_released());
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
            let expected = if release {
                "cannot release an active dispatch; settle, stop or abandon it first"
            } else {
                "injected close failure"
            };
            assert_eq!(result.unwrap_err().to_string(), expected);
            assert_eq!(OrchestrationStore::load(&dir)?, before);
        }
        OrchestrationStore::mutate(&dir, |store| {
            store.stop_or_release_worker(&dispatch.id, false)
        })?;
        let before = OrchestrationStore::load(&dir)?;
        let result = complete_worker_stop(&dir, &dispatch.id, true, || {
            Err(io::Error::other("settled close failure"))
        });
        assert_eq!(result.unwrap_err().to_string(), "settled close failure");
        assert_eq!(OrchestrationStore::load(&dir)?, before);
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

    #[test]
    fn ask_question_id_rewaits_without_duplicate_and_worker_start_supports_preamble_modes()
    -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-ask-rewait-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let run = OrchestrationStore::mutate(&dir, |s| s.create_run("Preamble & Ask", None))?;
        cmd_worker_start(
            &dir,
            &args(&[
                "--run",
                &run.id,
                "--worker",
                "w-min",
                "--prompt",
                "Check build",
                "--preamble",
                "minimal",
                "--no-spawn",
                "--json",
            ]),
        )?;
        cmd_worker_start(
            &dir,
            &args(&[
                "--run",
                &run.id,
                "--worker",
                "w-none",
                "--prompt",
                "Silent start",
                "--no-preamble",
                "--no-spawn",
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        let d = store.dispatches.first().unwrap();
        assert!(
            format_worker_preamble(d, PreambleMode::Minimal)
                .unwrap()
                .contains("Check build")
        );
        assert!(format_worker_preamble(d, PreambleMode::None).is_none());

        cmd_ask(
            &dir,
            &args(&[
                "--from",
                "w-min",
                "--to",
                "coordinator",
                "--question",
                "Approve?",
                "--wait",
                "--timeout-ms",
                "10",
                "--json",
            ]),
        )?;
        let qid = OrchestrationStore::load(&dir)?
            .messages
            .iter()
            .find(|m| m.msg_type == MessageType::Question)
            .unwrap()
            .id
            .clone();
        let before_len = OrchestrationStore::load(&dir)?.messages.len();
        // 타임아웃 후 --question-id 로 재대기해도 중복 Question이 생기지 않아야 한다(#30372).
        cmd_ask(
            &dir,
            &args(&[
                "--question-id",
                &qid,
                "--wait",
                "--timeout-ms",
                "10",
                "--json",
            ]),
        )?;
        assert_eq!(OrchestrationStore::load(&dir)?.messages.len(), before_len);
        cmd_reply(&dir, &args(&[&qid, "--answer", "Yes", "--json"]))?;
        cmd_ask(
            &dir,
            &args(&[
                "--question-id",
                &qid,
                "--wait",
                "--timeout-ms",
                "10",
                "--json",
            ]),
        )?;
        assert_eq!(
            OrchestrationStore::load(&dir)?.messages.len(),
            before_len + 1
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn orca_parity_subcommands_and_flag_aliases() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-orca-parity-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let ctx = OrchCtx::local(&dir);

        // 1. run-create with --objective and --worktree / --cwd
        cmd_run_create_ctx(
            &ctx,
            &args(&[
                "--objective",
                "Orca Objective",
                "--worktree",
                "current",
                "--cwd",
                "/tmp",
                "--json",
            ]),
        )?;
        let run = OrchestrationStore::load(&dir)?.runs[0].clone();
        assert_eq!(run.title, "Orca Objective");

        // 2. run-use with --id and run-show
        cmd_run_use_ctx(&ctx, &args(&["--id", &run.id, "--json"]))?;
        cmd_run_show_ctx(&ctx, &args(&["--id", &run.id, "--json"]))?;
        cmd_run_show_ctx(&ctx, &args(&[&run.id]))?;

        // 3. task-create with --task-title, --spec (deriving title), --parent, JSON --deps
        cmd_task_create_ctx(&ctx, &args(&["--task-title", "Parent Task", "--json"]))?;
        let t1 = OrchestrationStore::load(&dir)?.tasks[0].clone();
        assert_eq!(t1.title, "Parent Task");

        cmd_task_create_ctx(
            &ctx,
            &args(&[
                "--spec",
                "Derived Title From Spec\nSecond line details",
                "--parent",
                &t1.id,
                "--deps",
                &format!("[\"{}\"]", t1.id),
                "--json",
            ]),
        )?;
        let t2 = OrchestrationStore::load(&dir)?.tasks[1].clone();
        assert_eq!(t2.title, "Derived Title From Spec");
        assert_eq!(t2.deps, vec![t1.id.clone()]);

        // 4. task-update with --id and --result
        cmd_task_update_ctx(
            &ctx,
            &args(&[
                "--id",
                &t1.id,
                "--status",
                "completed",
                "--result",
                "Done via --result",
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        assert_eq!(
            store.tasks[0].result_summary.as_deref(),
            Some("Done via --result")
        );
        assert_eq!(store.tasks[1].status, TaskStatus::Ready);

        // 5. worker-start with plain-text --spec (auto-creates task and worker name).
        cmd_worker_start_ctx(
            &ctx,
            &args(&[
                "--spec",
                "Auto task from plain spec\nMore instructions",
                "--agent",
                "codex",
                "--worktree",
                "current",
                "--no-spawn",
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        assert_eq!(store.tasks.len(), 3);
        assert_eq!(store.tasks[2].title, "Auto task from plain spec");
        assert_eq!(store.tasks[2].status, TaskStatus::Dispatched);
        let disp = store.dispatches[0].clone();
        assert_eq!(disp.worker_name, "codex-1");
        assert_eq!(disp.task_id.as_deref(), Some(store.tasks[2].id.as_str()));

        // 6. dispatch-show (--dispatch, --task, --preamble) and worker-show (--dispatch)
        cmd_dispatch_show_ctx(&ctx, &args(&["--dispatch", &disp.id, "--json"]))?;
        cmd_dispatch_show_ctx(
            &ctx,
            &args(&["--task", &store.tasks[2].id, "--preamble", "--json"]),
        )?;
        cmd_dispatch_show_ctx(&ctx, &args(&[&disp.id, "--preamble"]))?;
        cmd_worker_show_ctx(&ctx, &args(&["--dispatch", &disp.id, "--json"]))?;
        cmd_worker_read_ctx(
            &ctx,
            &args(&["--dispatch", &disp.id, "--limit", "10", "--json"]),
        )?;

        // 7. worker-retain (--dispatch) clears active task lock without closing session
        cmd_worker_retain_ctx(&ctx, &args(&["--dispatch", &disp.id, "--json"]))?;
        let store = OrchestrationStore::load(&dir)?;
        assert_eq!(store.dispatches[0].status, DispatchStatus::Retained);
        assert_eq!(store.tasks[2].status, TaskStatus::Ready);
        assert_eq!(store.tasks[2].active_dispatch_id, None);

        // 8. send (default --to coordinator, --subject, --files-modified, --report-path) & inbox / check
        cmd_send_ctx(
            &ctx,
            &args(&[
                "--from",
                "codex-1",
                "--body",
                "First line summary\nDetails",
                "--files-modified",
                "src/lib.rs",
                "--report-path",
                "/tmp/rep.md",
                "--phase",
                "impl",
                "--thread-id",
                "th-1",
                "--priority",
                "high",
                "--task-id",
                &store.tasks[2].id,
                "--dispatch-id",
                &disp.id,
                "--json",
            ]),
        )?;
        let last_msg = OrchestrationStore::load(&dir)?
            .messages
            .last()
            .unwrap()
            .clone();
        assert_eq!(last_msg.to, "coordinator");
        assert_eq!(last_msg.summary, "First line summary");
        assert!(last_msg.body.contains("files_modified: src/lib.rs"));
        assert!(last_msg.body.contains("report_path: /tmp/rep.md"));

        cmd_inbox_ctx(&ctx, &args(&["--run", &run.id, "--json"]))?;
        cmd_check_ctx(
            &ctx,
            &args(&[
                "--terminal",
                "coordinator",
                "--all",
                "--peek",
                "--wait",
                "1",
                "--json",
            ]),
        )?;

        // 9. ask (default --to coordinator, --options, --no-wait, --resume) & reply (--id)
        cmd_ask_ctx(
            &ctx,
            &args(&[
                "--from",
                "codex-1",
                "--question",
                "Which option?",
                "--options",
                "A,B",
                "--no-wait",
                "--json",
            ]),
        )?;
        let q_msg = OrchestrationStore::load(&dir)?
            .messages
            .iter()
            .rev()
            .find(|m| m.msg_type == MessageType::Question)
            .unwrap()
            .clone();
        assert_eq!(q_msg.to, "coordinator");
        assert!(q_msg.body.contains("Options: A,B"));

        cmd_reply_ctx(&ctx, &args(&["--id", &q_msg.id, "--body", "A", "--json"]))?;
        cmd_ask_ctx(
            &ctx,
            &args(&[
                "--resume",
                &q_msg.id,
                "--wait",
                "--timeout-ms",
                "10",
                "--json",
            ]),
        )?;

        // 10. gate-resolve with --id and --resolution
        cmd_gate_create_ctx(
            &ctx,
            &args(&[
                "--from",
                "codex-1",
                "--question",
                "Merge?",
                "--options",
                "yes,no",
                "--json",
            ]),
        )?;
        let gate_id = OrchestrationStore::load(&dir)?.gates[0].id.clone();
        cmd_gate_resolve_ctx(
            &ctx,
            &args(&["--id", &gate_id, "--resolution", "yes", "--json"]),
        )?;
        assert_eq!(
            OrchestrationStore::load(&dir)?.gates[0].decision.as_deref(),
            Some("yes")
        );

        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn worker_start_parses_model_and_effort_for_spawned_sessions() {
        let parsed = parse_worker_start(
            &args(&[
                "--worker", "w-codex", "--agent", "codex", "--model", "gpt-5.4", "--effort",
                "xhigh", "--prompt", "verify",
            ]),
            false,
        )
        .unwrap();
        assert_eq!(parsed.spec.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(parsed.spec.effort.as_deref(), Some("xhigh"));

        let spec_json = r#"{"worker":"w-claude","agent":"claude","model":"opus","effort":"max","prompt":"verify"}"#;
        let parsed_spec = parse_worker_start(&args(&["--spec", spec_json]), false).unwrap();
        assert_eq!(parsed_spec.spec.model.as_deref(), Some("opus"));
        assert_eq!(parsed_spec.spec.effort.as_deref(), Some("max"));

        // Invalid effort or --session with --model is rejected.
        assert!(
            parse_worker_start(
                &args(&[
                    "--worker",
                    "w",
                    "--model",
                    "opus",
                    "--session",
                    "42",
                    "--prompt",
                    "p"
                ]),
                false
            )
            .is_err()
        );
        assert!(
            parse_worker_start(
                &args(&["--worker", "w", "--effort", "ultra", "--prompt", "p"]),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn worker_report_projects_liveness_reuse_and_next_actions() -> io::Result<()> {
        use krill_proto::control::AgentState;
        let mut store = OrchestrationStore::default();
        let run = store.create_run("liveness", None)?;
        let d_reused_old =
            store.start_worker(&run.id, None, "w-old", "codex", Some(10), "/tmp", "p1")?;
        store.send_message(&SendParams {
            from: "w-old".into(),
            to: "coordinator".into(),
            msg_type: MessageType::WorkerDone,
            summary: "done".into(),
            body: String::new(),
            run_id: Some(run.id.clone()),
            task_id: None,
            dispatch_id: Some(d_reused_old.id.clone()),
            outcome: Some("succeeded".into()),
            reply_to: None,
        })?;
        // Reuse session 10 on a new active dispatch.
        let d_reused_new =
            store.start_worker(&run.id, None, "w-new", "codex", Some(10), "/tmp", "p2")?;
        // Settled dispatch on session 20 (still open -> reclaimable).
        let d_reclaim =
            store.start_worker(&run.id, None, "w-reclaim", "claude", Some(20), "/tmp", "p3")?;
        store.send_message(&SendParams {
            from: "w-reclaim".into(),
            to: "coordinator".into(),
            msg_type: MessageType::WorkerDone,
            summary: "done".into(),
            body: String::new(),
            run_id: Some(run.id.clone()),
            task_id: None,
            dispatch_id: Some(d_reclaim.id.clone()),
            outcome: Some("succeeded".into()),
            reply_to: None,
        })?;
        // Active dispatch on session 30 that exited unexpectedly.
        let d_dead =
            store.start_worker(&run.id, None, "w-dead", "claude", Some(30), "/tmp", "p4")?;

        let observed = vec![
            crate::session::ObservedSession {
                id: 10,
                exit_code: None,
                agent_state: Some(AgentState::Working),
            },
            crate::session::ObservedSession {
                id: 20,
                exit_code: None,
                agent_state: Some(AgentState::Waiting),
            },
            crate::session::ObservedSession {
                id: 30,
                exit_code: Some(1),
                agent_state: Some(AgentState::Exited),
            },
        ];

        let old_disp = store
            .dispatches
            .iter()
            .find(|d| d.id == d_reused_old.id)
            .unwrap();
        assert!(!dispatch_owns_session(&store.dispatches, old_disp));
        let r_old = build_worker_report(old_disp, &store.dispatches, Some(&observed));
        assert_eq!(r_old.terminal_state, "reassigned");
        assert!(r_old.next_action.is_none());

        let new_disp = store
            .dispatches
            .iter()
            .find(|d| d.id == d_reused_new.id)
            .unwrap();
        assert!(dispatch_owns_session(&store.dispatches, new_disp));
        let r_new = build_worker_report(new_disp, &store.dispatches, Some(&observed));
        assert_eq!(r_new.terminal_state, "active");
        assert_eq!(r_new.session_state, "live");
        assert_eq!(r_new.agent_state, Some("working"));
        assert!(r_new.next_action.is_none());

        let reclaim_disp = store
            .dispatches
            .iter()
            .find(|d| d.id == d_reclaim.id)
            .unwrap();
        let r_reclaim = build_worker_report(reclaim_disp, &store.dispatches, Some(&observed));
        assert_eq!(r_reclaim.terminal_state, "reclaimable");
        assert_eq!(
            r_reclaim.next_action.as_ref().map(|a| a.action),
            Some("release_or_retain")
        );

        let mut retained = reclaim_disp.clone();
        retained.release_state = krill_proto::orchestration::DispatchReleaseState::Retained;
        let report = build_worker_report(&retained, &store.dispatches, Some(&observed));
        assert_eq!(report.terminal_state, "retained");
        assert_eq!(report.dispatch.status, DispatchStatus::Succeeded);
        assert!(report.next_action.is_none());
        retained.release_state = krill_proto::orchestration::DispatchReleaseState::Released;
        let report = build_worker_report(&retained, &store.dispatches, Some(&observed));
        assert_eq!(report.terminal_state, "released");
        assert!(report.next_action.is_none());
        retained.release_state = krill_proto::orchestration::DispatchReleaseState::Active;
        retained.session_origin = DispatchSessionOrigin::Borrowed;
        let report = build_worker_report(&retained, &store.dispatches, Some(&observed));
        assert_eq!(report.next_action.unwrap().action, "abandon");

        let dead_disp = store.dispatches.iter().find(|d| d.id == d_dead.id).unwrap();
        let r_dead = build_worker_report(dead_disp, &store.dispatches, Some(&observed));
        assert_eq!(r_dead.terminal_state, "active");
        assert_eq!(r_dead.session_state, "exited");
        assert_eq!(r_dead.session_live, Some(false));
        assert_eq!(
            r_dead.next_action.as_ref().map(|a| a.action),
            Some("abandon_or_release")
        );
        Ok(())
    }

    #[test]
    fn check_supports_run_filter_and_selective_ack_ids() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-check-ack-ids-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let ctx = OrchCtx::local(&dir);
        cmd_run_create_ctx(&ctx, &args(&["--title", "R1", "--json"]))?;
        cmd_run_create_ctx(&ctx, &args(&["--title", "R2", "--json"]))?;
        let store = OrchestrationStore::load(&dir)?;
        let r1 = store.runs[0].id.clone();
        let r2 = store.runs[1].id.clone();

        cmd_send_ctx(
            &ctx,
            &args(&[
                "--from",
                "w1",
                "--to",
                "coordinator",
                "--run",
                &r1,
                "--summary",
                "msg1",
                "--json",
            ]),
        )?;
        cmd_send_ctx(
            &ctx,
            &args(&[
                "--from",
                "w2",
                "--to",
                "coordinator",
                "--run",
                &r2,
                "--summary",
                "msg2",
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        let m1 = store.messages[0].id.clone();
        let m2 = store.messages[1].id.clone();

        // --ack-ids cannot be combined with --wait or --ack
        assert!(cmd_check_ctx(&ctx, &args(&["--ack-ids", &m1, "--wait", "--json"])).is_err());
        // Wrong run filter on --ack-ids is rejected without mutating
        assert!(
            cmd_check_ctx(
                &ctx,
                &args(&[
                    "--to",
                    "coordinator",
                    "--run",
                    &r1,
                    "--ack-ids",
                    &m2,
                    "--json"
                ]),
            )
            .is_err()
        );
        assert!(
            OrchestrationStore::load(&dir)?
                .messages
                .iter()
                .all(|m| !m.acked)
        );

        // Selective ack of m1 only
        cmd_check_ctx(
            &ctx,
            &args(&[
                "--to",
                "coordinator",
                "--run",
                &r1,
                "--unread",
                "--ack-ids",
                &m1,
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        assert!(store.messages.iter().find(|m| m.id == m1).unwrap().acked);
        assert!(!store.messages.iter().find(|m| m.id == m2).unwrap().acked);
        std::fs::remove_dir_all(dir)
    }
}

#[cfg(test)]
mod delivery_and_launch_cli_tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn check_run_and_named_ack_preserve_mail_arriving_after_peek() -> io::Result<()> {
        let dir =
            std::env::temp_dir().join(format!("krill-check-delivery-cli-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let ctx = OrchCtx::local(&dir);
        let (a, b) = OrchestrationStore::mutate(&dir, |s| {
            Ok((s.create_run("a", None)?, s.create_run("b", None)?))
        })?;
        for run in [&a.id, &b.id] {
            cmd_send_ctx(
                &ctx,
                &args(&[
                    "--from",
                    "worker",
                    "--to",
                    "coordinator",
                    "--run",
                    run,
                    "--summary",
                    "before",
                    "--json",
                ]),
            )?;
        }
        cmd_check_ctx(
            &ctx,
            &args(&["--to", "coordinator", "--run", &a.id, "--unread", "--json"]),
        )?;
        let delivery = OrchestrationStore::load(&dir)?
            .deliveries
            .first()
            .unwrap()
            .clone();
        cmd_send_ctx(
            &ctx,
            &args(&[
                "--from",
                "worker",
                "--to",
                "coordinator",
                "--run",
                &a.id,
                "--summary",
                "after",
                "--json",
            ]),
        )?;
        cmd_check_ctx(
            &ctx,
            &args(&[
                "--to",
                "coordinator",
                "--run",
                &a.id,
                "--ack",
                &delivery.id,
                "--json",
            ]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        assert_eq!(store.messages.iter().filter(|m| !m.acked).count(), 2);
        assert!(cmd_check_ctx(&ctx, &args(&["--run", &b.id, "--ack", &delivery.id])).is_err());
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn request_show_and_retry_inherit_launch_preferences() -> io::Result<()> {
        let dir =
            std::env::temp_dir().join(format!("krill-retry-request-cli-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir)?;
        let ctx = OrchCtx::local(&dir);
        let (run, task) = OrchestrationStore::mutate(&dir, |s| {
            let run = s.create_run("retry", None)?;
            let task = s.create_task(&run.id, "task", "", vec![])?;
            Ok((run, task))
        })?;
        cmd_worker_start_ctx(
            &ctx,
            &args(&[
                "--run",
                &run.id,
                "--task",
                &task.id,
                "--worker",
                "w",
                "--agent",
                "codex",
                "--model",
                "chosen",
                "--effort",
                "high",
                "--prompt",
                "work",
                "--no-spawn",
                "--json",
            ]),
        )?;
        let first = OrchestrationStore::load(&dir)?
            .dispatches
            .first()
            .unwrap()
            .clone();
        let request = first.request_id.as_deref().unwrap();
        cmd_request_show_ctx(&ctx, &args(&[request, "--json"]))?;
        OrchestrationStore::mutate(&dir, |s| {
            s.worker_start_failed(&first.id, "test start failure")
        })?;
        cmd_worker_start_ctx(
            &ctx,
            &args(&["--retry-request", request, "--no-spawn", "--json"]),
        )?;
        let store = OrchestrationStore::load(&dir)?;
        let retry = store.dispatches.last().unwrap();
        assert_eq!(retry.retry_of.as_deref(), Some(first.id.as_str()));
        assert_eq!(retry.model, first.model);
        assert_eq!(retry.effort, first.effort);
        assert_ne!(retry.request_id, first.request_id);
        assert!(cmd_request_show_ctx(&ctx, &args(&["req-missing", "--json"])).is_err());
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn release_checks_authority_before_running_close_callback() -> io::Result<()> {
        let dir =
            std::env::temp_dir().join(format!("krill-release-guard-cli-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let dispatch = OrchestrationStore::mutate(&dir, |s| {
            let run = s.create_run("guard", None)?;
            s.start_worker(&run.id, None, "w", "codex", Some(1234), "/tmp", "p")
        })?;
        let called = std::cell::Cell::new(false);
        assert!(
            complete_worker_stop(&dir, &dispatch.id, true, || {
                called.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!called.get());
        // Fencing succeeds without ever establishing a daemon connection or closing a PTY.
        cmd_worker_abandon_ctx(&OrchCtx::local(&dir), &args(&[&dispatch.id, "--json"]))?;
        let fenced = OrchestrationStore::load(&dir)?
            .dispatches
            .first()
            .unwrap()
            .clone();
        assert_eq!(fenced.session_id, Some(1234));
        assert_eq!(fenced.status, DispatchStatus::Abandoned);
        std::fs::remove_dir_all(dir)
    }
}

#[cfg(test)]
mod snapshot_fallback_tests {
    use super::*;

    /// foundation-02: 읽기 전용 디스크 fallback(로컬)·명시 오류(원격)는 구 데몬의 단일 프레임 초과 신호
    /// 두 가지(이전 빌드의 EOF, 현재 빌드의 프레임 상한 오류)에만 반응한다.
    #[test]
    fn snapshot_unavailable_matches_only_prior_daemon_overflow_signals() {
        assert!(snapshot_unavailable(&io::Error::from(
            io::ErrorKind::UnexpectedEof
        )));
        assert!(snapshot_unavailable(&io::Error::new(
            io::ErrorKind::InvalidInput,
            krill_proto::orchestration::frame_limit_error(1_204_357),
        )));
        assert!(!snapshot_unavailable(&io::Error::new(
            io::ErrorKind::NotFound,
            "run not found"
        )));
        assert!(!snapshot_unavailable(&io::Error::from(
            io::ErrorKind::TimedOut
        )));
        assert!(!snapshot_unavailable(&io::Error::other(
            "orchestration snapshot kept changing while paging; retry"
        )));
    }
}
