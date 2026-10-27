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

/// 현재 유효한 동의. 대기열 항목은 기록 당시 `generation`에 묶인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Consent {
    pub generation: String,
    pub enabled_at_ms: u64,
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

/// 현재 동의(실제 환경의 거부 포함). 거부·파일 없음·손상 파일이면 `None`이다.
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

/// 환경과 무관한 동의 파일 내용. 없거나 손상됐거나 일반 파일이 아니면(FIFO·링크) `None`이다 — 동의를 증명하지
/// 못하면 꺼진 것으로 본다. 막히지 않는 열기라 panic hook에서도 기다리지 않는다.
pub(crate) fn read(state: &Path) -> io::Result<Option<Consent>> {
    let raw = match file::read_bounded(&dir(state).join(CONSENT_FILE), MAX_CONSENT_BYTES) {
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
    Ok(serde_json::from_slice::<Consent>(&raw)
        .ok()
        .filter(|c| is_hex32(&c.generation)))
}

/// 동의한다(멱등: 이미 유효한 동의가 있으면 세대를 유지한다). 새 동의는 남아 있던 대기열·재시도·native
/// 캐시·실행 표식을 먼저 지우고 새 세대를 원자적으로 쓴다. 거부 변수가 켜져 있어도 파일은 남긴다.
///
/// # Errors
/// 디렉터리·파일 쓰기 실패.
pub fn enable(state: &Path) -> io::Result<()> {
    let dir = dir(state);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let lock = Lock::wait(&dir, QUEUE_LOCK, MANAGE_WAIT)?;
    if read(state)?.is_some() {
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

/// 철회한다(없어도 성공). 동의 파일을 먼저 지워 새 기록·전송·응답 반영을 막고, 대기열 잠금을 잠시
/// 기다린 뒤 대기열·재시도·native 캐시·실행 표식을 지운다. 잠금을 못 얻어도 지우기는 계속한다.
///
/// # Errors
/// 없음 외의 동의 파일 삭제 실패.
pub fn disable(state: &Path) -> io::Result<()> {
    let dir = dir(state);
    match fs::remove_file(dir.join(CONSENT_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if !dir.is_dir() {
        return Ok(());
    }
    let lock = Lock::wait(&dir, QUEUE_LOCK, MANAGE_WAIT).ok();
    wipe(&dir);
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
