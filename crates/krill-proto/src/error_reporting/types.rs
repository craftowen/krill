//! 와이어 타입(`schema_version` 1). 닫힌 enum과 상한 검증만 있다. 자유 텍스트 필드는 없다.

use std::fmt::Write as _;
use std::io;

use ring::rand::SecureRandom as _;
use serde::{Deserialize, Serialize};

/// 와이어 스키마 버전.
pub const SCHEMA_VERSION: u32 = 1;
/// 기본 수집 엔드포인트.
pub const ENDPOINT: &str = "https://errors.krill.page/v1/events";
/// 요청 하나의 이벤트 상한.
pub const MAX_BATCH: usize = 16;
/// 요청 본문 상한(바이트). 응답 본문도 이 안에서만 읽는다.
pub const MAX_REQUEST_BYTES: usize = 65_536;
/// 직렬화한 이벤트 하나의 상한(바이트). 대기열 파일 하나의 상한이기도 하다.
pub const MAX_EVENT_BYTES: usize = 4096;
/// native 프레임 상한.
pub const MAX_NATIVE_FRAMES: usize = 16;
/// 서버가 JSON 숫자로 정확히 읽는 정수 상한(2^53 - 1).
pub const MAX_SAFE_OFFSET: u64 = 9_007_199_254_740_991;
/// 대기열 상한(건).
pub const MAX_PENDING: usize = 64;
/// 대기열 보존 기간(밀리초, 7일).
pub const MAX_AGE_MS: u64 = 604_800_000;
/// 큐 전체 재시도 간격 하한(밀리초, 60초).
pub const RETRY_MIN_MS: u64 = 60_000;
/// 큐 전체 재시도 간격 상한(밀리초, 24시간).
pub const RETRY_MAX_MS: u64 = 86_400_000;
/// 이벤트별 전송 시도 상한.
pub const MAX_ATTEMPTS: u32 = 10;
/// `flush` 한 번의 HTTP 요청 상한.
pub const MAX_FLUSH_REQUESTS: usize = 4;
/// `source_loc` 상한(바이트).
pub const MAX_SOURCE_LOC: usize = 200;
/// `build_id` 상한(바이트).
pub const MAX_BUILD_ID: usize = 80;
/// `release` 상한(바이트).
const MAX_RELEASE: usize = 64;
/// UI 멈춤 구간(밀리초). 각 값은 "이 시간 이상 멈춤"을 뜻한다.
pub const HANG_BUCKETS_MS: [u32; 4] = [500, 1500, 5000, 30_000];

/// 이벤트를 낸 프로세스.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Process {
    #[serde(rename = "krill-app")]
    App,
    #[serde(rename = "krilld")]
    Daemon,
    #[serde(rename = "krill-cli")]
    Cli,
    #[serde(rename = "krill-holder")]
    Holder,
}

impl Process {
    pub const ALL: [Self; 4] = [Self::App, Self::Daemon, Self::Cli, Self::Holder];

    /// 와이어 이름.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "krill-app",
            Self::Daemon => "krilld",
            Self::Cli => "krill-cli",
            Self::Holder => "krill-holder",
        }
    }
}

/// 이벤트 종류. [`Code::kind`]가 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    OperationError,
    Panic,
    UiHang,
    NativeCrash,
    AbnormalExit,
}

/// 오류 코드(닫힌 목록). 새 코드는 서버 스키마와 함께 바꾼다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    SessionStartFailed,
    DaemonListenerFailed,
    SchedulerFailed,
    RecordingFailed,
    UpdateFailed,
    DaemonConnectFailed,
    Panic,
    UiStall,
    NativeCrash,
    UncleanExit,
}

impl Code {
    pub const ALL: [Self; 10] = [
        Self::SessionStartFailed,
        Self::DaemonListenerFailed,
        Self::SchedulerFailed,
        Self::RecordingFailed,
        Self::UpdateFailed,
        Self::DaemonConnectFailed,
        Self::Panic,
        Self::UiStall,
        Self::NativeCrash,
        Self::UncleanExit,
    ];

    /// 이 코드가 속한 종류. 서버도 같은 짝만 받는다.
    #[must_use]
    pub fn kind(self) -> Kind {
        match self {
            Self::SessionStartFailed
            | Self::DaemonListenerFailed
            | Self::SchedulerFailed
            | Self::RecordingFailed
            | Self::UpdateFailed
            | Self::DaemonConnectFailed => Kind::OperationError,
            Self::Panic => Kind::Panic,
            Self::UiStall => Kind::UiHang,
            Self::NativeCrash => Kind::NativeCrash,
            Self::UncleanExit => Kind::AbnormalExit,
        }
    }
}

/// 운영체제.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Os {
    #[serde(rename = "macos")]
    Macos,
    #[serde(rename = "windows")]
    Windows,
    #[serde(rename = "linux")]
    Linux,
}

impl Os {
    /// 이 빌드의 OS. 목록 밖 OS면 `None`(이벤트를 만들지 않는다).
    #[must_use]
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Macos)
        } else if cfg!(windows) {
            Some(Self::Windows)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else {
            None
        }
    }
}

/// CPU 아키텍처. 목록 밖은 `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arch {
    #[serde(rename = "aarch64")]
    Aarch64,
    #[serde(rename = "x86_64")]
    X86_64,
    #[serde(rename = "x86")]
    X86,
    #[serde(rename = "arm")]
    Arm,
    #[serde(rename = "other")]
    Other,
}

impl Arch {
    /// 이 빌드의 아키텍처.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::Aarch64
        } else if cfg!(target_arch = "x86_64") {
            Self::X86_64
        } else if cfg!(target_arch = "x86") {
            Self::X86
        } else if cfg!(target_arch = "arm") {
            Self::Arm
        } else {
            Self::Other
        }
    }
}

/// macOS Mach 예외 종류(닫힌 목록). 모르는 값은 `UNKNOWN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Exception {
    #[serde(rename = "EXC_BAD_ACCESS")]
    BadAccess,
    #[serde(rename = "EXC_BAD_INSTRUCTION")]
    BadInstruction,
    #[serde(rename = "EXC_ARITHMETIC")]
    Arithmetic,
    #[serde(rename = "EXC_BREAKPOINT")]
    Breakpoint,
    #[serde(rename = "EXC_CRASH")]
    Crash,
    #[serde(rename = "EXC_RESOURCE")]
    Resource,
    #[serde(rename = "EXC_GUARD")]
    Guard,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

impl Exception {
    pub const ALL: [Self; 8] = [
        Self::BadAccess,
        Self::BadInstruction,
        Self::Arithmetic,
        Self::Breakpoint,
        Self::Crash,
        Self::Resource,
        Self::Guard,
        Self::Unknown,
    ];

    /// 와이어 이름(`EXC_BAD_ACCESS` 등).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadAccess => "EXC_BAD_ACCESS",
            Self::BadInstruction => "EXC_BAD_INSTRUCTION",
            Self::Arithmetic => "EXC_ARITHMETIC",
            Self::Breakpoint => "EXC_BREAKPOINT",
            Self::Crash => "EXC_CRASH",
            Self::Resource => "EXC_RESOURCE",
            Self::Guard => "EXC_GUARD",
            Self::Unknown => "UNKNOWN",
        }
    }

    /// 이름이 정확히 같은 값, 없으면 [`Self::Unknown`].
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str() == name)
            .unwrap_or(Self::Unknown)
    }
}

/// 종료 신호(닫힌 목록). 모르는 값은 `UNKNOWN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Signal {
    #[serde(rename = "SIGSEGV")]
    Segv,
    #[serde(rename = "SIGABRT")]
    Abrt,
    #[serde(rename = "SIGBUS")]
    Bus,
    #[serde(rename = "SIGILL")]
    Ill,
    #[serde(rename = "SIGFPE")]
    Fpe,
    #[serde(rename = "SIGTRAP")]
    Trap,
    #[serde(rename = "SIGKILL")]
    Kill,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

impl Signal {
    pub const ALL: [Self; 8] = [
        Self::Segv,
        Self::Abrt,
        Self::Bus,
        Self::Ill,
        Self::Fpe,
        Self::Trap,
        Self::Kill,
        Self::Unknown,
    ];

    /// 와이어 이름(`SIGSEGV` 등).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Segv => "SIGSEGV",
            Self::Abrt => "SIGABRT",
            Self::Bus => "SIGBUS",
            Self::Ill => "SIGILL",
            Self::Fpe => "SIGFPE",
            Self::Trap => "SIGTRAP",
            Self::Kill => "SIGKILL",
            Self::Unknown => "UNKNOWN",
        }
    }

    /// 이름이 정확히 같은 값, 없으면 [`Self::Unknown`].
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == name)
            .unwrap_or(Self::Unknown)
    }
}

/// native 프레임 하나. `image_uuid`는 검증된 Krill 실행 파일 이미지만 넣는다(초기 수집기는 빈 목록).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFrame {
    pub image_uuid: String,
    pub offset: u64,
}

/// native 크래시 요약. 원본 보고서는 담지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeData {
    pub exception: Exception,
    pub signal: Signal,
    pub frames: Vec<NativeFrame>,
}

impl NativeData {
    /// 예외·신호를 모르는 요약(프레임 없음).
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            exception: Exception::Unknown,
            signal: Signal::Unknown,
            frames: Vec::new(),
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.frames.len() > MAX_NATIVE_FRAMES {
            return Err(invalid("native.frames"));
        }
        if self
            .frames
            .iter()
            .any(|f| !is_hex32(&f.image_uuid) || f.offset > MAX_SAFE_OFFSET)
        {
            return Err(invalid("native.frames"));
        }
        Ok(())
    }
}

/// 오류 이벤트. 필드 이름·값은 와이어 그대로다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "와이어 필드 이름(event_id) 그대로"
)]
pub struct Event {
    pub event_id: String,
    pub process: Process,
    pub kind: Kind,
    pub error_code: Code,
    pub release: String,
    pub build_id: String,
    pub os: Os,
    pub arch: Arch,
    pub source_loc: Option<String>,
    pub hang_bucket_ms: Option<u32>,
    pub native: Option<NativeData>,
}

impl Event {
    /// 무작위 128비트 ID와 이 빌드의 release·build·OS·아키텍처로 이벤트를 만든다. `kind`는 `code`가 정한다.
    /// `UiHang`·`NativeCrash`는 호출자가 `hang_bucket_ms`·`native`를 채운 뒤 기록한다.
    ///
    /// # Errors
    /// 목록 밖 OS, 공개 위치 형식이 아닌 `source_loc`, 난수 실패.
    pub fn new(process: Process, code: Code, source_loc: Option<String>) -> io::Result<Self> {
        let os = Os::current().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "error reporting: unsupported OS",
            )
        })?;
        if source_loc.as_deref().is_some_and(|s| !valid_source_loc(s)) {
            return Err(invalid("source_loc"));
        }
        Ok(Self {
            event_id: random_id()?,
            process,
            kind: code.kind(),
            error_code: code,
            release: env!("CARGO_PKG_VERSION").to_owned(),
            build_id: crate::BUILD.to_owned(),
            os,
            arch: Arch::current(),
            source_loc,
            hang_bucket_ms: None,
            native: None,
        })
    }

    /// 와이어 계약을 모두 검사한다: ID 형식, `kind`·`error_code` 짝, 종류별 필수·금지 필드, 상한.
    ///
    /// # Errors
    /// 어긋난 필드 이름을 담은 `InvalidData`.
    pub fn validate(&self) -> io::Result<()> {
        if !is_hex32(&self.event_id) {
            return Err(invalid("event_id"));
        }
        if self.kind != self.error_code.kind() {
            return Err(invalid("kind"));
        }
        if !valid_release(&self.release) {
            return Err(invalid("release"));
        }
        if !valid_build_id(&self.build_id) {
            return Err(invalid("build_id"));
        }
        if self
            .source_loc
            .as_deref()
            .is_some_and(|s| !valid_source_loc(s))
        {
            return Err(invalid("source_loc"));
        }
        let hang_ok = match self.hang_bucket_ms {
            Some(ms) => self.kind == Kind::UiHang && HANG_BUCKETS_MS.contains(&ms),
            None => self.kind != Kind::UiHang,
        };
        if !hang_ok {
            return Err(invalid("hang_bucket_ms"));
        }
        match (&self.native, self.kind) {
            (Some(native), Kind::NativeCrash) => native.validate()?,
            (None, kind) if kind != Kind::NativeCrash => {}
            _ => return Err(invalid("native")),
        }
        if serde_json::to_vec(self).map_err(io::Error::other)?.len() > MAX_EVENT_BYTES {
            return Err(invalid("event size"));
        }
        Ok(())
    }
}

/// 업로드 요청 본문.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u32,
    pub events: Vec<Event>,
}

/// 200 응답 본문. 요청한 ID만, 겹치지 않게 담겨야 유효하다([`Ack::parse`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub schema_version: u32,
    pub accepted_ids: Vec<String>,
    pub duplicate_ids: Vec<String>,
}

impl Ack {
    /// `requested`에 대한 ACK면 `Some`. 상한 초과·파싱 실패·버전 불일치·요청하지 않은 ID·
    /// 겹치거나 되풀이된 ID는 `None`(호출자는 묶음 전체를 남긴다).
    #[must_use]
    pub fn parse(body: &[u8], requested: &[String]) -> Option<Self> {
        if body.len() > MAX_REQUEST_BYTES {
            return None;
        }
        let ack: Self = serde_json::from_slice(body).ok()?;
        if ack.schema_version != SCHEMA_VERSION {
            return None;
        }
        let mut seen: Vec<&str> = Vec::new();
        for id in ack.accepted_ids.iter().chain(&ack.duplicate_ids) {
            if !requested.iter().any(|r| r == id) || seen.contains(&id.as_str()) {
                return None;
            }
            seen.push(id);
        }
        Some(ack)
    }

    /// ACK된 ID(수락 + 중복).
    pub fn acked(&self) -> impl Iterator<Item = &str> {
        self.accepted_ids
            .iter()
            .chain(&self.duplicate_ids)
            .map(String::as_str)
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid error report field: {what}"),
    )
}

/// 소문자 16진수.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        // `String`에 쓰는 `write!`는 실패하지 않는다.
        write!(out, "{b:02x}").ok();
    }
    out
}

/// 정확히 32자 소문자 16진수.
pub(crate) fn is_hex32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// 무작위 128비트 ID(32자 소문자 16진수).
pub(crate) fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("error reporting: system randomness unavailable"))?;
    Ok(hex(&bytes))
}

/// `MAJOR.MINOR.PATCH[-PRE]`(선행 0 없는 숫자, `PRE`는 `[0-9A-Za-z-]` 점 구분, 빌드 메타데이터 없음).
pub(crate) fn valid_release(s: &str) -> bool {
    let numeric = |p: &str| {
        !p.is_empty()
            && p.len() <= 9
            && p.bytes().all(|b| b.is_ascii_digit())
            && (p == "0" || !p.starts_with('0'))
    };
    let (core, pre) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };
    let mut parts = core.split('.');
    let core_ok = parts.next().is_some_and(numeric)
        && parts.next().is_some_and(numeric)
        && parts.next().is_some_and(numeric)
        && parts.next().is_none();
    s.len() <= MAX_RELEASE
        && core_ok
        && pre.is_none_or(|pre| {
            pre.split('.').all(|id| {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        })
}

/// 컴파일 상수 빌드 식별자: 80바이트 이하 `[A-Za-z0-9.+-]`.
pub(crate) fn valid_build_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_BUILD_ID
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// 공개 소스 위치 `crates/krill*/src/**.rs:line[:column]`(200바이트 이하, 절대·부모·숨김 경로 없음).
pub(crate) fn valid_source_loc(s: &str) -> bool {
    if s.len() > MAX_SOURCE_LOC {
        return false;
    }
    let mut parts = s.split(':');
    let (Some(path), Some(line)) = (parts.next(), parts.next()) else {
        return false;
    };
    let column = parts.next();
    parts.next().is_none()
        && valid_position(line)
        && column.is_none_or(valid_position)
        && valid_source_path(path)
}

/// 1 이상 `u32` 십진수(선행 0 없음).
fn valid_position(s: &str) -> bool {
    !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok()
}

fn valid_source_path(path: &str) -> bool {
    let segment = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    let mut segments = path.split('/');
    let (Some("crates"), Some(krate), Some("src")) =
        (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    let crate_ok = krate.starts_with("krill")
        && krate
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let rest: Vec<&str> = segments.collect();
    let Some((file, dirs)) = rest.split_last() else {
        return false;
    };
    crate_ok && dirs.iter().all(|d| segment(d)) && file.strip_suffix(".rs").is_some_and(segment)
}
