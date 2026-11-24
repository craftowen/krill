//! Pipito Local Bridge v1 HTTP/1.1 클라이언트(`std::net::TcpStream` 전용).
//!
//! `bridge.json`을 호출마다 동적으로 읽어 Pipito 재기동·포트 변경을 재시작 없이 감지한다.
//! `Origin` 헤더를 절대 붙이지 않고 `Authorization: Bearer <token>`만 127.0.0.1로 전송한다.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use krill_proto::voice::VoiceConnectionStatus;
use serde::{Deserialize, Serialize};

const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);
const WARM_TIMEOUT: Duration = Duration::from_millis(800);
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(20);
const LEARN_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_HTTP_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const BRIDGE_PATH_ENV: &str = "KRILL_PIPITO_BRIDGE_PATH";

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeDiscovery {
    pub version: u32,
    #[serde(default)]
    pub pid: u32,
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub app_version: Option<String>,
    #[serde(default)]
    pub engine_ready: bool,
    #[serde(default)]
    pub engine_error: Option<String>,
    #[serde(default)]
    pub refine_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscribePayload<'a> {
    pub audio_wav_b64: &'a str,
    pub refine: bool,
    pub mode: &'a str,
    pub target_lang: Option<&'a str>,
    pub vocab: &'a [String],
    pub surrounding: &'a str,
    pub app: &'a str,
    pub app_category: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranscribeResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub silence: bool,
    #[serde(default)]
    pub raw: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub refined: bool,
    #[serde(default)]
    pub refine_error: Option<String>,
    #[serde(default)]
    pub asr_ms: u64,
    #[serde(default)]
    pub refine_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearnResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub learned: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub status: VoiceConnectionStatus,
    pub app_version: Option<String>,
    pub engine_ready: bool,
    pub engine_error: Option<String>,
    pub refine_available: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BridgeClient {
    override_path: Option<PathBuf>,
}

impl BridgeClient {
    #[must_use]
    pub fn new(override_path: Option<PathBuf>) -> Self {
        Self { override_path }
    }

    fn effective_override(&self) -> Option<PathBuf> {
        if let Some(path) = &self.override_path {
            return Some(path.clone());
        }
        std::env::var_os(BRIDGE_PATH_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }

    fn bridge_path(&self) -> Option<PathBuf> {
        if let Some(path) = self.effective_override() {
            return Some(path);
        }
        default_bridge_path()
    }

    fn read_discovery(&self) -> io::Result<BridgeDiscovery> {
        let path = self
            .bridge_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "bridge path unavailable"))?;
        let raw = fs::read_to_string(&path)?;
        let info: BridgeDiscovery = serde_json::from_str(&raw)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid bridge.json"))?;
        if info.version != 1 || info.port == 0 || info.token.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported bridge.json schema",
            ));
        }
        Ok(info)
    }

    fn detect_installed(&self) -> bool {
        if let Some(override_path) = self.effective_override() {
            // 테스트 환경에서는 호스트의 실제 /Applications/Pipito.app을 보지 않고 지정 경로 존재 여부만 본다.
            return override_path.exists();
        }
        if let Some(path) = default_bridge_path() {
            if path.exists() {
                return true;
            }
            if path.parent().is_some_and(Path::exists) {
                return true;
            }
        }
        pipito_app_installed()
    }

    pub fn health(&self) -> ProbeOutcome {
        let Ok(bridge) = self.read_discovery() else {
            let status = if self.detect_installed() {
                VoiceConnectionStatus::NotRunning
            } else {
                VoiceConnectionStatus::NotInstalled
            };
            return ProbeOutcome {
                status,
                app_version: None,
                engine_ready: false,
                engine_error: None,
                refine_available: false,
            };
        };

        match http_json_request(
            &bridge,
            "GET",
            "/local/health",
            None,
            HEALTH_TIMEOUT,
            HEALTH_TIMEOUT,
        ) {
            Ok((200, body)) => match serde_json::from_slice::<HealthResponse>(&body) {
                Ok(resp) if resp.ok => ProbeOutcome {
                    status: if resp.engine_ready {
                        VoiceConnectionStatus::Ready
                    } else {
                        VoiceConnectionStatus::Loading
                    },
                    app_version: resp.app_version,
                    engine_ready: resp.engine_ready,
                    engine_error: resp.engine_error,
                    refine_available: resp.refine_available,
                },
                _ => ProbeOutcome {
                    status: VoiceConnectionStatus::NotRunning,
                    app_version: None,
                    engine_ready: false,
                    engine_error: Some("Invalid Pipito health response".to_owned()),
                    refine_available: false,
                },
            },
            _ => ProbeOutcome {
                status: VoiceConnectionStatus::NotRunning,
                app_version: None,
                engine_ready: false,
                engine_error: None,
                refine_available: false,
            },
        }
    }

    pub fn warm(&self) -> io::Result<bool> {
        let bridge = self.read_discovery()?;
        let (status, body) = http_json_request(
            &bridge,
            "POST",
            "/local/warm",
            Some(b"{}"),
            WARM_TIMEOUT,
            WARM_TIMEOUT,
        )?;
        if status != 200 {
            return Ok(false);
        }
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        Ok(parsed
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false))
    }

    pub fn transcribe(&self, payload: &TranscribePayload<'_>) -> io::Result<TranscribeResponse> {
        let bridge = self.read_discovery().map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                if self.detect_installed() {
                    "Pipito is not running"
                } else {
                    "Pipito is not installed"
                },
            )
        })?;
        let req_bytes = serde_json::to_vec(payload)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "serialize error"))?;
        let (status, body) = http_json_request(
            &bridge,
            "POST",
            "/local/transcribe",
            Some(&req_bytes),
            HEALTH_TIMEOUT,
            TRANSCRIBE_TIMEOUT,
        )?;
        if status == 401 || status == 403 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Pipito bridge token rejected",
            ));
        }
        let resp: TranscribeResponse = serde_json::from_slice(&body).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid Pipito transcribe response (HTTP {status})"),
            )
        })?;
        if status != 200 || !resp.ok {
            let msg = resp
                .error
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("Pipito transcribe failed (HTTP {status})"));
            return Err(io::Error::other(msg));
        }
        Ok(resp)
    }

    pub fn learn(&self, before: &str, after: &str) -> io::Result<Vec<(String, String)>> {
        let bridge = self.read_discovery()?;
        let body = serde_json::json!({
            "before": before,
            "after": after,
            "app": "Krill",
        });
        let req_bytes = serde_json::to_vec(&body)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "serialize error"))?;
        let (status, resp_bytes) = http_json_request(
            &bridge,
            "POST",
            "/local/learn",
            Some(&req_bytes),
            HEALTH_TIMEOUT,
            LEARN_TIMEOUT,
        )?;
        if status != 200 {
            return Err(io::Error::other(format!(
                "Pipito learn failed (HTTP {status})"
            )));
        }
        let parsed: LearnResponse = serde_json::from_slice(&resp_bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid learn response"))?;
        Ok(parsed.learned)
    }
}

fn default_bridge_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Pipito")
                .join("bridge.json"),
        )
    }
    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join("Pipito").join("bridge.json"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(xdg).join("Pipito").join("bridge.json"));
        }
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("Pipito")
                .join("bridge.json"),
        )
    }
}

fn pipito_app_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        if Path::new("/Applications/Pipito.app").exists() {
            return true;
        }
        if let Some(home) = std::env::var_os("HOME")
            && PathBuf::from(home)
                .join("Applications")
                .join("Pipito.app")
                .exists()
        {
            return true;
        }
        false
    }
    #[cfg(windows)]
    {
        for env_key in ["LOCALAPPDATA", "ProgramFiles"] {
            if let Some(base) = std::env::var_os(env_key)
                && PathBuf::from(base).join("Pipito").exists()
            {
                return true;
            }
        }
        false
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        false
    }
}

fn http_json_request(
    bridge: &BridgeDiscovery,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> io::Result<(u16, Vec<u8>)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], bridge.port));
    let mut stream = TcpStream::connect_timeout(&addr, connect_timeout)?;
    stream.set_read_timeout(Some(read_timeout))?;
    stream.set_write_timeout(Some(read_timeout))?;

    // 주의: Origin 헤더를 절대 포함하지 않는다.
    let mut header = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n",
        bridge.port, bridge.token
    );
    if let Some(payload) = body {
        use std::fmt::Write as _;
        header.push_str("Content-Type: application/json\r\n");
        writeln!(header, "Content-Length: {}\r", payload.len()).ok();
    }
    header.push_str("\r\n");

    stream.write_all(header.as_bytes())?;
    if let Some(payload) = body {
        stream.write_all(payload)?;
    }
    stream.flush()?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len().saturating_add(n) > MAX_HTTP_RESPONSE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "HTTP response exceeds size cap",
                    ));
                }
                if let Some(slice) = buf.get(..n) {
                    raw.extend_from_slice(slice);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }

    parse_http_response(&raw)
}

fn parse_http_response(raw: &[u8]) -> io::Result<(u16, Vec<u8>)> {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed HTTP response"))?;
    let head_bytes = raw
        .get(..sep)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "header slice failed"))?;
    let body_bytes = raw
        .get(sep.saturating_add(4)..)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "body slice failed"))?;
    let head = std::str::from_utf8(head_bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 HTTP header"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty HTTP status"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP status code"))?;

    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            if key == "transfer-encoding" && val.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if key == "content-length" {
                content_length = val.parse::<usize>().ok();
            }
        }
    }

    let body = if chunked {
        decode_chunked(body_bytes)?
    } else if let Some(len) = content_length {
        body_bytes
            .get(..len.min(body_bytes.len()))
            .unwrap_or(body_bytes)
            .to_vec()
    } else {
        body_bytes.to_vec()
    };
    Ok((status_code, body))
}

fn decode_chunked(mut data: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(pos) = data.windows(2).position(|w| w == b"\r\n") {
        let size_bytes = data
            .get(..pos)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "chunk size slice"))?;
        let size_str = std::str::from_utf8(size_bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk size utf8"))?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk size hex"))?;
        data = data.get(pos.saturating_add(2)..).unwrap_or(&[]);
        if size == 0 {
            break;
        }
        let chunk = data
            .get(..size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated chunk"))?;
        out.extend_from_slice(chunk);
        data = data.get(size.saturating_add(2)..).unwrap_or(&[]);
    }
    Ok(out)
}

/// 표준 RFC 4648 Base64 인코더(외부 의존성 없음).
#[must_use]
pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3).saturating_mul(4));
    for chunk in input.chunks(3) {
        let b0 = u32::from(*chunk.first().unwrap_or(&0));
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(
            *TABLE.get(((triple >> 18) & 0x3f) as usize).unwrap_or(&b'A'),
        ));
        out.push(char::from(
            *TABLE.get(((triple >> 12) & 0x3f) as usize).unwrap_or(&b'A'),
        ));
        if chunk.len() > 1 {
            out.push(char::from(
                *TABLE.get(((triple >> 6) & 0x3f) as usize).unwrap_or(&b'A'),
            ));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(
                *TABLE.get((triple & 0x3f) as usize).unwrap_or(&b'A'),
            ));
        } else {
            out.push('=');
        }
    }
    out
}

/// 표준 RFC 4648 Base64 디코더(공백 무시).
pub fn base64_decode(input: &str) -> io::Result<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if clean.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for quad in clean.chunks(4) {
        let Some(&q0) = quad.first() else {
            break;
        };
        let Some(&q1) = quad.get(1) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid base64 length",
            ));
        };
        let v0 = val(q0).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad base64"))?;
        let v1 = val(q1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad base64"))?;
        out.push((v0 << 2) | (v1 >> 4));
        if let Some(&q2) = quad.get(2)
            && q2 != b'='
        {
            let v2 =
                val(q2).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad base64"))?;
            out.push((v1 << 4) | (v2 >> 2));
            if let Some(&q3) = quad.get(3)
                && q3 != b'='
            {
                let v3 = val(q3)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad base64"))?;
                out.push((v2 << 6) | v3);
            }
        }
    }
    Ok(out)
}
