//! `krill hook <agent>`(MK-038): 에이전트 hook이 부른다. 무슨 일이 있어도 stdout에 `{}`를 먼저 내고 0으로 끝난다
//! (fail-open). 시작부터 [`BUDGET`] 안에 stdin JSON을 krilld에 넘기고, 못 넘기면 spool에 붙인다. 와이어·spool
//! 포맷은 [`krill_proto::hook`].

use std::io::{self, Write as _};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::daemon::{self, Paths};
use krill_proto::hook::{self, EventId, FEAT_HOOK, HookEvent, Input, MAX_PAYLOAD};
use krill_proto::ipc::{self, ClientHello, ClientRole};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;

/// `main` 진입부터 끝까지의 예산(재시도·spool 포함). ARCHITECTURE §6의 50ms는 에이전트가 보는 wall-clock이라
/// exec·종료와 macOS 타이머 지연(20ms 대기가 21.7~30ms에 깸) 몫으로 15ms를 남긴다(docs/spikes/MK-038.md).
const BUDGET: Duration = Duration::from_millis(35);
/// stdin이 닫히기를 기다리는 시간(시작부터). 넘으면 payload 없이 보낸다. 에이전트는 JSON을 쓰고 바로 닫는다.
const STDIN_WAIT: Duration = Duration::from_millis(10);
/// 소켓 시도 몫. 남은 5ms는 spool 쓰기 몫이다(flock 경합이 없으면 1ms 안).
const SEND_BUDGET: Duration = BUDGET.saturating_sub(Duration::from_millis(5));
/// 연결 시도 횟수와 그 사이 쉬는 시간. krilld 재기동 중이면 거절당할 수 있다.
const ATTEMPTS: u32 = 3;
const RETRY_PAUSE: Duration = Duration::from_millis(3);

/// krilld와 핸드셰이크가 끝난 뒤 프레임 송신·ACK 수신에 보장하는 최소 여유 시간(OWE-533).
const ACK_GRACE: Duration = Duration::from_millis(5);

/// `agent`가 없어도 `{}`를 내고 0으로 끝난다(hook 설정 실수로 에이전트를 막지 않는다).
pub fn run(agent: Option<&str>, event_arg: Option<&str>) -> ExitCode {
    let start = Instant::now();
    let event_hint = event_arg
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("MK_HOOK_EVENT")
                .ok()
                .filter(|s| !s.is_empty())
        });
    let is_agy = agent.is_some_and(|a| matches!(a, "agy" | "antigravity"));
    // 빈 출력이면 Claude 권한 hook이 fail-closed된다(T4 §에이전트, Orca hook-script.ts:64).
    // Antigravity(`agy`)는 PreToolUse에서 decision 필드가 빠진 `{}`를 받으면 도구를 거부하므로
    // `{"decision":"ask"}`를 돌려 기본 권한 흐름을 유지한다.
    let response: &[u8] = if is_agy {
        match event_hint.as_deref() {
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
    let Some(agent) = agent else {
        eprintln!("krill hook: usage: krill hook <agent> [event]");
        return ExitCode::SUCCESS;
    };
    let agent = krill_proto::agent::canonical_id(agent);
    let (mut input, mut payload) = read_stdin(Instant::now() + STDIN_WAIT);
    if let Some(ev) = event_hint.as_deref() {
        if payload.iter().all(u8::is_ascii_whitespace) {
            payload = serde_json::json!({ "hook_event_name": ev })
                .to_string()
                .into_bytes();
            input = Input::Complete;
        } else if let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(&payload)
            && let Some(obj) = val.as_object_mut()
            && !obj.contains_key("hook_event_name")
            && !obj.contains_key("hookEventName")
            && !obj.contains_key("event")
        {
            obj.insert(
                "hook_event_name".to_owned(),
                serde_json::Value::String(ev.to_owned()),
            );
            if let Ok(bytes) = serde_json::to_vec(&val)
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
        agent: agent.to_owned(),
        ppid: std::os::unix::process::parent_id(),
        input,
        payload,
    };
    let frame = match event.encode() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("krill hook: event dropped: {e}");
            return ExitCode::SUCCESS;
        }
    };
    let paths = match Paths::dev() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("krill hook: event dropped: {e}");
            return ExitCode::SUCCESS;
        }
    };
    // stdin이 열린 채 STDIN_WAIT를 다 쓴 경우에만 전체 시작 시각 기준 예산을 유지해 50ms 안에 끝낸다.
    // 정상 입력(Complete·Truncated)은 앞선 파이프 복사·선점 시간 때문에 소켓 예산을 잃지 않게 한다(OWE-533).
    let send_deadline = if input == Input::TimedOut {
        start + SEND_BUDGET
    } else {
        Instant::now() + SEND_BUDGET
    };
    if deliver(&paths.socket, &frame, send_deadline).is_err() {
        let spool = daemon::state_dir().map(|d| d.join(hook::SPOOL_FILE));
        let spool_deadline = (start + BUDGET).max(Instant::now() + ACK_GRACE);
        if let Some(Err(e)) = spool.map(|p| hook::append_spool(&p, &frame, spool_deadline)) {
            eprintln!("krill hook: event dropped: {e}");
        }
    }
    ExitCode::SUCCESS
}

/// stdin을 `deadline`까지 EOF까지 읽는다. [`MAX_PAYLOAD`]를 넘으면 앞부분만, 안 닫히면 빈 payload.
fn read_stdin(deadline: Instant) -> (Input, Vec<u8>) {
    let (input, buf) = read_input(io::stdin(), deadline);
    if input == Input::Truncated {
        // 나머지는 버리며 끝까지 읽는다(에이전트의 쓰기가 EPIPE로 실패하지 않게). 프로세스가 끝나면 같이 끝난다.
        thread::Builder::new()
            .name("stdin".into())
            .spawn(|| {
                io::copy(&mut io::stdin(), &mut io::sink()).ok();
            })
            .ok();
    }
    (input, buf)
}

/// `fd`를 main 스레드에서 `poll`로 기다려 EOF까지 읽는다. 스케줄러 선점으로 `deadline`이 지났어도 커널 버퍼에 이미
/// 도착한 입력과 EOF는 비차단 `poll(0)`으로 끝까지 수거한다(OWE-533).
fn read_input(fd: impl AsFd, deadline: Instant) -> (Input, Vec<u8>) {
    let fd = fd.as_fd();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        if !poll_in(fd, wait) && (wait.is_zero() || !poll_in(fd, Duration::ZERO)) {
            return (Input::TimedOut, Vec::new());
        }
        match rustix::io::read(fd, &mut chunk) {
            Ok(0) => return (Input::Complete, buf),
            Ok(n) => {
                if let Some(slice) = chunk.get(..n) {
                    buf.extend_from_slice(slice);
                }
                if buf.len() > MAX_PAYLOAD {
                    buf.truncate(MAX_PAYLOAD);
                    return (Input::Truncated, buf);
                }
            }
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) if Instant::now() < deadline => {}
            Err(Errno::AGAIN) => return (Input::TimedOut, Vec::new()),
            Err(_) => return (Input::Complete, buf),
        }
    }
}

fn poll_in(fd: BorrowedFd<'_>, timeout: Duration) -> bool {
    let until = (!timeout.is_zero()).then(|| Instant::now() + timeout);
    loop {
        let wait = until.map_or(Duration::ZERO, |d| {
            d.saturating_duration_since(Instant::now())
        });
        let ts = Timespec {
            tv_sec: i64::try_from(wait.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(wait.subsec_nanos()),
        };
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        match poll(&mut fds, Some(&ts)) {
            Ok(n) => return n > 0,
            Err(Errno::INTR) => {
                if until.is_none() {
                    return false;
                }
            }
            Err(_) => return true,
        }
    }
}

/// krilld에 붙어 `frame`을 보내고 ACK까지 받는다. 데몬은 띄우지 않는다. 거절·끊김은 [`ATTEMPTS`]번까지 다시 한다.
fn deliver(socket: &Path, frame: &[u8], deadline: Instant) -> io::Result<()> {
    let hello = ClientHello {
        features: FEAT_HOOK,
        ..daemon::client_hello(ClientRole::Hook, "krill hook")
    };
    let mut attempt = 1;
    loop {
        match send_once(socket, &hello, frame, deadline) {
            Err(e) if attempt < ATTEMPTS && retryable(&e) => {
                thread::sleep(RETRY_PAUSE.min(left(deadline)?));
                attempt += 1;
            }
            r => return r,
        }
    }
}

fn send_once(
    socket: &Path,
    hello: &ClientHello,
    frame: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    let c = daemon::handshake(socket, hello, left(deadline)?)?;
    if c.negotiated.features & FEAT_HOOK == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "krilld does not take hook events",
        ));
    }
    let mut s = c.stream;
    write_frame(&mut s, frame, deadline)?;
    wait_ack(&mut s, deadline)
}

fn write_frame(s: &mut UnixStream, frame: &[u8], deadline: Instant) -> io::Result<()> {
    s.set_write_timeout(Some(left(deadline).unwrap_or(ACK_GRACE)))
        .ok();
    s.write_all(frame).map_err(timeout_if_would_block)
}

fn wait_ack(s: &mut UnixStream, deadline: Instant) -> io::Result<()> {
    // 피어가 ACK 직후 먼저 닫았으면 macOS setsockopt는 EINVAL을 낸다(OWE-487). 닫힌 소켓은 막히지 않으므로 무시한다.
    s.set_read_timeout(Some(left(deadline).unwrap_or(ACK_GRACE)))
        .ok();
    match read_ack_frame(s) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) => {}
        r => return r,
    }
    if !poll_in(s.as_fd(), Duration::ZERO) {
        return Err(io::ErrorKind::TimedOut.into());
    }
    s.set_nonblocking(true).ok();
    read_ack_frame(s).map_err(timeout_if_would_block)
}

fn read_ack_frame(s: &mut UnixStream) -> io::Result<()> {
    match ipc::read_frame(s)? {
        Some((hook::kind::ACK, _)) => Ok(()),
        Some((k, _)) => Err(ipc::Error::UnexpectedKind {
            want: hook::kind::ACK,
            got: k,
        }
        .into()),
        None => Err(io::ErrorKind::UnexpectedEof.into()),
    }
}

fn timeout_if_would_block(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::WouldBlock {
        io::ErrorKind::TimedOut.into()
    } else {
        e
    }
}

/// krilld가 재기동 중이거나 막 죽었을 때 나는 오류. 소켓이 아예 없거나(데몬 없음) 시간이 다 된 경우는 다시 하지 않는다.
fn retryable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof
    )
}

/// `deadline`까지 남은 시간. 다 됐으면 `TimedOut`(0을 타임아웃으로 주면 std가 거부한다).
fn left(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::ErrorKind::TimedOut.into())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    use krill_proto::hook::{self, Input, MAX_PAYLOAD};

    use super::{read_input, wait_ack, write_frame};

    /// OWE-533: 파이프 버퍼에 이미 JSON과 EOF가 도착해 있으면, 스케줄러 선점으로 `deadline`이 지난 뒤에
    /// 읽더라도 `TimedOut`으로 버리지 않고 수거해야 한다.
    #[test]
    fn buffered_stdin_with_eof_is_collected_after_deadline() {
        let (rx, mut tx) = std::io::pipe().unwrap();
        let body = br#"{"hook_event_name":"Stop","session_id":"s1"}"#;
        tx.write_all(body).unwrap();
        drop(tx);
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(20))
            .unwrap();
        let (input, payload) = read_input(rx, expired);
        assert_eq!(input, Input::Complete);
        assert_eq!(payload, body);
    }

    /// OWE-533: EOF 없이 열린 채 마감이 지나면 빈 payload와 `TimedOut`을 돌려준다.
    #[test]
    fn open_stdin_without_eof_times_out_at_deadline() {
        let (rx, mut tx) = std::io::pipe().unwrap();
        tx.write_all(b"partial").unwrap();
        let (input, payload) = read_input(rx, Instant::now() + Duration::from_millis(5));
        assert_eq!(input, Input::TimedOut);
        assert!(payload.is_empty());
        drop(tx);
    }

    /// OWE-533: `MAX_PAYLOAD`를 넘는 입력은 EOF를 기다리지 않고 앞부분만 잘라 `Truncated`로 돌려준다.
    #[test]
    fn oversize_stdin_truncates_without_eof() {
        let (rx, mut tx) = std::io::pipe().unwrap();
        let writer = std::thread::spawn(move || {
            let chunk = vec![b'a'; 16 * 1024];
            for _ in 0..=(MAX_PAYLOAD / chunk.len()) {
                if tx.write_all(&chunk).is_err() {
                    break;
                }
            }
            tx
        });
        let (input, payload) = read_input(rx, Instant::now() + Duration::from_secs(1));
        assert_eq!(input, Input::Truncated);
        assert_eq!(payload.len(), MAX_PAYLOAD);
        drop(writer.join().unwrap());
    }

    /// OWE-533: 핸드셰이크 직후 `deadline`이 지났어도 커널 송신 버퍼에 여유가 있으면 비차단으로 프레임을 보낸다.
    #[test]
    fn write_frame_succeeds_nonblocking_after_deadline() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(20))
            .unwrap();
        write_frame(&mut client, b"frame", expired).unwrap();
        drop(client);
        let mut got = Vec::new();
        server.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"frame");
    }

    /// OWE-533: `write_all` 직후 선점되어 `deadline`이 지났어도 그사이 krilld가 소켓 수신 버퍼에 넣어 둔 ACK는
    /// 비차단 확인으로 수거해야 한다(불필요한 spool 중복 기록 방지).
    #[test]
    fn buffered_ack_is_collected_after_deadline() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.write_all(&hook::ack()).unwrap();
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(20))
            .unwrap();
        wait_ack(&mut client, expired).unwrap();
    }

    /// OWE-533 / OWE-487: krilld가 ACK를 쓰자마자 연결을 닫으면 macOS `setsockopt(SO_RCVTIMEO)`는 EINVAL을
    /// 낸다. `wait_ack`이 `set_read_timeout` 실패로 이미 도착한 ACK를 버리면 안 된다.
    #[test]
    fn ack_survives_peer_closing_before_read_timeout() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.write_all(&hook::ack()).unwrap();
        drop(server);
        wait_ack(&mut client, Instant::now() + Duration::from_secs(1)).unwrap();
    }

    /// ACK가 오지 않고 마감되면 `TimedOut`으로 실패한다.
    #[test]
    fn missing_ack_times_out() {
        let (mut client, _server) = UnixStream::pair().unwrap();
        let err = wait_ack(&mut client, Instant::now() + Duration::from_millis(5)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }
}
