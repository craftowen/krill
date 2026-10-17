//! MK-047: daemon-backed wait conditions. idle is derived, never an agent state.

use super::{Error, Result, capture, fail, id, request, state_name, usage};
use std::ffi::OsString;
use std::time::{Duration, Instant};

use crate::remote::Target;
use krill_proto::control::{
    AgentState, CaptureFormat, FEAT_CLI, FEAT_CONTROL, FEAT_WAIT_STATUS, Reply, Request,
};
use krill_proto::daemon::{self, Connection};
use krill_proto::ipc::{ClientHello, ClientRole};
use regex::Regex;

const POLL: Duration = Duration::from_millis(100);
const DEFAULT_QUIET: Duration = Duration::from_secs(3);

#[derive(Debug)]
enum Condition {
    State(AgentState),
    Idle,
    Quiet,
    Pattern,
}

impl Condition {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "idle" => Self::Idle,
            "quiet" => Self::Quiet,
            "pattern" => Self::Pattern,
            "working" => Self::State(AgentState::Working),
            "waiting" => Self::State(AgentState::Waiting),
            "blocked" => Self::State(AgentState::Blocked),
            "done" => Self::State(AgentState::Done),
            "exited" => Self::State(AgentState::Exited),
            "unknown" => Self::State(AgentState::Unknown),
            _ => return None,
        })
    }

    fn name(&self) -> &'static str {
        match self {
            Self::State(s) => state_name(*s),
            Self::Idle => "idle",
            Self::Quiet => "quiet",
            Self::Pattern => "pattern",
        }
    }
}

#[derive(Debug)]
struct Options {
    condition: Condition,
    quiet: Duration,
    pattern: Option<Regex>,
    timeout: Option<Duration>,
}

impl Options {
    fn parse(mut args: &[OsString]) -> Result<Self> {
        let mut condition = Condition::State(AgentState::Exited);
        let mut quiet = None;
        let mut pattern = None;
        let mut timeout = None;
        let mut for_seen = false;
        while !args.is_empty() {
            let [flag, value, rest @ ..] = args else {
                return Err(usage());
            };
            let value = value.to_str().ok_or_else(usage)?;
            match flag.to_str() {
                Some("--for") if !for_seen => {
                    for_seen = true;
                    if let Some(text) = value.strip_prefix("pattern:") {
                        if pattern.replace(text.to_owned()).is_some() {
                            return Err(usage());
                        }
                        condition = Condition::Pattern;
                    } else if let Some(ms) = value.strip_prefix("quiet:") {
                        if quiet.replace(milliseconds(ms)?).is_some() {
                            return Err(usage());
                        }
                        condition = Condition::Quiet;
                    } else {
                        condition = Condition::parse(value).ok_or_else(usage)?;
                    }
                }
                Some("--pattern") if pattern.is_none() => pattern = Some(value.to_owned()),
                Some("--quiet-ms" | "--idle-ms") if quiet.is_none() => {
                    quiet = Some(milliseconds(value)?);
                }
                Some("--timeout") if timeout.is_none() => {
                    let seconds = value
                        .strip_suffix('s')
                        .unwrap_or(value)
                        .parse::<f64>()
                        .map_err(|_| usage())?;
                    timeout = Some(Duration::try_from_secs_f64(seconds).map_err(|_| usage())?);
                }
                _ => return Err(usage()),
            }
            args = rest;
        }
        if matches!(condition, Condition::Pattern) != pattern.is_some()
            || (quiet.is_some() && !matches!(condition, Condition::Quiet | Condition::Idle))
        {
            return Err(usage());
        }
        let pattern = pattern
            .map(|text| {
                if text.is_empty() {
                    return Err(usage());
                }
                Regex::new(&text).map_err(|e| Error {
                    code: 2,
                    text: format!("invalid wait pattern: {e}"),
                })
            })
            .transpose()?;
        Ok(Self {
            condition,
            quiet: quiet.unwrap_or(DEFAULT_QUIET),
            pattern,
            timeout,
        })
    }
}

fn milliseconds(value: &str) -> Result<Duration> {
    value
        .parse::<u64>()
        .map(Duration::from_millis)
        .map_err(|_| usage())
}

fn timed_out() -> Error {
    Error {
        code: 2,
        text: "wait timed out".to_owned(),
    }
}

/// Bound Hello for an already running daemon as well as the later status reads.
/// Remote targets never launch a local daemon; a failed dial stays failed.
fn assured(target: &Target, hello: &ClientHello, required: u64) -> Result<Connection> {
    let c = target.connect(hello).map_err(fail)?;
    if c.negotiated.features & required != required {
        return Err(fail(
            "krilld does not support this command; update the daemon",
        ));
    }
    c.stream
        .set_read_timeout(Some(super::IO_TIMEOUT))
        .map_err(fail)?;
    c.stream
        .set_write_timeout(Some(super::IO_TIMEOUT))
        .map_err(fail)?;
    Ok(c)
}

fn connection(
    target: &Target,
    required: u64,
    start: Instant,
    limit: Option<Duration>,
) -> Result<Connection> {
    let hello = daemon::client_hello(ClientRole::Cli, "krill wait");
    let result = if let Some(limit) = limit {
        match target.dial(&hello, limit.min(super::IO_TIMEOUT)) {
            Ok(c) if c.negotiated.features & required == required => Ok(c),
            Ok(_) => Err(fail(
                "krilld does not support this command; update the daemon",
            )),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                assured(target, &hello, required)
            }
            Err(e) => Err(fail(e)),
        }
    } else {
        assured(target, &hello, required)
    };
    if limit.is_some_and(|t| start.elapsed() >= t) {
        return Err(timed_out());
    }
    result
}

fn done_since(
    stream: &mut std::os::unix::net::UnixStream,
    session: krill_proto::SessionId,
) -> Result<Option<u64>> {
    let Reply::States(states) = request(stream, &Request::States)? else {
        return Err(fail("unexpected states reply"));
    };
    Ok(states
        .into_iter()
        .find(|s| s.session == session && s.status.state == AgentState::Done)
        .map(|s| s.status.since_unix_ns))
}

pub(super) fn run(args: &[OsString], target: &Target) -> Result<()> {
    let (session, args) = args.split_first().ok_or_else(usage)?;
    let session = id(session)?;
    let options = Options::parse(args)?;
    let start = Instant::now();
    let started_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let mut initial_done: Option<Option<u64>> = None;
    // A zero timeout is an immediate condition probe, preserving MK-045 behavior.
    let limit = options.timeout.filter(|t| !t.is_zero());
    let mut c = connection(
        target,
        FEAT_CONTROL
            | FEAT_WAIT_STATUS
            | if options.pattern.is_some() {
                FEAT_CLI
            } else {
                0
            },
        start,
        limit,
    )?;
    loop {
        if let Some(t) = limit {
            let remaining = t.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(timed_out());
            }
            let io = remaining.min(super::IO_TIMEOUT);
            c.stream.set_read_timeout(Some(io)).map_err(fail)?;
            c.stream.set_write_timeout(Some(io)).map_err(fail)?;
        }
        let matched = (|| {
            if let Some(pattern) = &options.pattern {
                let (_, bytes) = capture(&mut c.stream, session, CaptureFormat::Plain)?;
                return Ok(pattern.is_match(&String::from_utf8_lossy(&bytes)));
            }
            let Reply::WaitStatus { state, quiet_ms } =
                request(&mut c.stream, &Request::WaitStatus { session })?
            else {
                return Err(fail("unexpected wait status reply"));
            };
            let quiet = Duration::from_millis(quiet_ms) >= options.quiet;
            Ok(match options.condition {
                Condition::State(AgentState::Done) => {
                    if state == AgentState::Done {
                        let since = done_since(&mut c.stream, session)?;
                        let baseline = *initial_done
                            .get_or_insert_with(|| since.filter(|&ns| ns <= started_ns));
                        since.is_some_and(|ns| ns >= started_ns && Some(ns) != baseline)
                    } else {
                        initial_done = Some(None);
                        false
                    }
                }
                Condition::State(target) => state == target,
                Condition::Idle => {
                    quiet && !matches!(state, AgentState::Working | AgentState::Unknown)
                }
                Condition::Quiet => quiet,
                Condition::Pattern => false, // handled by the capture path above
            })
        })();
        if limit.is_some_and(|t| start.elapsed() >= t) {
            return Err(timed_out());
        }
        if matched? {
            println!("{}\t{}", session.0, options.condition.name());
            return Ok(());
        }
        if options.timeout.is_some_and(|t| start.elapsed() >= t) {
            return Err(timed_out());
        }
        std::thread::sleep(
            options
                .timeout
                .map_or(POLL, |t| POLL.min(t.saturating_sub(start.elapsed()))),
        );
    }
}
