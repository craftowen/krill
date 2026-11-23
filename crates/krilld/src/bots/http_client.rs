// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Zero-`tokio` cross-platform HTTP/1.1 + `rustls` HTTPS client for Krill Bots.
//!
//! Used by `tool_relay`, `google`, `trackers`, `composio`, and Wave 2B channel connectors
//! (`Telegram`, `Slack`, `Discord`). Supports plain `http://` (loopback/tests) and `https://`
//! (`rustls` + `webpki-roots` + `ring`) over `std::net::TcpStream` with strict header validation,
//! chunked transfer decoding, timeouts, and bounded response caps.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom as _, SystemRandom};
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

/// SSRF protection policy applied to resolved socket addresses before connecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SsrfPolicy {
    /// Rejects private, loopback, link-local/cloud-metadata, CGNAT, multicast, and unspecified IPs.
    #[default]
    StrictExternal,
    /// Allows loopback (`127.0.0.0/8`, `::1`) for local test mocks while blocking private/metadata IPs.
    AllowLoopback,
    /// Allows any resolved IP address (reserved strictly for local diagnostic harnesses).
    AllowAny,
}

/// Optional handle allowing an in-flight request's active TCP socket to be closed cleanly.
pub type RequestCancelSlot = Arc<Mutex<Option<TcpStream>>>;

/// Single multipart/form-data field for file uploads.
#[derive(Debug, Clone)]
pub struct MultipartField {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
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
    pub ssrf_policy: SsrfPolicy,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("header_count", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .field("timeout", &self.timeout)
            .field("ssrf_policy", &self.ssrf_policy)
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
            ssrf_policy: SsrfPolicy::StrictExternal,
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

    /// Builds a `multipart/form-data` POST request from text and binary file parts.
    pub fn post_multipart(url: impl Into<String>, fields: &[MultipartField]) -> io::Result<Self> {
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| io::Error::other("multipart boundary RNG failure"))?;
        let mut boundary = String::from("----KrillBotBoundary");
        for byte in nonce {
            write!(boundary, "{byte:02x}").ok();
        }
        let mut body = Vec::new();
        for field in fields {
            let clean_name = sanitize_quoted_header_param(&field.name);
            write!(
                body,
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{clean_name}\""
            )
            .ok();
            if let Some(ref fname) = field.filename {
                let clean_fname = sanitize_quoted_header_param(fname);
                write!(body, "; filename=\"{clean_fname}\"").ok();
            }
            body.extend_from_slice(b"\r\n");
            if let Some(ref ctype) = field.content_type {
                let clean_ctype: String = ctype
                    .chars()
                    .filter(|c| !matches!(c, '\r' | '\n' | '\0'))
                    .collect();
                write!(body, "Content-Type: {clean_ctype}\r\n").ok();
            }
            body.extend_from_slice(b"\r\n");
            body.extend_from_slice(&field.data);
            body.extend_from_slice(b"\r\n");
        }
        write!(body, "--{boundary}--\r\n").ok();
        Ok(Self::new(HttpMethod::Post, url)
            .with_header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .with_body(body))
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

    #[must_use]
    pub fn with_ssrf_policy(mut self, policy: SsrfPolicy) -> Self {
        self.ssrf_policy = policy;
        self
    }
}

fn sanitize_quoted_header_param(raw: &str) -> String {
    raw.chars()
        .filter(|c| !matches!(c, '"' | '\r' | '\n' | '\0' | '\\'))
        .collect()
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
    if raw.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "URL contains forbidden whitespace or control characters",
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

/// Validates an [`IpAddr`] against the requested [`SsrfPolicy`].
pub fn validate_ip_against_ssrf(ip: IpAddr, policy: SsrfPolicy) -> io::Result<()> {
    if policy == SsrfPolicy::AllowAny {
        return Ok(());
    }
    match ip {
        IpAddr::V4(v4) => validate_ipv4_against_ssrf(v4, policy),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return validate_ipv4_against_ssrf(mapped, policy);
            }
            if v6.is_loopback() {
                if policy == SsrfPolicy::AllowLoopback {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SSRF blocked loopback IPv6 target",
                ));
            }
            let segs = v6.segments();
            // NAT64 (`64:ff9b::/96`): check embedded IPv4 address.
            if segs[0] == 0x0064
                && segs[1] == 0xff9b
                && segs[2] == 0
                && segs[3] == 0
                && segs[4] == 0
                && segs[5] == 0
            {
                let hi = segs[6].to_be_bytes();
                let lo = segs[7].to_be_bytes();
                let embedded = Ipv4Addr::new(hi[0], hi[1], lo[0], lo[1]);
                return validate_ipv4_against_ssrf(embedded, policy);
            }
            let is_nat64_local = segs[0] == 0x0064 && segs[1] == 0xff9b && segs[2] == 1;
            let is_site_local = (segs[0] & 0xffc0) == 0xfec0;
            if v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || is_site_local
                || is_nat64_local
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SSRF blocked private, link-local, or special IPv6 target",
                ));
            }
            Ok(())
        }
    }
}

fn validate_ipv4_against_ssrf(v4: Ipv4Addr, policy: SsrfPolicy) -> io::Result<()> {
    if v4.is_loopback() {
        if policy == SsrfPolicy::AllowLoopback {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSRF blocked loopback IPv4 target",
        ));
    }
    let octets = v4.octets();
    let is_unspecified_net = octets[0] == 0;
    let is_cgnat = octets[0] == 100 && (octets[1] & 0b1100_0000) == 64;
    let is_benchmark = octets[0] == 198 && (octets[1] & 0xfe) == 18;
    let is_reserved = octets[0] >= 240;
    if v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || is_unspecified_net
        || is_cgnat
        || is_benchmark
        || is_reserved
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSRF blocked private, link-local, metadata, or special IPv4 target",
        ));
    }
    Ok(())
}

/// Resolves `host:port`, checks every returned IP against `policy`, and iterates all
/// addresses until a connection succeeds or `deadline` expires.
pub(crate) fn connect_with_deadline(
    host: &str,
    port: u16,
    deadline: Instant,
    policy: SsrfPolicy,
) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "DNS resolution empty",
        ));
    }
    for addr in &addrs {
        validate_ip_against_ssrf(addr.ip(), policy)?;
    }
    let mut last_err: Option<io::Error> = None;
    let total = addrs.len();
    for (idx, addr) in addrs.iter().enumerate() {
        let now = Instant::now();
        if now >= deadline {
            return Err(last_err.unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "connection timed out")
            }));
        }
        let remaining = deadline.saturating_duration_since(now);
        let remaining_addrs = u32::try_from(total.saturating_sub(idx)).unwrap_or(1).max(1);
        let slice = if remaining_addrs > 1 {
            (remaining / remaining_addrs)
                .max(Duration::from_millis(250))
                .min(remaining)
        } else {
            remaining
        };
        match TcpStream::connect_timeout(addr, slice) {
            Ok(tcp) => return Ok(tcp),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "all resolved endpoints failed",
        )
    }))
}

enum HttpStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl HttpStream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => &s.sock,
        }
    }
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

struct ActiveSocketGuard<'a> {
    slot: Option<&'a RequestCancelSlot>,
}

impl Drop for ActiveSocketGuard<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot
            && let Ok(mut guard) = slot.lock()
        {
            *guard = None;
        }
    }
}

/// Immediately shuts down an active HTTP request's TCP socket stored in `slot`.
pub fn cancel_active_request(slot: &RequestCancelSlot) {
    if let Ok(mut guard) = slot.lock()
        && let Some(sock) = guard.take()
    {
        sock.shutdown(Shutdown::Both).ok();
    }
}

/// Convenience wrapper for sending an HTTP/1.1 request with [`SsrfPolicy::StrictExternal`].
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
    send_interruptible(req, None, None)
}

/// Sends an HTTP/1.1 request that can be aborted mid-poll via `cancel` / `active_socket`.
pub fn send_interruptible(
    req: &HttpRequest,
    cancel: Option<&AtomicBool>,
    active_socket: Option<&RequestCancelSlot>,
) -> io::Result<HttpResponse> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "HTTP request cancelled before connect",
        ));
    }
    let timeout = req.timeout.max(Duration::from_millis(1));
    let deadline = Instant::now() + timeout;
    let parsed = parse_url(&req.url)?;
    validate_headers(&req.headers)?;

    let tcp = connect_with_deadline(&parsed.host, parsed.port, deadline, req.ssrf_policy)?;
    tcp.set_nodelay(true).ok();
    let _guard = if let Some(slot) = active_socket {
        if let Ok(mut guard) = slot.lock() {
            *guard = tcp.try_clone().ok();
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            tcp.shutdown(Shutdown::Both).ok();
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "HTTP request cancelled",
            ));
        }
        Some(ActiveSocketGuard { slot: Some(slot) })
    } else {
        None
    };

    let mut stream = open_http_stream(tcp, &parsed, deadline)?;
    let wire = build_request_bytes(&parsed, req);
    write_all_with_deadline(&mut stream, &wire, deadline)?;

    read_response(&mut stream, req.max_response_bytes, deadline, cancel)
}

/// Streaming HTTP/1.1 response handle suitable for SSE (`text/event-stream`) or bounded reads.
pub struct StreamingHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    stream: HttpStream,
    raw: Vec<u8>,
    cursor: usize,
    is_chunked: bool,
    content_length: Option<usize>,
    body_read: usize,
    chunk_decoder: ChunkStreamState,
    deadline: Instant,
}

impl std::fmt::Debug for StreamingHttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingHttpResponse")
            .field("status", &self.status)
            .field("header_count", &self.headers.len())
            .field("is_chunked", &self.is_chunked)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy)]
enum ChunkStreamState {
    ReadingSize,
    ReadingData { remaining: usize },
    ExpectDataCrlf { remaining: usize },
    ReadingTrailers { bytes_read: usize },
    Done,
}

/// Sends a request and returns once HTTP response status + headers have arrived.
pub fn send_streaming(req: &HttpRequest) -> io::Result<StreamingHttpResponse> {
    let timeout = req.timeout.max(Duration::from_millis(1));
    let deadline = Instant::now() + timeout;
    let parsed = parse_url(&req.url)?;
    validate_headers(&req.headers)?;

    let tcp = connect_with_deadline(&parsed.host, parsed.port, deadline, req.ssrf_policy)?;
    tcp.set_nodelay(true).ok();
    let mut stream = open_http_stream(tcp, &parsed, deadline)?;
    let wire = build_request_bytes(&parsed, req);
    write_all_with_deadline(&mut stream, &wire, deadline)?;

    let (status, headers, initial_body) = read_response_head(&mut stream, deadline, None)?;
    let is_chunked = has_chunked_encoding(&headers);
    let content_length = find_content_length(&headers);

    Ok(StreamingHttpResponse {
        status,
        headers,
        stream,
        raw: initial_body,
        cursor: 0,
        is_chunked,
        content_length,
        body_read: 0,
        chunk_decoder: ChunkStreamState::ReadingSize,
        deadline,
    })
}

impl StreamingHttpResponse {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[must_use]
    pub fn is_event_stream(&self) -> bool {
        self.header("content-type")
            .is_some_and(|ct| ct.to_ascii_lowercase().contains("text/event-stream"))
    }

    /// Consumes the remainder of the response body up to `max_body_bytes`.
    pub fn into_response(mut self, max_body_bytes: usize) -> io::Result<HttpResponse> {
        if self.status == 204 || self.status == 304 {
            return Ok(HttpResponse {
                status: self.status,
                headers: self.headers,
                body: Vec::new(),
            });
        }
        let initial_body = self.raw.get(self.cursor..).unwrap_or_default().to_vec();
        let body = if self.is_chunked {
            read_chunked_body(
                &mut self.stream,
                initial_body,
                max_body_bytes,
                self.deadline,
                None,
            )?
        } else if let Some(expected_len) = self.content_length {
            if expected_len > max_body_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP response exceeds maximum allowed size",
                ));
            }
            read_exact_body(
                &mut self.stream,
                initial_body,
                expected_len,
                self.deadline,
                None,
            )?
        } else {
            read_until_eof_body(
                &mut self.stream,
                initial_body,
                max_body_bytes,
                self.deadline,
                None,
            )?
        };
        Ok(HttpResponse {
            status: self.status,
            headers: self.headers,
            body,
        })
    }

    /// Reads the next decoded SSE payload slice, de-chunking HTTP/1.1 chunked streams on the fly.
    pub fn read_next_stream_chunk(
        &mut self,
        idle_timeout: Duration,
        stop: Option<&AtomicBool>,
    ) -> io::Result<Option<Vec<u8>>> {
        let idle = idle_timeout.max(Duration::from_millis(50));
        if self.status == 204 || self.status == 304 {
            return Ok(None);
        }
        if !self.is_chunked {
            if let Some(expected) = self.content_length
                && self.body_read >= expected
            {
                return Ok(None);
            }
            if self.cursor < self.raw.len() {
                let mut chunk = self.raw.split_off(self.cursor);
                self.raw.clear();
                self.cursor = 0;
                if let Some(expected) = self.content_length {
                    let remain = expected.saturating_sub(self.body_read);
                    chunk.truncate(remain);
                }
                self.body_read = self.body_read.saturating_add(chunk.len());
                return if chunk.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(chunk))
                };
            }
            let deadline = Instant::now() + idle;
            let mut buf = [0u8; 8192];
            let n = match read_retry_deadline(&mut self.stream, &mut buf, deadline, stop) {
                Ok(0) => return Ok(None),
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => return Err(err),
            };
            let mut chunk = buf.get(..n).unwrap_or_default().to_vec();
            if let Some(expected) = self.content_length {
                let remain = expected.saturating_sub(self.body_read);
                chunk.truncate(remain);
            }
            self.body_read = self.body_read.saturating_add(chunk.len());
            return if chunk.is_empty() {
                Ok(None)
            } else {
                Ok(Some(chunk))
            };
        }

        // Incremental HTTP chunked decoder for live SSE streaming.
        let mut tmp = [0u8; 8192];
        loop {
            if self.cursor >= 8192 {
                self.raw.drain(..self.cursor);
                self.cursor = 0;
            }
            match self.chunk_decoder {
                ChunkStreamState::Done => return Ok(None),
                ChunkStreamState::ReadingSize => {
                    let slice = self.raw.get(self.cursor..).unwrap_or_default();
                    if let Some(pos) = slice.windows(2).position(|w| w == b"\r\n") {
                        let line_bytes = slice.get(..pos).unwrap_or_default();
                        let line = std::str::from_utf8(line_bytes).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "invalid chunk line UTF-8")
                        })?;
                        let hex_part = line.split(';').next().unwrap_or(line).trim();
                        let chunk_len = usize::from_str_radix(hex_part, 16).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size hex")
                        })?;
                        self.cursor = self.cursor.saturating_add(pos).saturating_add(2);
                        self.chunk_decoder = if chunk_len == 0 {
                            ChunkStreamState::ReadingTrailers { bytes_read: 0 }
                        } else {
                            ChunkStreamState::ReadingData {
                                remaining: chunk_len,
                            }
                        };
                        continue;
                    }
                    if slice.len() > MAX_CHUNK_LINE_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "chunk header line too long",
                        ));
                    }
                }
                ChunkStreamState::ReadingData { remaining } => {
                    let avail = self.raw.len().saturating_sub(self.cursor);
                    if avail > 0 {
                        let take = avail.min(remaining);
                        let end = self.cursor.saturating_add(take);
                        let out = self.raw.get(self.cursor..end).unwrap_or_default().to_vec();
                        self.cursor = end;
                        let left = remaining.saturating_sub(take);
                        self.chunk_decoder = if left == 0 {
                            ChunkStreamState::ExpectDataCrlf { remaining: 2 }
                        } else {
                            ChunkStreamState::ReadingData { remaining: left }
                        };
                        return Ok(Some(out));
                    }
                }
                ChunkStreamState::ExpectDataCrlf { remaining } => {
                    let avail = self.raw.len().saturating_sub(self.cursor);
                    if avail >= remaining {
                        let end = self.cursor.saturating_add(remaining);
                        let delim = self.raw.get(self.cursor..end).unwrap_or_default();
                        if (remaining == 2 && delim != b"\r\n")
                            || (remaining == 1 && delim != b"\n")
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "missing CRLF after HTTP chunk",
                            ));
                        }
                        self.cursor = end;
                        self.chunk_decoder = ChunkStreamState::ReadingSize;
                        continue;
                    }
                    if avail == 1 && remaining == 2 {
                        let first = self.raw.get(self.cursor).copied();
                        if first != Some(b'\r') {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "missing CRLF after HTTP chunk",
                            ));
                        }
                        self.cursor = self.cursor.saturating_add(1);
                        self.chunk_decoder = ChunkStreamState::ExpectDataCrlf { remaining: 1 };
                    }
                }
                ChunkStreamState::ReadingTrailers { bytes_read } => {
                    let slice = self.raw.get(self.cursor..).unwrap_or_default();
                    if let Some(pos) = slice.windows(2).position(|w| w == b"\r\n") {
                        let next_total = bytes_read.saturating_add(pos).saturating_add(2);
                        if next_total > MAX_HEADER_BYTES {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "chunked HTTP trailers exceed limit",
                            ));
                        }
                        self.cursor = self.cursor.saturating_add(pos).saturating_add(2);
                        if pos == 0 {
                            self.chunk_decoder = ChunkStreamState::Done;
                            return Ok(None);
                        }
                        self.chunk_decoder = ChunkStreamState::ReadingTrailers {
                            bytes_read: next_total,
                        };
                        continue;
                    }
                    if bytes_read.saturating_add(slice.len()) > MAX_HEADER_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "chunked HTTP trailers exceed limit",
                        ));
                    }
                }
            }

            let deadline = Instant::now() + idle;
            let n = read_retry_deadline(&mut self.stream, &mut tmp, deadline, stop)?;
            if n == 0 {
                self.chunk_decoder = ChunkStreamState::Done;
                return Ok(None);
            }
            self.raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
        }
    }
}

fn open_http_stream(
    tcp: TcpStream,
    parsed: &ParsedUrl,
    deadline: Instant,
) -> io::Result<HttpStream> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1));
    tcp.set_read_timeout(Some(remaining))?;
    tcp.set_write_timeout(Some(remaining))?;
    if parsed.https {
        let cfg = tls_client_config()?;
        let server_name = rustls_pki_types::ServerName::try_from(parsed.host.clone())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?;
        let conn = rustls::ClientConnection::new(cfg, server_name)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        Ok(HttpStream::Tls(Box::new(rustls::StreamOwned::new(
            conn, tcp,
        ))))
    } else {
        Ok(HttpStream::Plain(tcp))
    }
}

fn write_all_with_deadline(
    stream: &mut HttpStream,
    mut buf: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    while !buf.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP request write exceeded deadline",
            ));
        }
        stream.tcp().set_write_timeout(Some(remaining)).ok();
        stream.tcp().set_read_timeout(Some(remaining)).ok();
        match stream.write(buf) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write HTTP request",
                ));
            }
            Ok(n) => {
                buf = buf.get(n..).unwrap_or_default();
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1));
    stream.tcp().set_write_timeout(Some(remaining)).ok();
    stream.flush()
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

    // Case-insensitively deduplicate user headers (last occurrence wins).
    for (idx, (name, value)) in req.headers.iter().enumerate() {
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            continue;
        }
        let overridden = req
            .headers
            .get(idx.saturating_add(1)..)
            .unwrap_or_default()
            .iter()
            .any(|(later, _)| later.eq_ignore_ascii_case(name));
        if overridden {
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

fn has_chunked_encoding(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding")
            && v.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
    })
}

fn find_content_length(headers: &[(String, String)]) -> Option<usize> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
}

type RawHttpHeadResponse = (u16, Vec<(String, String)>, Vec<u8>);

fn read_response_head(
    stream: &mut HttpStream,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> io::Result<RawHttpHeadResponse> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    loop {
        let header_end = find_header_end(&buf);
        if let Some(end) = header_end {
            let head_bytes = buf.get(..end).unwrap_or_default();
            let (status, headers) = parse_response_head(head_bytes)?;
            let next_start = end.saturating_add(4);
            if status == 100 {
                // Drain `100 Continue` interim response and keep reading the real response.
                buf = buf.get(next_start..).unwrap_or_default().to_vec();
                continue;
            }
            let initial_body = buf.get(next_start..).unwrap_or_default().to_vec();
            return Ok((status, headers, initial_body));
        }
        if buf.len() >= MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response headers exceed 64 KiB limit",
            ));
        }
        let n = read_retry_deadline(stream, &mut tmp, deadline, cancel)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "EOF before HTTP response headers completed",
            ));
        }
        let chunk = tmp.get(..n).unwrap_or_default();
        buf.extend_from_slice(chunk);
    }
}

fn read_response(
    stream: &mut HttpStream,
    max_body_bytes: usize,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> io::Result<HttpResponse> {
    let (status, headers, initial_body) = read_response_head(stream, deadline, cancel)?;

    if status == 204 || status == 304 {
        return Ok(HttpResponse {
            status,
            headers,
            body: Vec::new(),
        });
    }

    let is_chunked = has_chunked_encoding(&headers);
    let content_length = find_content_length(&headers);

    let body = if is_chunked {
        read_chunked_body(stream, initial_body, max_body_bytes, deadline, cancel)?
    } else if let Some(expected_len) = content_length {
        if expected_len > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response exceeds maximum allowed size",
            ));
        }
        read_exact_body(stream, initial_body, expected_len, deadline, cancel)?
    } else {
        read_until_eof_body(stream, initial_body, max_body_bytes, deadline, cancel)?
    };

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn read_retry_deadline(
    stream: &mut HttpStream,
    buf: &mut [u8],
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> io::Result<usize> {
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "HTTP request cancelled",
            ));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP request exceeded total deadline",
            ));
        }
        let remaining = deadline.saturating_duration_since(now);
        let slice = if cancel.is_some() {
            remaining
                .min(Duration::from_millis(250))
                .max(Duration::from_millis(1))
        } else {
            remaining.max(Duration::from_millis(1))
        };
        stream.tcp().set_read_timeout(Some(slice)).ok();
        match stream.read(buf) {
            Ok(n) => return Ok(n),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "HTTP request cancelled",
                    ));
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "HTTP request exceeded total deadline",
                    ));
                }
            }
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
    stream: &mut HttpStream,
    mut body: Vec<u8>,
    expected_len: usize,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
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
        let n = read_retry_deadline(stream, slice, deadline, cancel)?;
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
    stream: &mut HttpStream,
    mut body: Vec<u8>,
    max_body_bytes: usize,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> io::Result<Vec<u8>> {
    if body.len() > max_body_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response body exceeds cap",
        ));
    }
    let mut tmp = [0u8; 8192];
    loop {
        let n = match read_retry_deadline(stream, &mut tmp, deadline, cancel) {
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
    stream: &mut HttpStream,
    mut raw: Vec<u8>,
    max_body_bytes: usize,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    let mut tmp = [0u8; 8192];

    let ensure_bytes = |stream: &mut HttpStream,
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
            let n = read_retry_deadline(stream, tmp, deadline, cancel)?;
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
            let n = read_retry_deadline(stream, &mut tmp, deadline, cancel)?;
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
            // Drain optional HTTP trailer lines through the terminating blank CRLF line.
            let mut trailer_bytes = 0usize;
            loop {
                let t_crlf = loop {
                    let slice = raw.get(cursor..).unwrap_or_default();
                    if let Some(pos) = slice.windows(2).position(|w| w == b"\r\n") {
                        break pos;
                    }
                    if trailer_bytes.saturating_add(slice.len()) > MAX_HEADER_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "chunked HTTP trailers exceed limit",
                        ));
                    }
                    let n = read_retry_deadline(stream, &mut tmp, deadline, cancel)?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "EOF reading chunked HTTP trailers",
                        ));
                    }
                    raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
                };
                cursor = cursor.saturating_add(t_crlf).saturating_add(2);
                trailer_bytes = trailer_bytes.saturating_add(t_crlf).saturating_add(2);
                if trailer_bytes > MAX_HEADER_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "chunked HTTP trailers exceed limit",
                    ));
                }
                if t_crlf == 0 {
                    break;
                }
            }
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
                    let resp = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nX-Trailer: done\r\n\r\n";
                    sock.write_all(resp.as_bytes()).unwrap();
                }
            }
        });

        let r1 = send(
            &HttpRequest::post_json(format!("http://{addr}/first"), b"{}".to_vec())
                .with_header("X-Test", "1")
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(r1.status, 200);
        assert_eq!(r1.text(), r#"{"ok":true}"#);

        let r2 = send(
            &HttpRequest::get(format!("http://{addr}/second"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(r2.status, 200);
        assert_eq!(r2.text(), "hello world");

        server.join().unwrap();
    }

    #[test]
    fn ssrf_policy_blocks_private_loopback_metadata_and_mapped_ipv6() {
        for blocked in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.10",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::10.0.0.1",
        ] {
            let ip: IpAddr = blocked.parse().unwrap();
            assert!(
                validate_ip_against_ssrf(ip, SsrfPolicy::StrictExternal).is_err(),
                "{blocked} should be blocked under StrictExternal"
            );
        }
        assert!(
            validate_ip_against_ssrf("127.0.0.1".parse().unwrap(), SsrfPolicy::AllowLoopback)
                .is_ok()
        );
        assert!(
            validate_ip_against_ssrf("::1".parse().unwrap(), SsrfPolicy::AllowLoopback).is_ok()
        );
        assert!(
            validate_ip_against_ssrf(
                "169.254.169.254".parse().unwrap(),
                SsrfPolicy::AllowLoopback
            )
            .is_err()
        );
        assert!(
            validate_ip_against_ssrf("8.8.8.8".parse().unwrap(), SsrfPolicy::StrictExternal)
                .is_ok()
        );
    }

    #[test]
    fn deduplicates_request_headers_case_insensitively_and_enforces_deadline() {
        let parsed = parse_url("http://127.0.0.1:8080/test").unwrap();
        let req = HttpRequest::post_json("http://127.0.0.1:8080/test", b"{}".to_vec())
            .with_header("content-type", "text/plain")
            .with_header("X-Custom", "first")
            .with_header("x-custom", "second");
        let wire = String::from_utf8(build_request_bytes(&parsed, &req)).unwrap();
        assert_eq!(
            wire.matches("content-type").count() + wire.matches("Content-Type").count(),
            1
        );
        assert!(wire.contains("content-type: text/plain\r\n"));
        assert!(!wire.contains("first"));
        assert!(wire.contains("x-custom: second\r\n"));

        // Slow-drip server that stalls past total deadline must time out cleanly.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 1024];
            let n = sock.read(&mut req_buf).unwrap_or(0);
            assert!(n > 0);
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc")
                .ok();
            thread::sleep(Duration::from_millis(350));
        });
        let err = send(
            &HttpRequest::get(format!("http://{addr}/slow"))
                .with_timeout(Duration::from_millis(120))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        server.join().unwrap();
    }

    #[test]
    fn streaming_chunked_sse_and_interruptible_cancel_work() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 1024];
            let n = sock.read(&mut req_buf).unwrap_or(0);
            assert!(n > 0);
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .unwrap();
            sock.write_all(b"d\r\ndata: hello\n\n\r\n").unwrap();
            sock.flush().unwrap();
            thread::sleep(Duration::from_millis(20));
            sock.write_all(b"e\r\ndata: second\n\n\r\n0\r\n\r\n")
                .unwrap();
        });

        let mut streaming = send_streaming(
            &HttpRequest::get(format!("http://{addr}/sse"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert!(streaming.is_event_stream());
        let mut collected = String::new();
        while let Some(chunk) = streaming
            .read_next_stream_chunk(Duration::from_secs(2), None)
            .unwrap()
        {
            collected.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert_eq!(collected, "data: hello\n\ndata: second\n\n");
        server.join().unwrap();

        // Interruptible abort test.
        let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr2 = listener2.local_addr().unwrap();
        let server2 = thread::spawn(move || {
            let (mut sock, _) = listener2.accept().unwrap();
            let mut req_buf = [0u8; 1024];
            let n = sock.read(&mut req_buf).unwrap_or(0);
            assert!(n > 0);
            thread::sleep(Duration::from_millis(600));
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .ok();
        });
        let flag = Arc::new(AtomicBool::new(false));
        let slot: RequestCancelSlot = Arc::new(Mutex::new(None));
        let flag_w = Arc::clone(&flag);
        let slot_w = Arc::clone(&slot);
        let killer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            flag_w.store(true, Ordering::Release);
            cancel_active_request(&slot_w);
        });
        let started = Instant::now();
        let err = send_interruptible(
            &HttpRequest::get(format!("http://{addr2}/poll"))
                .with_timeout(Duration::from_secs(10))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
            Some(&flag),
            Some(&slot),
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(matches!(
            err.kind(),
            io::ErrorKind::Interrupted
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
        ));
        killer.join().unwrap();
        server2.join().unwrap();
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

    #[test]
    fn parse_url_rejects_spaces_and_control_characters() {
        for bad in [
            "https://example.com/a b",
            "https://example.com/path\tfoo",
            "https://example.com/path\x1ffoo",
            "https://example.com/path\x7ffoo",
            "http://127.0.0.1:8080/hello world",
        ] {
            let err = parse_url(bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "url: {bad:?}");
        }
    }

    #[test]
    fn streaming_chunked_trailers_enforce_max_header_bytes_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 1024];
            let n = sock.read(&mut req_buf).unwrap_or(0);
            assert!(n > 0);
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n",
            )
            .ok();
            let filler = vec![b'X'; 8192];
            for _ in 0..16 {
                if sock.write_all(&filler).is_err() {
                    break;
                }
            }
        });

        let mut stream = send_streaming(
            &HttpRequest::get(format!("http://{addr}/events"))
                .with_timeout(Duration::from_secs(5))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        let err = stream
            .read_next_stream_chunk(Duration::from_secs(2), None)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("trailers"));
        server.join().ok();
    }
}
