//! MK-045: direct daemon commands. Text shown by these commands is kept here.

mod terminal;
mod wait;

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::UnixStream;
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

const USAGE: &str = "usage: krill [--host <user@host>] ls [--json] | krill new [--cwd <path>] [--worktree <name>] [--agent <profile> | -- <program> <args>...] [--cols N --rows N]\n       krill fork [<id>] [--from <id>] [--cwd <path>] [--prompt <text>] [--cols N --rows N] [--json]\n       krill send <id> [--enter] [--text <text>] [--stdin | -- <text>...]\n       krill read <id> [--lines N] | krill wait <id> [--for idle|waiting|blocked|pattern|quiet|exited|working|done|unknown] [--pattern <regex>] [--quiet-ms|--idle-ms N] [--timeout <seconds>]\n       krill kill <id> | krill attach <id> (Ctrl-] detaches; stdin EOF detaches)\n       remote (--host): SSH Unix-socket forwarding to the remote krilld; --cwd must be absolute on the remote host, --worktree is local only";
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
    let target = match Target::establish(host) {
        Ok(target) => target,
        Err(e) => {
            eprintln!("remote: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match rest.split_first() {
        Some((cmd, rest)) if cmd == "ls" => list(&target, rest),
        Some((cmd, rest)) if cmd == "new" => create(&target, rest),
        Some((cmd, rest)) if cmd == "fork" => fork(&target, rest),
        Some((cmd, rest)) if cmd == "send" => send(&target, rest),
        Some((cmd, rest)) if cmd == "read" => read(&target, rest),
        Some((cmd, rest)) if cmd == "wait" => wait::run(rest, &target),
        Some((cmd, rest)) if cmd == "attach" && rest.len() == 1 => {
            id(&rest[0]).and_then(|session| terminal::attach(&target, session))
        }
        Some((cmd, rest)) if cmd == "kill" || cmd == "close" => kill(&target, rest),
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
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else {
            return Err(usage());
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
    if json {
        let rows: Vec<_> = sessions
            .into_iter()
            .map(|s| {
                let state = states
                    .iter()
                    .find(|v| v.session == s.session)
                    .map_or(AgentState::Unknown, |v| v.status.state);
                serde_json::json!({
                    "id": s.session.0,
                    "pid": s.pid,
                    "process": if s.status.is_some() { "exited" } else { "running" },
                    "state": state_name(state),
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
    println!("ID\tPID\tPROCESS\tSTATE");
    for s in sessions {
        let state = states
            .iter()
            .find(|v| v.session == s.session)
            .map_or(AgentState::Unknown, |v| v.status.state);
        println!(
            "{}\t{}\t{}\t{}",
            s.session.0,
            s.pid,
            if s.status.is_some() {
                "exited"
            } else {
                "running"
            },
            state_name(state)
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
    if c.cols > screen::MAX_COLS || c.rows > screen::MAX_LINES {
        return Err(usage());
    }
    if target.is_remote() {
        if worktree.is_some() {
            return Err(remote_usage(
                "remote worktree creation is not supported in this version",
            ));
        }
        if c.cwd.is_empty() {
            return Err(remote_usage(
                "remote new requires --cwd <absolute path on the remote host>",
            ));
        }
    } else {
        if c.cwd.is_empty() {
            c.cwd = std::env::current_dir()
                .map_err(fail)?
                .as_os_str()
                .as_bytes()
                .to_vec();
        }
        if let Some(name) = worktree {
            // --cwd selects the repository; the daemon returns the actual path after name collision handling.
            c.cwd = crate::worktree::create(NewWorktree {
                repo: c.cwd,
                name,
                base: None,
                setup: Setup::Ask,
            })
            .map_err(fail)?
            .path;
        }
    }
    let mut conn = connect(target, FEAT_CONTROL)?;
    // 데몬 spawn 실패(ENOENT 등)가 프로그램명 없이 그대로 나오면 무엇이 없는지 알 수 없다(C3).
    let program_label = match &c.program {
        Program::Custom { program, .. } => Some(String::from_utf8_lossy(program).into_owned()),
        _ => None,
    };
    let reply = request(&mut conn.stream, &Request::Create(c))
        .map_err(|e| prefix_create_error(program_label.as_deref(), e))?;
    match reply {
        Reply::Created { session, .. } => {
            println!("{}", session.0);
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
    if let Some(prompt) = parsed.prompt.filter(|s| !s.is_empty()) {
        wait_for_spawned_agent_ready(session.0, BOOT_TIMEOUT).map_err(fail)?;
        send_session_line(session.0, &prompt).map_err(fail)?;
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
    let mut stdin = false;
    let mut text = Vec::new();
    let mut literal = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if !literal && arg == "--enter" {
            enter = true;
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
    if enter {
        text.push(b'\r');
    }
    if text.is_empty() || text.len() > MAX_INPUT {
        return Err(fail("send requires 1..=65536 bytes (including --enter)"));
    }
    let mut ctrl = connect(target, FEAT_CONTROL | FEAT_CLI)?;
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

fn state_name(s: AgentState) -> &'static str {
    match s {
        AgentState::Working => "working",
        AgentState::Waiting => "waiting",
        AgentState::Blocked => "blocked",
        AgentState::Done => "done",
        AgentState::Exited => "exited",
        AgentState::Unknown => "unknown",
    }
}

pub(crate) fn list_session_ids() -> std::result::Result<Vec<u64>, String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    let mut c = connect(&target, FEAT_CONTROL).map_err(|e| e.text)?;
    let Reply::Sessions(sessions) = request(&mut c.stream, &Request::List).map_err(|e| e.text)?
    else {
        return Err("unexpected session list reply".to_owned());
    };
    Ok(sessions.into_iter().map(|s| s.session.0).collect())
}

pub(crate) fn capture_session_text(session_id: u64) -> std::result::Result<String, String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    let mut c = connect(&target, FEAT_CONTROL | FEAT_CLI).map_err(|e| e.text)?;
    let (_, bytes) =
        capture(&mut c.stream, SessionId(session_id), CaptureFormat::Plain).map_err(|e| e.text)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn spawn_agent_session(
    agent: &str,
    cwd: &std::path::Path,
) -> std::result::Result<u64, String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    let mut conn = connect(&target, FEAT_CONTROL).map_err(|e| e.text)?;
    let program = if agent.is_empty()
        || agent.eq_ignore_ascii_case("shell")
        || agent.eq_ignore_ascii_case("login")
    {
        Program::Login
    } else {
        Program::Agent {
            id: agent.to_owned(),
        }
    };
    let req = Request::Create(Create {
        account: None,
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
    session_id: u64,
    timeout: Duration,
) -> std::result::Result<(), String> {
    let Ok(target) = Target::establish(None) else {
        return Ok(());
    };
    let Ok(mut conn) = connect(
        &target,
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

pub(crate) fn send_session_line(session_id: u64, line: &str) -> std::result::Result<(), String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    if !line.is_empty() {
        let text_args = [
            OsString::from(session_id.to_string()),
            OsString::from("--"),
            OsString::from(line),
        ];
        send(&target, &text_args).map_err(|e| e.text)?;
        std::thread::sleep(Duration::from_millis(50));
    }
    let enter_args = [
        OsString::from(session_id.to_string()),
        OsString::from("--enter"),
    ];
    send(&target, &enter_args).map_err(|e| e.text)
}

pub(crate) fn close_session(session_id: u64) -> std::result::Result<(), String> {
    let target = Target::establish(None).map_err(|e| e.to_string())?;
    close_and_wait(&target, SessionId(session_id)).map_err(|e| e.text)
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
