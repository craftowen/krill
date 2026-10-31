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
    Ack, Arch, Code, ENDPOINT, ENDPOINT_VAR, Event, Exception, HANG_BUCKETS_MS, Kind, MAX_AGE_MS,
    MAX_ATTEMPTS, MAX_BATCH, MAX_BUILD_ID, MAX_DETAIL_LEN, MAX_EVENT_BYTES, MAX_FLUSH_REQUESTS,
    MAX_FRAMES, MAX_NATIVE_FRAMES, MAX_PENDING, MAX_REQUEST_BYTES, MAX_SAFE_OFFSET, MAX_SOURCE_LOC,
    NativeData, NativeFrame, Os, Process, RETRY_MAX_MS, RETRY_MIN_MS, Request, SCHEMA_VERSION,
    Signal,
};

use types::{hex, valid_source_loc};

const INSTALL_HASH_DOMAIN: &str = "krill.error-reporting.install-hash.v1";

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

/// 동의 세대 토큰에서 파생한 익명 설치 식별 해시(소문자 16진수 32자).
#[must_use]
pub fn install_hash(consent: &Consent) -> String {
    event_id(INSTALL_HASH_DOMAIN, consent.generation.as_bytes())
}

pub(crate) fn install_hash_with_salt(consent: &Consent, salt: &str) -> String {
    let mut material = Vec::new();
    material.extend_from_slice(salt.as_bytes());
    material.push(b':');
    material.extend_from_slice(consent.generation.as_bytes());
    event_id(INSTALL_HASH_DOMAIN, &material)
}

/// 상태 JSON(`krill errors status --json`, 설정 화면).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    /// 동의 상태(기본 활성화 포함, 환경 거부와 무관).
    pub consented: bool,
    /// 실제로 기록·전송한다(동의 + 거부 변수 없음).
    pub enabled: bool,
    /// 대기 중인 이벤트 수.
    pub pending: usize,
    /// 거부를 일으킨 환경 변수 이름.
    pub veto: Option<String>,
    /// 명시적 동의 시각(UNIX 밀리초). 기본 동의 상태면 `None`.
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
        enabled_at_ms: consent.and_then(|c| (c.enabled_at_ms > 0).then_some(c.enabled_at_ms)),
        next_attempt_at_ms,
    })
}

/// 상태 디렉터리에 오류 코드를 기록한다(동의가 있을 때만). 실패는 삼킨다.
/// 잠금은 한 번만 시도하고 로그·네트워크가 없어 panic hook에서도 쓴다. panic이 프로세스 종료를 뜻하지는 않는다.
/// `UiStall`은 가장 짧은 구간(500ms 이상), `NativeCrash`는 예외·신호 미상으로 채운다 — 값을 알면
/// [`capture_ui_stall`]이나 [`Event::new`] + [`record_at`]을 쓴다.
pub fn capture(process: Process, code: Code, file: &str, line: u32) {
    if let Some(state) = crate::daemon::state_dir() {
        capture_at(&state, process, code, file, line);
    }
}

/// [`capture`]와 같지만 상태 디렉터리를 받는다.
pub fn capture_at(state: &Path, process: Process, code: Code, file: &str, line: u32) {
    capture_with(
        state,
        &Env::live(),
        process,
        code,
        None,
        None,
        &[],
        file,
        line,
    );
}

/// 리댁션된 상세 사유와 공개 소스 프레임(예: panic 백트레이스)을 함께 기록한다.
pub fn capture_detailed(
    process: Process,
    code: Code,
    detail: Option<&str>,
    frames: &[String],
    file: &str,
    line: u32,
) {
    if let Some(state) = crate::daemon::state_dir() {
        capture_detailed_at(&state, process, code, detail, frames, file, line);
    }
}

/// [`capture_detailed`]와 같지만 상태 디렉터리를 받는다.
pub fn capture_detailed_at(
    state: &Path,
    process: Process,
    code: Code,
    detail: Option<&str>,
    frames: &[String],
    file: &str,
    line: u32,
) {
    capture_with(
        state,
        &Env::live(),
        process,
        code,
        None,
        detail,
        frames,
        file,
        line,
    );
}

/// UI 멈춤을 구간으로 기록한다. 500ms 미만이면 기록하지 않는다. [`capture`]와 같이 실패를 삼킨다.
pub fn capture_ui_stall(process: Process, elapsed: Duration, file: &str, line: u32) {
    let Some(bucket) = hang_bucket_ms(elapsed) else {
        return;
    };
    if let Some(state) = crate::daemon::state_dir() {
        let env = Env::live();
        capture_with(
            &state,
            &env,
            process,
            Code::UiStall,
            Some(bucket),
            None,
            &[],
            file,
            line,
        );
    }
}

/// 테스트 프로세스가 실제 사용자(기본) 상태 폴더의 오류 보고를 건드리지 못하게 한다(2026-10-28: krill-app updater
/// 단위 테스트가 실제 대기열에 `update_failed`를 남겼다). 모든 기록·실행 표식·native 수집·전송은 동의 확인
/// ([`consent::read`])을 거치므로 거기서 동의를 없는 것으로 보고, 동의·철회는 거절한다. Krill 세션 안 셸은
/// `KRILL_STATE_DIR`에 기본 경로를 그대로 물려받으므로 오버라이드 여부가 아니라 실제 디렉터리로 가른다.
pub(crate) fn shielded(state: &Path) -> bool {
    is_test_process() && crate::daemon::default_state_dir().is_some_and(|d| same_dir(state, &d))
}

/// 프로덕션 수집 서버(`telemetry.krill.page`·`errors.krill.page`)로 기본 전송을 허용하는 실제 사용자 상태 폴더인가.
/// 임시 `KRILL_STATE_DIR`나 임시 `HOME`으로 띄운 스모크·브라우저·메모리·생존 테스트나 테스트 프로세스가 커스텀 엔드포인트 없이
/// 프로덕션 D1을 오염시키지 못하게 막는다.
#[must_use]
pub fn is_production_state_dir(state: &Path) -> bool {
    !is_test_process()
        && !is_temp_dir(state)
        && crate::daemon::default_state_dir().is_some_and(|d| same_dir(state, &d))
}

fn is_temp_dir(state: &Path) -> bool {
    let resolved = resolve(state);
    let temp = resolve(&std::env::temp_dir());
    if !temp.as_os_str().is_empty() && resolved.starts_with(&temp) {
        return true;
    }
    #[cfg(unix)]
    {
        for prefix in [
            "/tmp",
            "/private/tmp",
            "/var/folders",
            "/private/var/folders",
        ] {
            if resolved.starts_with(prefix) {
                return true;
            }
        }
    }
    false
}

/// 같은 디렉터리인가(`..`·심볼릭 링크 별칭 포함). 아직 없는 폴더도 별칭으로 동의를 만들 수 있으니 글자 비교로
/// 물러서지 않고 [`resolve`]로 비교한다.
fn same_dir(a: &Path, b: &Path) -> bool {
    resolve(a) == resolve(b)
}

/// 존재하는 가장 깊은 조상을 `canonicalize`하고, 없는 나머지 구성 요소는 글자 그대로 붙이며 `.`·`..`만 접는다.
fn resolve(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut rest = Vec::new();
    let mut base = path;
    let mut out = loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            break real;
        }
        match (base.parent(), base.components().next_back()) {
            (Some(parent), Some(last)) => {
                rest.push(last);
                base = parent;
            }
            _ => break PathBuf::new(),
        }
    };
    for part in rest.into_iter().rev() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// 테스트 하네스가 띄운 프로세스(와 그 자식)인가 — `krill_log::is_test_process`와 같은 판정이다(이 크레이트는
/// krill-log에 의존하지 않는다). `NEXTEST`, `cargo test`가 런타임에 두는 `CARGO_BIN_EXE_<이름>`, libtest
/// 실행 파일(`…/deps/<이름>-<해시>`). 일반 `cargo run`·설치본은 아니다. 프로세스 수명 동안 바뀌지 않아 한 번만 본다.
fn is_test_process() -> bool {
    static TEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TEST.get_or_init(|| {
        is_test_process_in(
            std::env::vars_os().map(|(key, _)| key),
            std::env::current_exe().ok().as_deref(),
        )
    })
}

fn is_test_process_in(
    keys: impl IntoIterator<Item = std::ffi::OsString>,
    exe: Option<&Path>,
) -> bool {
    keys.into_iter().any(|key| {
        key == "NEXTEST"
            || key
                .to_str()
                .is_some_and(|key| key.starts_with("CARGO_BIN_EXE_"))
    }) || exe
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "deps")
}

/// 동의·거부를 먼저 보고(꺼져 있으면 이벤트를 만들지 않는다) 종류별 필수 필드를 채워 기록한다.
#[expect(
    clippy::too_many_arguments,
    reason = "내부 기록 진입점으로 테스트 환경·멈춤 구간·상세 사유·프레임을 한곳에서 조합한다"
)]
fn capture_with(
    state: &Path,
    env: &Env,
    process: Process,
    code: Code,
    hang_bucket: Option<u32>,
    detail: Option<&str>,
    frames: &[String],
    file: &str,
    line: u32,
) {
    let Ok(Some(consent)) = consent_with(state, env) else {
        return;
    };
    let loc = source_location(file, line)
        .or_else(|| frames.first().filter(|f| valid_source_loc(f)).cloned());
    let Ok(mut event) = Event::new(process, code, loc) else {
        return;
    };
    event.detail = detail.and_then(sanitize_detail);
    event.frames = frames
        .iter()
        .filter(|f| valid_source_loc(f))
        .take(MAX_FRAMES)
        .cloned()
        .collect();
    match event.kind {
        Kind::UiHang => event.hang_bucket_ms = hang_bucket.or(HANG_BUCKETS_MS.first().copied()),
        Kind::NativeCrash => event.native = Some(NativeData::unknown()),
        Kind::OperationError | Kind::Panic | Kind::AbnormalExit => {}
    }
    record_with_consent(state, event, &consent).ok();
}

/// 비밀·사용자 경로·제어 문자를 제거하고 [`MAX_DETAIL_LEN`](256바이트)로 자른 한 줄 상세 사유.
#[must_use]
pub fn sanitize_detail(text: &str) -> Option<String> {
    let redacted = crate::diag::redact(text);
    let mut out = String::new();
    let mut prev_space = false;
    for ch in redacted.chars() {
        let ch = if ch.is_control() || ch.is_whitespace() {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if !out.is_empty() && !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        if out.len().saturating_add(ch.len_utf8()) > MAX_DETAIL_LEN {
            break;
        }
        out.push(ch);
        prev_space = false;
    }
    let trimmed = out.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 백트레이스 문자열에서 공개 크레이트 소스 위치(`crates/krill*/src/**.rs:line[:col]`)만 최대 [`MAX_FRAMES`]개 추출한다.
#[must_use]
pub fn extract_backtrace_frames(backtrace: &str) -> Vec<String> {
    let mut frames = Vec::new();
    for raw in backtrace.lines() {
        let normalized = raw.trim().replace('\\', "/");
        let Some(idx) = normalized.find("crates/krill") else {
            continue;
        };
        let Some(candidate) = normalized.get(idx..).map(str::trim) else {
            continue;
        };
        if candidate.starts_with("crates/krill-proto/src/diag.rs:") {
            continue;
        }
        if valid_source_loc(candidate) && frames.last().is_none_or(|prev| prev != candidate) {
            frames.push(candidate.to_owned());
            if frames.len() >= MAX_FRAMES {
                break;
            }
        }
    }
    frames
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
