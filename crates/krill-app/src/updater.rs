//! 자체 최소 업데이터(MK-063, ARCHITECTURE §7, T4-25, T1#22).
//!
//! 1. `latest.json` 매니페스트 파싱(`version`, `url`, `sha256`, `ed25519_sig`, `min_daemon_protocol`,
//!    `channel`, `rollout_pct`)과 필수 Ed25519 서명 검증(우회 경로 없음, CONVENTIONS C1-I3·C2-S6).
//! 2. 아카이브 다운로드(`https://` 또는 로컬 테스트용 `http://127.0.0.1`·`localhost`) 후 SHA-256 대조.
//! 3. `ditto -x -k`로 같은 볼륨의 `<번들>.staging`에 풀고 [`CodeVerifier`]로 코드 서명 검증.
//!    MK-104(Developer ID 인증서) 전에는 [`CodesignVerifier::adhoc`]으로 번들·내부 실행 파일의 서명 유효성을
//!    확인하고, 인증서 발급 후에는 [`CodesignVerifier::with_requirement`]로 팀 requirement(`-R`)를 강제한다.
//! 4. 단계별 저널(`update/journal.json`)을 남기며 2단계 원자 교체(`<번들>` → `<번들>.prev`, `<번들>.staging` → `<번들>`)
//!    및 중간 크래시 복구([`recover_journal`]).
//! 5. 별도 relauncher가 새 앱을 띄우고 기동 확인 신호(`ready`)를 기다린다(기본 30초). 새 앱이 비정상 종료되거나
//!    제때 신호를 보내지 않으면 `<번들>.prev`로 자동 롤백한 뒤 복구된 앱을 띄우고 롤백 대화상자를 표시한다.
//! 6. 데몬(`krill-holder`·`krilld`)은 업데이터가 죽이지 않으므로 업데이트·롤백 중에도 세션이 그대로 유지된다.
//!
//! Windows(OWE-837)는 같은 흐름에서 `.app` 번들 대신 설치 폴더(`%LOCALAPPDATA%\Programs\Krill`, `Krill.json` 표지)를 쓴다.
//! 압축은 `tar.exe`로 풀고, 교체는 실행 중 exe를 덮어쓸 수 없어 파일 단위 이름 바꾸기([`rename`])로 한다.
//! Authenticode 검증은 아직 없다(OWE-838). 무결성은 Ed25519 매니페스트와 SHA-256이 지킨다.

pub mod ed25519;
mod relaunch;
mod rename;
mod restart;

pub use restart::{RestartStatus, restart_staged_update};

use gpui::AppContext as _;
use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use krill_proto::control::is_newer;
use krill_proto::ipc::PROTO_MAX;
use serde::{Deserialize, Serialize};

use self::ed25519::{PublicKey, Signature, decode_hex};
pub use self::relaunch::{
    Journal, RelaunchOutcome, Stage, mark_ready, maybe_run_relauncher_cli, offer_rollback,
    recover_journal, relaunch_and_verify, rollback, spawn_detached_relauncher, swap_bundles,
};

/// 상태 디렉터리의 업데이터 설정 파일 이름.
pub const CONFIG_FILE: &str = "updater.toml";
/// 기본 업데이트 매니페스트 URL.
pub const DEFAULT_URL: &str = if cfg!(windows) {
    "https://github.com/craftowen/krill/releases/latest/download/latest-windows.json"
} else {
    "https://github.com/craftowen/krill/releases/latest/download/latest.json"
};
// RFC 8032 §7.1의 공개된 비밀키에 대응하는 테스트 키는 배포에 사용할 수 없다.
const TEST_PUBLIC_KEYS: [&str; 3] = [
    "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
    "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
    "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
];
/// 새 앱 재실행 뒤 정상 신호(`ready`)를 기다리는 기본 시간(T4 §3.4.2: 30초).
pub const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
/// relauncher가 새 앱에 전달하는 준비 신호 파일 경로 환경 변수.
pub const READY_ENV: &str = "KRILL_UPDATE_READY_FILE";
/// `krill-app`을 별도 relauncher 모드로 실행하는 CLI 플래그.
pub const RELAUNCH_ARG: &str = "--relaunch-update";
/// `krill update` CLI가 GUI 없이 업데이트 점검·설치를 위임할 때 사용하는 헤드리스 플래그(MK-156).
pub const UPDATE_CLI_ARG: &str = "--update-cli";

pub(super) const UPDATE_DIR: &str = "update";
/// 번들(설치 폴더) 안 앱 실행 파일 상대 경로.
const APP_EXE: &str = if cfg!(windows) {
    "krill-app.exe"
} else {
    "Contents/MacOS/krill-app"
};
/// 번들 정보 파일. Windows `Krill.json`은 `Info.plist`와 같은 키(`CFBundleShortVersionString`,
/// `KrillUpdatePublicKey`, `KrillUpdateURL`)를 가진 JSON이다(`packaging/windows/bundle.ps1`이 쓴다).
const BUNDLE_INFO: &str = if cfg!(windows) {
    "Krill.json"
} else {
    "Contents/Info.plist"
};
const JOURNAL_FILE: &str = "journal.json";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

/// 스테이징 완료된 업데이트 정보(MK-155, MK-156).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedUpdate {
    pub state_dir: PathBuf,
    pub app_bundle: PathBuf,
    pub from_version: String,
    pub to_version: String,
    pub manifest_url: String,
    pub relaunched: bool,
}

impl StagedUpdate {
    /// 분리된 relauncher 프로세스를 띄워 번들 스왑·재실행·헬스체크를 수행하게 한다.
    /// 호출자는 이 함수가 성공한 직후 앱을 종료(`cx.quit()`)해야 한다.
    pub fn spawn_relauncher(&self) -> Result<(), Error> {
        self.spawn_relauncher_child().map(drop)
    }

    pub(super) fn spawn_relauncher_child(&self) -> Result<std::process::Child, Error> {
        let exe = std::env::current_exe()?;
        spawn_detached_relauncher(
            &exe,
            &self.app_bundle,
            &self.state_dir,
            &self.from_version,
            &self.to_version,
            Some(std::process::id()),
            DEFAULT_HEALTH_TIMEOUT,
        )
    }

    /// 매니페스트 URL이 GitHub Releases 자산 URL이면 해당 버전의 릴리스 노트 페이지 URL을 돌려준다.
    #[must_use]
    pub fn release_notes_url(&self) -> Option<String> {
        let (repo, _, _) = parse_github_release_asset(&self.manifest_url)?;
        Some(format!(
            "https://github.com/{repo}/releases/tag/v{}",
            self.to_version
        ))
    }
}

/// 앱 내 업데이트 점검 상태(MK-156).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CheckStatus {
    #[default]
    Idle,
    Checking,
    UpToDate {
        version: String,
    },
    /// 받은 매니페스트의 채널이 설정 채널과 달라 설치하지 않음.
    ChannelMismatch {
        configured: Channel,
        offered: Channel,
    },
    Ready(StagedUpdate),
    Error(String),
}

/// `SettingsView`와 `Shell`이 공유하는 전역 업데이트 상태(MK-156).
#[derive(Debug, Clone, Default)]
pub struct UpdateState {
    pub status: CheckStatus,
    pub staged: Option<StagedUpdate>,
    pub card_collapsed: bool,
    pub restart: RestartStatus,
}

impl gpui::Global for UpdateState {}

/// 배포 채널(T4 §3.4.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

impl Channel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// `latest.json` 매니페스트(T4 §3.4.2 D14).
///
/// v1 서명 대상에는 알려진 필드만 포함된다. 미래 설치 조건을 구버전이 무시하면
/// 서명이 유효해도 잘못 설치할 수 있으므로, 확장은 새 서명/스키마 버전에서 협상한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub ed25519_sig: String,
    pub min_daemon_protocol: u16,
    pub channel: Channel,
    pub rollout_pct: u8,
}

/// 매니페스트 적용 판정.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 지금 설치 가능.
    Available,
    /// 현재 버전과 같음.
    UpToDate,
    /// 요청 채널과 다름.
    ChannelMismatch,
    /// 현재 데몬 프로토콜보다 높은 버전 요구.
    IncompatibleProtocol { required: u16, supported: u16 },
    /// 점진 롤아웃(`rollout_pct`) 대상 밖.
    DeferredByRollout,
}

impl Decision {
    fn cli_status(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::UpToDate => "up_to_date",
            Self::ChannelMismatch => "channel_mismatch",
            Self::IncompatibleProtocol { .. } => "incompatible_protocol",
            Self::DeferredByRollout => "deferred_by_rollout",
        }
    }

    fn skip_message(&self, current: &str, manifest: &Manifest, channel: Channel) -> String {
        match self {
            Self::UpToDate => format!("Krill v{current} is up to date."),
            Self::ChannelMismatch => format!(
                "Update skipped: settings channel ({}) differs from the offered build channel ({}).",
                channel.as_str(),
                manifest.channel.as_str()
            ),
            Self::IncompatibleProtocol {
                required,
                supported,
            } => format!(
                "Update v{} requires daemon protocol {required} or newer (current {supported}).",
                manifest.version
            ),
            Self::DeferredByRollout => format!(
                "Update v{} is available but this installation is outside the rollout cohort ({}%).",
                manifest.version, manifest.rollout_pct
            ),
            Self::Available => format!("Update v{} is available.", manifest.version),
        }
    }
}

/// 업데이터 설정(`updater.toml`). 서명 검증을 끄는 필드는 두지 않는다(CONVENTIONS C1-I3).
/// 미지 필드는 무시한다. 다시 직렬화하지 않으므로 다운그레이드 잠금(AS-12)을 두지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub url: String,
    pub channel: Channel,
    /// 비어 있으면 업데이터가 비활성화된다. 배포 키는 명시적으로 설정해야 한다.
    pub public_key: String,
    pub designated_requirement: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            url: DEFAULT_URL.to_owned(),
            channel: Channel::Stable,
            public_key: String::new(),
            designated_requirement: None,
        }
    }
}

impl Config {
    pub fn parse(toml_str: &str) -> Result<Self, Error> {
        let cfg: Self = toml::from_str(toml_str).map_err(|e| Error::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(state_dir: &Path) -> Result<Self, Error> {
        Self::load_for_bundle(state_dir, None)
    }

    /// `updater.toml`을 먼저 읽고, 비어 있는 필드는 `<app_bundle>/Contents/Info.plist`(Windows는 `Krill.json`)의
    /// `KrillUpdatePublicKey`·`KrillUpdateURL`·`KrillUpdateDesignatedRequirement`에서 채운다(MK-155).
    pub fn load_for_bundle(state_dir: &Path, app_bundle: Option<&Path>) -> Result<Self, Error> {
        let path = state_dir.join(CONFIG_FILE);
        let mut cfg: Self = match fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| Error::Config(e.to_string()))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(Error::Io(e)),
        };
        if let Some(bundle) = app_bundle
            && let Some(plist) = read_bundle_info_plist_json(bundle)
        {
            if cfg.public_key.is_empty()
                && let Some(pk) = plist
                    .get("KrillUpdatePublicKey")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            {
                pk.clone_into(&mut cfg.public_key);
            }
            if cfg.url == DEFAULT_URL
                && let Some(url) = plist
                    .get("KrillUpdateURL")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            {
                url.clone_into(&mut cfg.url);
            }
            if cfg.designated_requirement.is_none()
                && let Some(req) = plist
                    .get("KrillUpdateDesignatedRequirement")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            {
                cfg.designated_requirement = Some(req.to_owned());
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn public_key_bytes(&self) -> Result<PublicKey, Error> {
        if self.public_key.is_empty() {
            return Err(Error::Config(
                "updater disabled: public_key is not configured".to_owned(),
            ));
        }
        if TEST_PUBLIC_KEYS
            .iter()
            .any(|key| key.eq_ignore_ascii_case(&self.public_key))
        {
            return Err(Error::Config(
                "RFC 8032 test public_key is forbidden".to_owned(),
            ));
        }
        decode_hex::<32>(&self.public_key)
            .ok_or_else(|| Error::Config("invalid 32-byte hex public_key".to_owned()))
    }

    fn validate(&self) -> Result<(), Error> {
        validate_url(&self.url)?;
        if !self.public_key.is_empty() {
            let _ = self.public_key_bytes()?;
        }
        if let Some(req) = &self.designated_requirement
            && req.trim().is_empty()
        {
            return Err(Error::Config(
                "designated_requirement must not be empty".to_owned(),
            ));
        }
        Ok(())
    }
}

fn read_bundle_info_plist_json(app_bundle: &Path) -> Option<serde_json::Value> {
    let plist_path = app_bundle.join(BUNDLE_INFO);
    if !plist_path.is_file() {
        return None;
    }
    if cfg!(windows) {
        return serde_json::from_slice(&fs::read(plist_path).ok()?).ok();
    }
    let out = crate::settings::child::output(
        crate::platform::background_command("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&plist_path),
        Duration::from_secs(10),
        1024 * 1024,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// `<app_bundle>/Contents/Info.plist`의 `CFBundleShortVersionString`을 읽는다(MK-155).
#[must_use]
pub fn bundle_short_version(app_bundle: &Path) -> Option<String> {
    read_bundle_info_plist_json(app_bundle)?
        .get("CFBundleShortVersionString")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// 번들의 `CFBundleShortVersionString`을 우선 읽고, 없으면 `CARGO_PKG_VERSION`을 돌려준다.
#[must_use]
pub fn effective_version(app_bundle: Option<&Path>) -> String {
    app_bundle
        .and_then(bundle_short_version)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

/// 업데이터 오류.
#[derive(Debug)]
pub enum Error {
    Config(String),
    Json(String),
    InsecureUrl(String),
    InvalidVersion(String),
    Downgrade { current: String, offered: String },
    InvalidRollout(u8),
    BadSignature,
    Sha256Mismatch { expected: String, actual: String },
    Codesign(String),
    BundleStructure(String),
    Http(String),
    Unpack(String),
    NoBackup(PathBuf),
    NoJournal,
    Io(io::Error),
}

impl Clone for Error {
    fn clone(&self) -> Self {
        match self {
            Self::Config(v) => Self::Config(v.clone()),
            Self::Json(v) => Self::Json(v.clone()),
            Self::InsecureUrl(v) => Self::InsecureUrl(v.clone()),
            Self::InvalidVersion(v) => Self::InvalidVersion(v.clone()),
            Self::Downgrade { current, offered } => Self::Downgrade {
                current: current.clone(),
                offered: offered.clone(),
            },
            Self::InvalidRollout(v) => Self::InvalidRollout(*v),
            Self::BadSignature => Self::BadSignature,
            Self::Sha256Mismatch { expected, actual } => Self::Sha256Mismatch {
                expected: expected.clone(),
                actual: actual.clone(),
            },
            Self::Codesign(v) => Self::Codesign(v.clone()),
            Self::BundleStructure(v) => Self::BundleStructure(v.clone()),
            Self::Http(v) => Self::Http(v.clone()),
            Self::Unpack(v) => Self::Unpack(v.clone()),
            Self::NoBackup(v) => Self::NoBackup(v.clone()),
            Self::NoJournal => Self::NoJournal,
            Self::Io(v) => Self::Io(io::Error::new(v.kind(), v.to_string())),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(m) => write!(f, "updater config error: {m}"),
            Self::Json(m) => write!(f, "invalid latest.json: {m}"),
            Self::InsecureUrl(u) => write!(
                f,
                "update URL must use https:// (or loopback http:// for local tests): {u}"
            ),
            Self::InvalidVersion(v) => write!(f, "invalid version string: {v}"),
            Self::Downgrade { current, offered } => {
                write!(
                    f,
                    "downgrade refused: current {current} > offered {offered}"
                )
            }
            Self::InvalidRollout(p) => write!(f, "rollout_pct must be 0..=100, got {p}"),
            Self::BadSignature => write!(f, "ed25519 signature verification failed"),
            Self::Sha256Mismatch { expected, actual } => {
                write!(
                    f,
                    "archive sha256 mismatch: expected {expected}, got {actual}"
                )
            }
            Self::Codesign(m) => write!(f, "codesign verification failed: {m}"),
            Self::BundleStructure(m) => write!(f, "invalid app bundle structure: {m}"),
            Self::Http(m) => write!(f, "http transfer failed: {m}"),
            Self::Unpack(m) => write!(f, "archive unpack failed: {m}"),
            Self::NoBackup(p) => write!(f, "backup bundle not found for rollback: {}", p.display()),
            Self::NoJournal => write!(
                f,
                "update journal not found; backup completeness unknown, rollback refused"
            ),
            Self::Io(e) => write!(f, "updater I/O error: {e}"),
        }
    }
}

impl Error {
    /// 설정·상태바에 보이는 사용자용 문구(OWE-883). 로그·CLI 출력은 [`fmt::Display`] 그대로 둔다.
    #[must_use]
    pub fn user_message(&self) -> String {
        use crate::i18n::catalog::updater_relaunch as utext;
        match self {
            Self::Config(m) => utext::UPDATE_ERR_CONFIG.format(&[("detail", m)]),
            Self::Json(m) => utext::UPDATE_ERR_MANIFEST.format(&[("detail", m)]),
            Self::InsecureUrl(u) => utext::UPDATE_ERR_URL.format(&[("detail", u)]),
            Self::InvalidVersion(v) => utext::UPDATE_ERR_VERSION.format(&[("detail", v)]),
            Self::Downgrade { current, offered } => {
                utext::UPDATE_ERR_DOWNGRADE.format(&[("current", current), ("offered", offered)])
            }
            Self::InvalidRollout(p) => utext::UPDATE_ERR_ROLLOUT.format(&[("pct", &p.to_string())]),
            Self::BadSignature => utext::UPDATE_ERR_SIGNATURE.get().to_owned(),
            Self::Sha256Mismatch { .. } => utext::UPDATE_ERR_SHA256.get().to_owned(),
            Self::Codesign(m) => utext::UPDATE_ERR_CODESIGN.format(&[("detail", m)]),
            Self::BundleStructure(m) => utext::UPDATE_ERR_BUNDLE.format(&[("detail", m)]),
            Self::Http(m) => utext::UPDATE_ERR_NETWORK.format(&[("detail", m)]),
            Self::Unpack(m) => utext::UPDATE_ERR_UNPACK.format(&[("detail", m)]),
            Self::NoBackup(_) => utext::UPDATE_ERR_BACKUP.get().to_owned(),
            Self::NoJournal => utext::UPDATE_ERR_JOURNAL.get().to_owned(),
            Self::Io(e) => utext::UPDATE_ERR_IO.format(&[("detail", &e.to_string())]),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// HTTPS 또는 로컬 루프백(`127.0.0.1`, `localhost`, `[::1]`) HTTP만 허용한다.
pub fn validate_url(url: &str) -> Result<(), Error> {
    if let Some(rest) = url.strip_prefix("https://")
        && !rest.is_empty()
        && !rest.starts_with('/')
    {
        return Ok(());
    }
    for prefix in [
        "http://127.0.0.1:",
        "http://127.0.0.1/",
        "http://localhost:",
        "http://localhost/",
        "http://[::1]:",
        "http://[::1]/",
    ] {
        if url.starts_with(prefix) {
            return Ok(());
        }
    }
    Err(Error::InsecureUrl(url.to_owned()))
}

impl Manifest {
    /// JSON 문자열에서 `latest.json`을 파싱하고 형식·URL·범위를 검증한다.
    pub fn parse(json: &str) -> Result<Self, Error> {
        let m: Self = serde_json::from_str(json).map_err(|e| Error::Json(e.to_string()))?;
        m.validate_format()?;
        Ok(m)
    }

    fn validate_format(&self) -> Result<(), Error> {
        if self.rollout_pct > 100 {
            return Err(Error::InvalidRollout(self.rollout_pct));
        }
        validate_url(&self.url)?;
        if is_newer(&self.version, "0.0.0").is_none() {
            return Err(Error::InvalidVersion(self.version.clone()));
        }
        if decode_hex::<32>(&self.sha256).is_none() {
            return Err(Error::Json("sha256 must be 64 hex characters".to_owned()));
        }
        if decode_hex::<64>(&self.ed25519_sig).is_none() {
            return Err(Error::Json(
                "ed25519_sig must be 128 hex characters".to_owned(),
            ));
        }
        Ok(())
    }

    /// Ed25519 서명 대상이 되는 정규 바이트열. 모든 매니페스트 필드를 묶어 필드 변조·채널 교차를 막는다.
    ///
    /// 릴리스 도구는 이 바이트열에 배포 비밀키로 Ed25519 서명(RFC 8032)한 64바이트를 128자리 소문자 hex로
    /// 인코딩해 `ed25519_sig`에 기록한다(앱 바이너리는 검증만 수행하며 서명 생성 코드를 포함하지 않는다).
    pub fn signing_payload(&self) -> Vec<u8> {
        format!(
            "krill-update-v1\nversion={}\nurl={}\nsha256={}\nmin_daemon_protocol={}\nchannel={}\nrollout_pct={}\n",
            self.version,
            self.url,
            self.sha256.to_ascii_lowercase(),
            self.min_daemon_protocol,
            self.channel.as_str(),
            self.rollout_pct,
        )
        .into_bytes()
    }

    /// 공개키로 매니페스트 Ed25519 서명을 검증한다.
    pub fn verify_signature(&self, public_key: &PublicKey) -> Result<(), Error> {
        self.validate_format()?;
        let sig: Signature = decode_hex::<64>(&self.ed25519_sig).ok_or(Error::BadSignature)?;
        if !ed25519::verify(public_key, &self.signing_payload(), &sig) {
            return Err(Error::BadSignature);
        }
        Ok(())
    }

    /// 서명을 검증한 뒤 현재 버전·채널·데몬 프로토콜·롤아웃 비율로 업데이트 가능 여부를 판정한다.
    pub fn evaluate(
        &self,
        public_key: &PublicKey,
        current_version: &str,
        channel: Channel,
        daemon_proto_max: u16,
        rollout_bucket: u8,
    ) -> Result<Decision, Error> {
        self.verify_signature(public_key)?;
        if self.channel != channel {
            return Ok(Decision::ChannelMismatch);
        }
        match is_newer(&self.version, current_version) {
            Some(true) => {}
            Some(false) => {
                if same_version(&self.version, current_version) {
                    return Ok(Decision::UpToDate);
                }
                return Err(Error::Downgrade {
                    current: current_version.to_owned(),
                    offered: self.version.clone(),
                });
            }
            None => return Err(Error::InvalidVersion(self.version.clone())),
        }
        if self.min_daemon_protocol > daemon_proto_max {
            return Ok(Decision::IncompatibleProtocol {
                required: self.min_daemon_protocol,
                supported: daemon_proto_max,
            });
        }
        if self.rollout_pct == 0
            || (self.rollout_pct < 100 && (rollout_bucket % 100) >= self.rollout_pct)
        {
            return Ok(Decision::DeferredByRollout);
        }
        Ok(Decision::Available)
    }
}

fn same_version(a: &str, b: &str) -> bool {
    is_newer(a, b) == Some(false) && is_newer(b, a) == Some(false)
}

/// 풀린 `.app` 번들(Windows는 설치 폴더)의 코드 서명 검증 인터페이스(MK-063).
///
/// MK-104 인증서 준비 전에는 [`CodesignVerifier::adhoc`]으로 동작하고, 인증서 준비 후에는
/// [`CodesignVerifier::with_requirement`]로 Developer ID designated requirement를 강제한다.
pub trait CodeVerifier: fmt::Debug + Send + Sync {
    fn verify(&self, app_bundle: &Path) -> Result<(), Error>;
}

/// macOS `/usr/bin/codesign` 기반 검증기.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodesignVerifier {
    designated_requirement: Option<String>,
}

impl CodesignVerifier {
    /// ad-hoc 서명 확인 모드(MK-104 인증서 발급 전 기본값). 미서명·변조 바이너리를 거부한다.
    pub const fn adhoc() -> Self {
        Self {
            designated_requirement: None,
        }
    }

    /// Developer ID designated requirement(`-R=<req>`) 강제 모드(MK-104 이후). ad-hoc 서명을 거부한다.
    pub fn with_requirement(requirement: impl Into<String>) -> Self {
        Self {
            designated_requirement: Some(requirement.into()),
        }
    }

    pub fn from_config(config: &Config) -> Self {
        Self {
            designated_requirement: config.designated_requirement.clone(),
        }
    }
}

/// 번들의 필수 구조(`Contents/Info.plist`, `Contents/MacOS/krill-app`)를 확인하고
/// 서명 검증 대상 실행 파일 목록(`Contents/Helpers` 또는 `Contents/MacOS`의 `krill-holder`·`krilld`,
/// `Contents/MacOS`의 `krill`·`krill-app`)을 안쪽부터 순서대로 돌려준다.
pub fn validate_bundle_layout(app_bundle: &Path) -> Result<Vec<PathBuf>, Error> {
    if cfg!(windows) {
        return validate_install_layout(app_bundle);
    }
    let macos_dir = app_bundle.join("Contents/MacOS");
    let helpers_dir = app_bundle.join("Contents/Helpers");
    let plist = app_bundle.join("Contents/Info.plist");
    let main_exe = macos_dir.join("krill-app");
    if !plist.is_file() || !main_exe.is_file() {
        return Err(Error::BundleStructure(format!(
            "missing Contents/Info.plist or Contents/MacOS/krill-app in {}",
            app_bundle.display()
        )));
    }
    let mut binaries = Vec::new();
    for helper in ["krill-holder", "krilld", "krill-link"] {
        let in_helpers = helpers_dir.join(helper);
        let in_macos = macos_dir.join(helper);
        if in_helpers.is_file() {
            binaries.push(in_helpers);
        }
        if in_macos.is_file() {
            binaries.push(in_macos);
        }
    }
    for bin_name in ["krill", "krill-app"] {
        let bin = macos_dir.join(bin_name);
        if bin.is_file() {
            binaries.push(bin);
        }
    }
    Ok(binaries)
}

/// Windows 설치 폴더의 필수 구조(`Krill.json`, 실행 파일 4개, 번들 ConPTY)를 확인하고 실행 파일 목록을 돌려준다.
/// `krill-link.exe`는 선택이다. 새 세션은 고정된 번들 `ConPTY` ABI를 요구한다.
pub fn validate_install_layout(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut binaries = Vec::new();
    for name in [
        "krill-holder.exe",
        "krilld.exe",
        "krill.exe",
        "krill-app.exe",
    ] {
        let bin = dir.join(name);
        if !bin.is_file() {
            return Err(Error::BundleStructure(format!(
                "missing {name} in {}",
                dir.display()
            )));
        }
        binaries.push(bin);
    }
    if !dir.join("Krill.json").is_file() {
        return Err(Error::BundleStructure(format!(
            "missing Krill.json in {}",
            dir.display()
        )));
    }
    for name in ["conpty.dll", "x64/OpenConsole.exe"] {
        if !dir.join(name).is_file() {
            return Err(Error::BundleStructure(format!(
                "missing {name} in {}",
                dir.display()
            )));
        }
    }
    Ok(binaries)
}

#[cfg(test)]
#[test]
fn windows_install_layout_requires_both_conpty_components() {
    let dir = std::env::temp_dir().join(format!("krill-conpty-layout-{}", std::process::id()));
    fs::create_dir_all(dir.join("x64")).unwrap();
    for name in [
        "Krill.json",
        "krill-holder.exe",
        "krilld.exe",
        "krill.exe",
        "krill-app.exe",
        "conpty.dll",
        "x64/OpenConsole.exe",
    ] {
        fs::write(dir.join(name), b"fixture").unwrap();
    }
    assert_eq!(validate_install_layout(&dir).unwrap().len(), 4);
    for name in ["conpty.dll", "x64/OpenConsole.exe"] {
        fs::remove_file(dir.join(name)).unwrap();
        assert!(matches!(
            validate_install_layout(&dir),
            Err(Error::BundleStructure(_))
        ));
        fs::write(dir.join(name), b"fixture").unwrap();
    }
    fs::remove_dir_all(dir).unwrap();
}

impl CodeVerifier for CodesignVerifier {
    fn verify(&self, app_bundle: &Path) -> Result<(), Error> {
        // Windows: Authenticode 인증서가 없어(OWE-838) 구조만 확인한다. 위조 방지는 서명된 매니페스트의 SHA-256.
        if cfg!(windows) {
            return validate_install_layout(app_bundle).map(drop);
        }
        // 안쪽 실행 파일부터 개별 검증한다(CONVENTIONS A3: --deep 서명에 기대지 않음).
        for bin in validate_bundle_layout(app_bundle)? {
            run_codesign(&["--verify", "--strict", "--verbose=2"], &bin)?;
        }
        run_codesign(&["--verify", "--strict", "--verbose=2"], app_bundle)?;

        let info = crate::settings::child::output(
            crate::platform::background_command("/usr/bin/codesign")
                .args(["-dv", "--verbose=4"])
                .arg(app_bundle),
            Duration::from_secs(30),
            65536,
        )?;
        let stderr = String::from_utf8_lossy(&info.stderr);
        if !info.status.success()
            || !(stderr.contains("Signature=") || stderr.contains("Signature size="))
        {
            return Err(Error::Codesign(stderr.trim().to_owned()));
        }
        if let Some(req) = &self.designated_requirement {
            if stderr.contains("Signature=adhoc") || stderr.contains("(adhoc)") {
                return Err(Error::Codesign(
                    "ad-hoc signature rejected when designated requirement is set".to_owned(),
                ));
            }
            let req_arg = format!("-R={req}");
            run_codesign(&["--verify", "--strict", &req_arg], app_bundle)?;
        }
        Ok(())
    }
}

fn run_codesign(args: &[&str], target: &Path) -> Result<(), Error> {
    let out = crate::settings::child::output(
        crate::platform::background_command("/usr/bin/codesign")
            .args(args)
            .arg(target),
        Duration::from_secs(30),
        65536,
    )?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(Error::Codesign(format!("{}: {msg}", target.display())));
    }
    Ok(())
}

/// URL에서 바이트를 내려받는다(`https://` 또는 로컬 루프백 `http://`만 허용).
/// 1차 `/usr/bin/curl`이 실패하고 URL이 GitHub Releases 자산 형식이면 로컬 `gh release download`로 폴백한다(MK-155).
pub fn fetch_bytes(url: &str, max_bytes: usize) -> Result<Vec<u8>, Error> {
    validate_url(url)?;
    let max_str = max_bytes.to_string();
    let out = crate::settings::child::output(
        crate::platform::background_command(system_tool("curl")).args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "30",
            "--max-filesize",
            &max_str,
            "--proto",
            "=https,http",
            "--proto-redir",
            "=https",
            url,
        ]),
        Duration::from_secs(40),
        u64::try_from(max_bytes).unwrap_or(u64::MAX),
    )?;
    if out.status.success() {
        if out.stdout.len() > max_bytes {
            return Err(Error::Http(format!(
                "response exceeded max size ({max_bytes} bytes)"
            )));
        }
        return Ok(out.stdout);
    }

    let curl_err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if let Some((repo, tag, asset)) = parse_github_release_asset(url)
        && let Ok(bytes) = fetch_github_release_asset(&repo, tag.as_deref(), &asset, max_bytes)
    {
        return Ok(bytes);
    }
    Err(Error::Http(curl_err))
}

/// `https://github.com/<OWNER>/<REPO>/releases/latest/download/<ASSET>` 또는
/// `https://github.com/<OWNER>/<REPO>/releases/download/<TAG>/<ASSET>`를 파싱한다.
pub(crate) fn parse_github_release_asset(url: &str) -> Option<(String, Option<String>, String)> {
    let rest = url.strip_prefix("https://github.com/")?;
    if rest.contains('?') || rest.contains('#') {
        return None;
    }
    let parts: Vec<&str> = rest.split('/').collect();
    let valid_seg = |s: &str| {
        !s.is_empty() && !s.starts_with('-') && s != "." && s != ".." && !s.contains('\\')
    };
    match parts.as_slice() {
        [owner, repo, "releases", "latest", "download", asset]
            if valid_seg(owner) && valid_seg(repo) && valid_seg(asset) =>
        {
            Some((format!("{owner}/{repo}"), None, (*asset).to_owned()))
        }
        [owner, repo, "releases", "download", tag, asset]
            if valid_seg(owner) && valid_seg(repo) && valid_seg(tag) && valid_seg(asset) =>
        {
            Some((
                format!("{owner}/{repo}"),
                Some((*tag).to_owned()),
                (*asset).to_owned(),
            ))
        }
        _ => None,
    }
}

fn find_gh_binary() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KRILL_GH_BIN") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.is_file() {
                return Some(p);
            }
            return None;
        }
    }
    if let Some(path_os) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_os) {
            let cand = dir.join(format!("gh{}", std::env::consts::EXE_SUFFIX));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for fixed in ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"] {
        let p = PathBuf::from(fixed);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn fetch_github_release_asset(
    repo: &str,
    tag: Option<&str>,
    asset: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, Error> {
    let gh = find_gh_binary().ok_or_else(|| Error::Http("gh CLI not found".to_owned()))?;
    fetch_github_release_asset_with_bin(&gh, repo, tag, asset, max_bytes)
}

fn fetch_github_release_asset_with_bin(
    gh: &Path,
    repo: &str,
    tag: Option<&str>,
    asset: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, Error> {
    let mut cmd = crate::platform::background_command(gh);
    cmd.args(["release", "download"]);
    if let Some(t) = tag {
        cmd.arg(t);
    }
    cmd.args(["--repo", repo, "--pattern", asset, "--output", "-"]);
    let out = crate::settings::child::output(
        &mut cmd,
        Duration::from_secs(60),
        u64::try_from(max_bytes).unwrap_or(u64::MAX),
    )?;
    if !out.status.success() {
        return Err(Error::Http(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    if out.stdout.len() > max_bytes {
        return Err(Error::Http(format!(
            "response exceeded max size ({max_bytes} bytes)"
        )));
    }
    Ok(out.stdout)
}

/// `url`에서 `latest.json`을 받아 파싱한다.
pub fn fetch_manifest(url: &str) -> Result<Manifest, Error> {
    let raw = fetch_bytes(url, MAX_MANIFEST_BYTES)?;
    let text = String::from_utf8(raw).map_err(|e| Error::Json(e.to_string()))?;
    Manifest::parse(&text)
}

/// `<번들>.staging` 경로(같은 볼륨·같은 부모 디렉터리).
#[must_use]
pub fn staging_path(app_bundle: &Path) -> PathBuf {
    append_ext(app_bundle, "staging")
}

/// `<번들>.prev` 백업 경로(1개 보존).
#[must_use]
pub fn prev_path(app_bundle: &Path) -> PathBuf {
    append_ext(app_bundle, "prev")
}

/// `<번들>.trash`: Windows에서 실행 중이라 지울 수 없는 파일을 옮겨 두는 곳([`rename`]).
#[must_use]
pub fn trash_path(app_bundle: &Path) -> PathBuf {
    append_ext(app_bundle, "trash")
}

/// `<상태 디렉터리>/update/journal.json` 경로.
#[must_use]
pub fn journal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(UPDATE_DIR).join(JOURNAL_FILE)
}

/// `<상태 디렉터리>/update/ready-<버전>` 경로.
#[must_use]
pub fn ready_path(state_dir: &Path, version: &str) -> PathBuf {
    state_dir.join(UPDATE_DIR).join(format!("ready-{version}"))
}

pub(super) fn append_ext(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".");
    os.push(suffix);
    PathBuf::from(os)
}

/// 아카이브 바이트의 Ed25519·SHA-256을 검증한 뒤 `ditto`로 `<app_bundle>.staging`에 풀고 `verifier`로 검증한다.
/// 검증에 실패하면 `<app_bundle>.staging`을 즉시 지우고 원래 `app_bundle`은 건드리지 않는다.
pub fn stage_verified_archive(
    manifest: &Manifest,
    public_key: &PublicKey,
    archive_bytes: &[u8],
    app_bundle: &Path,
    verifier: &dyn CodeVerifier,
) -> Result<PathBuf, Error> {
    manifest.verify_signature(public_key)?;
    let path = unique_update_path(app_bundle, "download.zip");
    if archive_bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(Error::Http("archive exceeded size limit".into()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)?
        .write_all(archive_bytes)?;
    let result = stage_verified_file(manifest, public_key, &path, app_bundle, verifier);
    fs::remove_file(path).ok();
    result
}

fn unique_update_path(bundle: &Path, suffix: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    append_ext(bundle, &format!("{}-{serial}-{suffix}", std::process::id()))
}

fn archive_sha(path: &Path) -> Result<String, Error> {
    let mut file = fs::File::open(path)?;
    if file.metadata()?.len() > u64::try_from(MAX_ARCHIVE_BYTES).unwrap_or(u64::MAX) {
        return Err(Error::Http("archive exceeded size limit".into()));
    }
    let mut hash = Sha256::new();
    let mut total = 0_usize;
    let mut buffer = vec![0_u8; 65_536].into_boxed_slice();
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n);
        if total > MAX_ARCHIVE_BYTES {
            return Err(Error::Http("archive exceeded size limit".into()));
        }
        hash.update(&buffer[..n]);
    }
    Ok(ed25519::encode_hex(&hash.finalize()))
}

fn stage_verified_file(
    manifest: &Manifest,
    public_key: &PublicKey,
    archive: &Path,
    app_bundle: &Path,
    verifier: &dyn CodeVerifier,
) -> Result<PathBuf, Error> {
    manifest.verify_signature(public_key)?;
    let actual_sha = archive_sha(archive)?;
    if !actual_sha.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(Error::Sha256Mismatch {
            expected: manifest.sha256.clone(),
            actual: actual_sha,
        });
    }
    let parent = app_bundle
        .parent()
        .ok_or_else(|| Error::BundleStructure("bundle has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let _lock = staging_lock(app_bundle)?;
    recover_stage_publish_locked(app_bundle)?;
    let unpack = unique_update_path(app_bundle, "unpack.tmp");
    let candidate = unique_update_path(app_bundle, "candidate");
    let staging = staging_path(app_bundle);
    let incoming = append_ext(app_bundle, "staging.incoming");
    let backup = append_ext(app_bundle, "staging.previous");
    let result = (|| {
        unpack_with_ditto(archive, &unpack, &candidate)?;
        verifier.verify(&candidate)?;
        fs::remove_dir_all(&incoming).ok();
        fs::rename(&candidate, &incoming)?;
        write_stage_marker(app_bundle)?;
        if staging.exists() {
            fs::rename(&staging, &backup)?;
        }
        if let Err(error) = fs::rename(&incoming, &staging) {
            recover_stage_publish_locked(app_bundle).ok();
            return Err(error.into());
        }
        sync_stage_parent(app_bundle)?;
        fs::remove_file(append_ext(app_bundle, "stage-publish"))?;
        sync_stage_parent(app_bundle)?;
        fs::remove_dir_all(&backup).ok();
        Ok(staging)
    })();
    fs::remove_dir_all(&unpack).ok();
    fs::remove_dir_all(&candidate).ok();
    result
}

fn staging_lock(app_bundle: &Path) -> io::Result<fs::File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(append_ext(app_bundle, "update.lock"))?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while lock.try_lock().is_err() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "update staging lock deadline exceeded",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(lock)
}
#[cfg_attr(
    not(unix),
    expect(
        clippy::unnecessary_wraps,
        reason = "Unix directory sync can fail; staging shares its fallible publication contract on Windows"
    )
)]
fn sync_stage_parent(app_bundle: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = app_bundle.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = app_bundle;
    Ok(())
}
fn write_stage_marker(app_bundle: &Path) -> io::Result<()> {
    let path = append_ext(app_bundle, "stage-publish");
    let temporary = unique_update_path(app_bundle, "stage-publish.tmp");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(b"krill-stage-publish-v1\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    sync_stage_parent(app_bundle)
}
fn recover_stage_publish_locked(app_bundle: &Path) -> io::Result<()> {
    let marker = append_ext(app_bundle, "stage-publish");
    let staging = staging_path(app_bundle);
    let incoming = append_ext(app_bundle, "staging.incoming");
    let backup = append_ext(app_bundle, "staging.previous");
    if !marker.exists() {
        if staging.exists() {
            fs::remove_dir_all(&backup).ok();
            fs::remove_dir_all(&incoming).ok();
        }
        return Ok(());
    }
    let mut bytes = Vec::new();
    fs::File::open(&marker)?.take(128).read_to_end(&mut bytes)?;
    if bytes != b"krill-stage-publish-v1\n" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid stage publication journal",
        ));
    }
    if !staging.exists() {
        if backup.exists() {
            fs::rename(&backup, &staging)?;
        } else if incoming.exists() {
            fs::rename(&incoming, &staging)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "stage publication has no recoverable bundle",
            ));
        }
    }
    sync_stage_parent(app_bundle)?;
    fs::remove_file(marker)?;
    sync_stage_parent(app_bundle)?;
    fs::remove_dir_all(incoming).ok();
    fs::remove_dir_all(backup).ok();
    Ok(())
}

/// Recover interrupted staging publication off the event loop before normal update rollback recovery.
pub fn on_startup(state_dir: PathBuf, window: &mut gpui::Window, cx: &mut gpui::App) {
    let handle = window.window_handle();
    let work = cx.background_spawn(async move {
        if let Ok(exe) = std::env::current_exe() {
            let configured = std::env::var("KRILL_UPDATE_BUNDLE_PATH").ok();
            if let Some(bundle) = current_app_bundle_in(configured.as_deref(), &exe)
                && append_ext(&bundle, "stage-publish").exists()
                && let Ok(_lock) = staging_lock(&bundle)
            {
                recover_stage_publish_locked(&bundle).ok();
            }
        }
    });
    cx.spawn(async move |cx| {
        work.await;
        handle
            .update(cx, |_, window, cx| {
                relaunch::on_startup(state_dir, window, cx);
            })
            .ok();
    })
    .detach();
}

/// Stream stdout to a private file. A descendant holding a pipe cannot extend the total deadline.
fn stream_download(cmd: &mut std::process::Command, path: &Path) -> Result<(), Error> {
    use std::process::Stdio;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| -> io::Result<()> {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("download stdout unavailable"))?;
        #[cfg(unix)]
        {
            let flags = rustix::fs::fcntl_getfl(&stdout)?;
            rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK)?;
        }
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("download stderr unavailable"))?;
        let path = path.to_owned();
        let body = crate::process_io::PipeTask::spawn(move || {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            let mut total = 0_usize;
            let mut buffer = vec![0_u8; 65_536].into_boxed_slice();
            loop {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "download body deadline exceeded",
                    ));
                }
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        total = total.saturating_add(n);
                        if total > MAX_ARCHIVE_BYTES {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "archive exceeded size limit",
                            ));
                        }
                        file.write_all(&buffer[..n])?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => return Err(error),
                }
            }
            file.sync_all()
        })?;
        let errors = crate::process_io::read(stderr, 65537, deadline)?;
        let mut body_done = false;
        let status = loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "download command timed out",
                ));
            }
            if !body_done && let Some(result) = body.try_finish() {
                result?;
                body_done = true;
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !body_done {
            body.finish(deadline)?;
        }
        let errors = errors.finish(deadline)?;
        if errors.bytes.len() > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "download stderr exceeded limit",
            ));
        }
        if errors.timed_out {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "download stderr deadline exceeded",
            ));
        }
        if !status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&errors.bytes).trim().to_owned(),
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        child.kill().ok();
        child.wait().ok();
    }
    result.map_err(Error::Io)
}

fn fetch_archive(url: &str, bundle: &Path) -> Result<PathBuf, Error> {
    validate_url(url)?;
    if let Some(parent) = bundle.parent() {
        fs::create_dir_all(parent)?;
    }
    let path = unique_update_path(bundle, "download.zip");
    let mut curl = crate::platform::background_command(system_tool("curl"));
    curl.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "30",
        "--max-filesize",
        &MAX_ARCHIVE_BYTES.to_string(),
        "--proto",
        "=https,http",
        "--proto-redir",
        "=https",
        url,
    ]);
    match stream_download(&mut curl, &path) {
        Ok(()) => Ok(path),
        Err(error) => {
            fs::remove_file(&path).ok();
            if let Some((repo, tag, asset)) = parse_github_release_asset(url)
                && let Some(gh) = find_gh_binary()
            {
                let mut cmd = crate::platform::background_command(gh);
                cmd.args(["release", "download"]);
                if let Some(tag) = tag {
                    cmd.arg(tag);
                }
                cmd.args(["--repo", &repo, "--pattern", &asset, "--output", "-"]);
                if stream_download(&mut cmd, &path).is_ok() {
                    return Ok(path);
                }
                fs::remove_file(&path).ok();
            }
            Err(error)
        }
    }
}

/// 점진 롤아웃 판정용 안정 버킷(`0..100`). 상태 디렉터리 경로의 FNV-1a
/// 해시라 같은 기기·사용자는 항상 같은 버킷에 머문다(AS-05). 프로세스마다
/// 바뀌는 기본 해셔(`RandomState`)는 쓰지 않는다.
pub fn rollout_bucket(state_dir: &Path) -> u8 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in state_dir.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // `hash % 100`은 항상 100보다 작아 변환이 실패하지 않는다.
    u8::try_from(hash % 100).unwrap_or(0)
}

/// 매니페스트 검증·판정 후 아카이브를 내려받아 `<app_bundle>.staging`까지 준비한다.
pub fn download_and_stage(
    manifest: &Manifest,
    public_key: &PublicKey,
    current_version: &str,
    channel: Channel,
    app_bundle: &Path,
    verifier: &dyn CodeVerifier,
    rollout_bucket: u8,
) -> Result<Option<PathBuf>, Error> {
    let decision = manifest.evaluate(
        public_key,
        current_version,
        channel,
        PROTO_MAX,
        rollout_bucket,
    )?;
    if decision != Decision::Available {
        return Ok(None);
    }
    let archive = fetch_archive(&manifest.url, app_bundle)?;
    let staged = stage_verified_file(manifest, public_key, &archive, app_bundle, verifier);
    fs::remove_file(archive).ok();
    staged.map(Some)
}

/// 매니페스트 판정까지만 보고 설치 가능할 때만 스테이징한다.
/// 채널 불일치는 `None`에 묻히지 않고 [`CheckStatus::ChannelMismatch`]로 돌려준다.
/// `skip_rolled_back`이면 시작 확인에 실패해 롤백된 버전을 다시 받지 않는다. 자동 점검이 그 버전을
/// 곧바로 다시 스테이징해 자동 재시작·롤백을 되풀이했다(v0.2.29). 사용자가 직접 점검하면 다시 시도한다.
fn decide_and_stage(
    cfg: &Config,
    manifest: &Manifest,
    app_bundle: &Path,
    state_dir: &Path,
    skip_rolled_back: bool,
) -> Result<CheckStatus, Error> {
    let pk = cfg.public_key_bytes()?;
    let from_version = effective_version(Some(app_bundle));
    let bucket = rollout_bucket(state_dir);
    match manifest.evaluate(&pk, &from_version, cfg.channel, PROTO_MAX, bucket)? {
        Decision::UpToDate => {
            return Ok(CheckStatus::UpToDate {
                version: from_version,
            });
        }
        Decision::ChannelMismatch => {
            return Ok(CheckStatus::ChannelMismatch {
                configured: cfg.channel,
                offered: manifest.channel,
            });
        }
        Decision::DeferredByRollout => {
            return Ok(CheckStatus::Error(
                crate::i18n::catalog::updater_relaunch::UPDATE_ERR_ROLLOUT
                    .format(&[("pct", &manifest.rollout_pct.to_string())]),
            ));
        }
        Decision::IncompatibleProtocol {
            required,
            supported,
        } => {
            return Ok(CheckStatus::Error(
                crate::i18n::catalog::updater_relaunch::UPDATE_ERR_PROTOCOL.format(&[
                    ("version", &manifest.version),
                    ("required", &required.to_string()),
                    ("supported", &supported.to_string()),
                ]),
            ));
        }
        Decision::Available => {}
    }
    if skip_rolled_back
        && matches!(
            Journal::load(state_dir),
            Ok(Some(journal)) if same_version(&journal.to_version, &manifest.version)
                && matches!(journal.stage, Stage::RolledBack { .. } | Stage::Acknowledged)
        )
    {
        return Ok(CheckStatus::Error(
            crate::i18n::catalog::updater_relaunch::UPDATE_SKIP_ROLLED_BACK
                .format(&[("version", &manifest.version)]),
        ));
    }
    let verifier = CodesignVerifier::from_config(cfg);
    let staged = download_and_stage(
        manifest,
        &pk,
        &from_version,
        cfg.channel,
        app_bundle,
        &verifier,
        bucket,
    )?;
    Ok(match staged {
        Some(_) => CheckStatus::Ready(StagedUpdate {
            state_dir: state_dir.to_path_buf(),
            app_bundle: app_bundle.to_path_buf(),
            from_version,
            to_version: manifest.version.clone(),
            manifest_url: cfg.url.clone(),
            relaunched: false,
        }),
        None => CheckStatus::UpToDate {
            version: from_version,
        },
    })
}

#[derive(Default)]
struct FlightState {
    running: bool,
    waiters: usize,
    result: Option<Result<CheckStatus, Error>>,
}
#[derive(Default)]
struct UpdateFlight {
    state: Mutex<FlightState>,
    changed: Condvar,
}
struct FlightOwner(Arc<UpdateFlight>, bool);
impl Drop for FlightOwner {
    fn drop(&mut self) {
        if self.1 {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.running = false;
            state.result = Some(Err(Error::Http("update worker stopped".into())));
            self.0.changed.notify_all();
        }
    }
}

type FlightRegistry = std::collections::BTreeMap<String, Arc<UpdateFlight>>;
fn flight_registry() -> &'static Mutex<FlightRegistry> {
    static FLIGHTS: OnceLock<Mutex<FlightRegistry>> = OnceLock::new();
    FLIGHTS.get_or_init(Mutex::default)
}
fn update_flight(key: &str) -> (Arc<UpdateFlight>, bool) {
    let mut flights = flight_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(flight) = flights.get(key)
        && flight
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running
    {
        return (flight.clone(), false);
    }
    // Completed generations are immutable to their waiters. A later caller owns a new Arc.
    let flight = Arc::new(UpdateFlight {
        state: Mutex::new(FlightState {
            running: true,
            ..Default::default()
        }),
        changed: Condvar::new(),
    });
    flights.insert(key.to_owned(), flight.clone());
    (flight, true)
}
fn await_flight(flight: &UpdateFlight) -> Result<CheckStatus, Error> {
    let mut state = flight
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.waiters = state.waiters.saturating_add(1);
    while state.running {
        state = flight
            .changed
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    state.waiters = state.waiters.saturating_sub(1);
    state
        .result
        .clone()
        .unwrap_or_else(|| Err(Error::Http("update result unavailable".into())))
}
fn single_flight(
    key: &str,
    work: impl FnOnce() -> Result<CheckStatus, Error>,
) -> Result<CheckStatus, Error> {
    let (flight, owns) = update_flight(key);
    if !owns {
        return await_flight(&flight);
    }
    let mut owner = FlightOwner(flight.clone(), true);
    let result = work();
    let mut state = flight
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.result = Some(result.clone());
    state.running = false;
    owner.1 = false;
    flight.changed.notify_all();
    result
}

fn check_and_stage_with(
    manifest_url: Option<&str>,
    app_bundle: &Path,
    state_dir: &Path,
    skip_rolled_back: bool,
) -> Result<CheckStatus, Error> {
    if append_ext(app_bundle, "stage-publish").exists() {
        let _lock = staging_lock(app_bundle)?;
        recover_stage_publish_locked(app_bundle)?;
    }
    let mut cfg = Config::load_for_bundle(state_dir, Some(app_bundle))?;
    if let Some(url) = manifest_url.map(str::trim).filter(|url| !url.is_empty()) {
        url.clone_into(&mut cfg.url);
        cfg.validate()?;
    }
    let bundle = app_bundle
        .canonicalize()
        .unwrap_or_else(|_| app_bundle.to_owned());
    #[expect(
        clippy::unnecessary_debug_formatting,
        reason = "Escaped path representations keep distinct update-flight keys unambiguous"
    )]
    let key = format!(
        "{:?}|{:?}|{}|{}|{}|{}",
        bundle,
        state_dir,
        cfg.url,
        cfg.channel.as_str(),
        cfg.public_key,
        skip_rolled_back
    );
    single_flight(&key, || {
        let manifest = match fetch_manifest(&cfg.url) {
            Ok(manifest) => manifest,
            Err(error) => {
                if !matches!(error, Error::Http(_)) {
                    let detail = error.to_string();
                    krill_proto::error_reporting::capture_detailed_at(
                        state_dir,
                        krill_proto::error_reporting::Process::App,
                        krill_proto::error_reporting::Code::UpdateFailed,
                        Some(&detail),
                        &[],
                        file!(),
                        line!(),
                    );
                }
                return Err(error);
            }
        };
        decide_and_stage(&cfg, &manifest, app_bundle, state_dir, skip_rolled_back).inspect_err(
            |error| {
                let detail = error.to_string();
                krill_proto::error_reporting::capture_detailed_at(
                    state_dir,
                    krill_proto::error_reporting::Process::App,
                    krill_proto::error_reporting::Code::UpdateFailed,
                    Some(&detail),
                    &[],
                    file!(),
                    line!(),
                );
            },
        )
    })
}

/// 자동 점검 경로. 시작 확인에 실패해 롤백된 버전은 다시 받지 않는다([`decide_and_stage`]).
pub fn stage_update_for_bundle(
    manifest_url: Option<&str>,
    app_bundle: &Path,
    state_dir: &Path,
) -> Result<Option<StagedUpdate>, Error> {
    match check_and_stage_with(manifest_url, app_bundle, state_dir, true)? {
        CheckStatus::Ready(staged) => Ok(Some(staged)),
        _ => Ok(None),
    }
}

/// `manifest_url`의 `latest.json`을 내려받아 `<app_bundle>.staging`까지 준비한다.
/// 성공하면 `(상태 디렉터리, 매니페스트 버전)`을 돌려준다.
#[cfg(test)]
#[cfg(unix)]
fn stage_update_manifest(
    manifest_url: &str,
    app_bundle: &Path,
    state_dir: &Path,
) -> Result<Option<(PathBuf, String)>, Error> {
    Ok(
        stage_update_for_bundle(Some(manifest_url), app_bundle, state_dir)?
            .map(|s| (s.state_dir, s.to_version)),
    )
}

/// 자체 업데이터가 동작하는 OS: macOS `.app` 번들과 Windows 설치 폴더. Linux는 패키지 관리자에 맡긴다.
const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

/// 번들을 찾지 못했을 때 쓰는 기본 설치 위치.
fn default_app_bundle() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Programs")
            .join("Krill")
    } else {
        PathBuf::from("/Applications/Krill.app")
    }
}

/// 현재 실행 중인 앱 번들의 업데이트를 점검·스테이징한다(Settings UI 및 CLI에서 사용).
/// 자체 업데이터는 macOS `.app` 번들과 Windows 설치 폴더 전용이다. 다른 OS는 점검하지 않는다.
pub fn check_and_stage_current_app(state_dir: &Path) -> Result<CheckStatus, Error> {
    if !SUPPORTED {
        return Err(io::Error::from(io::ErrorKind::Unsupported).into());
    }
    let exe = std::env::current_exe()?;
    let bundle_env = std::env::var("KRILL_UPDATE_BUNDLE_PATH").ok();
    let app_bundle =
        current_app_bundle_in(bundle_env.as_deref(), &exe).unwrap_or_else(default_app_bundle);
    check_and_stage_bundle(&app_bundle, state_dir)
}

/// 지정한 `.app` 번들의 업데이트를 점검하고 새 버전이 있으면 스테이징까지 완료한다.
pub fn check_and_stage_bundle(app_bundle: &Path, state_dir: &Path) -> Result<CheckStatus, Error> {
    let manifest_env = std::env::var("KRILL_UPDATE_MANIFEST_URL").ok();
    check_and_stage_with(manifest_env.as_deref(), app_bundle, state_dir, false)
}

/// 환경 변수(`KRILL_UPDATE_*`) 또는 번들 `Info.plist`/`updater.toml` 설정으로 업데이트를 내려받아 스테이징하고,
/// `KRILL_UPDATE_RELAUNCH_NOW=1`이면 즉시 별도 relauncher를 띄운다.
/// 환경변수 읽기는 여기에만 있고 본체는 [`poll_update_with`]라 테스트에서 값을 직접 넣는다.
pub fn poll_env_update(state_dir: &Path) -> Result<Option<StagedUpdate>, Error> {
    if !SUPPORTED {
        return Ok(None);
    }
    let manifest_url = std::env::var("KRILL_UPDATE_MANIFEST_URL").ok();
    let bundle_path = std::env::var("KRILL_UPDATE_BUNDLE_PATH").ok();
    let relaunch_now = std::env::var("KRILL_UPDATE_RELAUNCH_NOW").ok().as_deref() == Some("1");
    let exe = std::env::current_exe()?;
    poll_update_with(
        manifest_url.as_deref(),
        bundle_path.as_deref(),
        relaunch_now,
        &exe,
        state_dir,
    )
}

/// [`poll_env_update`]의 본체. 매니페스트 URL·번들 경로·즉시 재실행을 인자로 받아
/// 환경변수 없이 검증한다.
fn poll_update_with(
    manifest_url: Option<&str>,
    bundle_path: Option<&str>,
    relaunch_now: bool,
    exe: &Path,
    state_dir: &Path,
) -> Result<Option<StagedUpdate>, Error> {
    let explicit_url = manifest_url.map(str::trim).filter(|s| !s.is_empty());
    if explicit_url.is_none() && std::env::var("KRILL_NO_UPDATE_CHECK").ok().as_deref() == Some("1")
    {
        return Ok(None);
    }
    let Some(app_bundle) = current_app_bundle_in(bundle_path, exe) else {
        return Ok(None);
    };
    let mut cfg = Config::load_for_bundle(state_dir, Some(&app_bundle))?;
    if let Some(url) = explicit_url {
        url.clone_into(&mut cfg.url);
        cfg.validate()?;
    } else if cfg.public_key.is_empty() {
        return Ok(None);
    }
    let Some(mut staged) = stage_update_for_bundle(Some(&cfg.url), &app_bundle, state_dir)? else {
        return Ok(None);
    };
    if relaunch_now {
        let _child = spawn_detached_relauncher(
            exe,
            &app_bundle,
            state_dir,
            &staged.from_version,
            &staged.to_version,
            Some(std::process::id()),
            DEFAULT_HEALTH_TIMEOUT,
        )?;
        staged.relaunched = true;
    }
    Ok(Some(staged))
}

/// `krill-app --update-cli [--check] [--app <path>] [--state-dir <path>] [--json]` 헤드리스 진입점(MK-156).
#[must_use]
pub fn maybe_run_update_cli() -> Option<i32> {
    let mut args = std::env::args_os();
    let _bin = args.next()?;
    if args.next().as_deref() != Some(std::ffi::OsStr::new(UPDATE_CLI_ARG)) {
        return None;
    }
    Some(match run_update_cli_args(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("krill update error: {e}");
            1
        }
    })
}

fn run_update_cli_args(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<i32, Error> {
    let mut check_only = false;
    let mut json_out = false;
    let mut app_arg: Option<String> = None;
    let mut state_arg: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        let s = arg.to_string_lossy();
        match s.as_ref() {
            "--check" => check_only = true,
            "--json" => json_out = true,
            "--app" => {
                let val = args
                    .next()
                    .ok_or_else(|| Error::Config("--app requires a path".to_owned()))?;
                app_arg = Some(val.to_string_lossy().into_owned());
            }
            "--state-dir" => {
                let val = args
                    .next()
                    .ok_or_else(|| Error::Config("--state-dir requires a path".to_owned()))?;
                state_arg = Some(PathBuf::from(val));
            }
            other => {
                return Err(Error::Config(format!("unknown --update-cli flag: {other}")));
            }
        }
    }

    let exe = std::env::current_exe()?;
    let env_bundle = std::env::var("KRILL_UPDATE_BUNDLE_PATH").ok();
    let app_bundle = current_app_bundle_in(app_arg.as_deref().or(env_bundle.as_deref()), &exe)
        .unwrap_or_else(default_app_bundle);
    let state_dir = match state_arg {
        Some(dir) => dir,
        None => krill_proto::daemon::state_dir().ok_or_else(|| {
            Error::Config("state directory unavailable; pass --state-dir".to_owned())
        })?,
    };

    let mut cfg = Config::load_for_bundle(&state_dir, Some(&app_bundle))?;
    if let Some(url) = std::env::var("KRILL_UPDATE_MANIFEST_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        url.trim().clone_into(&mut cfg.url);
        cfg.validate()?;
    }
    let pk = cfg.public_key_bytes()?;
    let manifest = fetch_manifest(&cfg.url)?;
    let from_version = effective_version(Some(&app_bundle));
    let decision = manifest.evaluate(
        &pk,
        &from_version,
        cfg.channel,
        PROTO_MAX,
        rollout_bucket(&state_dir),
    )?;

    if check_only {
        let available = decision == Decision::Available;
        if json_out {
            println!(
                "{}",
                serde_json::json!({
                    "status": decision.cli_status(),
                    "current_version": from_version,
                    "latest_version": manifest.version,
                    "available": available,
                })
            );
        } else if available {
            println!(
                "Update available: v{} -> v{} ({})",
                from_version,
                manifest.version,
                app_bundle.display()
            );
        } else {
            println!(
                "{}",
                decision.skip_message(&from_version, &manifest, cfg.channel)
            );
        }
        return Ok(0);
    }

    if decision != Decision::Available {
        if json_out {
            println!(
                "{}",
                serde_json::json!({
                    "status": decision.cli_status(),
                    "current_version": from_version,
                    "latest_version": manifest.version,
                    "updated": false,
                })
            );
        } else {
            println!(
                "{}",
                decision.skip_message(&from_version, &manifest, cfg.channel)
            );
        }
        return Ok(0);
    }

    let verifier = CodesignVerifier::from_config(&cfg);
    let _ = download_and_stage(
        &manifest,
        &pk,
        &from_version,
        cfg.channel,
        &app_bundle,
        &verifier,
        rollout_bucket(&state_dir),
    )?;

    // Windows는 실행 중 GUI를 끄지 않는다. 이름 바꾸기 교체라 GUI는 구 이미지로 계속 돌고 다음 실행부터 새 버전이다.
    if !cfg!(windows)
        && let Some(gui_pid) = find_running_gui_pid(&app_bundle)
    {
        crate::settings::child::output(
            crate::platform::background_command("/bin/kill").args(["-TERM", &gui_pid.to_string()]),
            Duration::from_secs(10),
            1024,
        )
        .ok();
        let status = spawn_detached_relauncher(
            &exe,
            &app_bundle,
            &state_dir,
            &from_version,
            &manifest.version,
            Some(gui_pid),
            DEFAULT_HEALTH_TIMEOUT,
        )?
        .wait()?;
        // relauncher는 롤백한 뒤에도 0으로 끝난다. 새 버전이 시작 확인을 통과했는지는 저널만 안다.
        let journal = Journal::load(&state_dir)?;
        if !status.success()
            || !journal
                .as_ref()
                .is_some_and(|j| j.to_version == manifest.version && j.stage == Stage::Healthy)
        {
            return Err(Error::Io(io::Error::other(format!(
                "update to v{} did not complete (relauncher {status}, journal {:?})",
                manifest.version,
                journal.map(|j| j.stage)
            ))));
        }
    } else {
        let _ = swap_bundles(
            &app_bundle,
            &state_dir,
            &from_version,
            &manifest.version,
            DEFAULT_HEALTH_TIMEOUT,
            &verifier,
        )?;
        if let Ok(Some(mut journal)) = Journal::load(&state_dir) {
            journal.stage = Stage::Healthy;
            journal.save(&state_dir).ok();
        }
        // 교체 뒤 구 holder·krilld에 새 실행 파일로 업그레이드를 요청한다(기존 IPC). Windows inbox 축소 모드는
        // 세션이 있으면 holder가 거절(EBUSY)해 구 holder가 다음 기동까지 남는다(OWE-832).
        let socket = krill_proto::daemon::socket_path(&state_dir)?;
        let helpers = if cfg!(windows) {
            app_bundle.clone()
        } else {
            app_bundle.join("Contents/Helpers")
        };
        let exe_name = |name: &str| format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let holder = helpers.join(exe_name("krill-holder"));
        let krilld = helpers.join(exe_name("krilld"));
        if socket.exists() && holder.is_file() && krilld.is_file() {
            let paths = krill_proto::daemon::Paths {
                socket,
                holder,
                krilld,
            };
            let hello = krill_proto::daemon::heartbeat_hello(
                krill_proto::ipc::ClientRole::Cli,
                "krill-update",
            );
            krill_proto::daemon::upgrade_as(&paths, &hello, &manifest.version).ok();
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::json!({
                "status": "updated",
                "from_version": from_version,
                "to_version": manifest.version,
                "app_bundle": app_bundle.display().to_string(),
                "updated": true,
            })
        );
    } else {
        println!(
            "Updated Krill v{} -> v{} ({})",
            from_version,
            manifest.version,
            app_bundle.display()
        );
    }
    Ok(0)
}

fn find_running_gui_pid(app_bundle: &Path) -> Option<u32> {
    let target_exe = app_bundle.join(APP_EXE);
    let target_canon = fs::canonicalize(&target_exe).unwrap_or(target_exe);
    let target_str = target_canon.to_string_lossy();
    let self_pid = std::process::id();
    let out = crate::settings::child::output(
        crate::platform::background_command("/bin/ps").args(["-axo", "pid=,command="]),
        Duration::from_secs(10),
        4 * 1024 * 1024,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let trimmed = line.trim();
        let Some((pid_str, cmd)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_str.trim().parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let cmd = cmd.trim();
        if cmd.starts_with(target_str.as_ref())
            && !cmd.contains(UPDATE_CLI_ARG)
            && !cmd.contains(RELAUNCH_ARG)
        {
            return Some(pid);
        }
    }
    None
}

/// 번들 경로 결정을 테스트에서 직접 검증한다. `bundle_path`가 비어 있지 않으면 그대로 쓰고,
/// 아니면 실행 파일 위치(`....app/Contents/MacOS/krill-app`, Windows는 `Krill.json`이 있는 exe 폴더)에서 번들을 찾는다.
pub fn current_app_bundle_in(bundle_path: Option<&str>, exe: &Path) -> Option<PathBuf> {
    if let Some(bundle) = bundle_path.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(bundle));
    }
    if cfg!(windows) {
        let dir = exe.parent()?;
        return dir.join(BUNDLE_INFO).is_file().then(|| dir.to_path_buf());
    }
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().is_some_and(|e| e == "app") || bundle.join("Contents/Info.plist").is_file())
        .then(|| bundle.to_path_buf())
}

/// OS 기본 도구의 절대 경로(PATH 가로채기 방지). Windows 10 1803+는 `System32`에 `curl.exe`·`tar.exe`가 있다.
fn system_tool(name: &str) -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
            .join(format!("{name}.exe"))
    } else {
        Path::new("/usr/bin").join(name)
    }
}

/// zip을 푼다. macOS는 `ditto -x -k`(확장 속성·서명 보존), Windows는 `tar.exe -xf`(bsdtar, zip 지원).
fn unpack_with_ditto(zip_path: &Path, unpack_tmp: &Path, staging: &Path) -> Result<(), Error> {
    fs::create_dir_all(unpack_tmp)?;
    let mut cmd = if cfg!(windows) {
        let mut c = crate::platform::background_command(system_tool("tar"));
        c.arg("-xf").arg(zip_path).arg("-C").arg(unpack_tmp);
        c
    } else {
        let mut c = crate::platform::background_command(system_tool("ditto"));
        c.args(["-x", "-k"]).arg(zip_path).arg(unpack_tmp);
        c
    };
    let out = crate::settings::child::output(&mut cmd, Duration::from_secs(120), 65536)?;
    if !out.status.success() {
        return Err(Error::Unpack(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    reject_escaping_symlinks(unpack_tmp, unpack_tmp)?;
    let extracted = find_extracted_bundle(unpack_tmp)?;
    fs::rename(extracted, staging)?;
    Ok(())
}

fn find_extracted_bundle(unpack_tmp: &Path) -> Result<PathBuf, Error> {
    if unpack_tmp.join(BUNDLE_INFO).is_file() {
        return Ok(unpack_tmp.to_path_buf());
    }
    for entry in fs::read_dir(unpack_tmp)? {
        let path = entry?.path();
        if (cfg!(windows) || path.extension().is_some_and(|e| e == "app"))
            && path.join(BUNDLE_INFO).is_file()
        {
            return Ok(path);
        }
    }
    Err(Error::BundleStructure(
        "archive does not contain a valid .app bundle".to_owned(),
    ))
}

fn reject_escaping_symlinks(root: &Path, dir: &Path) -> Result<(), Error> {
    let canonical_root = root.canonicalize()?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let path = entry.path();
        if ft.is_symlink() {
            let target = fs::read_link(&path)?;
            let resolved = if target.is_absolute() {
                target
            } else {
                path.parent().unwrap_or(dir).join(target)
            };
            let canon = resolved
                .canonicalize()
                .map_err(|e| Error::BundleStructure(format!("broken symlink: {e}")))?;
            if !canon.starts_with(&canonical_root) {
                return Err(Error::BundleStructure(format!(
                    "symlink escapes bundle: {}",
                    path.display()
                )));
            }
        } else if ft.is_dir() {
            reject_escaping_symlinks(root, &path)?;
        }
    }
    Ok(())
}

pub(super) fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;

#[cfg(all(test, windows))]
#[test]
fn windows_bundle_without_update_url_uses_windows_manifest() {
    let root =
        std::env::temp_dir().join(format!("krill-windows-update-url-{}", std::process::id()));
    let bundle = root.join("Krill");
    let state = root.join("state");
    fs::create_dir_all(&bundle).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(
        bundle.join("Krill.json"),
        r#"{"CFBundleShortVersionString":"0.2.6"}"#,
    )
    .unwrap();
    let config = Config::load_for_bundle(&state, Some(&bundle)).unwrap();
    assert_eq!(
        config.url,
        "https://github.com/craftowen/krill/releases/latest/download/latest-windows.json"
    );
    assert_eq!(effective_version(Some(&bundle)), "0.2.6");
    fs::remove_dir_all(root).unwrap();
}
