//! 오류 보고 동의(텔레메트리 동의와 별도). `<상태 디렉터리>/error-reporting/consent.json`(0600)에
//! 무작위 세대와 동의 시각을 둔다. 다시 동의하면 세대가 바뀌어 옛 세대 대기열은 보내지 않는다.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::file;
use super::outbox::{self, Lock, QUEUE_LOCK};
use super::types::{is_hex32, random_id};
use super::{NATIVE_DIR, RUNS_DIR, dir, now_ms};
use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use crate::telemetry;

/// 동의 파일 이름(오류 보고 디렉터리 바로 아래).
pub const CONSENT_FILE: &str = "consent.json";
/// 오류 보고 전용 거부 변수. 참값이면 동의 파일이 있어도 끈다.
pub const DISABLED_VAR: &str = "KRILL_ERROR_REPORTING_DISABLED";
/// 동의 파일 읽기 상한(바이트).
const MAX_CONSENT_BYTES: usize = 512;
/// 동의·철회가 대기열 잠금을 기다리는 상한. 쓰기 쪽은 파일 몇 개만 다루고 곧 놓는다.
const MANAGE_WAIT: Duration = Duration::from_secs(3);
const DEFAULT_CONSENT_DOMAIN: &str = "krill.error-reporting.default-consent.v1";
const DISABLED_JSON: &[u8] = br#"{"enabled":false}"#;

/// 현재 유효한 동의. 대기열 항목은 기록 당시 `generation`에 묶인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Consent {
    pub generation: String,
    pub enabled_at_ms: u64,
}

impl Consent {
    #[must_use]
    pub(crate) fn default_for(state: &Path) -> Self {
        Self {
            generation: super::event_id(
                DEFAULT_CONSENT_DOMAIN,
                super::resolve(state).as_os_str().as_encoded_bytes(),
            ),
            enabled_at_ms: 0,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConsent {
    enabled: Option<bool>,
    generation: Option<String>,
    enabled_at_ms: Option<u64>,
}

/// 거부 판정에 쓰는 환경 값. [`Env::live`]는 프로세스 환경을, 테스트는 [`Env::stub`]을 쓴다.
/// 전용 변수([`DISABLED_VAR`]) 다음에 텔레메트리와 같은 `DO_NOT_TRACK`·`KRILL_TELEMETRY_DISABLED`·CI 변수를 본다.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Env {
    disabled: Option<String>,
    shared: telemetry::Env,
}

impl Env {
    #[must_use]
    pub fn live() -> Self {
        Self {
            disabled: std::env::var_os(DISABLED_VAR).map(|v| v.to_string_lossy().into_owned()),
            shared: telemetry::Env::live(),
        }
    }

    /// 테스트·진단용 고정 환경.
    #[must_use]
    pub fn stub(pairs: &[(&str, &str)]) -> Self {
        Self {
            disabled: pairs
                .iter()
                .find(|(k, _)| *k == DISABLED_VAR)
                .map(|(_, v)| (*v).to_owned()),
            shared: telemetry::Env::stub(pairs),
        }
    }

    /// 거부를 일으킨 변수 이름.
    #[must_use]
    pub fn veto(&self) -> Option<&'static str> {
        if self.disabled.as_deref().is_some_and(telemetry::truthy) {
            return Some(DISABLED_VAR);
        }
        telemetry::consent(None, &self.shared).veto_var()
    }
}

/// 현재 동의(실제 환경의 거부 포함). 거부·명시 철회·손상 파일이면 `None`이다.
///
/// # Errors
/// 없음 외의 읽기 실패.
pub fn consent_at(state: &Path) -> io::Result<Option<Consent>> {
    consent_with(state, &Env::live())
}

/// [`consent_at`]과 같지만 환경을 주입한다. 거부면 파일을 읽지 않는다.
///
/// # Errors
/// 없음 외의 읽기 실패.
pub fn consent_with(state: &Path, env: &Env) -> io::Result<Option<Consent>> {
    if env.veto().is_some() {
        return Ok(None);
    }
    read(state)
}

/// 환경과 무관한 동의 상태. `consent.json`이 없으면 기본 활성화 세대를 돌려주고, 명시적 철회(`{"enabled":false}`)·
/// 손상 파일·비일반 파일(FIFO·링크)이면 `None`이다. 막히지 않는 열기라 panic hook에서도 기다리지 않는다.
pub(crate) fn read(state: &Path) -> io::Result<Option<Consent>> {
    if super::shielded(state) {
        return Ok(None);
    }
    let dir = dir(state);
    if fs::symlink_metadata(&dir).is_ok_and(|m| !m.file_type().is_dir()) {
        return Ok(None);
    }
    let path = dir.join(CONSENT_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => return Ok(None),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Some(Consent::default_for(state)));
        }
        Err(e) => return Err(e),
    }
    read_explicit(&path)
}

fn read_explicit(path: &Path) -> io::Result<Option<Consent>> {
    let raw = match file::read_bounded(path, MAX_CONSENT_BYTES) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(None),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let Ok(stored) = serde_json::from_slice::<StoredConsent>(&raw) else {
        return Ok(None);
    };
    match (stored.enabled, stored.generation, stored.enabled_at_ms) {
        (None | Some(true), Some(generation), Some(enabled_at_ms)) if is_hex32(&generation) => {
            Ok(Some(Consent {
                generation,
                enabled_at_ms,
            }))
        }
        _ => Ok(None),
    }
}

/// 테스트 프로세스는 실제 사용자 동의를 바꾸지 않는다([`super::shielded`]). `read`가 `None`을 주므로 그대로 두면
/// `enable`이 실제 대기열을 지우고 세대를 새로 쓴다.
fn refuse_shielded(state: &Path) -> io::Result<()> {
    if super::shielded(state) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test process must not change the default error-reporting consent",
        ));
    }
    Ok(())
}

/// 동의한다(멱등: 이미 명시적으로 동의한 파일이 있으면 세대를 유지한다). 새 동의·재동의는 남아 있던 대기열·재시도·native
/// 캐시·실행 표식을 먼저 지우고 새 세대를 원자적으로 쓴다. 거부 변수가 켜져 있어도 파일은 남긴다.
///
/// # Errors
/// 디렉터리·파일 쓰기 실패.
pub fn enable(state: &Path) -> io::Result<()> {
    refuse_shielded(state)?;
    let dir = dir(state);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let lock = Lock::wait(&dir, QUEUE_LOCK, MANAGE_WAIT)?;
    let path = dir.join(CONSENT_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file())
        && read_explicit(&path)?.is_some()
    {
        return Ok(());
    }
    wipe(&dir);
    let consent = Consent {
        generation: random_id()?,
        enabled_at_ms: now_ms(),
    };
    let text = serde_json::to_vec(&consent).map_err(io::Error::other)?;
    write_atomic(&dir, CONSENT_FILE, &text)?;
    drop(lock);
    Ok(())
}

/// 철회한다(영속 opt-out). `{"enabled":false}`를 먼저 기록해 새 기록·전송·응답 반영을 막고, 대기열 잠금을 잠시
/// 기다린 뒤 대기열·재시도·native 캐시·실행 표식을 지운다. 잠금을 못 얻어도 지우기는 계속한다.
///
/// # Errors
/// 동의 파일 기록 실패, 또는 동시 관리 작업 때문에 정리를 직렬화하지 못한 경우.
pub fn disable(state: &Path) -> io::Result<()> {
    refuse_shielded(state)?;
    let dir = dir(state);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let path = dir.join(CONSENT_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
        fs::remove_file(&path)?;
    }
    write_atomic(&dir, CONSENT_FILE, DISABLED_JSON)?;
    let lock = Lock::wait(&dir, QUEUE_LOCK, MANAGE_WAIT);
    // An overlapping enable may have published a fresh consent while we waited.
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
        fs::remove_file(&path)?;
    }
    write_atomic(&dir, CONSENT_FILE, DISABLED_JSON)?;
    wipe(&dir);
    // Revocation takes effect immediately, but do not claim completed cleanup
    // if an enable could still publish after our final removal.
    if let Err(error) = &lock
        && error.kind() == io::ErrorKind::WouldBlock
    {
        return Err(io::Error::new(
            error.kind(),
            "error reporting cleanup is busy; retry disable",
        ));
    }
    drop(lock);
    Ok(())
}

/// 동의 세대에 묶인 데이터를 지운다. 잠금 파일과 디렉터리는 조정 지점이라 남긴다.
fn wipe(dir: &Path) {
    outbox::wipe(dir);
    fs::remove_dir_all(dir.join(NATIVE_DIR)).ok();
    fs::remove_dir_all(dir.join(RUNS_DIR)).ok();
}

/// 같은 디렉터리의 임시 파일(0600)에 쓰고 이름을 바꾼다.
pub(crate) fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!(".{name}.{}.tmp", random_id()?));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        drop(file);
        fs::rename(&tmp, dir.join(name))
    })();
    if written.is_err() {
        fs::remove_file(&tmp).ok();
    }
    written
}
