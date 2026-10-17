//! `krill orchestration` (`krill orch`) CLI — Multi-Agent Orchestration (MK-114 / OWE-624).
//!
//! Coordinates multi-agent runs, task DAGs, worker dispatches, threaded messages,
//! blocking ask/reply, and decision gates backed by `state_dir/orchestration.json`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use krill_proto::daemon;
use krill_proto::orchestration::{
    GateStatus, MessageType, OrchestrationStore, SendParams, TaskStatus, worker_preamble,
};

const USAGE: &str = "\
usage: krill orchestration <subcommand> [options]
  run-create --title <title> [--coordinator <session-id>] [--json]
  run-list [--json]
  task-create --run <run-id> --title <title> [--desc <text>] [--deps <t1,t2>] [--json]
  task-list [--run <run-id>] [--json]
  task-update <task-id> --status <pending|ready|dispatched|completed|failed|blocked> [--summary <text>] [--json]
  worker-start --run <run-id> [--task <task-id>] --worker <name> [--agent <agent>] [--session <id>] [--cwd <dir>] --prompt <text> [--json]
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
  reset [--run <run-id>] [--json]";

pub fn run(raw: &[OsString]) -> ExitCode {
    let Ok(args) = raw
        .iter()
        .cloned()
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
    let Some(state_dir) = daemon::state_dir() else {
        eprintln!("orchestration: HOME is not set");
        return ExitCode::FAILURE;
    };

    let res = match sub.as_str() {
        "run-create" => cmd_run_create(&state_dir, rest),
        "run-list" | "runs" => cmd_run_list(&state_dir, rest),
        "task-create" => cmd_task_create(&state_dir, rest),
        "task-list" | "tasks" => cmd_task_list(&state_dir, rest),
        "task-update" => cmd_task_update(&state_dir, rest),
        "worker-start" => cmd_worker_start(&state_dir, rest),
        "send" => cmd_send(&state_dir, rest),
        "check" => cmd_check(&state_dir, rest),
        "ask" => cmd_ask(&state_dir, rest),
        "reply" => cmd_reply(&state_dir, rest),
        "gate-create" => cmd_gate_create(&state_dir, rest),
        "gate-list" | "gates" => cmd_gate_list(&state_dir, rest),
        "gate-resolve" => cmd_gate_resolve(&state_dir, rest),
        "worker-list" | "workers" => cmd_worker_list(&state_dir, rest),
        "worker-show" => cmd_worker_show(&state_dir, rest),
        "worker-read" => cmd_worker_read(&state_dir, rest),
        "worker-stop" => cmd_worker_stop(&state_dir, rest, false),
        "worker-release" => cmd_worker_stop(&state_dir, rest, true),
        "reset" => cmd_reset(&state_dir, rest),
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

fn cmd_run_create(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let created = OrchestrationStore::mutate(state_dir, |s| s.create_run(&title, coordinator))?;
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

fn cmd_run_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
    let json = has_flag(args, "--json");
    let store = OrchestrationStore::load(state_dir)?;
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

fn cmd_task_create(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let run_id = run_id.ok_or_else(|| bad_arg("missing --run"))?;
    let title = title.ok_or_else(|| bad_arg("missing --title"))?;
    let task =
        OrchestrationStore::mutate(state_dir, |s| s.create_task(&run_id, &title, &desc, deps))?;
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

fn cmd_task_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let store = OrchestrationStore::load(state_dir)?;
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

fn cmd_task_update(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let updated =
        OrchestrationStore::mutate(state_dir, |s| s.update_task(task_id, status, summary))?;
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

fn cmd_worker_start(state_dir: &Path, args: &[String]) -> io::Result<()> {
    let mut run_id = None;
    let mut task_id = None;
    let mut worker_name = None;
    let mut agent = "claude".to_owned();
    let mut session_id = None;
    let mut cwd = None;
    let mut prompt = None;
    let mut no_spawn = false;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--run" => run_id = it.next().cloned(),
            "--task" => task_id = it.next().cloned(),
            "--worker" | "--name" => worker_name = it.next().cloned(),
            "--agent" => {
                agent.clone_from(take_opt(&mut it, "--agent")?);
            }
            "--session" => {
                let v = it.next().ok_or_else(|| bad_arg("missing --session"))?;
                session_id = Some(v.parse::<u64>().map_err(|_| bad_arg("invalid --session"))?);
            }
            "--cwd" => cwd = it.next().cloned(),
            "--prompt" => prompt = it.next().cloned(),
            "--no-spawn" => no_spawn = true,
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let run_id = run_id.ok_or_else(|| bad_arg("missing --run"))?;
    let worker_name = worker_name.ok_or_else(|| bad_arg("missing --worker"))?;
    let prompt = prompt.ok_or_else(|| bad_arg("missing --prompt"))?;
    let agent = krill_proto::agent::canonical_id(&agent).to_owned();
    let work_cwd = match cwd {
        Some(c) => PathBuf::from(c),
        None => std::env::current_dir()?,
    };
    let cwd_str = work_cwd.to_string_lossy().into_owned();

    // 스폰 전에 run/task 유효성과 상태를 먼저 검증해 잘못된 요청이 고아 PTY 세션을 남기지 않게 한다.
    OrchestrationStore::load(state_dir)?.validate_worker_start(
        &run_id,
        task_id.as_deref(),
        &worker_name,
    )?;

    // 존재하지 않는 세션을 지정하면 dispatch가 Active 고스트로 남고 stop/release가 영구 실패한다(C1).
    // mutate 전에 세션 존재를 확인해 고스트 기록 자체를 만들지 않는다.
    if let Some(sid) = session_id {
        ensure_worker_session_exists(&worker_name, sid)?;
    }

    let newly_spawned = session_id.is_none() && !no_spawn;
    if newly_spawned {
        let spawned = crate::session::spawn_agent_session(&agent, &work_cwd)
            .map_err(|e| io::Error::other(format!("spawn {agent} worker session: {e}")))?;
        session_id = Some(spawned);
    }

    let dispatch = match OrchestrationStore::mutate(state_dir, |s| {
        s.start_worker(
            &run_id,
            task_id.as_deref(),
            &worker_name,
            &agent,
            session_id,
            &cwd_str,
            &prompt,
        )
    }) {
        Ok(d) => d,
        Err(e) => {
            if newly_spawned && let Some(sid) = session_id {
                crate::session::close_session(sid).ok();
            }
            return Err(e);
        }
    };

    let delivered = dispatch.session_id.map_or(Ok(()), |sid| {
        let target = crate::remote::Target::establish(None).map_err(|e| e.to_string())?;
        let preamble = worker_preamble(&dispatch).replace('\n', " | ");
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

fn cmd_send(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let created = OrchestrationStore::mutate(state_dir, |s| s.send_message(&params))?;
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

fn cmd_check(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let start = Instant::now();
    let deadline = Duration::from_millis(timeout_ms);
    let messages = loop {
        let mut preview_store = OrchestrationStore::load(state_dir)?;
        let preview = preview_store.check_messages(to.as_deref(), &types, unread_only, false);
        if !preview.is_empty() {
            if ack {
                break OrchestrationStore::mutate(state_dir, |s| {
                    Ok(s.check_messages(to.as_deref(), &types, unread_only, true))
                })?;
            }
            break preview;
        }
        if !wait || start.elapsed() >= deadline {
            break Vec::new();
        }
        std::thread::sleep(Duration::from_millis(80));
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

fn cmd_ask(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let sent = OrchestrationStore::mutate(state_dir, |s| {
        s.send_message(&SendParams {
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
        })
    })?;
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

    let start = Instant::now();
    let deadline = Duration::from_millis(timeout_ms);
    loop {
        let store = OrchestrationStore::load(state_dir)?;
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

fn cmd_reply(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let reply = OrchestrationStore::mutate(state_dir, |s| s.reply_message(msg_id, &from, &body))?;
    if json {
        print_json(&reply)?;
    } else {
        println!("{}\treply->{}\t{}", reply.id, reply.to, reply.body);
    }
    Ok(())
}

fn cmd_gate_create(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let gate = OrchestrationStore::mutate(state_dir, |s| {
        s.create_gate(&from, &question, options, run_id, task_id)
    })?;
    if json {
        print_json(&gate)?;
    } else {
        println!("{}\t{}\t{}", gate.id, gate.status.as_str(), gate.question);
    }
    Ok(())
}

fn cmd_gate_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
    let pending_only = args.iter().any(|a| a == "--pending");
    let json = args.iter().any(|a| a == "--json");
    let store = OrchestrationStore::load(state_dir)?;
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

fn cmd_gate_resolve(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let gate = OrchestrationStore::mutate(state_dir, |s| {
        s.resolve_gate(gate_id, &decision, rationale, &by)
    })?;
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

fn cmd_worker_list(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    let store = OrchestrationStore::load(state_dir)?;
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

fn cmd_worker_show(state_dir: &Path, args: &[String]) -> io::Result<()> {
    let Some((target, rest)) = args.split_first() else {
        return Err(bad_arg("missing <worker-or-dispatch>"));
    };
    let json = rest.iter().any(|a| a == "--json");
    let store = OrchestrationStore::load(state_dir)?;
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

fn cmd_worker_read(state_dir: &Path, args: &[String]) -> io::Result<()> {
    let Some((target, rest)) = args.split_first() else {
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
    let store = OrchestrationStore::load(state_dir)?;
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
    let mut screen_text = match (disp.status, disp.session_id) {
        (krill_proto::orchestration::DispatchStatus::Released, _) | (_, None) => {
            disp.archived_output.clone().unwrap_or_default()
        }
        (_, Some(sid)) => crate::session::capture_session_text(sid)
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

fn cmd_worker_stop(state_dir: &Path, args: &[String], release: bool) -> io::Result<()> {
    let Some((target, rest)) = args.split_first() else {
        return Err(bad_arg("missing <worker-or-dispatch>"));
    };
    if let Some(arg) = rest.iter().find(|a| *a != "--json") {
        return Err(bad_arg(format!("unexpected argument {arg}")));
    }
    let json = rest.iter().any(|a| a == "--json");
    let store = OrchestrationStore::load(state_dir)?;
    let selected = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.id == *target || d.worker_name == *target)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("worker not found: {target}"),
            )
        })?;
    let captured = match selected.session_id {
        Some(sid) if selected.status != krill_proto::orchestration::DispatchStatus::Released => {
            match crate::session::capture_session_text(sid) {
                Ok(text) => Some(text),
                Err(capture_err) => settle_missing_session_capture(
                    &selected.worker_name,
                    sid,
                    capture_err,
                    selected.archived_output.clone(),
                    session_present(sid),
                )?,
            }
        }
        _ => selected.archived_output.clone(),
    };
    // Persist terminal output before closing. Failure must leave the dispatch status unchanged.
    if let Some(text) = captured {
        OrchestrationStore::mutate(state_dir, |s| {
            let stored = s
                .dispatches
                .iter_mut()
                .find(|d| d.id == selected.id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "dispatch missing"))?;
            stored.archived_output = Some(text);
            Ok(())
        })?;
    }
    let disp = complete_worker_stop(state_dir, &selected.id, release, || {
        if selected.status != krill_proto::orchestration::DispatchStatus::Released
            && let Some(sid) = selected.session_id
            && crate::session::list_session_ids()
                .map_err(io::Error::other)?
                .contains(&sid)
        {
            crate::session::close_session(sid).map_err(io::Error::other)?;
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

fn complete_worker_stop(
    state_dir: &Path,
    dispatch_id: &str,
    release: bool,
    close: impl FnOnce() -> io::Result<()>,
) -> io::Result<krill_proto::orchestration::Dispatch> {
    close()?;
    OrchestrationStore::mutate(state_dir, |s| {
        s.stop_or_release_worker(dispatch_id, release)
    })
}

/// `worker-start --session <id>`용 세션 존재 확인. 데몬에 닿지 않으면 그 오류를 그대로 돌려준다.
fn ensure_worker_session_exists(worker: &str, sid: u64) -> io::Result<()> {
    match crate::session::list_session_ids() {
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
fn session_present(sid: u64) -> Option<bool> {
    crate::session::list_session_ids()
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

fn cmd_reset(state_dir: &Path, args: &[String]) -> io::Result<()> {
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
    OrchestrationStore::mutate(state_dir, |s| {
        s.reset(run_id.as_deref());
        Ok(())
    })?;
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
}
