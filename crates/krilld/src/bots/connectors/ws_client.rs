// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Zero-`tokio` cross-platform RFC 6455 WebSocket client for Krill Bots connectors.
//!
//! Supports `wss://` (via `crate::bots::http_client::tls_client_config()` + `rustls::StreamOwned`)
//! and plain `ws://` restricted strictly to loopback hosts (`127.0.0.1`, `localhost`, `::1`)
//! for deterministic mock server testing. Adapts RFC 6455 masked framing conventions from
//! `crate::mobile::ws` without touching mobile relay behavior, while supporting fragmented
//! messages and large JSON payloads (Slack Socket Mode envelopes, Discord Gateway `READY`).

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use ring::digest;
use ring::rand::{SecureRandom as _, SystemRandom};

use crate::bots::http_client::tls_client_config;

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
const MAX_WS_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONTROL_PAYLOAD_BYTES: usize = 125;
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_SLICE_MAX: Duration = Duration::from_millis(100);

/// Parsed WebSocket endpoint URL (`wss://` or loopback-only `ws://`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWsUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub host_header: String,
    pub target: String,
}

/// Parses a WebSocket URL (`wss://` for any host, `ws://` strictly for loopback hosts).
pub fn parse_ws_url(raw: &str) -> io::Result<ParsedWsUrl> {
    if raw.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "WebSocket URL contains forbidden control characters",
        ));
    }
    let (tls, rest, default_port) = if let Some(r) = raw.strip_prefix("wss://") {
        (true, r, 443_u16)
    } else if let Some(r) = raw.strip_prefix("ws://") {
        (false, r, 80_u16)
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "WebSocket URL must start with wss:// or ws://",
        ));
    };

    let without_fragment = rest.split('#').next().unwrap_or(rest);
    let authority_end = without_fragment
        .find(['/', '?'])
        .unwrap_or(without_fragment.len());
    let authority = without_fragment.get(..authority_end).unwrap_or_default();
    let tail = without_fragment.get(authority_end..).unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid WebSocket URL authority",
        ));
    }

    let (host, port, host_header) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed
            .find(']')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unterminated IPv6 host"))?;
        let host = bracketed.get(..close).unwrap_or_default().to_owned();
        let after = bracketed.get(close.saturating_add(1)..).unwrap_or_default();
        if host.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty IPv6 host",
            ));
        }
        let (port, host_header) = if after.is_empty() {
            (default_port, format!("[{host}]"))
        } else if let Some(port_str) = after.strip_prefix(':') {
            let port = port_str
                .parse::<u16>()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid URL port"))?;
            (port, format!("[{host}]:{port}"))
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unexpected suffix after IPv6 host",
            ));
        };
        (host, port, host_header)
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        let port = p
            .parse::<u16>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid URL port"))?;
        if h.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty URL host",
            ));
        }
        let header = if port == default_port {
            h.to_owned()
        } else {
            format!("{h}:{port}")
        };
        (h.to_owned(), port, header)
    } else {
        (authority.to_owned(), default_port, authority.to_owned())
    };

    if !tls && !is_loopback_host(&host) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "plaintext ws:// is permitted only for loopback hosts",
        ));
    }

    let target = if tail.is_empty() {
        "/".to_owned()
    } else if tail.starts_with('?') {
        format!("/{tail}")
    } else {
        tail.to_owned()
    };

    Ok(ParsedWsUrl {
        tls,
        host,
        port,
        host_header,
        target,
    })
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// High-level event produced by [`WsClient::poll_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsEvent {
    Text(String),
    Binary(Vec<u8>),
    Pong(Vec<u8>),
    Closed { code: Option<u16>, reason: String },
    Timeout,
}

enum WsTransport {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl WsTransport {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => &s.sock,
        }
    }
}

impl Read for WsTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for WsTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

struct FragmentState {
    opcode: u8,
    data: Vec<u8>,
}

/// Synchronous RFC 6455 WebSocket client with bounded timed polling for responsive shutdown.
pub struct WsClient {
    transport: WsTransport,
    rx_buf: Vec<u8>,
    fragment: Option<FragmentState>,
    closed: bool,
}

impl std::fmt::Debug for WsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsClient")
            .field("rx_buffered", &self.rx_buf.len())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl WsClient {
    /// Connects to a `wss://` (or loopback `ws://`) URL and completes the RFC 6455 upgrade handshake.
    pub fn connect(
        url: &str,
        extra_headers: &[(String, String)],
        timeout: Duration,
    ) -> io::Result<Self> {
        let parsed = parse_ws_url(url)?;
        validate_extra_headers(extra_headers)?;

        let addr = (parsed.host.as_str(), parsed.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::AddrNotAvailable, "DNS resolution empty")
            })?;
        let tcp = TcpStream::connect_timeout(&addr, timeout)?;
        tcp.set_nodelay(true).ok();
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;

        let mut transport = if parsed.tls {
            let cfg = tls_client_config()?;
            let server_name =
                rustls_pki_types::ServerName::try_from(parsed.host.clone()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name")
                })?;
            let conn = rustls::ClientConnection::new(cfg, server_name)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            WsTransport::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
        } else {
            WsTransport::Plain(tcp)
        };

        let mut nonce = [0u8; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| io::Error::other("WebSocket key RNG failure"))?;
        let sec_key = base64(&nonce);

        let mut req = String::with_capacity(256);
        write!(
            req,
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {}\r\nUser-Agent: krilld-bots/1.0\r\n",
            parsed.target, parsed.host_header, sec_key
        )
        .ok();
        for (name, value) in extra_headers {
            write!(req, "{name}: {value}\r\n").ok();
        }
        req.push_str("\r\n");

        transport.write_all(req.as_bytes())?;
        transport.flush()?;

        // Read handshake response while preserving any trailing WebSocket frame bytes.
        let mut raw = Vec::with_capacity(1024);
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            if raw.len() >= MAX_HANDSHAKE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WebSocket handshake response exceeds limit",
                ));
            }
            let n = read_retry(&mut transport, &mut tmp)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF during WebSocket upgrade handshake",
                ));
            }
            raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
        };

        let head_bytes = raw.get(..header_end).unwrap_or_default();
        verify_handshake_response(head_bytes, &sec_key)?;
        let leftover = raw
            .get(header_end.saturating_add(4)..)
            .unwrap_or_default()
            .to_vec();

        transport.tcp().set_write_timeout(Some(WRITE_TIMEOUT)).ok();
        transport.tcp().set_read_timeout(Some(POLL_SLICE_MAX)).ok();

        Ok(Self {
            transport,
            rx_buf: leftover,
            fragment: None,
            closed: false,
        })
    }

    /// Sends a masked RFC 6455 text frame.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.send_frame(OP_TEXT, text.as_bytes())
    }

    /// Sends a masked RFC 6455 binary frame.
    pub fn send_binary(&mut self, data: &[u8]) -> io::Result<()> {
        self.send_frame(OP_BINARY, data)
    }

    /// Sends a masked RFC 6455 ping control frame.
    pub fn send_ping(&mut self, payload: &[u8]) -> io::Result<()> {
        if payload.len() > MAX_CONTROL_PAYLOAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket ping payload exceeds 125 bytes",
            ));
        }
        self.send_frame(OP_PING, payload)
    }

    /// Sends a masked RFC 6455 pong control frame.
    pub fn send_pong(&mut self, payload: &[u8]) -> io::Result<()> {
        if payload.len() > MAX_CONTROL_PAYLOAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket pong payload exceeds 125 bytes",
            ));
        }
        self.send_frame(OP_PONG, payload)
    }

    /// Sends a masked RFC 6455 close control frame with status code and optional reason.
    pub fn send_close(&mut self, code: u16, reason: &str) -> io::Result<()> {
        let mut payload = Vec::with_capacity(2 + reason.len().min(123));
        payload.extend_from_slice(&code.to_be_bytes());
        let mut end = reason.len().min(123);
        while end > 0 && !reason.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        payload.extend_from_slice(reason.as_bytes().get(..end).unwrap_or_default());
        self.closed = true;
        self.send_frame(OP_CLOSE, &payload)
    }

    /// Closes the connection gracefully (best-effort close frame + TCP shutdown).
    pub fn close(mut self) {
        if !self.closed {
            self.send_close(1000, "shutdown").ok();
        }
        self.transport.tcp().shutdown(Shutdown::Both).ok();
    }

    /// Polls for the next complete WebSocket message, returning [`WsEvent::Timeout`] if `timeout`
    /// elapses without completing a frame. Automatically replies to server Ping frames with Pong.
    pub fn poll_event(&mut self, timeout: Duration) -> io::Result<WsEvent> {
        let deadline = Instant::now() + timeout;
        let mut tmp = [0u8; 8192];

        loop {
            while let Some(frame) = try_decode_server_frame(&mut self.rx_buf)? {
                if let Some(event) = self.process_frame(frame)? {
                    return Ok(event);
                }
            }

            let now = Instant::now();
            if now >= deadline {
                return Ok(WsEvent::Timeout);
            }
            let remaining = deadline.saturating_duration_since(now).min(POLL_SLICE_MAX);
            let slice = if remaining.is_zero() {
                Duration::from_millis(1)
            } else {
                remaining
            };
            self.transport.tcp().set_read_timeout(Some(slice)).ok();

            match self.transport.read(&mut tmp) {
                Ok(0) => {
                    self.closed = true;
                    return Ok(WsEvent::Closed {
                        code: None,
                        reason: String::new(),
                    });
                }
                Ok(n) => {
                    if self.rx_buf.len().saturating_add(n) > MAX_WS_MESSAGE_BYTES.saturating_add(64)
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "WebSocket receive buffer exceeded limit",
                        ));
                    }
                    self.rx_buf
                        .extend_from_slice(tmp.get(..n).unwrap_or_default());
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if Instant::now() >= deadline {
                        return Ok(WsEvent::Timeout);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn send_frame(&mut self, opcode: u8, body: &[u8]) -> io::Result<()> {
        let mut mask = [0u8; 4];
        SystemRandom::new()
            .fill(&mut mask)
            .map_err(|_| io::Error::other("WebSocket mask RNG failure"))?;
        let frame = encode_masked(opcode, body, mask)?;
        self.transport.write_all(&frame)?;
        self.transport.flush()
    }

    fn process_frame(&mut self, frame: DecodedFrame) -> io::Result<Option<WsEvent>> {
        match frame.opcode {
            OP_PING => {
                self.send_pong(&frame.payload)?;
                Ok(None)
            }
            OP_PONG => Ok(Some(WsEvent::Pong(frame.payload))),
            OP_CLOSE => {
                let (code, reason) = parse_close_payload(&frame.payload);
                if !self.closed {
                    self.send_close(code.unwrap_or(1000), "").ok();
                }
                Ok(Some(WsEvent::Closed { code, reason }))
            }
            OP_TEXT | OP_BINARY => {
                if self.fragment.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected new data frame while fragmented message in progress",
                    ));
                }
                if frame.fin {
                    finalize_data_message(frame.opcode, frame.payload).map(Some)
                } else {
                    self.fragment = Some(FragmentState {
                        opcode: frame.opcode,
                        data: frame.payload,
                    });
                    Ok(None)
                }
            }
            OP_CONT => {
                let Some(mut state) = self.fragment.take() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected continuation frame without initial data frame",
                    ));
                };
                if state.data.len().saturating_add(frame.payload.len()) > MAX_WS_MESSAGE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "fragmented WebSocket message exceeds size limit",
                    ));
                }
                state.data.extend_from_slice(&frame.payload);
                if frame.fin {
                    finalize_data_message(state.opcode, state.data).map(Some)
                } else {
                    self.fragment = Some(state);
                    Ok(None)
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported WebSocket opcode",
            )),
        }
    }
}

struct DecodedFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

fn try_decode_server_frame(buf: &mut Vec<u8>) -> io::Result<Option<DecodedFrame>> {
    let (Some(&b0), Some(&b1)) = (buf.first(), buf.get(1)) else {
        return Ok(None);
    };
    let fin = (b0 & 0x80) != 0;
    let rsv = b0 & 0x70;
    let opcode = b0 & 0x0f;
    let masked = (b1 & 0x80) != 0;
    if rsv != 0 || masked {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid server WebSocket frame flags or mask bit",
        ));
    }
    let is_control = opcode >= 0x8;
    if is_control && !fin {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fragmented WebSocket control frame",
        ));
    }
    if !matches!(
        opcode,
        OP_CONT | OP_TEXT | OP_BINARY | OP_CLOSE | OP_PING | OP_PONG
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown WebSocket opcode",
        ));
    }

    let small = b1 & 0x7f;
    let (header_len, payload_len) = match small {
        126 => {
            let Some(bytes) = buf.get(2..4) else {
                return Ok(None);
            };
            let arr: [u8; 2] = bytes
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "short length"))?;
            let n = usize::from(u16::from_be_bytes(arr));
            if n < 126 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "non-canonical 16-bit WebSocket length",
                ));
            }
            (4usize, n)
        }
        127 => {
            let Some(bytes) = buf.get(2..10) else {
                return Ok(None);
            };
            let arr: [u8; 8] = bytes
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "short length"))?;
            let raw_u64 = u64::from_be_bytes(arr);
            if raw_u64 < 65_536 || (raw_u64 & (1_u64 << 63)) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid 64-bit WebSocket length",
                ));
            }
            let n = usize::try_from(raw_u64).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "WebSocket length overflow")
            })?;
            (10usize, n)
        }
        n => (2usize, usize::from(n)),
    };

    if is_control && payload_len > MAX_CONTROL_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "WebSocket control frame exceeds 125 bytes",
        ));
    }
    if payload_len > MAX_WS_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "WebSocket frame exceeds maximum message cap",
        ));
    }

    let total = header_len
        .checked_add(payload_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame size overflow"))?;
    if buf.len() < total {
        return Ok(None);
    }

    let payload = buf.get(header_len..total).unwrap_or_default().to_vec();
    buf.drain(..total);
    Ok(Some(DecodedFrame {
        fin,
        opcode,
        payload,
    }))
}

fn finalize_data_message(opcode: u8, payload: Vec<u8>) -> io::Result<WsEvent> {
    if opcode == OP_TEXT {
        let text = String::from_utf8(payload).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "WebSocket text frame not UTF-8")
        })?;
        Ok(WsEvent::Text(text))
    } else {
        Ok(WsEvent::Binary(payload))
    }
}

fn parse_close_payload(payload: &[u8]) -> (Option<u16>, String) {
    if payload.len() >= 2 {
        let code = payload
            .get(..2)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())
            .map(u16::from_be_bytes);
        let reason = String::from_utf8_lossy(payload.get(2..).unwrap_or_default()).into_owned();
        (code, reason)
    } else {
        (None, String::new())
    }
}

fn validate_extra_headers(headers: &[(String, String)]) -> io::Result<()> {
    for (name, value) in headers {
        if name.is_empty()
            || name.bytes().any(|b| b <= 0x20 || b == b':' || b == 0x7f)
            || value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid WebSocket header name or value",
            ));
        }
    }
    Ok(())
}

fn verify_handshake_response(head: &[u8], sec_key: &str) -> io::Result<()> {
    let text = std::str::from_utf8(head).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "non-UTF8 WebSocket handshake response",
        )
    })?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing WebSocket status line")
    })?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    let status = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") || status != "101" {
        let kind = if status == "401" || status == "403" {
            io::ErrorKind::PermissionDenied
        } else {
            io::ErrorKind::ConnectionRefused
        };
        return Err(io::Error::new(
            kind,
            format!("WebSocket upgrade failed with HTTP {status}"),
        ));
    }

    let mut upgrade_ok = false;
    let mut connection_ok = false;
    let mut accept_header: Option<&str> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let v = value.trim();
            if name.eq_ignore_ascii_case("upgrade") && v.eq_ignore_ascii_case("websocket") {
                upgrade_ok = true;
            } else if name.eq_ignore_ascii_case("connection")
                && v.split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            {
                connection_ok = true;
            } else if name.eq_ignore_ascii_case("sec-websocket-accept") {
                accept_header = Some(v);
            }
        }
    }

    let challenge = format!("{sec_key}{WS_GUID}");
    let expected =
        base64(digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, challenge.as_bytes()).as_ref());
    if !upgrade_ok || !connection_ok || accept_header != Some(expected.as_str()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid WebSocket upgrade response headers",
        ));
    }
    Ok(())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let a = u32::from(*chunk.first().unwrap_or(&0));
        let b = u32::from(*chunk.get(1).unwrap_or(&0));
        let c = u32::from(*chunk.get(2).unwrap_or(&0));
        let value = (a << 16) | (b << 8) | c;
        let n = chunk.len().saturating_add(1);
        for shift in [18, 12, 6, 0].into_iter().take(n) {
            let idx = usize::try_from((value >> shift) & 63).unwrap_or(0);
            out.push(char::from(*ALPHABET.get(idx).unwrap_or(&b'=')));
        }
        for _ in n..4 {
            out.push('=');
        }
    }
    out
}

fn encode_masked(opcode: u8, body: &[u8], mask: [u8; 4]) -> io::Result<Vec<u8>> {
    let n = body.len();
    let mut frame = Vec::with_capacity(n.saturating_add(14));
    frame.push(0x80 | (opcode & 0x0f));
    if n < 126 {
        frame.push(0x80 | u8::try_from(n).map_err(|_| io::ErrorKind::InvalidInput)?);
    } else if n <= 65_535 {
        frame.push(0x80 | 0x7e);
        frame.extend_from_slice(
            &u16::try_from(n)
                .map_err(|_| io::ErrorKind::InvalidInput)?
                .to_be_bytes(),
        );
    } else {
        frame.push(0x80 | 0x7f);
        frame.extend_from_slice(
            &u64::try_from(n)
                .map_err(|_| io::ErrorKind::InvalidInput)?
                .to_be_bytes(),
        );
    }
    frame.extend_from_slice(&mask);
    for (byte, key) in body.iter().zip(mask.into_iter().cycle()) {
        frame.push(byte ^ key);
    }
    Ok(frame)
}

fn read_retry(stream: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match stream.read(buf) {
            Ok(n) => return Ok(n),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Minimal RFC 6455 loopback test server helper reused by connector E2E tests.
    pub(crate) fn accept_ws_test_handshake(stream: &mut TcpStream) -> io::Result<String> {
        let mut req = Vec::new();
        let mut byte = [0u8; 1];
        while !req.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte)?;
            req.extend_from_slice(&byte);
        }
        let text = String::from_utf8_lossy(&req).into_owned();
        let mut key = String::new();
        let mut target = String::new();
        for (idx, line) in text.split("\r\n").enumerate() {
            if idx == 0 {
                if let Some(path) = line.split_whitespace().nth(1) {
                    target = path.to_owned();
                }
            } else if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("sec-websocket-key")
            {
                key = v.trim().to_owned();
            }
        }
        let challenge = format!("{key}{WS_GUID}");
        let accept = base64(
            digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, challenge.as_bytes()).as_ref(),
        );
        write!(
            stream,
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )?;
        stream.flush()?;
        Ok(target)
    }

    pub(crate) fn write_server_frame(
        stream: &mut TcpStream,
        fin: bool,
        opcode: u8,
        payload: &[u8],
    ) -> io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 10);
        frame.push((if fin { 0x80 } else { 0 }) | (opcode & 0x0f));
        if payload.len() < 126 {
            frame.push(u8::try_from(payload.len()).unwrap_or(0));
        } else if payload.len() <= 65_535 {
            frame.push(126);
            frame.extend_from_slice(&u16::try_from(payload.len()).unwrap_or(0).to_be_bytes());
        } else {
            frame.push(127);
            frame.extend_from_slice(&u64::try_from(payload.len()).unwrap_or(0).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        stream.write_all(&frame)?;
        stream.flush()
    }

    pub(crate) fn read_client_frame(stream: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
        let mut hdr = [0u8; 2];
        stream.read_exact(&mut hdr)?;
        let opcode = hdr[0] & 0x0f;
        let masked = (hdr[1] & 0x80) != 0;
        if !masked {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "client frame must be masked",
            ));
        }
        let len: usize = match hdr[1] & 0x7f {
            126 => {
                let mut b = [0u8; 2];
                stream.read_exact(&mut b)?;
                usize::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0u8; 8];
                stream.read_exact(&mut b)?;
                usize::try_from(u64::from_be_bytes(b))
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "len overflow"))?
            }
            n => usize::from(n),
        };
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask)?;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload)?;
        for (byte, key) in payload.iter_mut().zip(mask.into_iter().cycle()) {
            *byte ^= key;
        }
        Ok((opcode, payload))
    }

    #[test]
    fn parse_ws_url_allows_wss_and_loopback_ws_and_rejects_remote_ws() {
        let wss = parse_ws_url("wss://gateway.discord.gg/?v=10&encoding=json").unwrap();
        assert!(wss.tls);
        assert_eq!(wss.host, "gateway.discord.gg");
        assert_eq!(wss.port, 443);
        assert_eq!(wss.target, "/?v=10&encoding=json");

        let local = parse_ws_url("ws://127.0.0.1:9001/socket?ticket=abc").unwrap();
        assert!(!local.tls);
        assert_eq!(local.host, "127.0.0.1");
        assert_eq!(local.port, 9001);
        assert_eq!(local.target, "/socket?ticket=abc");

        assert!(parse_ws_url("ws://example.com/socket").is_err());
        assert!(parse_ws_url("http://127.0.0.1:8080/ws").is_err());
    }

    #[test]
    fn ws_client_roundtrip_handles_large_text_fragmentation_ping_pong_timeout_and_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let big_payload = "A".repeat(6_000);
        let big_clone = big_payload.clone();

        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let target = accept_ws_test_handshake(&mut sock).unwrap();
            assert_eq!(target, "/stream?v=10");

            // Send large >4 KiB text immediately after 101 (tests leftover buffer preservation too).
            write_server_frame(&mut sock, true, OP_TEXT, big_clone.as_bytes()).unwrap();

            // Send fragmented text with interleaved ping frame.
            write_server_frame(&mut sock, false, OP_TEXT, b"hello ").unwrap();
            write_server_frame(&mut sock, true, OP_PING, b"hb").unwrap();
            write_server_frame(&mut sock, true, OP_CONT, b"krill").unwrap();

            // Expect client auto-pong for "hb", then client text message.
            let (op_pong, pong_body) = read_client_frame(&mut sock).unwrap();
            assert_eq!(op_pong, OP_PONG);
            assert_eq!(pong_body, b"hb");

            let (op_txt, txt_body) = read_client_frame(&mut sock).unwrap();
            assert_eq!(op_txt, OP_TEXT);
            assert_eq!(txt_body, b"client-ack");

            // Pause briefly so client observes Timeout cleanly, then send Close(1000).
            thread::sleep(Duration::from_millis(80));
            write_server_frame(&mut sock, true, OP_CLOSE, &[0x03, 0xe8, b'b', b'y', b'e']).unwrap();
        });

        let mut client = WsClient::connect(
            &format!("ws://{addr}/stream?v=10"),
            &[],
            Duration::from_secs(5),
        )
        .unwrap();

        let ev1 = client.poll_event(Duration::from_secs(2)).unwrap();
        assert_eq!(ev1, WsEvent::Text(big_payload));

        let ev2 = client.poll_event(Duration::from_secs(2)).unwrap();
        assert_eq!(ev2, WsEvent::Text("hello krill".to_owned()));

        client.send_text("client-ack").unwrap();

        // Short poll must return Timeout promptly (≤200ms shutdown guarantee).
        let started = Instant::now();
        let ev3 = client.poll_event(Duration::from_millis(25)).unwrap();
        assert_eq!(ev3, WsEvent::Timeout);
        assert!(started.elapsed() < Duration::from_millis(180));

        let ev4 = client.poll_event(Duration::from_secs(2)).unwrap();
        assert_eq!(
            ev4,
            WsEvent::Closed {
                code: Some(1000),
                reason: "bye".to_owned(),
            }
        );

        server.join().unwrap();
    }
}
