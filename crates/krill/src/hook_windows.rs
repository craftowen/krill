//! Native Windows hook delivery: bounded stdin, `AF_UNIX` + ACK, spool fallback.
//! No shell or FFI is needed. Parent PID uses the locked sysinfo safe API.

use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::daemon;
use krill_proto::hook::{self, EventId, FEAT_HOOK, HookEvent, Input, MAX_PAYLOAD};
use krill_proto::ipc::{self, ClientRole};

const BUDGET: Duration = Duration::from_millis(35);
// Windows thread startup and scheduling can exceed the Unix 10ms input budget.
// Start the first-byte wait after spawn returns; a received byte gets its own
// bounded EOF window. Normal closed pipes complete immediately.
const FIRST_BYTE_WAIT: Duration = Duration::from_millis(500);
const EOF_WAIT: Duration = Duration::from_millis(100);
const ACK_GRACE: Duration = Duration::from_millis(5);

pub fn run(agent: Option<&str>, event_arg: Option<&str>) -> ExitCode {
    let start = Instant::now();
    let event = event_arg
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("MK_HOOK_EVENT")
                .ok()
                .filter(|s| !s.is_empty())
        });
    let response: &[u8] = if agent.is_some_and(|a| matches!(a, "agy" | "antigravity")) {
        match event.as_deref() {
            Some("PreToolUse") => b"{\"decision\":\"ask\"}\n",
            Some("Stop") => b"{\"decision\":\"\"}\n",
            _ => b"{}\n",
        }
    } else {
        b"{}\n"
    };
    let mut out = io::stdout().lock();
    out.write_all(response).and_then(|()| out.flush()).ok();
    drop(out);
    // Persistent exec-form hooks may run outside Krill. Keep those invocations
    // fail-open and silent without reading stdin, process metadata or state files.
    let Some(agent) =
        agent.filter(|_| std::env::var_os("MK_PANE_ID").is_some_and(|v| !v.is_empty()))
    else {
        return ExitCode::SUCCESS;
    };
    if let Err(error) = collect_and_deliver(agent, event.as_deref(), start) {
        eprintln!("krill hook: event dropped: {error}");
    }
    ExitCode::SUCCESS
}

fn parent_pid() -> u32 {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(std::process::id());
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), ProcessRefreshKind::new());
    system
        .process(pid)
        .and_then(sysinfo::Process::parent)
        .map_or(0, Pid::as_u32)
}

enum StdinRead {
    Started,
    Finished(Input, Vec<u8>),
}

fn read_stdin() -> (Input, Vec<u8>) {
    let (tx, rx) = mpsc::sync_channel(2);
    if thread::Builder::new()
        .name("hook-stdin".into())
        .spawn(move || {
            let mut input = io::stdin().lock();
            let mut first = [0];
            if input.read(&mut first).ok() != Some(1) {
                tx.send(StdinRead::Finished(Input::Complete, Vec::new()))
                    .ok();
                return;
            }
            if tx.send(StdinRead::Started).is_err() {
                return;
            }
            let mut bytes = first.to_vec();
            let result = input
                .by_ref()
                .take(u64::try_from(MAX_PAYLOAD).unwrap_or(u64::MAX))
                .read_to_end(&mut bytes);
            let truncated = bytes.len() > MAX_PAYLOAD;
            bytes.truncate(MAX_PAYLOAD);
            let kind = if truncated {
                Input::Truncated
            } else {
                Input::Complete
            };
            let finished = if result.is_ok() {
                StdinRead::Finished(kind, bytes)
            } else {
                StdinRead::Finished(Input::Complete, Vec::new())
            };
            tx.send(finished).ok();
            if truncated {
                io::copy(&mut input, &mut io::sink()).ok();
            }
        })
        .is_err()
    {
        return (Input::TimedOut, Vec::new());
    }
    match rx.recv_timeout(FIRST_BYTE_WAIT) {
        Ok(StdinRead::Finished(kind, bytes)) => (kind, bytes),
        Ok(StdinRead::Started) => match rx.recv_timeout(EOF_WAIT) {
            Ok(StdinRead::Finished(kind, bytes)) => (kind, bytes),
            _ => (Input::TimedOut, Vec::new()),
        },
        Err(_) => (Input::TimedOut, Vec::new()),
    }
}

fn collect_and_deliver(agent: &str, hint: Option<&str>, start: Instant) -> io::Result<()> {
    let (mut input, mut payload) = read_stdin();
    if let Some(hint) = hint {
        if payload.iter().all(u8::is_ascii_whitespace) {
            payload = serde_json::json!({"hook_event_name": hint})
                .to_string()
                .into_bytes();
            input = Input::Complete;
        } else if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&payload)
            && let Some(object) = value.as_object_mut()
            && !["hook_event_name", "hookEventName", "event"]
                .iter()
                .any(|key| object.contains_key(*key))
        {
            object.insert("hook_event_name".into(), serde_json::json!(hint));
            if let Ok(bytes) = serde_json::to_vec(&value)
                && bytes.len() <= MAX_PAYLOAD
            {
                payload = bytes;
            }
        }
    }
    let event = HookEvent {
        id: EventId {
            pid: std::process::id(),
            unix_ns: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)),
        },
        agent: krill_proto::agent::canonical_id(agent).to_owned(),
        ppid: parent_pid(),
        input,
        payload,
    };
    let frame = event.encode()?;
    let state = daemon::state_dir().ok_or(io::ErrorKind::NotFound)?;
    // As on Unix, fully received input gets a fresh send window. Thread startup
    // and scheduling can be delayed on Windows before the input waiter begins.
    let deadline = if input == Input::TimedOut {
        start + BUDGET
    } else {
        Instant::now() + BUDGET
    };
    if deliver(
        &state.join("run/krilld.sock"),
        &frame,
        deadline.checked_sub(ACK_GRACE).unwrap_or(deadline),
    )
    .is_err()
    {
        hook::append_spool(
            &state.join(hook::SPOOL_FILE),
            &frame,
            deadline.max(Instant::now() + ACK_GRACE),
        )?;
    }
    Ok(())
}

fn left(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(remaining)
    }
}

fn deliver(socket: &Path, frame: &[u8], deadline: Instant) -> io::Result<()> {
    let hello = daemon::client_hello(ClientRole::Hook, "krill hook");
    let hello = ipc::ClientHello {
        features: FEAT_HOOK,
        ..hello
    };
    let connection = daemon::handshake(socket, &hello, left(deadline)?)?;
    if connection.negotiated.features & FEAT_HOOK == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut stream = connection.stream;
    stream.set_write_timeout(Some(left(deadline)?))?;
    stream.write_all(frame)?;
    stream.set_read_timeout(Some(left(deadline)?))?;
    match ipc::read_frame(&mut stream)? {
        Some((hook::kind::ACK, body)) if body.is_empty() => Ok(()),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}
