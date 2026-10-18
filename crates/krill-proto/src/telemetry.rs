#![forbid(unsafe_code)]
//! 텔레메트리 opt-in(MK-089, T4 §3.4.4·T4-34).
//!
//! - **기본 꺼짐**: 상태 디렉터리에 [`CONSENT_FILE`]이 있을 때만 켜진다. 파일이 없으면
//!   어떤 네트워크 전송도 하지 않는다(완료 조건).
//! - **환경 거부**: `DO_NOT_TRACK`·`KRILL_TELEMETRY_DISABLED`·CI 환경 변수가 참값이면
//!   동의 파일이 있어도 꺼진 것으로 본다(Orca `consent.ts`와 같은 범주, T4 §3.4.4).
//! - **이벤트 상한**: [`MAX_EVENTS`]개 이하. 이름은 [`Event::ALL`]에 고정된다.
//! - **엄격 스키마**: 봉투·페이로드 모두 `deny_unknown_fields`. 프롬프트·경로·명령 문자열·
//!   레포 이름 같은 자유 텍스트는 스키마에 없다(개수·성공 여부·지연·RSS 구간만).
//! - **네트워크 없음(구조적)**: 이 모듈은 소켓·HTTP 코드를 전혀 포함하지 않는다. 전송은
//!   호출자가 넣는 [`Transport`]로만 나가고, 꺼져 있으면 [`emit`]이 트랜스포트를 건드리지
//!   않는다. 실제 HTTPS 엔드포인트 연결(계정·키·수집 서버 결정)은 별도 작업이다.

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// opt-in 표시 파일(상태 디렉터리 바로 아래, 0600). 있으면 동의, 없으면 거부(기본값).
pub const CONSENT_FILE: &str = "telemetry.enabled";
/// 허용하는 이벤트 이름 상한(T4 §3.4.4: 10개 이하로 시작).
pub const MAX_EVENTS: usize = 10;
/// [`Envelope`] 스키마 버전.
pub const SCHEMA_VERSION: u32 = 1;
/// 일반 거부 변수. 값이 참값(아래 [`truthy`])이면 동의 파일과 무관하게 끈다.
pub const DO_NOT_TRACK: &str = "DO_NOT_TRACK";
/// Krill 전용 거부 변수.
pub const KRILL_DISABLED: &str = "KRILL_TELEMETRY_DISABLED";
/// 먼저 보는 거부 변수.
const VETO_VARS: [&str; 2] = [DO_NOT_TRACK, KRILL_DISABLED];
/// CI 환경 변수(T4 §3.4.4). 하나라도 참값이면 끈다.
const CI_VARS: [&str; 8] = [
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "JENKINS_URL",
    "TEAMCITY_VERSION",
    "BUILDKITE",
    "CIRCLECI",
    "TRAVIS",
];

/// 동의 판정에 쓰는 환경 값. [`Env::live`]는 실제 프로세스 환경을 읽고, 테스트는
/// [`Env::stub`]으로 고정 값을 넣는다(프로세스 전역 환경을 건드리지 않는다).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Env {
    entries: Vec<(String, String)>,
}

impl Env {
    /// [`VETO_VARS`]·[`CI_VARS`]에 든 변수만 읽는다.
    #[must_use]
    pub fn live() -> Self {
        let mut entries = Vec::new();
        for name in VETO_VARS.iter().chain(CI_VARS.iter()) {
            if let Some(value) = std::env::var_os(name) {
                entries.push(((*name).to_owned(), value.to_string_lossy().into_owned()));
            }
        }
        Self { entries }
    }

    /// 테스트·진단용 고정 환경.
    #[must_use]
    pub fn stub(pairs: &[(&str, &str)]) -> Self {
        Self {
            entries: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// 참값이면 참. 빈 문자열과 `0`, `false`, `no`, `off`(대소문자·앞뒤 공백 무시)는 거짓이다.
#[must_use]
pub fn truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// 동의 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Enabled,
    Disabled(Reason),
}

/// 꺼진 이유. `EnvVeto`는 원인이 된 환경 변수 이름이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NoConsentFile,
    EnvVeto(&'static str),
}

impl Consent {
    /// 켜져 있으면 참.
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// 꺼진 원인이 환경 변수면 그 이름.
    #[must_use]
    pub fn veto_var(self) -> Option<&'static str> {
        match self {
            Self::Disabled(Reason::EnvVeto(name)) => Some(name),
            Self::Enabled | Self::Disabled(Reason::NoConsentFile) => None,
        }
    }
}

/// 현재 동의 상태. 동의 파일이 있어고 거부 변수가 하나라도 참값이면 꺼짐이 이긴다.
#[must_use]
pub fn consent(state: Option<&Path>, env: &Env) -> Consent {
    for name in VETO_VARS.iter().chain(CI_VARS.iter()) {
        if env.get(name).is_some_and(truthy) {
            return Consent::Disabled(Reason::EnvVeto(name));
        }
    }
    if state.is_some_and(|dir| dir.join(CONSENT_FILE).is_file()) {
        Consent::Enabled
    } else {
        Consent::Disabled(Reason::NoConsentFile)
    }
}

/// `state`에 동의 파일을 만든다(멱등). 거부 변수가 켜져 있어도 파일은 남긴다(변수를
/// 끄면 다시 묻지 않고 켜진다).
///
/// # Errors
/// 상태 디렉터리를 만들거나 파일을 쓰지 못하면 OS 오류.
pub fn enable(state: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)?;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(state.join(CONSENT_FILE))?
        .write_all(b"")?;
    Ok(())
}

/// 동의 파일을 지운다(없어도 성공).
///
/// # Errors
/// 없음 외의 삭제 실패는 OS 오류.
pub fn disable(state: &Path) -> io::Result<()> {
    match fs::remove_file(state.join(CONSENT_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// 허용된 이벤트 이름(T4 §3.4.4 목록 그대로, 7개).
impl Event {
    pub const ALL: [&'static str; 7] = [
        "app_opened",
        "daemon_reattached",
        "agent_status_source",
        "agent_waiting_missed",
        "crash_detected",
        "update_result",
        "memory_sample",
    ];

    /// 이 이벤트의 이름. [`Self::ALL`] 안에 있다.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::AppOpened(_) => "app_opened",
            Self::DaemonReattached(_) => "daemon_reattached",
            Self::AgentStatusSource(_) => "agent_status_source",
            Self::AgentWaitingMissed(_) => "agent_waiting_missed",
            Self::CrashDetected(_) => "crash_detected",
            Self::UpdateResult(_) => "update_result",
            Self::MemorySample(_) => "memory_sample",
        }
    }
}

/// 텔레메트리 이벤트. 새 변형을 더하면 [`Event::ALL`]에도 이름을 등록하고
/// [`MAX_EVENTS`]를 넘지 않아야 한다(테스트가 막는다).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "name", content = "data")]
pub enum Event {
    AppOpened(AppOpened),
    DaemonReattached(DaemonReattached),
    AgentStatusSource(AgentStatusSource),
    AgentWaitingMissed(AgentWaitingMissed),
    CrashDetected(CrashDetected),
    UpdateResult(UpdateResult),
    MemorySample(MemorySample),
}

/// 앱 실행. 존재 자체가 신호라 필드가 없다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppOpened {}

/// 데몬 재연결(복구 성공 여부, 지연 시간 — T4 §3.4.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonReattached {
    pub recovered: bool,
    pub delay_ms: u32,
}

/// 상태 근거 출처별 개수(L1~L4 비율 — T4 §3.4.4). 출처만 세고 화면 내용·프롬프트는 안 담는다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentStatusSource {
    pub hook: u32,
    pub process: u32,
    pub title: u32,
    pub screen: u32,
}

/// 사용자가 수동으로 정정한 횟수(T4 §3.4.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentWaitingMissed {
    pub corrections: u32,
}

/// 크래시를 낸 프로세스. 메시지·백트레이스는 안 담는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashProcess {
    App,
    Daemon,
    Holder,
    Cli,
}

/// 크래시 감지.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashDetected {
    pub process: CrashProcess,
}

/// 업데이터 결과.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateResult {
    pub success: bool,
}

/// RSS 구간(T4 §3.4.4 "앱·데몬 RSS 구간"). 정확한 바이트는 안 담는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RssBucket {
    #[serde(rename = "0-50MB")]
    Mb0To50,
    #[serde(rename = "50-100MB")]
    Mb50To100,
    #[serde(rename = "100-150MB")]
    Mb100To150,
    #[serde(rename = "150-200MB")]
    Mb150To200,
    #[serde(rename = "200MB+")]
    Mb200Plus,
}

/// 메모리 표본.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySample {
    pub app: RssBucket,
    pub daemon: RssBucket,
}

/// 전송 봉투. `event` 안의 이름·값만 바뀌고 키는 고정된다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u32,
    pub app_version: String,
    pub os: String,
    pub arch: String,
    pub ts_unix: u64,
    pub event: Event,
}

/// `event`를 봉투에 넣는다. 시각은 호출자가 준다(테스트 결정성).
#[must_use]
pub fn envelope(event: Event, ts_unix: u64) -> Envelope {
    Envelope {
        v: SCHEMA_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        ts_unix,
        event,
    }
}

/// 지금 시각(UNIX 초). 실패하면 0.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 전송 통로. 구현이 실패를 삼킨다(텔레메트리가 앱·데몬 동작에 영향을 주지 않는다).
/// 이 모듈에는 실제 네트워크 구현이 없다 — HTTPS 엔드포인트가 정해지면 별도 이슈에서
/// `Transport` 구현으로 붙인다.
pub trait Transport {
    fn send(&mut self, body: &str);
}

/// 보낸 본문을 모아 두는 테스트·진단용 구현.
#[derive(Debug, Default)]
pub struct Recorder {
    pub sent: Vec<String>,
}

impl Transport for Recorder {
    fn send(&mut self, body: &str) {
        self.sent.push(body.to_owned());
    }
}

/// 동의가 켜져 있을 때만 봉투 JSON을 [`Transport`]에 넘긴다. 꺼져 있으면 트랜스포트를
/// 건드리지 않는다(네트워크 전송 0회). 보냈으면 참.
pub fn emit(consent: &Consent, event: Event, ts_unix: u64, transport: &mut impl Transport) -> bool {
    if !consent.enabled() {
        return false;
    }
    let body = serde_json::to_string(&envelope(event, ts_unix)).unwrap_or_default();
    if body.is_empty() {
        return false;
    }
    transport.send(&body);
    true
}

#[cfg(test)]
mod tests;
