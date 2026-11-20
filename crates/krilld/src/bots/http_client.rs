// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Zero-`tokio` cross-platform HTTP/1.1 + `rustls` HTTPS client for Krill Bots.
//!
//! Used by `tool_relay`, `google`, `trackers`, `composio`, and Wave 2B channel connectors
//! (`Telegram`, `Slack`, `Discord`). Supports plain `http://` (loopback/tests) and `https://`
//! (`rustls` + `webpki-roots` + `ring`) over `std::net::TcpStream` with strict header validation,
//! chunked transfer decoding, timeouts, and bounded response caps.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::de::DeserializeOwned;

/// Default connect/read/write timeout for outbound bot HTTP requests.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default response body cap (4 MiB) to protect daemon memory budgets.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum HTTP response header block accepted (64 KiB).
const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Maximum single chunk line length in chunked transfer encoding.
const MAX_CHUNK_LINE_BYTES: usize = 256;

/// HTTP methods supported by [`HttpRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    /// Parses an ASCII HTTP method token (case-insensitive).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.eq_ignore_ascii_case("GET") {
            Some(Self::Get)
        } else if raw.eq_ignore_ascii_case("POST") {
            Some(Self::Post)
        } else if raw.eq_ignore_ascii_case("PUT") {
            Some(Self::Put)
        } else if raw.eq_ignore_ascii_case("PATCH") {
            Some(Self::Patch)
        } else if raw.eq_ignore_ascii_case("DELETE") {
            Some(Self::Delete)
        } else {
            None
        }
    }
}

/// Outbound HTTP/1.1 request descriptor.
#[derive(Clone)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("header_count", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl HttpRequest {
    #[must_use]
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Get, url)
    }

    #[must_use]
    pub fn post_json(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self::new(HttpMethod::Post, url)
            .with_header("Content-Type", "application/json")
            .with_body(body)
    }

    #[must_use]
    pub fn post_form(url: impl Into<String>, pairs: &[(&str, &str)]) -> Self {
        let encoded = encode_form_pairs(pairs);
        Self::new(HttpMethod::Post, url)
            .with_header("Content-Type", "application/x-www-form-urlencoded")
            .with_body(encoded.into_bytes())
    }

    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_max_response_bytes(mut self, cap: usize) -> Self {
        self.max_response_bytes = cap;
        self
    }
}

/// Outbound HTTP/1.1 response.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("header_count", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl HttpResponse {
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json<T: DeserializeOwned>(&self) -> io::Result<T> {
        serde_json::from_slice(&self.body)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }
}

/// Parsed URL components for HTTP/1.1 requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub host_header: String,
    pub target: String,
}

/// Parses an `http://` or `https://` URL into host, port, `Host` header, and request target.
pub fn parse_url(raw: &str) -> io::Result<ParsedUrl> {
    if raw.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "URL contains forbidden control characters",
        ));
    }
    let (https, rest, default_port) = if let Some(r) = raw.strip_prefix("https://") {
        (true, r, 443_u16)
    } else if let Some(r) = raw.strip_prefix("http://") {
        (false, r, 80_u16)
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "URL must start with http:// or https://",
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
            "invalid URL authority",
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

    let target = if tail.is_empty() {
        "/".to_owned()
    } else if tail.starts_with('?') {
        format!("/{tail}")
    } else {
        tail.to_owned()
    };

    Ok(ParsedUrl {
        https,
        host,
        port,
        host_header,
        target,
    })
}

/// Returns a lazily initialized, shared `rustls::ClientConfig` backed by `webpki-roots` and `ring`.
pub fn tls_client_config() -> io::Result<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let cfg = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .ok()?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Some(Arc::new(cfg))
        })
        .clone()
        .ok_or_else(|| io::Error::other("failed to initialize rustls client config"))
}

enum HttpStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for HttpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for HttpStream {
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

/// Convenience wrapper for sending an HTTP/1.1 request.
pub fn send_request(
    method: HttpMethod,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    timeout: Duration,
) -> io::Result<HttpResponse> {
    let mut req = HttpRequest::new(method, url)
        .with_body(body.to_vec())
        .with_timeout(timeout);
    req.headers.extend_from_slice(headers);
    send(&req)
}

/// Sends a single HTTP/1.1 request over plain TCP or TLS (`rustls`) and reads the bounded response.
pub fn send(req: &HttpRequest) -> io::Result<HttpResponse> {
    let parsed = parse_url(&req.url)?;
    validate_headers(&req.headers)?;

    let addr = (parsed.host.as_str(), parsed.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "DNS resolution empty"))?;
    let tcp = TcpStream::connect_timeout(&addr, req.timeout)?;
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(req.timeout))?;
    tcp.set_write_timeout(Some(req.timeout))?;

    let mut stream = if parsed.https {
        let cfg = tls_client_config()?;
        let server_name = rustls_pki_types::ServerName::try_from(parsed.host.clone())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?;
        let conn = rustls::ClientConnection::new(cfg, server_name)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        HttpStream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        HttpStream::Plain(tcp)
    };

    let wire = build_request_bytes(&parsed, req);
    stream.write_all(&wire)?;
    stream.flush()?;

    read_response(&mut stream, req.max_response_bytes)
}

fn validate_headers(headers: &[(String, String)]) -> io::Result<()> {
    for (name, value) in headers {
        if name.is_empty()
            || name.bytes().any(|b| b <= 0x20 || b == b':' || b == 0x7f)
            || value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid HTTP header name or value",
            ));
        }
    }
    Ok(())
}

fn build_request_bytes(parsed: &ParsedUrl, req: &HttpRequest) -> Vec<u8> {
    let mut head = String::with_capacity(256);
    write!(
        head,
        "{} {} HTTP/1.1\r\n",
        req.method.as_str(),
        parsed.target
    )
    .ok();

    let has_host = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("host"));
    let has_conn = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("connection"));
    let has_ua = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"));
    let has_accept_encoding = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"));

    if !has_host {
        write!(head, "Host: {}\r\n", parsed.host_header).ok();
    }
    if !has_ua {
        head.push_str("User-Agent: krilld-bots/1.0\r\n");
    }
    if !has_accept_encoding {
        head.push_str("Accept-Encoding: identity\r\n");
    }
    if !has_conn {
        head.push_str("Connection: close\r\n");
    }

    for (name, value) in &req.headers {
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            continue;
        }
        write!(head, "{name}: {value}\r\n").ok();
    }

    if !req.body.is_empty()
        || matches!(
            req.method,
            HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch
        )
    {
        write!(head, "Content-Length: {}\r\n", req.body.len()).ok();
    }
    head.push_str("\r\n");

    let mut out = Vec::with_capacity(head.len().saturating_add(req.body.len()));
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(&req.body);
    out
}

fn read_response(stream: &mut impl Read, max_body_bytes: usize) -> io::Result<HttpResponse> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    let (status, headers, body_start) = loop {
        let header_end = find_header_end(&buf);
        if let Some(end) = header_end {
            let head_bytes = buf.get(..end).unwrap_or_default();
            let (status, headers) = parse_response_head(head_bytes)?;
            let next_start = end.saturating_add(4);
            if status == 100 {
                // Drain `100 Continue` interim response and keep reading the real response.
                let tail = buf.get(next_start..).unwrap_or_default().to_vec();
                buf = tail;
                continue;
            }
            break (status, headers, next_start);
        }
        if buf.len() >= MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response headers exceed 64 KiB limit",
            ));
        }
        let n = read_retry(stream, &mut tmp)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "EOF before HTTP response headers completed",
            ));
        }
        let chunk = tmp.get(..n).unwrap_or_default();
        buf.extend_from_slice(chunk);
    };

    let initial_body = buf.get(body_start..).unwrap_or_default().to_vec();

    if status == 204 || status == 304 {
        return Ok(HttpResponse {
            status,
            headers,
            body: Vec::new(),
        });
    }

    let is_chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding")
            && v.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
    });
    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());

    let body = if is_chunked {
        read_chunked_body(stream, initial_body, max_body_bytes)?
    } else if let Some(expected_len) = content_length {
        if expected_len > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response exceeds maximum allowed size",
            ));
        }
        read_exact_body(stream, initial_body, expected_len)?
    } else {
        read_until_eof_body(stream, initial_body, max_body_bytes)?
    };

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn read_retry(stream: &mut (impl Read + ?Sized), buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match stream.read(buf) {
            Ok(n) => return Ok(n),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_response_head(head: &[u8]) -> io::Result<(u16, Vec<(String, String)>)> {
    let text = std::str::from_utf8(head)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 HTTP response head"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status line"))?;
    let mut parts = status_line.split_whitespace();
    let version = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP version"))?;
    if !version.starts_with("HTTP/1.") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported HTTP response version",
        ));
    }
    let status_str = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status code"))?;
    let status = status_str
        .parse::<u16>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP status code"))?;

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    Ok((status, headers))
}

fn read_exact_body(
    stream: &mut impl Read,
    mut body: Vec<u8>,
    expected_len: usize,
) -> io::Result<Vec<u8>> {
    if body.len() > expected_len {
        body.truncate(expected_len);
        return Ok(body);
    }
    body.reserve(expected_len.saturating_sub(body.len()));
    let mut tmp = [0u8; 8192];
    while body.len() < expected_len {
        let remaining = expected_len.saturating_sub(body.len());
        let to_read = remaining.min(tmp.len());
        let slice = tmp.get_mut(..to_read).unwrap_or(&mut []);
        let n = read_retry(stream, slice)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated HTTP response body",
            ));
        }
        body.extend_from_slice(tmp.get(..n).unwrap_or_default());
    }
    Ok(body)
}

fn read_until_eof_body(
    stream: &mut impl Read,
    mut body: Vec<u8>,
    max_body_bytes: usize,
) -> io::Result<Vec<u8>> {
    if body.len() > max_body_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response body exceeds cap",
        ));
    }
    let mut tmp = [0u8; 8192];
    loop {
        let n = match read_retry(stream, &mut tmp) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        };
        if body.len().saturating_add(n) > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response body exceeds cap",
            ));
        }
        body.extend_from_slice(tmp.get(..n).unwrap_or_default());
    }
    Ok(body)
}

fn read_chunked_body(
    stream: &mut impl Read,
    mut raw: Vec<u8>,
    max_body_bytes: usize,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    let mut tmp = [0u8; 8192];

    let ensure_bytes = |stream: &mut dyn Read,
                        raw: &mut Vec<u8>,
                        cursor: &mut usize,
                        needed: usize,
                        tmp: &mut [u8; 8192]|
     -> io::Result<()> {
        if *cursor >= 8192 {
            raw.drain(..*cursor);
            *cursor = 0;
        }
        while raw.len().saturating_sub(*cursor) < needed {
            let n = read_retry(&mut *stream, tmp)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated chunked HTTP body",
                ));
            }
            raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
        }
        Ok(())
    };

    loop {
        // Read chunk-size line terminated by `\r\n`.
        let crlf_rel = loop {
            let slice = raw.get(cursor..).unwrap_or_default();
            if let Some(pos) = slice.windows(2).position(|w| w == b"\r\n") {
                break pos;
            }
            if slice.len() > MAX_CHUNK_LINE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "chunk header line too long",
                ));
            }
            let n = read_retry(stream, &mut tmp)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF reading chunk size",
                ));
            }
            raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
        };

        let line_bytes = raw
            .get(cursor..cursor.saturating_add(crlf_rel))
            .unwrap_or_default();
        let line = std::str::from_utf8(line_bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk line UTF-8"))?;
        let hex_part = line.split(';').next().unwrap_or(line).trim();
        let chunk_len = usize::from_str_radix(hex_part, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size hex"))?;
        cursor = cursor.saturating_add(crlf_rel).saturating_add(2);

        if chunk_len == 0 {
            break;
        }
        if out.len().saturating_add(chunk_len) > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunked HTTP response exceeds cap",
            ));
        }

        ensure_bytes(
            stream,
            &mut raw,
            &mut cursor,
            chunk_len.saturating_add(2),
            &mut tmp,
        )?;
        let data_end = cursor.saturating_add(chunk_len);
        let data = raw.get(cursor..data_end).unwrap_or_default();
        out.extend_from_slice(data);
        let delim = raw.get(data_end..data_end.saturating_add(2));
        if delim != Some(b"\r\n".as_slice()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing CRLF after HTTP chunk",
            ));
        }
        cursor = data_end.saturating_add(2);
    }

    Ok(out)
}

/// Percent-encodes a string for URL query parameters or `application/x-www-form-urlencoded` bodies.
#[must_use]
pub fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            let hi = usize::from(byte >> 4);
            let lo = usize::from(byte & 0x0f);
            out.push(char::from(HEX.get(hi).copied().unwrap_or(b'0')));
            out.push(char::from(HEX.get(lo).copied().unwrap_or(b'0')));
        }
    }
    out
}

/// Percent-decodes a URL query parameter component (`+` treated as space).
#[must_use]
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes.get(i).copied() {
            Some(b'+') => {
                out.push(b' ');
                i = i.saturating_add(1);
            }
            Some(b'%') => {
                if let (Some(h1), Some(h2)) = (
                    bytes.get(i.saturating_add(1)).copied().and_then(hex_nibble),
                    bytes.get(i.saturating_add(2)).copied().and_then(hex_nibble),
                ) {
                    out.push((h1 << 4) | h2);
                    i = i.saturating_add(3);
                } else {
                    out.push(b'%');
                    i = i.saturating_add(1);
                }
            }
            Some(b) => {
                out.push(b);
                i = i.saturating_add(1);
            }
            None => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Encodes `(key, value)` pairs as `application/x-www-form-urlencoded`.
#[must_use]
pub fn encode_form_pairs(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (idx, (k, v)) in pairs.iter().enumerate() {
        if idx > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(k));
        out.push('=');
        out.push_str(&percent_encode(v));
    }
    out
}

/// Parses a URL query string (`a=1&b=hello%20world`) into decoded key/value pairs.
#[must_use]
pub fn parse_query_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(part), String::new()),
        })
        .collect()
}

/// RFC 4648 URL-safe Base64 encoding without padding (used by PKCE S256).
#[must_use]
pub fn base64url_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3).saturating_mul(4));
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        let idx0 = usize::try_from((n >> 18) & 0x3f).unwrap_or(0);
        let idx1 = usize::try_from((n >> 12) & 0x3f).unwrap_or(0);
        out.push(char::from(TABLE.get(idx0).copied().unwrap_or(b'A')));
        out.push(char::from(TABLE.get(idx1).copied().unwrap_or(b'A')));
        if chunk.len() > 1 {
            let idx2 = usize::try_from((n >> 6) & 0x3f).unwrap_or(0);
            out.push(char::from(TABLE.get(idx2).copied().unwrap_or(b'A')));
        }
        if chunk.len() > 2 {
            let idx3 = usize::try_from(n & 0x3f).unwrap_or(0);
            out.push(char::from(TABLE.get(idx3).copied().unwrap_or(b'A')));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn parses_http_https_and_ipv6_urls() {
        let u1 = parse_url("https://api.github.com/repos/a/b?state=open#frag").unwrap();
        assert!(u1.https);
        assert_eq!(u1.host, "api.github.com");
        assert_eq!(u1.port, 443);
        assert_eq!(u1.host_header, "api.github.com");
        assert_eq!(u1.target, "/repos/a/b?state=open");

        let u2 = parse_url("http://127.0.0.1:8080/bot-tools/c1/mcp").unwrap();
        assert!(!u2.https);
        assert_eq!(u2.host, "127.0.0.1");
        assert_eq!(u2.port, 8080);
        assert_eq!(u2.host_header, "127.0.0.1:8080");
        assert_eq!(u2.target, "/bot-tools/c1/mcp");

        let u3 = parse_url("http://[::1]:9000?a=1").unwrap();
        assert_eq!(u3.host, "::1");
        assert_eq!(u3.port, 9000);
        assert_eq!(u3.target, "/?a=1");

        assert!(parse_url("ftp://example.com").is_err());
        assert!(parse_url("http://evil\r\nHeader: x").is_err());
    }

    #[test]
    fn loopback_roundtrip_handles_content_length_and_chunked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            for idx in 0..2 {
                let (mut sock, _) = listener.accept().unwrap();
                let mut req_buf = [0u8; 4096];
                let _ = sock.read(&mut req_buf).unwrap();
                if idx == 0 {
                    let body = r#"{"ok":true}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    sock.write_all(resp.as_bytes()).unwrap();
                } else {
                    let resp = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n";
                    sock.write_all(resp.as_bytes()).unwrap();
                }
            }
        });

        let r1 = send(
            &HttpRequest::post_json(format!("http://{addr}/first"), b"{}".to_vec())
                .with_header("X-Test", "1"),
        )
        .unwrap();
        assert_eq!(r1.status, 200);
        assert_eq!(r1.text(), r#"{"ok":true}"#);

        let r2 = send(&HttpRequest::get(format!("http://{addr}/second"))).unwrap();
        assert_eq!(r2.status, 200);
        assert_eq!(r2.text(), "hello world");

        server.join().unwrap();
    }

    #[test]
    fn base64url_and_query_helpers_match_rfc() {
        assert_eq!(base64url_encode(b""), "");
        assert_eq!(base64url_encode(b"f"), "Zg");
        assert_eq!(base64url_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(
            parse_query_params("code=abc%2F123&state=hello+world"),
            vec![
                ("code".to_owned(), "abc/123".to_owned()),
                ("state".to_owned(), "hello world".to_owned()),
            ]
        );
    }
}
