//! 오류 보고(opt-in). 텔레메트리 동의(`telemetry.enabled`)와 별개다.
//!
//! - **별도 동의**: `<상태 디렉터리>/error-reporting/consent.json`(0600)이 있을 때만 기록한다. 거부 변수
//!   ([`DISABLED_VAR`], 텔레메트리와 같은 `DO_NOT_TRACK`·CI 변수)가 참값이면 파일이 있어도 끈다.
//! - **닫힌 스키마**: 코드·공개 소스 위치·release/build·OS·아키텍처·멈춤 구간·native 예외/신호만 담는다.
//!   메시지·프롬프트·터미널 출력·경로·계정·호스트 이름은 타입에 없다. 기존 raw 크래시 파일은 대기열에 넣지 않는다.
//! - **네트워크 없음(구조적)**: 이 모듈은 bounded 디스크 대기열까지만 다룬다. HTTPS 전송은 `krill errors flush`가
//!   OS curl로 한다. 앱은 주기적으로 그 CLI를 깨우고, 헤드리스 환경은 직접 실행한다.
//! - **비차단**: 기록은 잠금을 한 번만 시도하고 막히면 버린다. [`capture`]는 로그·네트워크·대기가 없다.

mod consent;
mod file;
pub mod native;
mod outbox;
mod rate;
pub mod runs;
mod types;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use consent::{
    CONSENT_FILE, Consent, DISABLED_VAR, Env, consent_at, consent_with, disable, enable,
};
pub use outbox::{
    FlushOutcome, FlushReport, Reply, UploadLock, flush, record_at, record_with_consent,
};
pub use types::{
    Ack, Arch, Code, ENDPOINT, Event, Exception, HANG_BUCKETS_MS, Kind, MAX_AGE_MS, MAX_ATTEMPTS,
    MAX_BATCH, MAX_BUILD_ID, MAX_EVENT_BYTES, MAX_FLUSH_REQUESTS, MAX_NATIVE_FRAMES, MAX_PENDING,
    MAX_REQUEST_BYTES, MAX_SAFE_OFFSET, MAX_SOURCE_LOC, NativeData, NativeFrame, Os, Process,
    RETRY_MAX_MS, RETRY_MIN_MS, Request, SCHEMA_VERSION, Signal,
};

use types::{hex, valid_source_loc};

/// 오류 보고 디렉터리 이름(상태 디렉터리 바로 아래, 0700). 동의·대기열·잠금 파일이 여기 있다.
pub const DIR: &str = "error-reporting";
/// native 수집기 캐시 디렉터리(오류 보고 디렉터리 아래). 철회·재동의하면 통째로 지운다.
pub const NATIVE_DIR: &str = "native";
/// 앱·데몬 실행 표식 디렉터리(오류 보고 디렉터리 아래). 철회·재동의하면 통째로 지운다.
pub const RUNS_DIR: &str = "runs";

/// `<상태 디렉터리>/error-reporting`.
#[must_use]
pub fn dir(state: &Path) -> PathBuf {
    state.join(DIR)
}

/// 상태 JSON(`krill errors status --json`, 설정 화면).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    /// 동의 파일이 있다(환경 거부와 무관).
    pub consented: bool,
    /// 실제로 기록·전송한다(동의 + 거부 변수 없음).
    pub enabled: bool,
    /// 대기 중인 이벤트 수.
    pub pending: usize,
    /// 거부를 일으킨 환경 변수 이름.
    pub veto: Option<String>,
    /// 동의 시각(UNIX 밀리초).
    #[serde(default)]
    pub enabled_at_ms: Option<u64>,
    /// 큐 전체 재시도 대기 중이면 다음 시도 시각(UNIX 밀리초).
    #[serde(default)]
    pub next_attempt_at_ms: Option<u64>,
}

/// 실제 환경 기준 상태.
///
/// # Errors
/// 없음 외의 동의 파일 읽기 실패.
pub fn status(state: &Path) -> std::io::Result<Status> {
    status_with(state, &Env::live())
}

/// [`status`]와 같지만 환경을 주입한다.
///
/// # Errors
/// 없음 외의 동의 파일 읽기 실패.
pub fn status_with(state: &Path, env: &Env) -> std::io::Result<Status> {
    let consent = consent::read(state)?;
    let veto = env.veto();
    let dir = dir(state);
    let next_attempt_at_ms = consent
        .as_ref()
        .and_then(|c| outbox::next_attempt(&dir, &c.generation, now_ms()));
    Ok(Status {
        consented: consent.is_some(),
        enabled: consent.is_some() && veto.is_none(),
        pending: outbox::pending(&dir),
        veto: veto.map(str::to_owned),
        enabled_at_ms: consent.map(|c| c.enabled_at_ms),
        next_attempt_at_ms,
    })
}

/// 상태 디렉터리에 오류 코드를 기록한다(동의가 있을 때만). 실패는 삼킨다.
/// 잠금은 한 번만 시도하고 로그·네트워크가 없어 panic hook에서도 쓴다. panic이 프로세스 종료를 뜻하지는 않는다.
/// `UiStall`은 가장 짧은 구간(500ms 이상), `NativeCrash`는 예외·신호 미상으로 채운다 — 값을 알면
/// [`capture_ui_stall`]이나 [`Event::new`] + [`record_at`]을 쓴다.
pub fn capture(process: Process, code: Code, file: &str, line: u32) {
    if let Some(state) = capture_state_dir() {
        capture_at(&state, process, code, file, line);
    }
}

/// [`capture`]와 같지만 상태 디렉터리를 받는다.
pub fn capture_at(state: &Path, process: Process, code: Code, file: &str, line: u32) {
    capture_with(state, &Env::live(), process, code, None, file, line);
}

/// UI 멈춤을 구간으로 기록한다. 500ms 미만이면 기록하지 않는다. [`capture`]와 같이 실패를 삼킨다.
pub fn capture_ui_stall(process: Process, elapsed: Duration, file: &str, line: u32) {
    let Some(bucket) = hang_bucket_ms(elapsed) else {
        return;
    };
    if let Some(state) = capture_state_dir() {
        let env = Env::live();
        capture_with(
            &state,
            &env,
            process,
            Code::UiStall,
            Some(bucket),
            file,
            line,
        );
    }
}

/// [`capture`]가 쓸 상태 디렉터리. 테스트 프로세스는 기본(실제 사용자) 상태 폴더에 쓰지 않는다 — 동의가 있으면
/// 테스트가 일부러 낸 실패가 실제 대기열로 올라간다(2026-10-28 `update_failed`). Krill 세션 안 셸은
/// `KRILL_STATE_DIR`에 기본 경로를 그대로 물려받으므로 오버라이드 여부가 아니라 경로로 가른다.
fn capture_state_dir() -> Option<PathBuf> {
    let state = crate::daemon::state_dir()?;
    let get = |key: &str| std::env::var_os(key);
    if is_test_process(get, std::env::current_exe().ok().as_deref())
        && crate::daemon::default_state_dir().is_some_and(|default| default == state)
    {
        return None;
    }
    Some(state)
}

/// nextest(`NEXTEST`)·`cargo test`·`cargo run --example`(`CARGO_MANIFEST_DIR`)이 띄웠거나, 직접 실행한 cargo
/// 테스트 바이너리(`target/<프로필>/deps/<이름>-<해시>`)다. 설치본은 어느 것에도 해당하지 않는다.
fn is_test_process(get: impl Fn(&str) -> Option<std::ffi::OsString>, exe: Option<&Path>) -> bool {
    get("NEXTEST").is_some()
        || get("CARGO_MANIFEST_DIR").is_some()
        || exe
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|dir| dir == "deps")
}

/// 동의·거부를 먼저 보고(꺼져 있으면 이벤트를 만들지 않는다) 종류별 필수 필드를 채워 기록한다.
fn capture_with(
    state: &Path,
    env: &Env,
    process: Process,
    code: Code,
    hang_bucket: Option<u32>,
    file: &str,
    line: u32,
) {
    let Ok(Some(consent)) = consent_with(state, env) else {
        return;
    };
    let Ok(mut event) = Event::new(process, code, source_location(file, line)) else {
        return;
    };
    match event.kind {
        Kind::UiHang => event.hang_bucket_ms = hang_bucket.or(HANG_BUCKETS_MS.first().copied()),
        Kind::NativeCrash => event.native = Some(NativeData::unknown()),
        Kind::OperationError | Kind::Panic | Kind::AbnormalExit => {}
    }
    record_with_consent(state, event, &consent).ok();
}

/// 멈춘 시간이 속한 구간(넘은 가장 큰 [`HANG_BUCKETS_MS`] 값). 500ms 미만이면 `None`.
#[must_use]
pub fn hang_bucket_ms(elapsed: Duration) -> Option<u32> {
    let ms = elapsed.as_millis();
    HANG_BUCKETS_MS
        .iter()
        .rev()
        .copied()
        .find(|bucket| ms >= u128::from(*bucket))
}

/// 공개 소스 위치 `crates/krill*/src/…/*.rs:line`. 역슬래시는 `/`로 바꾼다. 절대·부모·숨김·공개 크레이트
/// 밖 경로와 0행, 200바이트 초과는 `None`.
#[must_use]
pub fn source_location(file: &str, line: u32) -> Option<String> {
    let location = format!("{}:{line}", file.replace('\\', "/"));
    valid_source_loc(&location).then_some(location)
}

/// 지금 시각(UNIX 밀리초). 시계가 1970년 이전이면 0.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// 결정적 이벤트 ID: `SHA-256("krill-error-reporting/v1\0" ‖ len(domain) ‖ domain ‖ bytes)`의 앞 128비트(소문자 16진수 32자).
/// 같은 사고를 다시 수집해도 같은 ID가 돼 대기열·서버에서 중복으로 걸러진다. 원본 바이트는 밖으로 나가지 않는다.
#[must_use]
pub fn event_id(domain: &str, bytes: &[u8]) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(b"krill-error-reporting/v1\0");
    ctx.update(
        &u64::try_from(domain.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    ctx.update(domain.as_bytes());
    ctx.update(bytes);
    let digest = ctx.finish();
    hex(digest.as_ref().get(..16).unwrap_or_default())
}

#[cfg(test)]
mod tests;
