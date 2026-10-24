//! MK-045: direct daemon commands. Text shown by these commands is kept here.

#[cfg(unix)]
mod terminal;
#[cfg(windows)]
#[path = "session/terminal_windows.rs"]
mod terminal;
mod wait;

use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::os::net::UnixStream;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::control::{
    AgentState, CaptureFormat, Create, FEAT_CLI, FEAT_CONTROL, FEAT_SESSION_FORK, FEAT_SIDEBAR,
    NewWorktree, Program, Reply, Request, Setup,
};
use krill_proto::daemon::{self, Connection};
use krill_proto::hook::FEAT_HOOK;
use krill_proto::input::{self, Input, InputAck, InputId, InputResult, MAX_INPUT};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::{self, Attach, Attached, Msg, Seq, Stream};

use crate::remote::Target;

const USAGE: &str = "usage: krill [--host <user@host>] ls [--worktree <path>] [--json] | krill new [--cwd <path>] [--worktree <name>] [--agent <profile> [--model <id>] [--effort <level>] | -- <program> <args>...] [--account <id>] [--prompt <text>] [--cols N --rows N] [--json]\n       krill fork [<id>] [--from <id>] [--cwd <path>] [--prompt <text>] [--cols N --rows N] [--json]\n       krill send <id> [--enter] [--wait-submit] [--text <text>] [--stdin | -- <text>...]\n       krill read <id> [--lines N] | krill wait <id> [--for idle|waiting|blocked|pattern|quiet|exited|working|done|unknown] [--pattern <regex>] [--quiet-ms|--idle-ms N] [--timeout <seconds>]\n       krill kill <id> | krill attach <id> (Ctrl-] detaches; stdin EOF detaches)\n       remote (--host): SSH Unix-socket forwarding to the remote krilld; --cwd must be absolute on the remote host";
/// capture→attach 사이 세션 종료와 권한 거부를 구분할 수 없어 둘 다 이 문구로 나온다.
/// "방금 kill한 세션에 send" 같은 레이스에서 권한 문제로 오인하지 않게 닫힘 가능성을 먼저 적는다(C5).
const RAW_REFUSED_MSG: &str =
    "session unavailable (the session may have closed just now) or permission denied";
const IO_TIMEOUT: Duration = Duration::from_secs(5);
// Holder escalates HUP -> TERM at 2s -> KILL at 5s; leave time to observe Exited.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
/// 새로 띄운 에이전트 TUI가 입력을 받을 때까지 기다리는 상한(`worker-start`·`handoff`).
pub(crate) const BOOT_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug)]
struct Error {
    code: u8,
    text: String,
}
type Result<T> = std::result::Result<T, Error>;
fn fail(e: impl fmt::Display) -> Error {
    Error {
        code: 1,
        text: e.to_string(),
    }
}
fn usage() -> Error {
    Error {
        code: 2,
        text: USAGE.to_owned(),
    }
}

fn remote_usage(text: &str) -> Error {
    Error {
        code: 2,
        text: format!("{text}\n{USAGE}"),
    }
}

pub fn run(args: &[OsString]) -> ExitCode {
    let (host, rest) = match crate::remote::extract_host(args) {
        Ok(parsed) => parsed,
        Err(text) => {
            eprintln!("{text}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // 도움말은 데몬·SSH 연결 전에 판정한다. 연결 실패와 usage를 구분하기 위해서다.
    if has_help_before_literal(&rest) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let target = match Target::establish(host) {
        Ok(target) => target,
        Err(e) => {
            eprintln!("remote: {e}");
            return ExitCode::FAILURE;
        }
    };
    run_with_target(&target, &rest)
}

fn is_help_flag(s: &OsString) -> bool {
    matches!(s.to_str(), Some("-h" | "--help" | "help"))
}

/// 도움말 판정: 서브커맨드 자리(`ls --help`, `--help`)와 `--` 앞의 전역 플래그만 본다.
/// 세션 ID·전송 텍스트(`send 1 help`, `send 1 --text --help`)·`--` 뒤 리터럴은 도움말이 아니다.
fn has_help_before_literal(args: &[OsString]) -> bool {
    let mut it = args.iter();
    // `--host`는 extract_host가 이미 걷어냈으나, 직접 호출(run_with_target) 경로를 위해 여기서도 건너뛴다.
    let first = if args.first().is_some_and(|a| a == "--host") {
        it.next();
        it.next();
        it.next()
    } else {
        it.next()
    };
    let Some(cmd) = first else {
        return false;
    };
    // 베어 `--help` (서브커맨드 없음)
    if is_help_flag(cmd) {
        return true;
    }
    // 서브커맨드 직후 플래그만 본다. 그 뒤는 각 명령의 인자(ID·텍스트·쿼리)다.
    matches!(it.next().and_then(|s| s.to_str()), Some("-h" | "--help"))
        || matches!(cmd.to_str(), Some("-h" | "--help" | "help"))
}

pub(crate) fn run_with_target(target: &Target, rest: &[OsString]) -> ExitCode {
    if has_help_before_literal(rest) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = match rest.split_first() {
        Some((cmd, rest)) if cmd == "ls" => list(target, rest),
        Some((cmd, rest)) if cmd == "new" => create(target, rest),
        Some((cmd, rest)) if cmd == "fork" => fork(target, rest),
        Some((cmd, rest)) if cmd == "send" => send(target, rest),
        Some((cmd, rest)) if cmd == "read" => read(target, rest),
        Some((cmd, rest)) if cmd == "wait" => wait::run(rest, target),
        Some((cmd, rest)) if cmd == "attach" && rest.len() == 1 => {
            id(&rest[0]).and_then(|session| terminal::attach(target, session))
        }
        Some((cmd, rest)) if cmd == "kill" || cmd == "close" => kill(target, rest),
        _ => Err(usage()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", e.text);
            ExitCode::from(e.code)
        }
    }
}

fn id(s: &OsString) -> Result<SessionId> {
    s.to_str()
        .map(|s| s.strip_prefix('s').unwrap_or(s))
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .map(SessionId)
        .ok_or_else(usage)
}

fn connect(target: &Target, required: u64) -> Result<Connection> {
    let c = target
        .connect(&daemon::client_hello(ClientRole::Cli, "krill"))
        .map_err(fail)?;
    if c.negotiated.features & required != required {
        return Err(fail(
            "krilld does not support this command; update the daemon",
        ));
    }
    c.stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(fail)?;
    c.stream.set_write_timeout(Some(IO_TIMEOUT)).map_err(fail)?;
    Ok(c)
}

fn frame(c: &mut UnixStream) -> Result<(u8, Vec<u8>)> {
    ipc::read_frame(c)
        .map_err(fail)?
        .ok_or_else(|| fail("krilld disconnected"))
}

fn request(c: &mut UnixStream, r: &Request) -> Result<Reply> {
    c.write_all(&r.encode().map_err(fail)?).map_err(fail)?;
    let (kind, body) = frame(c)?;
    let reply = Reply::decode(kind, &body).map_err(fail)?;
    if let Reply::Failed(e) = reply {
        return Err(fail(e));
    }
    Ok(reply)
}

/// OWE-684: 세션 목록에 붙는 holder 상실 경고. 정상이면 `None`(기본 출력 그대로).
fn holder_loss_warning(orphan: bool) -> Option<&'static str> {
    orphan.then_some(
        "warning: krill-holder is lost — sessions keep running, but if krilld restarts before a new holder takes over, sessions may be lost",
    )
}

/// 감시 핸드셰이크로 holder 상실 여부를 묻는다(데몬을 띄우지 않는다).
/// 소켓이 없거나 감시 미지원이면 `false`.
fn probe_holder_orphan(target: &Target) -> bool {
    let hello = daemon::watch_hello(ClientRole::Cli, "krill");
    target
        .dial(&hello, IO_TIMEOUT)
        .ok()
        .and_then(|c| c.holder_status)
        .is_some_and(|s| s == daemon::HolderStatus::Orphan)
}

fn list(target: &Target, args: &[OsString]) -> Result<()> {
    struct EnrichedSession {
        id: u64,
        pid: u32,
        process: &'static str,
        state: &'static str,
        agent: Option<String>,
        account: Option<String>,
        cwd: String,
        worktree: String,
        title: String,
    }

    let mut json = false;
    let mut worktree_filter: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--json") => json = true,
            Some("--worktree" | "--cwd") if worktree_filter.is_none() => {
                let raw = it
                    .next()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(usage)?;
                let resolved = if target.is_remote() {
                    raw.to_owned()
                } else {
                    crate::set::resolve_worktree(std::path::Path::new(raw))
                        .to_string_lossy()
                        .into_owned()
                };
                worktree_filter = Some(resolved);
            }
            _ => return Err(usage()),
        }
    }
    // holder 상실은 연결(치유) 전에 관측한다. 목록은 그대로 보여주고 경고만 남긴다.
    let orphan = probe_holder_orphan(target);
    let mut c = connect(target, FEAT_CONTROL | FEAT_HOOK)?;
    let Reply::Sessions(sessions) = request(&mut c.stream, &Request::List)? else {
        return Err(fail("unexpected session list reply"));
    };
    let Reply::States(states) = request(&mut c.stream, &Request::States)? else {
        return Err(fail("unexpected state list reply"));
    };
    let sidebar = if c.negotiated.features & FEAT_SIDEBAR != 0 {
        match request(&mut c.stream, &Request::Sidebar) {
            Ok(Reply::Sidebar(sb)) => Some(sb),
            _ => None,
        }
    } else {
        None
    };
    let accounts = if c.negotiated.features & krill_proto::control::FEAT_SESSION_ACCOUNT != 0 {
        match request(&mut c.stream, &Request::SessionAccounts) {
            Ok(Reply::SessionAccounts(list)) => list,
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let mut enriched = Vec::new();
    for s in sessions {
        let state_entry = states.iter().find(|v| v.session == s.session);
        let state = state_entry.map_or(AgentState::Unknown, |v| v.status.state);
        let sb_entry = sidebar
            .as_ref()
            .and_then(|sb| sb.sessions.iter().find(|e| e.session == s.session));
        let cwd = sb_entry
            .map(|e| String::from_utf8_lossy(&e.cwd).into_owned())
            .unwrap_or_default();
        let mut worktree = sidebar
            .as_ref()
            .and_then(|sb| {
                sb.repositories
                    .iter()
                    .flat_map(|r| r.worktrees.iter())
                    .map(|w| String::from_utf8_lossy(w).into_owned())
                    .filter(|w| {
                        cwd == *w
                            || cwd
                                .strip_prefix(w.as_str())
                                .is_some_and(|rest| rest.starts_with('/'))
                    })
                    .max_by_key(String::len)
            })
            .unwrap_or_default();
        if worktree.is_empty() && !cwd.is_empty() && !target.is_remote() {
            worktree = crate::set::resolve_worktree(std::path::Path::new(&cwd))
                .to_string_lossy()
                .into_owned();
        }
        if let Some(filter) = &worktree_filter {
            let matches_wt = worktree == *filter
                || cwd == *filter
                || cwd
                    .strip_prefix(filter.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
            if !matches_wt {
                continue;
            }
        }
        let acc = accounts.iter().find(|a| a.session == s.session);
        let agent = state_entry
            .map(|v| v.status.agent.clone())
            .filter(|v| !v.is_empty());
        let account = acc
            .map(|a| {
                if a.account_id.is_empty() {
                    a.label.clone()
                } else {
                    a.account_id.clone()
                }
            })
            .filter(|v| !v.is_empty());
        let title = state_entry
            .and_then(|v| v.status.foreground.clone())
            .unwrap_or_default();
        enriched.push(EnrichedSession {
            id: s.session.0,
            pid: s.pid,
            process: if s.status.is_some() {
                "exited"
            } else {
                "running"
            },
            state: state_name(state),
            agent,
            account,
            cwd,
            worktree,
            title,
        });
    }

    if json {
        let rows: Vec<_> = enriched
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "pid": s.pid,
                    "process": s.process,
                    "state": s.state,
                    "agent": s.agent,
                    "account": s.account,
                    "cwd": s.cwd,
                    "worktree": s.worktree,
                    "title": s.title,
                })
            })
            .collect();
        let out = serde_json::to_string_pretty(&rows).map_err(fail)?;
        println!("{out}");
        if let Some(warning) = holder_loss_warning(orphan) {
            eprintln!("{warning}");
        }
        return Ok(());
    }
    println!("ID\tPID\tPROCESS\tSTATE\tAGENT\tACCOUNT\tWORKTREE\tCWD");
    for s in enriched {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            s.id,
            s.pid,
            s.process,
            s.state,
            s.agent.as_deref().unwrap_or("-"),
            s.account.as_deref().unwrap_or("-"),
            if s.worktree.is_empty() {
                "-"
            } else {
                &s.worktree
            },
            if s.cwd.is_empty() { "-" } else { &s.cwd },
        );
    }
    if let Some(warning) = holder_loss_warning(orphan) {
        eprintln!("{warning}");
    }
    Ok(())
}

fn create(target: &Target, args: &[OsString]) -> Result<()> {
    let size = terminal::size().unwrap_or((80, 24));
    let mut c = Create {
        account: None,
        program: Program::Login,
        cwd: Vec::new(),
        cols: size.0,
        rows: size.1,
    };
    let mut worktree = None;
    let mut prompt: Option<String> = None;
    let mut overrides = krill_proto::agent::LaunchOverrides::default();
    let mut json = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--worktree") if worktree.is_none() => {
                worktree = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(usage)?
                        .to_owned(),
                );
            }
            Some("--cwd") => {
                let given = args.next().ok_or_else(usage)?;
                if target.is_remote() {
                    // 원격 경로는 원격 호스트 기준이다. 로컬에서 canonicalize하면 안 된다.
                    let bytes = given.as_bytes().to_vec();
                    if !bytes.starts_with(b"/") {
                        return Err(remote_usage(
                            "remote --cwd must be an absolute path on the remote host",
                        ));
                    }
                    c.cwd = bytes;
                } else {
                    c.cwd = std::fs::canonicalize(given)
                        .map_err(fail)?
                        .as_os_str()
                        .as_bytes()
                        .to_vec();
                }
            }
            Some("--cols") => c.cols = number(args.next())?,
            Some("--rows") => c.rows = number(args.next())?,
            Some("--account") if c.account.is_none() => {
                c.account = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(usage)?
                        .to_owned(),
                );
            }
            Some("--prompt") if prompt.is_none() => {
                prompt = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .ok_or_else(usage)?
                        .to_owned(),
                );
            }
            Some("--model") if overrides.model.is_none() => {
                overrides.model = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(usage)?
                        .to_owned(),
                );
            }
            Some("--effort") if overrides.effort.is_none() => {
                overrides.effort = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(usage)?
                        .to_owned(),
                );
            }
            Some("--json") => json = true,
            Some("--agent") if c.program == Program::Login => {
                c.program = Program::Agent {
                    id: args
                        .next()
                        .and_then(|s| s.to_str())
                        .ok_or_else(usage)?
                        .to_owned(),
                }
            }
            Some("--") if c.program == Program::Login => {
                let program = args.next().ok_or_else(usage)?.as_bytes().to_vec();
                validate_custom_program(&program)?;
                c.program = Program::Custom {
                    program,
                    args: args.map(|s| s.as_bytes().to_vec()).collect(),
                };
                break;
            }
            _ => return Err(usage()),
        }
    }
    if !overrides.is_empty() {
        let Program::Agent { ref mut id } = c.program else {
            return Err(usage());
        };
        if !target.is_remote()
            && let Ok(agents) = load_local_agents(daemon::state_dir().as_deref())
        {
            let (canon, profile) = agents
                .resolve(id)
                .ok_or_else(|| fail(format!("unknown agent profile '{id}'")))?;
            krill_proto::agent::validate_launch_overrides(canon, profile, &overrides)
                .map_err(fail)?;
        }
        *id = krill_proto::agent::encode_agent_program_id(id, &overrides).map_err(fail)?;
    }
    if c.cols > screen::MAX_COLS || c.rows > screen::MAX_LINES {
        return Err(usage());
    }
    if target.is_remote() {
        if c.cwd.is_empty() {
            return Err(remote_usage(
                "remote new requires --cwd <absolute path on the remote host>",
            ));
        }
    } else if c.cwd.is_empty() {
        c.cwd = std::env::current_dir()
            .map_err(fail)?
            .as_os_str()
            .as_bytes()
            .to_vec();
    }
    if let Some(name) = worktree {
        // --cwd selects the repository; the daemon returns the actual path after name collision handling.
        c.cwd = crate::worktree::create_on_target(
            target,
            NewWorktree {
                repo: c.cwd,
                name,
                base: None,
                setup: Setup::Ask,
            },
        )
        .map_err(fail)?
        .path;
    }
    let cwd_str = String::from_utf8_lossy(&c.cwd).into_owned();
    let is_agent = matches!(c.program, Program::Agent { .. });
    let mut conn = connect(target, FEAT_CONTROL)?;
    // 데몬 spawn 실패(ENOENT 등)가 프로그램명 없이 그대로 나오면 무엇이 없는지 알 수 없다(C3).
    let program_label = match &c.program {
        Program::Custom { program, .. } => Some(String::from_utf8_lossy(program).into_owned()),
        _ => None,
    };
    let reply = request(&mut conn.stream, &Request::Create(c))
        .map_err(|e| prefix_create_error(program_label.as_deref(), e))?;
    drop(conn);
    match reply {
        Reply::Created { session, pid } => {
            if let Some(prompt_text) = prompt.filter(|s| !s.is_empty()) {
                if is_agent {
                    wait_for_spawned_agent_ready(target, session.0, BOOT_TIMEOUT).map_err(fail)?;
                }
                send_session_line(target, session.0, &prompt_text).map_err(fail)?;
            }
            if json {
                let out = serde_json::to_string_pretty(&serde_json::json!({
                    "session": session.0,
                    "pid": pid,
                    "cwd": cwd_str,
                }))
                .map_err(fail)?;
                println!("{out}");
            } else {
                println!("{}", session.0);
            }
            Ok(())
        }
        _ => Err(fail("unexpected create reply")),
    }
}

struct ForkArgs {
    from: SessionId,
    cwd: Vec<u8>,
    cols: u16,
    rows: u16,
    prompt: Option<String>,
    json: bool,
}

fn parse_fork_args(
    args: &[OsString],
    remote: bool,
    env_pane: Option<OsString>,
    default_size: (u16, u16),
) -> Result<ForkArgs> {
    let mut from: Option<SessionId> = None;
    let mut cwd = Vec::new();
    let mut cols = default_size.0;
    let mut rows = default_size.1;
    let mut prompt: Option<String> = None;
    let mut json = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--from") if from.is_none() => {
                from = Some(id(iter.next().ok_or_else(usage)?)?);
            }
            Some("--cwd") => {
                let given = iter.next().ok_or_else(usage)?;
                if remote {
                    let bytes = given.as_bytes().to_vec();
                    if !bytes.starts_with(b"/") {
                        return Err(remote_usage(
                            "remote --cwd must be an absolute path on the remote host",
                        ));
                    }
                    cwd = bytes;
                } else {
                    cwd = std::fs::canonicalize(given)
                        .map_err(fail)?
                        .as_os_str()
                        .as_bytes()
                        .to_vec();
                }
            }
            Some("--cols") => cols = number(iter.next())?,
            Some("--rows") => rows = number(iter.next())?,
            Some("--prompt") if prompt.is_none() => {
                prompt = Some(
                    iter.next()
                        .and_then(|s| s.to_str())
                        .ok_or_else(usage)?
                        .to_owned(),
                );
            }
            Some("--json") => json = true,
            Some(s) if !s.starts_with('-') && from.is_none() => {
                from = Some(id(arg)?);
            }
            _ => return Err(usage()),
        }
    }
    if cols > screen::MAX_COLS || rows > screen::MAX_LINES {
        return Err(usage());
    }
    let from = match from {
        Some(sid) => sid,
        None => id(&env_pane.ok_or_else(usage)?)?,
    };
    Ok(ForkArgs {
        from,
        cwd,
        cols,
        rows,
        prompt,
        json,
    })
}

fn fork_on_stream(
    features: u64,
    stream: &mut UnixStream,
    parsed: &ForkArgs,
) -> Result<(SessionId, u32)> {
    let required = FEAT_CONTROL | FEAT_SESSION_FORK;
    if features & required != required {
        return Err(fail(
            "krilld does not support this command; update the daemon",
        ));
    }
    let reply = request(
        stream,
        &Request::Create(Create {
            account: None,
            program: Program::Agent {
                id: format!("fork:{}", parsed.from.0),
            },
            cwd: parsed.cwd.clone(),
            cols: parsed.cols,
            rows: parsed.rows,
        }),
    )?;
    let Reply::Created { session, pid } = reply else {
        return Err(fail("unexpected fork reply"));
    };
    Ok((session, pid))
}

fn fork(target: &Target, args: &[OsString]) -> Result<()> {
    let size = terminal::size().unwrap_or((80, 24));
    let parsed = parse_fork_args(
        args,
        target.is_remote(),
        std::env::var_os("MK_PANE_ID"),
        size,
    )?;
    let mut conn = connect(target, FEAT_CONTROL | FEAT_SESSION_FORK)?;
    let (session, pid) = fork_on_stream(conn.negotiated.features, &mut conn.stream, &parsed)?;
    drop(conn);
    if let Some(prompt) = parsed.prompt.filter(|s| !s.is_empty()) {
        wait_for_spawned_agent_ready(target, session.0, BOOT_TIMEOUT).map_err(fail)?;
        send_session_line(target, session.0, &prompt).map_err(fail)?;
    }
    if parsed.json {
        let out = serde_json::to_string_pretty(&serde_json::json!({
            "session": session.0,
            "from": parsed.from.0,
            "pid": pid,
        }))
        .map_err(fail)?;
        println!("{out}");
    } else {
        println!("{}", session.0);
    }
    Ok(())
}

fn number(value: Option<&OsString>) -> Result<u16> {
    value
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(usage)
}

/// `--` 뒤 프로그램명 검증. 빈 값이나 `--` 단독은 오타가 확실하므로 usage(종료 2)로 돌린다(C3).
fn validate_custom_program(program: &[u8]) -> Result<()> {
    if program.is_empty() || program == b"--" {
        return Err(usage());
    }
    Ok(())
}

/// 세션 생성 데몬 오류 앞에 프로그램 주체를 붙인다(C3). 로그인·에이전트 기본값은 그대로 둔다.
fn prefix_create_error(label: Option<&str>, e: Error) -> Error {
    match label {
        Some(name) => fail(format!("program '{name}': {}", e.text)),
        None => e,
    }
}

fn capture(
    c: &mut UnixStream,
    session: SessionId,
    format: CaptureFormat,
) -> Result<(Seq, Vec<u8>)> {
    match request(c, &Request::Capture { session, format })? {
        Reply::Captured { seq, bytes } if bytes.len() <= krill_proto::control::MAX_CAPTURE => {
            Ok((seq, bytes))
        }
        _ => Err(fail("unexpected capture reply")),
    }
}

fn read(target: &Target, args: &[OsString]) -> Result<()> {
    let (session, rest) = args.split_first().ok_or_else(usage)?;
    let session = id(session)?;
    let mut lines_limit: Option<usize> = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--lines") if lines_limit.is_none() => {
                let n = it
                    .next()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(usage)?;
                lines_limit = Some(n);
            }
            _ => return Err(usage()),
        }
    }
    let mut c = connect(target, FEAT_CONTROL | FEAT_CLI)?;
    let (_, bytes) = capture(&mut c.stream, session, CaptureFormat::Plain)?;
    let output = if let Some(limit) = lines_limit {
        let text = String::from_utf8_lossy(&bytes);
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(limit);
        let mut s = all.get(start..).unwrap_or_default().join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        s.into_bytes()
    } else {
        bytes
    };
    match io::stdout().lock().write_all(&output) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(fail(e)),
    }
}

fn raw(target: &Target, session: SessionId, from: Seq) -> Result<Connection> {
    let mut c = connect(
        target,
        screen::FEAT_SCREEN | input::FEAT_INPUT | krill_proto::roles::FEAT_ROLES,
    )?;
    c.stream
        .write_all(
            &Attach {
                session,
                stream: Stream::Raw { from },
            }
            .encode()
            .map_err(fail)?,
        )
        .map_err(fail)?;
    let (kind, body) = frame(&mut c.stream)?;
    if kind == screen::kind::REFUSED {
        return Err(fail(RAW_REFUSED_MSG));
    }
    Attached::decode(kind, &body).map_err(fail)?;
    Ok(c)
}

fn send(target: &Target, args: &[OsString]) -> Result<()> {
    let (session, args) = args.split_first().ok_or_else(usage)?;
    let session = id(session)?;
    let mut enter = false;
    let mut wait_submit = false;
    let mut stdin = false;
    let mut text = Vec::new();
    let mut literal = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if !literal && arg == "--enter" {
            enter = true;
        } else if !literal && arg == "--wait-submit" {
            wait_submit = true;
        } else if !literal && arg == "--stdin" {
            stdin = true;
        } else if !literal && arg == "--text" {
            let val = it.next().ok_or_else(usage)?;
            if !text.is_empty() {
                text.push(b' ');
            }
            text.extend_from_slice(val.as_bytes());
        } else if !literal && arg == "--" {
            literal = true;
        } else {
            if !literal && arg.as_bytes().starts_with(b"--") {
                return Err(usage());
            }
            if !text.is_empty() {
                text.push(b' ');
            }
            text.extend_from_slice(arg.as_bytes());
        }
    }
    if stdin {
        if !text.is_empty() {
            return Err(usage());
        }
        io::stdin()
            .take(u64::try_from(MAX_INPUT + 1).map_err(fail)?)
            .read_to_end(&mut text)
            .map_err(fail)?;
    }
    let total_bytes = text.len().saturating_add(usize::from(enter));
    if total_bytes == 0 || total_bytes > MAX_INPUT {
        return Err(fail("send requires 1..=65536 bytes (including --enter)"));
    }
    let mut ctrl = connect(target, FEAT_CONTROL | FEAT_CLI)?;
    match request(
        &mut ctrl.stream,
        &Request::SendInput {
            session,
            bytes: text.clone(),
            submit: enter,
            wait_submit,
        },
    ) {
        Ok(Reply::InputSent) => return Ok(()),
        Ok(_) => return Err(fail("unexpected send reply")),
        Err(e) if e.text == "bad request" => {}
        Err(e) => return Err(e),
    }
    if enter {
        text.push(b'\r');
    }
    let (seq, _) = capture(&mut ctrl.stream, session, CaptureFormat::Plain)?;
    drop(ctrl);
    let mut c = raw(target, session, seq)?;
    c.stream
        .write_all(
            &Input {
                id: InputId(1),
                bytes: text,
            }
            .encode()
            .map_err(fail)?,
        )
        .map_err(fail)?;
    loop {
        let (kind, body) = frame(&mut c.stream).map_err(|_| fail("send disconnected before acknowledgement; delivery uncertain, do not automatically retry"))?;
        if kind == input::kind::INPUT_ACK {
            let ack = InputAck::decode(kind, &body).map_err(fail)?;
            return if ack.id == InputId(1) && ack.result == InputResult::Applied {
                Ok(())
            } else {
                Err(fail("session closed before input was applied"))
            };
        }
        if kind == screen::kind::REFUSED {
            return Err(fail("input refused"));
        }
        // CLOSED can precede the input ACK when the session exits concurrently.
    }
}

pub(crate) fn state_name(s: AgentState) -> &'static str {
    match s {
        AgentState::Working => "working",
        AgentState::Waiting => "waiting",
        AgentState::Blocked => "blocked",
        AgentState::Done => "done",
        AgentState::Exited => "exited",
        AgentState::Unknown => "unknown",
    }
}

pub(crate) fn load_local_agents(
    state_dir: Option<&std::path::Path>,
) -> std::result::Result<krill_proto::agent::Agents, String> {
    let file = state_dir.map(|d| d.join(krill_proto::agent::FILE_NAME));
    let user = match file.as_deref().map(std::fs::read_to_string) {
        Some(Ok(text)) => Some(text),
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e.to_string());
        }
        _ => None,
    };
    krill_proto::agent::Agents::load(user.as_deref()).map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
pub(crate) struct ObservedSession {
    pub id: u64,
    pub exit_code: Option<i32>,
    pub agent_state: Option<AgentState>,
}

/// 데몬을 새로 띄우지 않고(`Target::dial`) 현재 세션 목록과 훅 상태를 조회한다.
/// 소켓이 없거나 연결에 실패하면 `None`을 돌려준다.
pub(crate) fn observe_sessions_on_target(target: &Target) -> Option<Vec<ObservedSession>> {
    let mut c = target
        .dial(&daemon::client_hello(ClientRole::Cli, "krill"), IO_TIMEOUT)
        .ok()?;
    if c.negotiated.features & FEAT_CONTROL == 0 {
        return None;
    }
    c.stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
    c.stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;
    let Reply::Sessions(sessions) = request(&mut c.stream, &Request::List).ok()? else {
        return None;
    };
    let states = if c.negotiated.features & FEAT_HOOK != 0 {
        match request(&mut c.stream, &Request::States) {
            Ok(Reply::States(s)) => s,
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    Some(
        sessions
            .into_iter()
            .map(|s| {
                let agent_state = states
                    .iter()
                    .find(|v| v.session == s.session)
                    .map(|v| v.status.state);
                ObservedSession {
                    id: s.session.0,
                    exit_code: s.status,
                    agent_state,
                }
            })
            .collect(),
    )
}

pub(crate) fn list_session_ids() -> std::result::Result<Vec<u64>, String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    list_session_ids_on_target(&target)
}

pub(crate) fn list_session_ids_on_target(target: &Target) -> std::result::Result<Vec<u64>, String> {
    let mut c = connect(target, FEAT_CONTROL).map_err(|e| e.text)?;
    let Reply::Sessions(sessions) = request(&mut c.stream, &Request::List).map_err(|e| e.text)?
    else {
        return Err("unexpected session list reply".to_owned());
    };
    Ok(sessions.into_iter().map(|s| s.session.0).collect())
}

pub(crate) fn capture_session_text(session_id: u64) -> std::result::Result<String, String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    capture_session_text_on_target(&target, session_id)
}

pub(crate) fn capture_session_text_on_target(
    target: &Target,
    session_id: u64,
) -> std::result::Result<String, String> {
    let mut c = connect(target, FEAT_CONTROL | FEAT_CLI).map_err(|e| e.text)?;
    let (_, bytes) =
        capture(&mut c.stream, SessionId(session_id), CaptureFormat::Plain).map_err(|e| e.text)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn spawn_agent_session(
    agent: &str,
    cwd: &std::path::Path,
) -> std::result::Result<u64, String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    spawn_agent_session_on_target(&target, agent, None, cwd)
}

pub(crate) fn spawn_agent_session_on_target(
    target: &Target,
    agent: &str,
    account: Option<String>,
    cwd: &std::path::Path,
) -> std::result::Result<u64, String> {
    spawn_agent_session_with_overrides_on_target(
        target,
        agent,
        &krill_proto::agent::LaunchOverrides::default(),
        account,
        cwd,
    )
}

pub(crate) fn spawn_agent_session_with_overrides_on_target(
    target: &Target,
    agent: &str,
    overrides: &krill_proto::agent::LaunchOverrides,
    account: Option<String>,
    cwd: &std::path::Path,
) -> std::result::Result<u64, String> {
    let program = if agent.is_empty()
        || agent.eq_ignore_ascii_case("shell")
        || agent.eq_ignore_ascii_case("login")
    {
        if !overrides.is_empty() {
            return Err("--model/--effort requires an agent profile, not shell/login".to_owned());
        }
        Program::Login
    } else {
        let id = krill_proto::agent::encode_agent_program_id(agent, overrides)?;
        Program::Agent { id }
    };
    let mut conn = connect(target, FEAT_CONTROL).map_err(|e| e.text)?;
    let req = Request::Create(Create {
        account,
        program,
        cwd: cwd.as_os_str().as_bytes().to_vec(),
        cols: 120,
        rows: 36,
    });
    match request(&mut conn.stream, &req).map_err(|e| e.text)? {
        Reply::Created { session, .. } => Ok(session.0),
        _ => Err("unexpected create reply".to_owned()),
    }
}

/// 새로 띄운 에이전트 세션이 셸 초기화(`/bin/zsh -l -i -c ...`)를 마치고 프롬프트 대기 상태에 도달할 때까지 기다린다.
/// 부팅 중 종료(실행 파일 없음 등)하거나 trust·권한 메뉴에 멈추면 오류다 — 그 위에 프롬프트를 치면 메뉴 입력이 된다.
// ponytail: 시간 초과면 계속 진행한다(스피너만 도는 TUI도 입력은 받는다). 잃는 사례가 나오면 오류로 바꾼다.
pub(crate) fn wait_for_spawned_agent_ready(
    target: &Target,
    session_id: u64,
    timeout: Duration,
) -> std::result::Result<(), String> {
    let Ok(mut conn) = connect(
        target,
        FEAT_CONTROL | FEAT_CLI | krill_proto::control::FEAT_WAIT_STATUS,
    ) else {
        std::thread::sleep(Duration::from_millis(1500));
        return Ok(());
    };
    let rules = krill_proto::agent::rules::ScreenRules::default();
    let start = std::time::Instant::now();
    let sid = SessionId(session_id);
    while start.elapsed() < timeout {
        if let Ok(Reply::WaitStatus { state, quiet_ms }) =
            request(&mut conn.stream, &Request::WaitStatus { session: sid })
        {
            let screen = capture(&mut conn.stream, sid, CaptureFormat::Plain)
                .map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            if let Some(verdict) = boot_verdict(state, &screen, quiet_ms, start.elapsed(), &rules) {
                std::thread::sleep(Duration::from_millis(150));
                return verdict.map_err(|e| format!("agent session {session_id} {e}"));
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

/// 부팅 대기 한 번의 판정. `None`이면 계속 기다린다.
fn boot_verdict(
    state: AgentState,
    screen: &str,
    quiet_ms: u64,
    elapsed: Duration,
    rules: &krill_proto::agent::rules::ScreenRules,
) -> Option<std::result::Result<(), &'static str>> {
    const BLOCKED: &str =
        "is blocked on a trust/permission prompt; answer it, then resend the prompt";
    match state {
        AgentState::Exited => return Some(Err("exited during startup")),
        AgentState::Blocked => return Some(Err(BLOCKED)),
        AgentState::Done | AgentState::Waiting => return Some(Ok(())),
        AgentState::Working | AgentState::Unknown => {}
    }
    if rules.menu_hit(screen) == Some(krill_proto::agent::rules::MenuHit::Blocked) {
        return Some(Err(BLOCKED));
    }
    let non_empty: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let ready = non_empty.iter().rev().take(4).any(|l| rules.is_prompt(l))
        || (!non_empty.is_empty() && quiet_ms >= 1200 && elapsed >= Duration::from_millis(1500));
    ready.then_some(Ok(()))
}

pub(crate) fn send_session_line(
    target: &Target,
    session_id: u64,
    line: &str,
) -> std::result::Result<(), String> {
    if line.is_empty() {
        let enter_args = [
            OsString::from(session_id.to_string()),
            OsString::from("--enter"),
        ];
        return send(target, &enter_args).map_err(|e| e.text);
    }
    let args = [
        OsString::from(session_id.to_string()),
        OsString::from("--enter"),
        OsString::from("--"),
        OsString::from(line),
    ];
    send(target, &args).map_err(|e| e.text)
}

pub(crate) fn close_session(session_id: u64) -> std::result::Result<(), String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    close_session_on_target(&target, session_id)
}

pub(crate) fn close_session_on_target(
    target: &Target,
    session_id: u64,
) -> std::result::Result<(), String> {
    close_and_wait(target, SessionId(session_id)).map_err(|e| e.text)
}

fn kill(target: &Target, args: &[OsString]) -> Result<()> {
    let [raw] = args else {
        return Err(usage());
    };
    if raw == "--help" || raw == "-h" {
        return Err(usage());
    }
    let session = id(raw)?;
    close_and_wait(target, session)?;
    println!("killed session {}", session.0);
    Ok(())
}

fn close_and_wait(target: &Target, session: SessionId) -> Result<()> {
    let mut conn = connect(target, FEAT_CONTROL)?;
    match request(&mut conn.stream, &Request::Close { session })? {
        Reply::Closed => {}
        _ => return Err(fail("unexpected close reply")),
    }
    // Closed acknowledges termination; krilld removes the session after holder's Exited event.
    // Wait for that removal so a successful kill/worker-stop cannot leave a visible live session.
    let started = std::time::Instant::now();
    loop {
        let Reply::Sessions(sessions) = request(&mut conn.stream, &Request::List)? else {
            return Err(fail("unexpected session list reply"));
        };
        if !sessions.iter().any(|info| info.session == session) {
            return Ok(());
        }
        if started.elapsed() >= CLOSE_TIMEOUT {
            return Err(fail(format!(
                "session {} did not close within timeout",
                session.0
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn resolve_session_cwd(id: u64) -> io::Result<std::path::PathBuf> {
    let target = Target::establish(None)?;
    let required = FEAT_CONTROL | FEAT_SIDEBAR;
    let mut c = target.dial(&daemon::client_hello(ClientRole::Cli, "krill"), IO_TIMEOUT)?;
    if c.negotiated.features & required != required {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "krilld does not support sidebar query",
        ));
    }
    let Reply::Sidebar(sidebar) =
        request(&mut c.stream, &Request::Sidebar).map_err(|e| io::Error::other(e.text))?
    else {
        return Err(io::Error::other("unexpected sidebar reply"));
    };
    let Some(entry) = sidebar
        .sessions
        .into_iter()
        .find(|s| s.session == SessionId(id))
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("session {id} not found"),
        ));
    };
    if entry.cwd.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("session {id} has no cwd"),
        ));
    }
    Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
        &entry.cwd,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::agent::rules::ScreenRules;

    #[test]
    fn lone_separator_is_usage_and_create_error_names_program() {
        assert!(validate_custom_program(b"/bin/sleep").is_ok());
        for bad in [b"".as_slice(), b"--".as_slice()] {
            let err = validate_custom_program(bad).unwrap_err();
            assert_eq!(err.code, 2, "program={bad:?}");
        }
        let daemon_err = Error {
            code: 1,
            text: "No such file or directory (os error 2)".to_owned(),
        };
        let named = prefix_create_error(Some("--"), daemon_err);
        assert_eq!(named.code, 1);
        assert!(named.text.contains("program '--'"), "{}", named.text);
        assert!(named.text.contains("No such file"), "{}", named.text);
        let plain = prefix_create_error(
            None,
            Error {
                code: 1,
                text: "boom".to_owned(),
            },
        );
        assert_eq!(plain.text, "boom");
    }

    #[test]
    fn holder_loss_warning_only_when_orphan() {
        assert!(holder_loss_warning(false).is_none());
        let warning = holder_loss_warning(true).unwrap();
        assert!(warning.contains("holder"), "{warning}");
        assert!(warning.contains("krilld"), "{warning}");
    }

    #[test]
    fn refused_attach_mentions_race_close_first() {
        assert!(RAW_REFUSED_MSG.contains("closed"), "{RAW_REFUSED_MSG}");
        assert!(
            RAW_REFUSED_MSG.contains("permission denied"),
            "{RAW_REFUSED_MSG}"
        );
    }

    #[test]
    fn boot_verdict_waits_for_prompt_and_rejects_exit_or_trust_menu() {
        let rules = ScreenRules::default();
        let early = Duration::from_millis(100);
        let verdict = |state, screen, quiet_ms, elapsed| {
            boot_verdict(state, screen, quiet_ms, elapsed, &rules).map(|r| r.is_ok())
        };
        assert_eq!(verdict(AgentState::Unknown, "", 0, early), None);
        assert_eq!(
            verdict(AgentState::Working, "Loading agy...", 0, early),
            None
        );
        assert_eq!(
            verdict(AgentState::Unknown, "Antigravity CLI\n>\n", 0, early),
            Some(true)
        );
        assert_eq!(verdict(AgentState::Done, "", 0, early), Some(true));
        assert_eq!(verdict(AgentState::Exited, "", 0, early), Some(false));
        assert_eq!(verdict(AgentState::Blocked, "", 0, early), Some(false));
        assert_eq!(
            verdict(
                AgentState::Unknown,
                "Do you trust the contents of this project?\n> Yes, I trust this folder",
                5000,
                early
            ),
            Some(false)
        );
        // 프롬프트 모양을 몰라도 화면이 조용해지면 준비된 것으로 본다.
        assert_eq!(
            verdict(
                AgentState::Working,
                "banner",
                1200,
                Duration::from_millis(1500)
            ),
            Some(true)
        );
    }

    #[test]
    fn parse_fork_args_supports_positional_flag_and_env_fallback() {
        let args = [
            OsString::from("s7"),
            OsString::from("--prompt"),
            OsString::from("continue here"),
            OsString::from("--json"),
        ];
        let parsed = parse_fork_args(&args, false, None, (80, 24)).unwrap();
        assert_eq!(parsed.from, SessionId(7));
        assert_eq!(parsed.prompt.as_deref(), Some("continue here"));
        assert!(parsed.json);

        let flag_args = [OsString::from("--from"), OsString::from("12")];
        let parsed_flag = parse_fork_args(&flag_args, false, None, (100, 30)).unwrap();
        assert_eq!(parsed_flag.from, SessionId(12));
        assert_eq!((parsed_flag.cols, parsed_flag.rows), (100, 30));

        let env_parsed = parse_fork_args(&[], false, Some(OsString::from("9")), (80, 24)).unwrap();
        assert_eq!(env_parsed.from, SessionId(9));

        assert!(parse_fork_args(&[], false, None, (80, 24)).is_err());
    }

    #[test]
    fn help_flag_detection_respects_literal_separator() {
        let help =
            |s: &[&str]| has_help_before_literal(&s.iter().map(OsString::from).collect::<Vec<_>>());
        assert!(is_help_flag(&OsString::from("--help")));
        assert!(is_help_flag(&OsString::from("-h")));
        assert!(is_help_flag(&OsString::from("help")));
        assert!(!is_help_flag(&OsString::from("--hel")));
        assert!(help(&["send", "--help"]));
        assert!(help(&["ls", "--help"]));
        assert!(help(&["--help"]));
        assert!(!help(&["send", "--", "--help"]));
        assert!(!help(&["send", "1", "--", "--help"]));
        // 세션 ID·전송 텍스트 자리의 `help`는 도움말이 아니다.
        assert!(!help(&["send", "1", "help"]));
        assert!(!help(&["send", "1", "--text", "--help"]));
        // 없는 서브커맨드 뒤 `--help`도 usage를 보여준다(오타 탐색 친화적).
        assert!(help(&["search-is-not-a-session-cmd", "--help"]));
        // `read 1 --lines --help`에서 --help는 --lines 값 자리가 아니라 오인 방지를 위해 도움말이 아니다.
        assert!(!help(&["read", "1", "--lines", "--help"]));
    }

    #[test]
    fn fork_on_stream_refuses_unnegotiated_daemon_without_sending_frame() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let parsed = ForkArgs {
            from: SessionId(7),
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
            prompt: None,
            json: false,
        };
        let err = fork_on_stream(FEAT_CONTROL, &mut client, &parsed).unwrap_err();
        assert_eq!(err.code, 1);
        assert!(err.text.contains("update the daemon"), "{}", err.text);
        drop(client);
        assert!(ipc::read_frame(&mut server).unwrap().is_none());
    }
}
