//! `POST /v1/telemetry`를 OS curl 자식 프로세스로 보낸다. 본문(JSON)은 stdin으로만 넘긴다.
//!
//! `--disable`(사용자 curlrc 무시)이 첫 인자, OS 절대 경로의 curl, HTTPS만(테스트의 명시 루프백 주소만 HTTP),
//! TLS 검증 유지(`-k` 없음), 리디렉션을 따르지 않는다(`-L` 없음). 연결 10초·전체 30초에 벽시계 상한을 한 번 더
//! 두고, 출력은 상한까지만 읽으며, 넘기거나 늦으면 자식을 죽이고 회수한다. 응답 헤더는 버리고 본문과 상태 코드만 쓴다.

use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(super) enum Reply {
    Transport,
    Http { status: u16, body: Vec<u8> },
}

/// curl이 스스로 끝내지 못할 때 쓰는 벽시계 상한(`--max-time` 30초 + 여유).
pub(super) const DEADLINE: Duration = Duration::from_secs(35);
/// 응답 출력 상한.
const MAX_OUTPUT: usize = 4096;
/// URL 상한(바이트).
const MAX_URL: usize = 2048;

/// 검증한 업로드 주소.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Endpoint {
    url: String,
    loopback_http: bool,
}

impl Endpoint {
    /// `https://<host>[:port][/path]`, 또는 테스트용 `http://127.0.0.1|localhost|[::1]`만 받는다.
    /// 자격 증명·쿼리·조각·공백·제어 문자는 거부한다.
    pub(super) fn parse(raw: &str) -> Result<Self, String> {
        let invalid = || "invalid telemetry endpoint".to_owned();
        if raw.len() > MAX_URL
            || raw.chars().any(|c| c.is_control() || c.is_whitespace())
            || raw.contains(['\\', '@', '?', '#', '"'])
        {
            return Err(invalid());
        }
        let (scheme, rest) = raw.split_once("://").ok_or_else(invalid)?;
        let authority = rest.split('/').next().unwrap_or_default();
        let host = if authority.starts_with('[') {
            authority
                .find(']')
                .and_then(|end| authority.get(..=end))
                .unwrap_or_default()
        } else {
            authority.split(':').next().unwrap_or_default()
        };
        if host.is_empty() {
            return Err(invalid());
        }
        let loopback = matches!(
            host.to_ascii_lowercase().as_str(),
            "127.0.0.1" | "localhost" | "[::1]"
        );
        match scheme {
            "https" => Ok(Self {
                url: raw.to_owned(),
                loopback_http: false,
            }),
            "http" if loopback => Ok(Self {
                url: raw.to_owned(),
                loopback_http: true,
            }),
            _ => Err(invalid()),
        }
    }
}

/// OS 기본 curl의 절대 경로(PATH 가로채기 방지). Windows 10 1803+는 `System32\curl.exe`가 있다.
pub(super) fn system_curl() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
            .join("curl.exe")
    } else {
        PathBuf::from("/usr/bin/curl")
    }
}

/// 한 번 POST한다. curl을 못 띄우거나 실패·시간 초과·출력 상한 초과·형식 오류면 [`Reply::Transport`].
pub(super) fn post(curl: &Path, endpoint: &Endpoint, body: &[u8], deadline: Duration) -> Reply {
    match run(curl, &args(endpoint), body, deadline) {
        Ok((true, out)) => parse(&out).unwrap_or(Reply::Transport),
        Ok((false, _)) | Err(_) => Reply::Transport,
    }
}

pub(super) fn args(endpoint: &Endpoint) -> Vec<&str> {
    vec![
        // 사용자 curlrc를 읽지 않는다(첫 인자여야 효과가 있다).
        "--disable",
        "--silent",
        "--globoff",
        "--proto",
        if endpoint.loopback_http {
            "=http"
        } else {
            "=https"
        },
        "--connect-timeout",
        "10",
        "--max-time",
        "30",
        "--header",
        "Content-Type: application/json",
        // 100-continue 왕복을 없앤다.
        "--header",
        "Expect:",
        "--data-binary",
        "@-",
        // 헤더와 본문을 stdout 하나로 받고 마지막 줄에 상태 코드를 붙인다.
        "--dump-header",
        "-",
        "--write-out",
        "\n%{http_code}",
        "--url",
        &endpoint.url,
    ]
}

/// 자식을 띄워 `body`를 stdin에 쓰고 stdout을 상한까지 읽는다. 실패하면 자식을 죽이고 회수한다.
fn run(curl: &Path, args: &[&str], body: &[u8], timeout: Duration) -> io::Result<(bool, Vec<u8>)> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::other("invalid deadline"))?;
    let mut child = krill_proto::os::background_command(curl)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = drive(&mut child, body, deadline);
    if result.is_err() {
        child.kill().ok();
        child.wait().ok();
    }
    result
}

fn drive(child: &mut Child, body: &[u8], deadline: Instant) -> io::Result<(bool, Vec<u8>)> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("curl stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("curl stdout unavailable"))?;
    // 파이프 버퍼보다 큰 본문에서도 막히지 않게 따로 쓰고, 다 쓰면 닫아 curl이 EOF를 본다.
    let body = body.to_vec();
    std::thread::spawn(move || stdin.write_all(&body).ok());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let limit = u64::try_from(MAX_OUTPUT)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let read = stdout.take(limit).read_to_end(&mut out).map(|_| out);
        tx.send(read).ok();
    });
    let out = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "curl timed out"))??;
    if out.len() > MAX_OUTPUT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "curl output exceeded limit",
        ));
    }
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.success(), out));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "curl timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `[헤더 블록…] 본문 \n 상태코드`를 나눈다. 1xx·프록시 CONNECT 블록은 건너뛴다.
pub(super) fn parse(out: &[u8]) -> Option<Reply> {
    let split = out.iter().rposition(|&b| b == b'\n')?;
    let (mut rest, code) = out.split_at(split);
    let status: u16 = std::str::from_utf8(code.get(1..)?)
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if !(100..=599).contains(&status) {
        return None;
    }

    while rest.starts_with(b"HTTP/") {
        loop {
            let newline = rest.iter().position(|&b| b == b'\n')?;
            let (line, tail) = rest.split_at(newline);
            rest = tail.get(1..)?;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                break;
            }
        }
    }
    Some(Reply::Http {
        status,
        body: rest.to_vec(),
    })
}
