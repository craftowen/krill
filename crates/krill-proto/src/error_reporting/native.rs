//! macOS 진단 보고서(`.ips`) 수집기와 남은 실행 표식 회수. 다음 정상 실행·주기 유지보수(`krill errors flush`)에서만
//! 돈다. 상주 reporter·시그널 처리기·holder 코드는 없다.
//!
//! - `~/Library/Logs/DiagnosticReports` 맨 위 항목 중 Krill 실행 파일 이름(`<이름>-<날짜>….ips`)인 것만 연다.
//!   다른 앱의 보고서는 열지 않는다. `O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`로 열고 일반 파일·크기·dev/ino를 다시 본다.
//!   디렉터리 항목·후보·여는 파일·크기·첫 줄 길이·캐시 모두 상한이 있다.
//! - 두 JSON(첫 줄 메타데이터 + 본문)에서 필요한 필드만 typed로 읽는다. 나머지는 serde가 `IgnoredAny`로 건너뛰어
//!   값 트리를 만들지 않는다. `bug_type` 309, 실행 파일 이름과 코드 서명 ID가 Krill 목록과 정확히 일치해야 하고,
//!   메타데이터·본문·예외 어디에서든 `isNonFatal`·`isSimulated`가 거짓이 아니면 버린다. `captureTime`과 파일 생성
//!   시각이 모두 동의 시각 이후여야 한다(mtime은 쓰지 않는다). 하나라도 확인하지 못하면 건너뛴다.
//! - 보내는 것은 예외·신호 닫힌 enum과 검증된 release뿐이다. 같은 실행의 표식이 있으면 그 표식의 release·build를,
//!   없으면 Krill 번들의 `CFBundleShortVersionString`과 build `unknown`을 쓴다. 둘 다 없으면 버린다.
//!   프레임은 빈 배열이다(검증된 Krill 실행 파일 UUID를 도입하기 전까지 제3자 이미지 지문을 보내지 않는다).
//!   incident ID는 결정적 event ID의 해시 입력으로만 쓴다.
//! - 처리한 파일·사고는 해시 토큰만 상한 캐시(`native/seen`)에 둔다. 기록하지 못한 보고서는 다음에 다시 본다.

use std::io;
use std::path::{Path, PathBuf};

use super::Consent;

#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::fmt;
#[cfg(unix)]
use std::fs::{self, DirBuilder, Metadata};
#[cfg(unix)]
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use serde::Deserialize;
#[cfg(unix)]
use serde::de::{self, Deserializer};

#[cfg(unix)]
use super::runs::{self, Stale};
#[cfg(unix)]
use super::types::is_hex32;
#[cfg(unix)]
use super::{Code, Event, Exception, NativeData, Process, Signal};

/// 진단 보고서 디렉터리에서 보는 항목 상한.
#[cfg(unix)]
const MAX_DIR_ENTRIES: usize = 4096;
/// 이름으로 고른 후보 중 최신 몇 개만 본다.
#[cfg(unix)]
const MAX_CANDIDATES: usize = 64;
/// 한 번에 여는 보고서 상한.
#[cfg(unix)]
const MAX_OPENED: usize = 8;
/// 보고서 파일 상한(바이트, 2MiB). 넘으면 읽지 않는다.
#[cfg(unix)]
const MAX_REPORT_BYTES: u64 = 2_097_152;
/// 첫 줄 메타데이터 상한(바이트).
#[cfg(unix)]
const MAX_HEADER_BYTES: usize = 16_384;
/// 보고서 파일 이름 상한(바이트).
#[cfg(unix)]
const MAX_NAME_BYTES: usize = 160;
/// 시각 문자열 상한(바이트).
#[cfg(unix)]
const MAX_TIME_BYTES: usize = 64;
/// 보고 시각이 이보다 오래되면 버린다(밀리초, 7일 — 대기열 보존 기간과 같다).
#[cfg(unix)]
const MAX_REPORT_AGE_MS: u64 = 604_800_000;
/// 보고 시각이 지금보다 이만큼 넘게 미래면 이번에는 건너뛴다(밀리초, 5분).
#[cfg(unix)]
const FUTURE_SLACK_MS: u64 = 300_000;
/// 처리 토큰 캐시 상한(개). 넘치면 오래된 것부터 버린다.
#[cfg(unix)]
const MAX_SEEN: usize = 512;
/// 캐시 파일 상한: 머리말 7바이트 + 토큰 [`MAX_SEEN`]개 × 33바이트.
#[cfg(unix)]
const MAX_SEEN_BYTES: usize = 16_903;
#[cfg(unix)]
const SEEN_FILE: &str = "seen";
#[cfg(unix)]
const SEEN_MAGIC: &str = "KSEEN1";
#[cfg(unix)]
const NATIVE_DOMAIN: &str = "krill.error-reporting.native.v1";
#[cfg(unix)]
const FILE_DOMAIN: &str = "krill.error-reporting.native-file.v1";
#[cfg(unix)]
const CRASH_BUG_TYPE: &str = "309";
/// 앱 번들 ID(`packaging/bundle.sh`).
#[cfg(unix)]
const BUNDLE_ID: &str = "dev.krill.app";
/// 보고서로 build를 확인하지 못했을 때 보내는 값.
#[cfg(unix)]
const UNKNOWN_BUILD: &str = "unknown";

/// Krill 실행 파일 이름 → 프로세스·코드 서명 ID(`packaging/bundle.sh`의 `--identifier`). 순서대로 이름을 맞춘다.
#[cfg(unix)]
const KNOWN: [(&str, Process, &str); 4] = [
    ("krill-app", Process::App, "dev.krill.app"),
    ("krilld", Process::Daemon, "dev.krill.app.krilld"),
    ("krill-holder", Process::Holder, "dev.krill.app.holder"),
    ("krill", Process::Cli, "dev.krill.app.cli"),
];

/// bounded `.ips` 스캔과 남은 실행 표식 회수를 한 유지보수 경계에서 처리하고 대기열에 넣은 이벤트 수를 돌려준다.
/// 현재 동의(환경 거부 포함)가 없으면 아무것도 읽지 않고 0이다. 파일 하나의 실패는 그 파일만 건너뛴다.
///
/// # Errors
/// 동의 파일 읽기 실패.
pub fn collect(state: &Path) -> io::Result<usize> {
    let Some(consent) = super::consent_at(state)? else {
        return Ok(0);
    };
    Ok(collect_with(
        state,
        &consent,
        reports_dir().as_deref(),
        super::now_ms(),
    ))
}

/// 사용자 진단 보고서 디렉터리(macOS만).
fn reports_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        crate::os::home_dir()
            .map(|home| home.join("Library").join("Logs").join("DiagnosticReports"))
    } else {
        None
    }
}

/// [`collect`]의 본체. 동의·보고서 디렉터리·시각을 주입한다.
#[cfg(unix)]
fn collect_with(state: &Path, consent: &Consent, reports: Option<&Path>, now_ms: u64) -> usize {
    let mut stale = Stale::scan(state, consent);
    let mut recorded = 0usize;
    if let Some(dir) = reports {
        let mut seen = Seen::load(state);
        let scan = Scan {
            state,
            consent,
            now_ms,
        };
        recorded = scan.reports(dir, &mut seen, &mut stale);
        seen.store(state, consent);
    }
    recorded.saturating_add(runs::settle(state, consent, now_ms, stale))
}

#[cfg(not(unix))]
fn collect_with(state: &Path, consent: &Consent, reports: Option<&Path>, now_ms: u64) -> usize {
    let _ = (state, consent, reports, now_ms);
    0
}

#[cfg(unix)]
struct Scan<'a> {
    state: &'a Path,
    consent: &'a Consent,
    now_ms: u64,
}

/// 보고서 하나의 처리 결과.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// 대기열에 넣었다.
    Recorded,
    /// 다시 볼 필요가 없다(확인 실패·대상 아님·이미 처리함).
    Done,
    /// 이번에는 넣지 못했다(대기열 가득·경합·미래 시각). 다음 유지보수에서 다시 본다.
    Retry,
}

#[cfg(unix)]
impl Scan<'_> {
    fn reports(&self, dir: &Path, seen: &mut Seen, stale: &mut [Stale]) -> usize {
        let Ok(entries) = fs::read_dir(dir) else {
            return 0;
        };
        let mut candidates: Vec<(String, String, Process)> = Vec::new();
        for entry in entries.take(MAX_DIR_ENTRIES) {
            let Ok(entry) = entry else { continue };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if let Some((process, stamp)) = candidate(&name) {
                candidates.push((stamp.to_owned(), name, process));
            }
        }
        // 이름의 날짜 부분으로 최신 보고서부터 본다.
        candidates.sort_unstable_by(|a, b| (&b.0, &b.1).cmp(&(&a.0, &a.1)));
        candidates.truncate(MAX_CANDIDATES);
        let mut opened = 0usize;
        let mut recorded = 0usize;
        for (_, name, process) in candidates {
            if opened >= MAX_OPENED {
                break;
            }
            let path = dir.join(&name);
            // 심볼릭 링크·FIFO·디렉터리는 열지 않는다.
            let Ok(listed) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !listed.file_type().is_file() {
                continue;
            }
            let token = file_token(&listed);
            if seen.contains(&token) {
                continue;
            }
            opened = opened.saturating_add(1);
            match self.examine(&path, &listed, process, seen, stale) {
                Verdict::Recorded => {
                    recorded = recorded.saturating_add(1);
                    seen.insert(token);
                }
                Verdict::Done => seen.insert(token),
                Verdict::Retry => {}
            }
        }
        recorded
    }

    fn examine(
        &self,
        path: &Path,
        listed: &Metadata,
        process: Process,
        seen: &mut Seen,
        stale: &mut [Stale],
    ) -> Verdict {
        let Some((bytes, born_ms)) = read_report(path, listed) else {
            return Verdict::Done;
        };
        let Some(report) = Report::parse(&bytes) else {
            return Verdict::Done;
        };
        drop(bytes);
        if report.process != process {
            return Verdict::Done;
        }
        match report.timing(born_ms, self.consent, self.now_ms) {
            Timing::Accept => {}
            Timing::Later => return Verdict::Retry,
            Timing::Reject => return Verdict::Done,
        }
        let run = report.pid.and_then(|pid| {
            stale
                .iter_mut()
                .find(|run| run.matches(report.process, pid, report.capture_ms, report.launch_ms))
        });
        let event_id = super::event_id(NATIVE_DOMAIN, report.incident.as_bytes());
        if seen.contains(&event_id) {
            if let Some(run) = run {
                run.claim_native();
            }
            return Verdict::Done;
        }
        let build = run.as_deref().map(|run| {
            let (release, build_id) = run.build();
            (release.to_owned(), build_id.to_owned())
        });
        let Some(event) = report.event(event_id.clone(), build) else {
            return Verdict::Done;
        };
        match super::record_with_consent(self.state, event, self.consent) {
            Ok(true) => {
                seen.insert(event_id);
                if let Some(run) = run {
                    run.claim_native();
                }
                Verdict::Recorded
            }
            Ok(false) | Err(_) => Verdict::Retry,
        }
    }
}

/// `<실행 파일>-<숫자로 시작하는 꼬리>.ips` 이름이면 (프로세스, 꼬리). 다른 앱의 보고서는 여기서 걸러 열지 않는다.
#[cfg(unix)]
fn candidate(name: &str) -> Option<(Process, &str)> {
    if name.len() > MAX_NAME_BYTES {
        return None;
    }
    let stem = name.strip_suffix(".ips")?;
    KNOWN.iter().find_map(|(exe, process, _)| {
        let stamp = stem.strip_prefix(exe)?.strip_prefix('-')?;
        stamp
            .starts_with(|c: char| c.is_ascii_digit())
            .then_some((*process, stamp))
    })
}

/// 처리 캐시 키: 디렉터리 항목의 dev·ino·크기·mtime. 내용이나 이름은 담지 않는다.
#[cfg(unix)]
fn file_token(meta: &Metadata) -> String {
    let mut bytes = Vec::with_capacity(40);
    bytes.extend_from_slice(&meta.dev().to_le_bytes());
    bytes.extend_from_slice(&meta.ino().to_le_bytes());
    bytes.extend_from_slice(&meta.size().to_le_bytes());
    bytes.extend_from_slice(&meta.mtime().to_le_bytes());
    bytes.extend_from_slice(&meta.mtime_nsec().to_le_bytes());
    super::event_id(FILE_DOMAIN, &bytes)
}

/// 목록에서 본 그 일반 파일이면(`O_NOFOLLOW|O_NONBLOCK`로 열어 dev/ino를 다시 본다) 상한 안에서 읽고 생성
/// 시각(UNIX 밀리초)을 함께 돌려준다.
#[cfg(unix)]
fn read_report(path: &Path, listed: &Metadata) -> Option<(Vec<u8>, u64)> {
    let file = super::file::open_regular(path).ok()?;
    let meta = file.metadata().ok()?;
    if meta.dev() != listed.dev() || meta.ino() != listed.ino() || meta.len() > MAX_REPORT_BYTES {
        return None;
    }
    let born_ms = meta.created().ok().and_then(epoch_ms)?;
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).ok()?);
    file.take(MAX_REPORT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= MAX_REPORT_BYTES).then_some((bytes, born_ms))
}

#[cfg(unix)]
fn epoch_ms(time: SystemTime) -> Option<u64> {
    u64::try_from(time.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// 보고서에서 확인한 값. 원본 문자열 중 남기는 것은 형식을 검사한 incident ID와 release뿐이다.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Report {
    process: Process,
    incident: String,
    exception: Exception,
    signal: Signal,
    capture_ms: u64,
    launch_ms: Option<u64>,
    pid: Option<u32>,
    release: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Timing {
    Accept,
    /// 보고 시각이 미래다. 시계가 맞춰진 뒤 다시 본다.
    Later,
    Reject,
}

#[cfg(unix)]
impl Report {
    /// 두 JSON 문서(첫 줄 메타데이터, 나머지 본문)를 검사한다. 하나라도 어긋나면 `None`.
    fn parse(bytes: &[u8]) -> Option<Self> {
        if u64::try_from(bytes.len()).ok()? > MAX_REPORT_BYTES {
            return None;
        }
        let newline = bytes
            .iter()
            .take(MAX_HEADER_BYTES)
            .position(|b| *b == b'\n')?;
        let (head, rest) = bytes.split_at_checked(newline)?;
        // serde_json은 문서 뒤에 공백이 아닌 것이 남으면 실패한다: 세 번째 문서나 꼬리 쓰레기는 버린다.
        let header: Header = serde_json::from_slice(head).ok()?;
        let body: Body = serde_json::from_slice(rest).ok()?;
        if header.bug_type != CRASH_BUG_TYPE
            || body
                .bug_type
                .as_deref()
                .is_some_and(|t| t != CRASH_BUG_TYPE)
            || !valid_incident(&header.incident_id)
            || body
                .incident
                .as_deref()
                .is_some_and(|i| i != header.incident_id)
        {
            return None;
        }
        let flags = [
            header.non_fatal,
            header.simulated,
            body.non_fatal,
            body.simulated,
            body.exception.non_fatal,
            body.exception.simulated,
        ];
        if flags.contains(&Flag::True) {
            return None;
        }
        let (_, process, signing_id) = KNOWN.iter().find(|(exe, ..)| *exe == body.proc_name)?;
        if body.code_signing_id != *signing_id {
            return None;
        }
        let capture_ms = ips_time_ms(&body.capture_time)?;
        let launch_ms = match body.proc_launch.as_deref() {
            Some(launch) => Some(ips_time_ms(launch)?),
            None => None,
        };
        let release = body
            .bundle_info
            .filter(|bundle| bundle.identifier.as_deref() == Some(BUNDLE_ID))
            .and_then(|bundle| bundle.version);
        Some(Self {
            process: *process,
            incident: header.incident_id,
            exception: Exception::from_name(&body.exception.kind),
            signal: body
                .exception
                .signal
                .as_deref()
                .map_or(Signal::Unknown, Signal::from_name),
            capture_ms,
            launch_ms,
            pid: body.pid,
            release,
        })
    }

    /// 보고 시각과 파일 생성 시각이 모두 동의 이후이고 보존 기간 안인가.
    fn timing(&self, born_ms: u64, consent: &Consent, now_ms: u64) -> Timing {
        if self.capture_ms < consent.enabled_at_ms || born_ms < consent.enabled_at_ms {
            return Timing::Reject;
        }
        if self.capture_ms > now_ms.saturating_add(FUTURE_SLACK_MS) {
            return Timing::Later;
        }
        if now_ms.saturating_sub(self.capture_ms) > MAX_REPORT_AGE_MS {
            return Timing::Reject;
        }
        Timing::Accept
    }

    /// 전송 이벤트. `build`는 같은 실행 표식의 release·build다. 없으면 번들 release와 build `unknown`을 쓴다.
    fn event(&self, event_id: String, build: Option<(String, String)>) -> Option<Event> {
        let (release, build_id) = match build {
            Some(build) => build,
            None => (self.release.clone()?, UNKNOWN_BUILD.to_owned()),
        };
        let mut event = Event::new(self.process, Code::NativeCrash, None).ok()?;
        event.event_id = event_id;
        event.release = release;
        event.build_id = build_id;
        event.native = Some(NativeData {
            exception: self.exception,
            signal: self.signal,
            frames: Vec::new(),
        });
        event.validate().ok()?;
        Some(event)
    }
}

/// `YYYY-MM-DD HH:MM:SS[.분수] ±HHMM`(`.ips` 시각)을 UNIX 밀리초로 푼다. 분수는 버린다.
#[cfg(unix)]
fn ips_time_ms(text: &str) -> Option<u64> {
    if text.len() > MAX_TIME_BYTES {
        return None;
    }
    let mut parts = text.split(' ');
    let (date, clock, zone) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !zone.starts_with(['+', '-']) {
        return None;
    }
    let secs = crate::time::parse_rfc3339_epoch(&format!("{date}T{clock}{zone}"))?;
    u64::try_from(secs).ok()?.checked_mul(1000)
}

/// `8-4-4-4-12` 16진수 UUID.
#[cfg(unix)]
fn valid_incident(id: &str) -> bool {
    id.len() == 36
        && id.split('-').map(str::len).eq([8, 4, 4, 4, 12])
        && id.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
}

/// 비치명·모의 표시. 거짓(`false`·`0`)만 통과한다. 다른 형식의 값은 문서 전체를 버린다.
#[cfg(unix)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Flag {
    #[default]
    Absent,
    False,
    True,
}

#[cfg(unix)]
impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FlagVisitor;
        impl de::Visitor<'_> for FlagVisitor {
            type Value = Flag;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a boolean or integer flag")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Flag, E> {
                Ok(if v { Flag::True } else { Flag::False })
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Flag, E> {
                Ok(if v == 0 { Flag::False } else { Flag::True })
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Flag, E> {
                Ok(if v == 0 { Flag::False } else { Flag::True })
            }
        }
        deserializer.deserialize_any(FlagVisitor)
    }
}

/// 첫 줄 메타데이터. 모르는 키는 serde가 `IgnoredAny`로 건너뛴다. 같은 키가 두 번 나오면 실패한다.
#[cfg(unix)]
#[derive(Deserialize)]
struct Header {
    bug_type: String,
    incident_id: String,
    #[serde(rename = "is_non_fatal", alias = "isNonFatal", default)]
    non_fatal: Flag,
    #[serde(rename = "is_simulated", alias = "isSimulated", default)]
    simulated: Flag,
}

/// 본문. 경로·스레드·이미지·레지스터·메시지 필드는 선언하지 않아 읽지 않는다. 선택 필드도 있으면 형식이 맞아야
/// 한다(`null`은 거절).
#[cfg(unix)]
#[derive(Deserialize)]
struct Body {
    #[serde(default, deserialize_with = "present")]
    bug_type: Option<String>,
    #[serde(rename = "captureTime")]
    capture_time: String,
    #[serde(rename = "procName")]
    proc_name: String,
    #[serde(rename = "codeSigningID")]
    code_signing_id: String,
    #[serde(default, deserialize_with = "present")]
    pid: Option<u32>,
    #[serde(rename = "procLaunch", default, deserialize_with = "present")]
    proc_launch: Option<String>,
    #[serde(default, deserialize_with = "present")]
    incident: Option<String>,
    exception: ExceptionInfo,
    #[serde(rename = "isNonFatal", alias = "is_non_fatal", default)]
    non_fatal: Flag,
    #[serde(rename = "isSimulated", alias = "is_simulated", default)]
    simulated: Flag,
    #[serde(rename = "bundleInfo", default, deserialize_with = "present")]
    bundle_info: Option<BundleInfo>,
}

#[cfg(unix)]
#[derive(Deserialize)]
struct ExceptionInfo {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "present")]
    signal: Option<String>,
    #[serde(rename = "isNonFatal", alias = "is_non_fatal", default)]
    non_fatal: Flag,
    #[serde(rename = "isSimulated", alias = "is_simulated", default)]
    simulated: Flag,
}

#[cfg(unix)]
#[derive(Deserialize)]
struct BundleInfo {
    #[serde(
        rename = "CFBundleShortVersionString",
        default,
        deserialize_with = "present"
    )]
    version: Option<String>,
    #[serde(rename = "CFBundleIdentifier", default, deserialize_with = "present")]
    identifier: Option<String>,
}

/// 없으면(`default`) `None`, 있으면 `T`여야 한다. `null`이나 다른 형식은 문서 전체를 버린다.
#[cfg(unix)]
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

/// 처리한 파일·사고의 해시 토큰(상한 [`MAX_SEEN`]). 손상되면 빈 캐시로 시작한다(중복은 결정적 ID가 거른다).
#[cfg(unix)]
#[derive(Debug, Default)]
struct Seen {
    tokens: VecDeque<String>,
    dirty: bool,
}

#[cfg(unix)]
impl Seen {
    fn path(state: &Path) -> PathBuf {
        super::dir(state).join(super::NATIVE_DIR).join(SEEN_FILE)
    }

    fn load(state: &Path) -> Self {
        let tokens = super::file::read_bounded(&Self::path(state), MAX_SEEN_BYTES)
            .ok()
            .flatten()
            .and_then(|bytes| Self::decode(&bytes))
            .unwrap_or_default();
        Self {
            tokens,
            dirty: false,
        }
    }

    fn decode(bytes: &[u8]) -> Option<VecDeque<String>> {
        let text = std::str::from_utf8(bytes).ok()?;
        if !text.ends_with('\n') {
            return None;
        }
        let mut lines = text.split_terminator('\n');
        if lines.next()? != SEEN_MAGIC {
            return None;
        }
        let mut tokens = VecDeque::new();
        for line in lines {
            if tokens.len() >= MAX_SEEN || !is_hex32(line) {
                return None;
            }
            tokens.push_back(line.to_owned());
        }
        Some(tokens)
    }

    fn encode(&self) -> String {
        let mut text = String::with_capacity(MAX_SEEN_BYTES);
        text.push_str(SEEN_MAGIC);
        text.push('\n');
        for token in &self.tokens {
            text.push_str(token);
            text.push('\n');
        }
        text
    }

    fn contains(&self, token: &str) -> bool {
        self.tokens.iter().any(|t| t == token)
    }

    fn insert(&mut self, token: String) {
        if self.contains(&token) {
            return;
        }
        if self.tokens.len() >= MAX_SEEN {
            self.tokens.pop_front();
        }
        self.tokens.push_back(token);
        self.dirty = true;
    }

    /// 바뀌었으면 원자적으로 쓴다. 쓴 뒤 동의 파일이 `consent`와 다르면(그 사이 철회·재동의) 되살린 캐시를 지운다.
    fn store(&self, state: &Path, consent: &Consent) {
        if !self.dirty {
            return;
        }
        let native = super::dir(state).join(super::NATIVE_DIR);
        match DirBuilder::new().mode(0o700).create(&native) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return,
        }
        if !super::file::real_dir(&native) {
            return;
        }
        if super::consent::write_atomic(&native, SEEN_FILE, self.encode().as_bytes()).is_err() {
            return;
        }
        if super::consent::read(state).ok().flatten().as_ref() != Some(consent) {
            fs::remove_file(native.join(SEEN_FILE)).ok();
        }
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
