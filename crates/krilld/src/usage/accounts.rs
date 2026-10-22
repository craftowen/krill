//! 프로바이더 다계정 레지스트리(`accounts.toml`)·계정별 5h/7d 사용량 측정·원클릭 전환·자동전환 엔진(MK-107·MK-108).

pub(crate) mod codex_home;
mod identity;
pub(super) use identity::from_credentials as identity_from_credentials;
mod login;
pub(crate) mod orca_vault;
mod policy;
pub(crate) mod switch_work;
mod profile_name;
pub use profile_name::{Keychain, Vault, adopt_email_dirs};

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::usage::{
    AUTO_SWITCH_HYSTERESIS, AUTO_SWITCH_THRESHOLD_MAX, AUTO_SWITCH_THRESHOLD_MIN, AccountInfo,
    AccountStatus, AccountStatusInfo, AccountsSnapshot, AutoSwitchEvent, AutoSwitchMetric,
    AutoSwitchPolicy, AutoSwitchRule, DEFAULT_AUTO_SWITCH_PERCENT, OrphanProfile, ProviderKind,
    ResetConsumeOutcome, Snapshot, Window as ProtoWindow,
};
use serde::{Deserialize, Serialize};

use super::{
    AccountUsage, CLAUDE_CACHE, ORCA_CACHE, ProfileUsage, Usage, Window, latest_rollout,
    mtime_secs, parse_claude, parse_orca, read_codex,
};
use crate::session::lock;

const ACCOUNTS_FILE: &str = "accounts.toml";
const ACTIVE_ENV_FILE: &str = "active-accounts.env";
/// 상태 꼬리 재료: (Orca `error` 원문, `detail` 원문, 갱신 시각 epoch 초).
type StatusMeta = (Option<String>, Option<String>, Option<u64>);
/// `StopFailure` `rate_limit` 감지 계정 기록(OWE-703 2단계 지시·OWE-710).
/// (프로바이더 키, 계정 id) → 감지 시각(epoch 초). 메모리 전용이라 데몬 재기동이면
/// 사라진다. OWE-712가 소진 창 리셋 또는 리셋 미상의 15분 뒤 해제한다.
/// `SAVE_LOCK`과 중첩하지 않게 `refresh_*`는 시작 때 통째로 복사해 쓴다.
static RATE_LIMITED: Mutex<BTreeMap<(String, String), u64>> = Mutex::new(BTreeMap::new());

/// 한도 도달을 기록한다. 캐시 사용량이 미상·낮아도 후보에서 빠지고 전환 평가가 일어난다.
/// 수동 전환 경로(`switch_account`)·`switch_on_rate_limit=false`·비활성 정책은 그대로 존중된다.
pub fn record_rate_limited(provider: ProviderKind, account_id: &str) {
    let now = now_epoch();
    let now = u64::try_from(now).unwrap_or(u64::MAX);
    lock(&RATE_LIMITED).insert((provider.as_str().to_owned(), account_id.to_owned()), now);
}

pub(crate) fn has_rate_limits() -> bool {
    !lock(&RATE_LIMITED).is_empty()
}

/// 한도 기록을 지운다(복구·테스트 정리용). 있었으면 참이다.
pub fn clear_rate_limited(provider: ProviderKind, account_id: &str) -> bool {
    lock(&RATE_LIMITED)
        .remove(&(provider.as_str().to_owned(), account_id.to_owned()))
        .is_some()
}

fn rate_limited_at(
    limited: &BTreeMap<(String, String), u64>,
    provider: ProviderKind,
    account_id: &str,
) -> Option<u64> {
    limited
        .get(&(provider.as_str().to_owned(), account_id.to_owned()))
        .copied()
}

const MAX_MANUAL_PINS: usize = 256;

/// K-12·OWE-770: 수동 전환(`switch_account`)으로 고른 계정의 런타임 고정 상태.
/// 선택 시점의 사용량·정책이 유지되는 동안 주기 `refresh_accounts`/`ListAccounts` 자동전환에서 제외하며,
/// 사용량·리셋 시각 변화, 한도(`RATE_LIMITED`) 도달, 자동전환 정책 변경, 데몬 재기동 시 해제된다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManualPin {
    account_id: String,
    usage: Snapshot,
    policy: AutoSwitchPolicy,
    advanced_policy: krill_proto::usage::AdvancedPolicy,
}

static MANUAL_PINS: Mutex<BTreeMap<(PathBuf, ProviderKind), ManualPin>> =
    Mutex::new(BTreeMap::new());

fn set_manual_pin(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    account_id: &str,
    usage: Snapshot,
    file: &AccountsFile,
) {
    let key = (config_path(home, state_dir), provider);
    let pin = ManualPin {
        account_id: account_id.to_owned(),
        usage,
        policy: AutoSwitchPolicy::from(file.auto_switch.clone()),
        advanced_policy: file.advanced_policy.clone(),
    };
    let mut pins = lock(&MANUAL_PINS);
    if !pins.contains_key(&key) && pins.len() >= MAX_MANUAL_PINS {
        pins.pop_first();
    }
    pins.insert(key, pin);
}

fn manually_pinned(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    active_acc: &AccountInfo,
    limited_active: bool,
    policy: &AutoSwitchPolicy,
    advanced_policy: &krill_proto::usage::AdvancedPolicy,
) -> bool {
    let key = (config_path(home, state_dir), provider);
    let mut pins = lock(&MANUAL_PINS);
    let Some(pin) = pins.get(&key) else {
        return false;
    };
    if pin.account_id != active_acc.id
        || limited_active
        || &pin.policy != policy
        || &pin.advanced_policy != advanced_policy
    {
        pins.remove(&key);
        return false;
    }
    if active_acc.usage.is_empty() || pin.usage == active_acc.usage {
        return true;
    }
    pins.remove(&key);
    false
}
/// `accounts.toml` 읽기-수정-쓰기 전체를 직렬화하는 프로세스 전역 잠금(K-09). 파일 IO만 안에서 하고
/// 자동전환 작업 알림은 마지막에 발행하며, 작업 큐 잠금 안에서는 이 잠금을 잡지 않는다.
static SAVE_LOCK: Mutex<()> = Mutex::new(());
/// 원자 쓰기 임시 파일명 중복 방지용 단조 증가 값(K-09). pid와 합쳐 같은 프로세스 스레드끼리도 겹치지 않는다.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredAccount {
    pub id: String,
    pub provider: String,
    pub label: String,
    #[serde(default)]
    pub email: Option<String>,
    pub profile_dir: String,
}

impl StoredAccount {
    pub fn config_dir(&self) -> &Path {
        Path::new(&self.profile_dir)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountsFile {
    #[serde(default)]
    pub auto_switch: StoredAutoSwitch,
    /// provider (`"claude"`, `"codex"` 등) → 활성 account `id`.
    #[serde(default)]
    pub active: BTreeMap<String, String>,
    #[serde(default)]
    pub accounts: Vec<StoredAccount>,
    #[serde(default)]
    pub advanced_policy: krill_proto::usage::AdvancedPolicy,
    /// OWE-699: 계정 프로필 프로비저닝(`super::provision`). 키가 없으면 켜짐(Orca처럼 스킬·대화 기록 공유,
    /// 오웬 지시 2026-10-22). 명시한 `provisioning = false`는 그대로 존중한다.
    #[serde(default = "provisioning_default")]
    pub provisioning: bool,
    /// OWE-719: 기존 자격증명을 읽기만 하는 직접 사용량 조회(오웬 지시로 기본 켜짐).
    #[serde(default = "direct_usage_default")]
    pub direct_usage: bool,
}

fn direct_usage_default() -> bool {
    true
}

fn provisioning_default() -> bool {
    true
}

impl Default for AccountsFile {
    fn default() -> Self {
        Self {
            auto_switch: StoredAutoSwitch::default(),
            active: BTreeMap::new(),
            accounts: Vec::new(),
            advanced_policy: krill_proto::usage::AdvancedPolicy::default(),
            provisioning: provisioning_default(),
            direct_usage: true,
        }
    }
}

impl AccountsFile {
    pub fn load(state_dir: &Path) -> Self {
        let home = default_home();
        let state = (!state_dir.as_os_str().is_empty()).then_some(state_dir);
        load_file(&home, state)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAutoSwitch {
    /// K-03: 필드 누락(부분 쓰기·구버전 파일) 때도 파싱이 깨지지 않게 정책 기본값으로 채운다.
    #[serde(default = "default_switch_enabled")]
    pub enabled: bool,
    #[serde(default = "default_switch_threshold")]
    pub threshold_percent: u8,
    #[serde(default = "default_switch_on_rate_limit")]
    pub switch_on_rate_limit: bool,
    #[serde(default)]
    pub rules: Vec<AutoSwitchRule>,
}

fn default_switch_enabled() -> bool {
    AutoSwitchPolicy::default().enabled
}

fn default_switch_threshold() -> u8 {
    DEFAULT_AUTO_SWITCH_PERCENT
}

fn default_switch_on_rate_limit() -> bool {
    AutoSwitchPolicy::default().switch_on_rate_limit
}

impl Default for StoredAutoSwitch {
    fn default() -> Self {
        let d = AutoSwitchPolicy::default();
        Self {
            enabled: d.enabled,
            threshold_percent: d.threshold_percent,
            switch_on_rate_limit: d.switch_on_rate_limit,
            rules: d.rules,
        }
    }
}

impl From<StoredAutoSwitch> for AutoSwitchPolicy {
    fn from(s: StoredAutoSwitch) -> Self {
        Self {
            enabled: s.enabled,
            threshold_percent: s
                .threshold_percent
                .clamp(AUTO_SWITCH_THRESHOLD_MIN, AUTO_SWITCH_THRESHOLD_MAX),
            switch_on_rate_limit: s.switch_on_rate_limit,
            rules: s.rules,
        }
    }
}

impl From<&AutoSwitchPolicy> for StoredAutoSwitch {
    fn from(p: &AutoSwitchPolicy) -> Self {
        Self {
            enabled: p.enabled,
            threshold_percent: p
                .threshold_percent
                .clamp(AUTO_SWITCH_THRESHOLD_MIN, AUTO_SWITCH_THRESHOLD_MAX),
            switch_on_rate_limit: p.switch_on_rate_limit,
            rules: p.rules.clone(),
        }
    }
}

impl Usage {
    pub fn to_proto(&self) -> Snapshot {
        Snapshot {
            five_hour: self.five_hour.as_ref().map(Window::to_proto),
            seven_day: self.seven_day.as_ref().map(Window::to_proto),
        }
    }
}

impl Window {
    pub fn to_proto(&self) -> ProtoWindow {
        ProtoWindow {
            percent: self.percent,
            resets_at: self.resets_at.clone(),
        }
    }
}

/// `accounts.toml` 경로: `state_dir`이 주어지면 `<state_dir>/accounts.toml`, 아니면 `<home>/.config/krill/accounts.toml`.
pub fn config_path(home: &Path, state_dir: Option<&Path>) -> PathBuf {
    if let Some(state) = state_dir {
        state.join(ACCOUNTS_FILE)
    } else {
        krill_proto::user_dirs::config_dir(home).join(ACCOUNTS_FILE)
    }
}

/// K-03: 파일 없음만 기본값으로 처리한다. 파싱 실패·IO 오류는 `Err`로 돌려써서
/// 호출자가 원문을 보존하고 쓰기를 거부하게 한다.
pub fn load_file_result(home: &Path, state_dir: Option<&Path>) -> io::Result<AccountsFile> {
    let path = config_path(home, state_dir);
    let text = std::fs::read_to_string(&path)?;
    toml::from_str::<AccountsFile>(&text).map_err(io::Error::other)
}

/// 쓰기 경로용 로드: 파일 없음은 기본값, 그 밖의 오류는 그대로 돌려써 원문을 보존한다(K-03).
fn load_writable(home: &Path, state_dir: Option<&Path>) -> io::Result<AccountsFile> {
    match load_file_result(home, state_dir) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AccountsFile::default()),
        Err(e) => Err(e),
    }
}

pub fn load_file(home: &Path, state_dir: Option<&Path>) -> AccountsFile {
    match load_file_result(home, state_dir) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => AccountsFile::default(),
        // 읽기 전용 경로(히스토리 스캔 등)의 관대한 폴백. 쓰기 경로는
        // `load_file_result`를 직접 써서 손상된 원문을 덮어쓰지 않는다.
        Err(_) => AccountsFile::default(),
    }
}

pub fn save_file(home: &Path, state_dir: Option<&Path>, file: &AccountsFile) -> io::Result<()> {
    let _guard = lock(&SAVE_LOCK);
    save_inner(home, state_dir, file)
}

fn save_inner(home: &Path, state_dir: Option<&Path>, file: &AccountsFile) -> io::Result<()> {
    let path = config_path(home, state_dir);
    let text = krill_proto::config_document::save_toml(&path, file)?;
    write_atomic_0600(&path, text.as_bytes())?;
    if let Err(error) = write_active_env(home, state_dir, file) {
        // accounts.toml은 이미 커밋됐다. env 내보내기 실패를 기록하되 저장 성공은 유지한다.
        tracing::warn!(kind = ?error.kind(), "active account env export failed");
    }
    Ok(())
}

pub(crate) fn write_atomic_0600(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no parent dir"));
    };
    std::fs::create_dir_all(parent)?;
    // K-09: pid만으로는 같은 프로세스의 연결 스레드끼리 겹친다. 단조 증가 값을 더해 겹치지 않게 한다.
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("accounts"),
        std::process::id(),
        seq
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use krill_proto::os::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let write = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if write.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    write
}

pub(crate) fn resolve_codex_dir(home: &Path, id: &str, raw_dir: &Path) -> PathBuf {
    let slug = id.strip_prefix("codex-").unwrap_or(id);
    krill_proto::usage::profiles::migrate_long_codex_profile_if_needed(home, slug, raw_dir)
}

fn write_active_env(home: &Path, state_dir: Option<&Path>, file: &AccountsFile) -> io::Result<()> {
    use std::fmt::Write as _;
    let mut lines = String::new();
    for provider in ProviderKind::ALL {
        let key = provider.as_str();
        if let Some(active_id) = file.active.get(key) {
            // OWE-696: `active_profile_env`와 같은 규칙. 폴더가 없는 계정(Orca 감지 포함)은
            // 실행 시 env를 주입하지 않아 호스트 기본 폴더로 뜨므로 여기도 호스트 경로를 적는다.
            let dir = file
                .accounts
                .iter()
                .find(|a| a.id == *active_id && a.config_dir().is_dir())
                .map_or_else(
                    || host_default_dir(home, provider).display().to_string(),
                    |a| {
                        let raw_dir = a.config_dir();
                        if provider == ProviderKind::Codex {
                            resolve_codex_dir(home, &a.id, raw_dir)
                                .display()
                                .to_string()
                        } else {
                            a.profile_dir.clone()
                        }
                    },
                );
            // K-28: `{:?}`(Rust Debug)는 `$`·백틱을 이스케이프하지 않아 source 시 실행·확장된다.
            // 작은따옴표로 감싸고 안의 따옴표만 `'\''`로 끊어 셸에 안전하게 넘긴다.
            writeln!(lines, "export {}={}", provider.env_var(), sh_quote(&dir)).ok();
        }
    }
    let target = state_dir.map_or_else(
        || krill_proto::user_dirs::config_dir(home).join(ACTIVE_ENV_FILE),
        |s| s.join(ACTIVE_ENV_FILE),
    );
    write_atomic_0600(&target, lines.as_bytes())
}

fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for (i, part) in s.split('\'').enumerate() {
        if i > 0 {
            out.push_str("'\\''");
        }
        out.push_str(part);
    }
    out.push('\'');
    out
}

pub fn default_home() -> PathBuf {
    krill_proto::os::home_dir().unwrap_or_else(std::env::temp_dir)
}

pub fn host_default_dir(home: &Path, provider: ProviderKind) -> PathBuf {
    match provider {
        ProviderKind::Claude => home.join(".claude"),
        ProviderKind::Codex => home.join(".codex"),
        ProviderKind::Gemini => PathBuf::new(),
        ProviderKind::OpenCode => home.join(".config").join("opencode"),
        ProviderKind::Antigravity => home.join(".gemini").join("antigravity-cli"),
    }
}

pub fn host_default_id(provider: ProviderKind) -> String {
    format!("{}-host", provider.as_str())
}

/// OWE-696: Krill이 이 계정으로 실행할 수 있는지. 호스트 기본 계정 외에는 프로필 폴더가 실제로 있어야 한다.
/// Orca 감지 계정은 Krill이 아는 폴더가 없어(`profile_dir` 빈 값) 항상 거짓이다.
fn runnable(a: &AccountInfo) -> bool {
    a.host_default || Path::new(&a.profile_dir).is_dir()
}

/// 프로필 디렉터리에서 해당 프로바이더 계정의 5h/7d 사용량·상태 근거를 읽는다.
/// 실제로 읽은 파일의 수정 시각을 `updated_at`에 담고, 프로필 파일이 없고
/// Orca 캐시 행에서 받았을 때만 `error`·`detail`이 든다.
pub fn read_profile_usage(
    home: &Path,
    provider: ProviderKind,
    profile_dir: &Path,
    label: &str,
    email: Option<&str>,
    host_default: bool,
) -> ProfileUsage {
    if let Some(usage) = super::direct::cached(home, provider, profile_dir) {
        return usage;
    }
    match provider {
        ProviderKind::Gemini => ProfileUsage::default(),
        ProviderKind::Claude => {
            for name in [CLAUDE_CACHE, "usage_cache.json"] {
                let path = profile_dir.join(name);
                if let Ok(s) = std::fs::read_to_string(&path) {
                    let u = parse_claude(&s);
                    if !u.is_empty() {
                        return ProfileUsage {
                            usage: u,
                            stale: false,
                            updated_at: mtime_secs(&path),
                            error: None,
                            detail: None,
                        };
                    }
                }
            }
            // 멀티계정 캐시(`orca-usage-cache.json`)에서 일치하는 계정을 찾는다.
            let orca_path = if profile_dir.join(ORCA_CACHE).exists() {
                profile_dir.join(ORCA_CACHE)
            } else {
                home.join(".claude").join(ORCA_CACHE)
            };
            if let Ok(s) = std::fs::read_to_string(&orca_path) {
                let rows = parse_orca(&s);
                let stamp = mtime_secs(&orca_path);
                let found = rows
                    .iter()
                    .find(|r| {
                        (!r.label.is_empty()
                            && (r.label.eq_ignore_ascii_case(label)
                                || email.is_some_and(|e| r.label.eq_ignore_ascii_case(e))))
                            || (host_default && r.host)
                    })
                    .or_else(|| {
                        if host_default {
                            super::primary(&rows)
                        } else {
                            None
                        }
                    });
                if let Some(acc) = found {
                    return ProfileUsage {
                        usage: acc.usage.clone(),
                        stale: acc.stale,
                        updated_at: acc.updated_at.or(stamp),
                        error: acc.error.clone(),
                        detail: acc.detail.clone(),
                    };
                }
            }
            ProfileUsage::default()
        }
        ProviderKind::Codex => {
            let sessions = profile_dir.join("sessions");
            // `read_codex`와 같은 순서(세션 dir → 프로필 root)로 최신 rollout을 본다.
            let usage = read_codex(&sessions).or_else(|| read_codex(profile_dir));
            if let Some(u) = usage {
                let updated_at = latest_rollout(&sessions)
                    .or_else(|| latest_rollout(profile_dir))
                    .as_deref()
                    .and_then(mtime_secs);
                return ProfileUsage {
                    usage: u,
                    stale: false,
                    updated_at,
                    error: None,
                    detail: None,
                };
            }
            for name in [CLAUDE_CACHE, "usage_cache.json"] {
                let path = profile_dir.join(name);
                if let Ok(s) = std::fs::read_to_string(&path) {
                    let u = parse_claude(&s);
                    if !u.is_empty() {
                        return ProfileUsage {
                            usage: u,
                            stale: false,
                            updated_at: mtime_secs(&path),
                            error: None,
                            detail: None,
                        };
                    }
                }
            }
            ProfileUsage::default()
        }
        ProviderKind::OpenCode | ProviderKind::Antigravity => {
            for name in [
                CLAUDE_CACHE,
                "usage.json",
                "usage_cache.json",
                "quota.json",
                "stats-cache.json",
                "rate_limits.json",
            ] {
                let path = profile_dir.join(name);
                if let Ok(s) = std::fs::read_to_string(&path) {
                    let u = parse_claude(&s);
                    if !u.is_empty() {
                        return ProfileUsage {
                            usage: u,
                            stale: false,
                            updated_at: mtime_secs(&path),
                            error: None,
                            detail: None,
                        };
                    }
                    let agy = krill_proto::usage::parse_antigravity_usage(&s);
                    if !agy.is_empty() {
                        return ProfileUsage {
                            usage: Usage {
                                five_hour: agy.five_hour.map(|w| Window {
                                    percent: w.percent,
                                    resets_at: w.resets_at,
                                }),
                                seven_day: agy.seven_day.map(|w| Window {
                                    percent: w.percent,
                                    resets_at: w.resets_at,
                                }),
                            },
                            stale: false,
                            updated_at: mtime_secs(&path),
                            error: None,
                            detail: None,
                        };
                    }
                }
            }
            ProfileUsage::default()
        }
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch == '-' || ch == '_' || ch == ' ' || ch == '.' || ch == '@')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "account".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Orca 감지 행의 안정 id(OWE-696). uuid 행은 `claude-orca-<uuid>`, 구버전 행만 순번 id.
fn orca_detected_id(idx: usize, row: &AccountUsage) -> String {
    if row.id.is_empty() {
        format!("claude-orca-{idx}-{}", slugify(&row.label))
    } else {
        format!("claude-orca-{}", slugify(&row.id))
    }
}

/// Orca 프로필 경로에서 계정 uuid를 뽑는다(OWE-896).
/// `claude-profiles/<uuid>/home`·`claude-accounts/<uuid>/auth`·`codex-accounts/<uuid>/home`.
/// uuid 형태가 아니면 `None`(사용자 임의 경로와 오인 병합 방지).
fn orca_uuid_from_profile_dir(dir: &str) -> Option<&str> {
    let mut parts = dir.split('/');
    let leaf = parts.next_back()?;
    let uuid = parts.next_back()?;
    let parent = parts.next_back()?;
    if !matches!(
        parent,
        "claude-profiles" | "claude-accounts" | "codex-accounts"
    ) || !matches!(leaf, "home" | "auth")
    {
        return None;
    }
    is_uuid_shape(uuid).then_some(uuid)
}

/// uuid v4 텍스트 형태(8-4-4-4-12 hex)만 인정한다.
fn is_uuid_shape(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, c) in bytes.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if *c != b'-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// `"email (조직)"` 라벨에서 비교용 이메일만 뽑는다(OWE-896). `@`가 없으면 라벨 전체.
fn primary_email(label: &str) -> &str {
    match label.split_once(" (") {
        Some((head, _)) if head.contains('@') => head.trim(),
        _ => label.trim(),
    }
}

/// K-04: 비ASCII 라벨은 `slugify`가 모두 `"account"`로 만들어 id·프로필 디렉터리가 충돌한다.
/// 같은 라벨이면 기존 항목을 그대로 고쳐 멱등성을 지키고(기존 id·디렉터리 유지),
/// 다른 라벨이 같은 id를 차지하면 `-2`, `-3` 접미사로 새 id·디렉터리를 만든다.
fn unique_id_and_slug(
    file: &AccountsFile,
    provider: ProviderKind,
    clean_label: &str,
) -> (String, String) {
    let base = slugify(clean_label);
    // 빈 기본 슬롯이 생겨도 접미사·수동 지정 ID가 있는 같은 계정을 먼저 찾는다.
    if let Some(existing) = file
        .accounts
        .iter()
        .find(|a| a.label == clean_label && ProviderKind::parse(&a.provider) == Some(provider))
    {
        let prefix = format!("{}-", provider.as_str());
        let slug = existing
            .id
            .strip_prefix(&prefix)
            .unwrap_or(&base)
            .to_owned();
        return (existing.id.clone(), slug);
    }
    let mut slug = base.clone();
    let mut suffix = 2u32;
    loop {
        let id = format!("{}-{slug}", provider.as_str());
        match file.accounts.iter().find(|a| a.id == id) {
            None => return (id, slug),
            Some(existing)
                if existing.label == clean_label && existing.provider == provider.as_str() =>
            {
                return (id, slug);
            }
            Some(_) => {
                slug = format!("{base}-{suffix}");
                suffix = suffix.saturating_add(1);
            }
        }
    }
}

/// 현재 시각(epoch 초). 실패하면 0(모든 `resets_at`을 미래로 취급해 기존 동작 유지).
fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// K-11: `resets_at`(RFC 3339 또는 epoch 초 문자열)을 epoch 초로 푼다. 형식을 모르면 `None`.
/// OWE-709: 중복 파서를 `krill_proto::time` 하나로 통합했다. 기존 `T`·`Z`·숫자 오프셋·소수초는
/// 같은 값으로 풀리고, `t`/공백 구분자·`±HHMM`·`,` 소수가 추가로 풀린다(실측 캐시 형식은 불변).
/// 테스트에서 리셋 시각 픽스처를 풀 때 쓴다.
#[cfg(test)]
#[cfg(unix)]
pub(crate) use krill_proto::time::parse_reset_epoch;

/// Orca `error` 원문을 상태로 푼다(OWE-703). 로그인 계열 문구면 로그인 필요, 그 밖은 오류다.
/// 실측 행: `error: "재로그인"`, `detail: "token 400"`.
fn classify_error(text: &str) -> AccountStatus {
    let lower = text.to_ascii_lowercase();
    if [
        "재로그인",
        "로그인 필요",
        "login required",
        "not logged in",
        "authentication required",
        "unauthorized",
        "token expired",
        "invalid token",
        "401",
        "403",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        AccountStatus::NeedsLogin
    } else {
        AccountStatus::Error
    }
}

/// 계정 상태 분류(OWE-703). 한도 기록이 있으면 값을 덮어쓰고, 오류가 있으면 값을 덮어쓰고,
/// 값이 없으면 모름이다. 모름은 0%가 아니라 자동전환 후보·트리거에서 빠진다(K-10).
/// `limited`는 한도 감지 시각(`record_rate_limited`)으로, 있으면 꼬리 갱신 시각에 든다.
fn classify_account(
    error: Option<&str>,
    stale: bool,
    empty: bool,
    limited: Option<u64>,
) -> (AccountStatus, Option<u64>) {
    if let Some(detected) = limited {
        return (AccountStatus::RateLimited, Some(detected));
    }
    if let Some(text) = error {
        return (classify_error(text), None);
    }
    if empty {
        return (AccountStatus::Unknown, None);
    }
    if stale {
        return (AccountStatus::Stale, None);
    }
    (AccountStatus::Ok, None)
}

/// 호스트 기본 계정 + `orca-usage-cache.json` 감지 계정 + `accounts.toml` 등록 계정을 합쳐
/// 사용량을 측정하고 필요 시 자동전환(Auto-Switch)까지 수행해 돌려준다.
pub fn refresh_accounts(home: &Path, state_dir: Option<&Path>) -> AccountsSnapshot {
    refresh_accounts_with(home, state_dir, true)
}

/// K-12: 수동 전환 직후에는 자동전환 평가·저장을 건너뛰고 저장된 선택을 그대로 보여준다.
/// `switch_account`가 이 경로로 새로고침한다.
pub(crate) fn refresh_accounts_with(
    home: &Path,
    state_dir: Option<&Path>,
    allow_auto_switch: bool,
) -> AccountsSnapshot {
    let trigger = if allow_auto_switch {
        RefreshTrigger::Automatic
    } else {
        RefreshTrigger::ReadOnly
    };
    refresh_accounts_inner(home, state_dir, trigger)
}

#[derive(Clone, Copy)]
enum RefreshTrigger {
    Automatic,
    ReadOnly,
    RateLimit,
}

/// `StopFailure` 전용 평가. 한도 전환 옵션은 같은 `SAVE_LOCK` 안에서 읽어 적용한다.
pub(crate) fn refresh_accounts_on_rate_limit(
    home: &Path,
    state_dir: Option<&Path>,
) -> AccountsSnapshot {
    refresh_accounts_inner(home, state_dir, RefreshTrigger::RateLimit)
}

fn refresh_accounts_inner(
    home: &Path,
    state_dir: Option<&Path>,
    trigger: RefreshTrigger,
) -> AccountsSnapshot {
    // CLI 프로세스를 기다리는 동안 계정 저장 잠금을 잡지 않는다.
    super::direct::refresh_on_request(home, state_dir);
    let auth = login::statuses(home, state_dir);
    refresh_accounts_authenticated(home, state_dir, trigger, &auth)
}

fn refresh_accounts_authenticated(
    home: &Path,
    state_dir: Option<&Path>,
    trigger: RefreshTrigger,
    auth: &login::Statuses,
) -> AccountsSnapshot {
    let _guard = lock(&SAVE_LOCK);
    // 한도 기록은 잎 잠금이라 여기서 통째로 복사한다(중첩 역전 없음).
    let mut limited: BTreeMap<(String, String), u64> = lock(&RATE_LIMITED).clone();
    // K-03: 손상된 파일은 기본값 스냅샷만 내보내고 저장하지 않는다(계정·규칙 소실 방지).
    let (mut file, writable) = match load_file_result(home, state_dir) {
        Ok(file) => (file, true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (AccountsFile::default(), true),
        Err(_) => (AccountsFile::default(), false),
    };
    let policy = AutoSwitchPolicy::from(file.auto_switch.clone());
    let allow_auto_switch = match trigger {
        RefreshTrigger::Automatic => true,
        RefreshTrigger::ReadOnly => false,
        RefreshTrigger::RateLimit => policy.switch_on_rate_limit,
    };

    let mut all: Vec<AccountInfo> = Vec::new();
    // 계정 id → 상태 꼬리 재료.
    let mut meta: BTreeMap<String, StatusMeta> = BTreeMap::new();
    // 1. 각 프로바이더의 호스트 기본 계정 추가.
    for provider in ProviderKind::ALL {
        let id = host_default_id(provider);
        let dir = host_default_dir(home, provider);
        let label = format!(
            "Host ({})",
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default")
        );
        let pu = read_profile_usage(home, provider, &dir, &label, None, true);
        meta.insert(id.clone(), (pu.error, pu.detail, pu.updated_at));
        all.push(AccountInfo {
            id,
            provider,
            label,
            email: None,
            profile_dir: dir.display().to_string(),
            active: false,
            host_default: true,
            stale: pu.stale,
            usage: pu.usage.to_proto(),
        });
    }

    // 2. `~/.claude/orca-usage-cache.json`에 있는 추가 멀티계정 자동 감지.
    // OWE-896: uuid 행 id → 감지 id 매핑. 3단계에서 등록 계정의 프로필 경로 속
    // Orca uuid와 대조해 라벨·이메일 형식과 무관하게 같은 계정으로 병합한다.
    let mut orca_uuids: BTreeMap<String, String> = BTreeMap::new();
    let orca_path = home.join(".claude").join(ORCA_CACHE);
    if let Ok(s) = std::fs::read_to_string(&orca_path) {
        let orca_stamp = mtime_secs(&orca_path);
        for (idx, row) in parse_orca(&s).into_iter().enumerate() {
            if row.host {
                // 호스트 기본 계정의 라벨/사용량 보강.
                if let Some(host_acc) = all
                    .iter_mut()
                    .find(|a| a.provider == ProviderKind::Claude && a.host_default)
                {
                    if !row.label.is_empty() {
                        host_acc.email = Some(row.label.clone());
                    }
                    if host_acc.usage.is_empty() && !row.usage.is_empty() {
                        host_acc.usage = row.usage.to_proto();
                        host_acc.stale = row.stale;
                        meta.insert(
                            host_acc.id.clone(),
                            (
                                row.error.clone(),
                                row.detail.clone(),
                                row.updated_at.or(orca_stamp),
                            ),
                        );
                    }
                }
                continue;
            }
            // OWE-696: 캐시 행 uuid로 안정 id를 만든다(순번이 바뀌어도 같은 계정). uuid가 없는
            // 구버전 캐시만 순번 id. Orca 프로필 폴더는 Krill이 모르므로 경로를 비워 실행 불가로 둔다.
            let id = orca_detected_id(idx, &row);
            if !row.id.is_empty() {
                orca_uuids.insert(id.clone(), row.id.clone());
            }
            let profile_dir = String::new();
            meta.insert(
                id.clone(),
                (
                    row.error.clone(),
                    row.detail.clone(),
                    row.updated_at.or(orca_stamp),
                ),
            );
            all.push(AccountInfo {
                id,
                provider: ProviderKind::Claude,
                label: if row.label.is_empty() {
                    format!("Claude #{idx}")
                } else {
                    row.label.clone()
                },
                email: (!row.label.is_empty()).then_some(row.label),
                profile_dir,
                active: false,
                host_default: false,
                stale: row.stale,
                usage: row.usage.to_proto(),
            });
        }
    }

    // 3. `accounts.toml`에 사용자가 등록한 계정 병합.
    for stored in &file.accounts {
        let Some(provider) = ProviderKind::parse(&stored.provider) else {
            continue;
        };
        let raw_dir = PathBuf::from(&stored.profile_dir);
        let dir = if provider == ProviderKind::Codex {
            resolve_codex_dir(home, &stored.id, &raw_dir)
        } else {
            raw_dir
        };
        let resolved_profile_dir = dir.display().to_string();
        let pu = read_profile_usage(
            home,
            provider,
            &dir,
            &stored.label,
            stored.email.as_deref(),
            false,
        );
        // 값이 없어도 오류·갱신 시각은 상태 꼬리에 남긴다(로그인 필요 판정).
        meta.insert(
            stored.id.clone(),
            (pu.error.clone(), pu.detail.clone(), pu.updated_at),
        );
        if let Some(existing) = all.iter_mut().find(|a| {
            a.id == stored.id
                || (!a.host_default
                    && a.provider == provider
                    && a.profile_dir.is_empty()
                    && (a.label.eq_ignore_ascii_case(&stored.label)
                        || orca_uuids.get(&a.id).is_some_and(|uuid| {
                            orca_uuid_from_profile_dir(&stored.profile_dir)
                                .is_some_and(|dir_uuid| dir_uuid == uuid)
                        })
                        || stored.email.as_deref().is_some_and(|e| {
                            let peer = a.email.as_deref().unwrap_or(a.label.as_str());
                            (primary_email(peer).contains('@')
                                && primary_email(peer).eq_ignore_ascii_case(primary_email(e)))
                                || a.label.eq_ignore_ascii_case(e)
                        })
                        // OWE-896: 이메일이 없어도 `"email (조직)"` 라벨 속 이메일로 병합한다.
                        // 행에는 조직 정보가 없어 이메일 일치를 같은 계정으로 본다.
                        || {
                            let stored_mail = stored.email.as_deref().unwrap_or(&stored.label);
                            let peer = a.email.as_deref().unwrap_or(a.label.as_str());
                            primary_email(peer).contains('@')
                                && primary_email(peer)
                                    .eq_ignore_ascii_case(primary_email(stored_mail))
                        }))
        }) {
            if existing.id != stored.id {
                if let Some(m) = meta.remove(&existing.id) {
                    meta.entry(stored.id.clone()).or_insert(m);
                }
                existing.id.clone_from(&stored.id);
            }
            existing.label.clone_from(&stored.label);
            existing.email.clone_from(&stored.email);
            existing.profile_dir.clone_from(&resolved_profile_dir);
            if !pu.usage.is_empty() {
                existing.usage = pu.usage.to_proto();
                existing.stale = pu.stale;
            }
        } else {
            let usage = pu.usage.to_proto();
            let stale = pu.stale;
            all.push(AccountInfo {
                id: stored.id.clone(),
                provider,
                label: stored.label.clone(),
                email: stored.email.clone(),
                profile_dir: resolved_profile_dir,
                active: false,
                host_default: false,
                stale,
                usage,
            });
        }
    }

    // 로그인 프로브의 현재 신원은 등록 이름이나 오래된 사용량 캐시보다 우선한다.
    for account in &mut all {
        if let Some(identity) = auth
            .get(&(account.provider, PathBuf::from(&account.profile_dir)))
            .filter(|status| status.logged_in)
            .and_then(|status| status.identity.as_ref())
        {
            account.email = Some(identity.email.clone());
        }
    }
    let now = now_epoch();
    let now_secs = u64::try_from(now).unwrap_or_default();
    let mut rate_limits = Vec::new();
    for a in &all {
        let key = (a.provider.as_str().to_owned(), a.id.clone());
        if let Some(&detected) = limited.get(&key) {
            let clears_at = policy::clears_at(&a.usage, detected);
            if now_secs >= clears_at {
                limited.remove(&key);
                // 감지 직후의 새 기록을 이전 refresh가 지우지 않는다.
                let mut global = lock(&RATE_LIMITED);
                if global.get(&key) == Some(&detected) {
                    global.remove(&key);
                }
            } else {
                rate_limits.push(krill_proto::usage::RateLimitExpiry {
                    account_id: a.id.clone(),
                    clears_at,
                });
            }
        }
    }
    let mut runtime = policy::Runtime::load(home, state_dir);
    let before = runtime.clone();
    let stamps = meta
        .iter()
        .filter_map(|(id, (_, _, stamp))| Some((id.clone(), (*stamp)?)))
        .collect();
    runtime.observe(&all, &stamps);
    let forecasts = runtime.forecasts(now_secs);
    let mut predicted_switches = std::collections::BTreeSet::new();
    // 4. 프로바이더별 현재 활성 계정 표시 및 자동전환(Auto-Switch) 평가.
    let mut switched = Vec::new();
    let mut dirty = false;
    let readings: BTreeMap<_, _> = all
        .iter()
        .map(|a| (a.id.clone(), super::meters::read(home, a)))
        .collect();
    let effective = |a: &AccountInfo, metric: AutoSwitchMetric, now: i64| {
        if metric == AutoSwitchMetric::Max {
            readings.get(&a.id).and_then(|m| m.effective_max(now))
        } else {
            metric.effective_percent(&a.usage, now)
        }
    };

    for provider in ProviderKind::ALL {
        let key = provider.as_str().to_owned();
        let default_id = host_default_id(provider);
        let current_id = file
            .active
            .get(&key)
            .filter(|id| all.iter().any(|a| a.provider == provider && a.id == **id))
            .cloned()
            .unwrap_or_else(|| default_id.clone());

        let mut active_id = current_id.clone();

        // 자동전환 조건 검사: 활성 계정의 기준 창 사용률이 임계치 이상이면 전환한다.
        // K-27: `switch_on_rate_limit=false`면 100% 도달만으로는 전환하지 않는다(임계치 구간만 전환).
        // 규칙에는 전용 플래그가 없어 전역 값을 쓴다.
        // OWE-712 쿨다운은 사용률 왕복만 막는다. 실제 한도 기록이 있는 활성 계정에는 묶어 두지 않는다.
        if allow_auto_switch
            && let Some(active_acc) = all
                .iter()
                .find(|a| a.provider == provider && a.id == current_id)
            && !manually_pinned(
                home,
                state_dir,
                provider,
                active_acc,
                matches!(trigger, RefreshTrigger::RateLimit)
                    || rate_limited_at(&limited, provider, &current_id).is_some(),
                &policy,
                &file.advanced_policy,
            )
            && (file.advanced_policy.cooldown_secs == 0
                || rate_limited_at(&limited, provider, &current_id).is_some()
                || !runtime.cooling(provider, now_secs))
        {
            let rule = policy.rule_for(provider, &current_id);
            let (enabled, threshold, metric, target) = rule.map_or(
                (
                    policy.enabled,
                    policy
                        .threshold_percent
                        .clamp(AUTO_SWITCH_THRESHOLD_MIN, AUTO_SWITCH_THRESHOLD_MAX),
                    AutoSwitchMetric::Max,
                    None,
                ),
                |r| {
                    (
                        r.enabled,
                        r.threshold_percent
                            .clamp(AUTO_SWITCH_THRESHOLD_MIN, AUTO_SWITCH_THRESHOLD_MAX),
                        r.metric,
                        r.target_account_id.as_deref(),
                    )
                },
            );
            // K-10·K-11: 사용량을 모르면(`None`) 전환을 걸지도 후보로 고르지도 않는다.
            // 지난 창의 수치(`resets_at` 경과)는 버리고 유효 창만 본다.
            // Max는 제공자가 보고한 모든 풀을 본다. 5h/7d 명시 규칙은 해당 창만 본다.
            // OWE-703 2단계 지시: 한도 기록이 있는 활성 계정은 캐시와 무관하게 100%로 보고
            // 기존 트리거(임계치·`switch_on_rate_limit`·비활성 규칙)를 그대로 탄다.
            let now = now_epoch();
            let limited_active = rate_limited_at(&limited, provider, &current_id).is_some();
            let active_pct = if limited_active {
                Some(100)
            } else {
                effective(active_acc, metric, now)
            };
            let predicted = file.advanced_policy.predict_exhaustion
                && policy::predictive_trigger(&forecasts, &current_id, metric, now_secs);
            let trigger = active_pct.is_some_and(|pct| {
                enabled
                    && (pct >= threshold || predicted)
                    && (pct < 100 || policy.switch_on_rate_limit)
            });
            if trigger && let Some(pct) = active_pct {
                // K-14: 후보는 활성 계정보다 히스테리시스만큼 확실히 여유로워야 한다.
                // 지정 타깃이 동률이거나 더 차면 폴백도 막혀 왕복이 멈춘다.
                // 어느 한 창이라도 소진(100%)이면 후보에서 제외한다(레이트리밋 계정으로 전환 금지).
                // OWE-703 2단계 지시: 한도 기록 계정도 후보에서 제외한다(캐시 사용량과 무관).
                let better = |a: &&AccountInfo| -> Option<u8> {
                    if a.stale
                        || auth
                            .get(&(a.provider, PathBuf::from(&a.profile_dir)))
                            .is_some_and(|status| !status.logged_in)
                        || effective(a, AutoSwitchMetric::Max, now).is_none()
                        || !runnable(a)
                        || rate_limited_at(&limited, provider, &a.id).is_some()
                    {
                        return None;
                    }
                    if effective(a, AutoSwitchMetric::Max, now).is_none_or(|max| max >= 100) {
                        return None;
                    }
                    let v = effective(a, metric, now)?;
                    (v.saturating_add(AUTO_SWITCH_HYSTERESIS) <= pct).then_some(v)
                };
                let preferred = target
                    .and_then(|id| {
                        all.iter()
                            .find(|a| a.provider == provider && a.id == id && a.id != current_id)
                    })
                    .filter(|a| better(a).is_some());
                let fallback = || {
                    all.iter()
                        .filter(|a| {
                            a.provider == provider && a.id != current_id && better(a).is_some()
                        })
                        .min_by_key(|a| {
                            let v = better(a).unwrap_or(u8::MAX);
                            (
                                // 리셋 우선이어도 임계치 아래 후보가 먼저다(다음 틱 연쇄 전환 방지).
                                v >= threshold,
                                if file.advanced_policy.prefer_soonest_reset {
                                    policy::reset_key(a, now)
                                } else {
                                    0
                                },
                                v,
                            )
                        })
                };
                if let Some(candidate) = preferred.or_else(fallback) {
                    if predicted && pct < threshold {
                        predicted_switches.insert(provider);
                    }
                    active_id.clone_from(&candidate.id);
                    file.active.insert(key.clone(), active_id.clone());
                    dirty = true;
                    switched.push(AutoSwitchEvent {
                        provider,
                        from_id: active_acc.id.clone(),
                        from_label: active_acc.label.clone(),
                        to_id: candidate.id.clone(),
                        to_label: candidate.label.clone(),
                        reason_percent: pct,
                    });
                }
            }
        }

        for acc in all.iter_mut().filter(|a| a.provider == provider) {
            acc.active = acc.id == active_id;
        }
    }

    // K-03: 손상된 파일(`writable=false`)에서는 저장하지 않는다. `SAVE_LOCK`을 쥐고 있어
    // `save_file`이 아니라 `save_inner`를 쓴다.
    if dirty && (!writable || save_inner(home, state_dir, &file).is_err()) {
        // 저장하지 못한 전환을 UI·틱·실행 경로에 성공으로 돌려주지 않는다.
        tracing::warn!("automatic account switch save failed");
        for event in &switched {
            for account in all.iter_mut().filter(|a| a.provider == event.provider) {
                account.active = account.id == event.from_id;
            }
        }
        switched.clear();
    }

    switch_work::notify(home, state_dir, &switched);

    for event in &switched {
        runtime.record(
            event.clone(),
            now_secs,
            &file.advanced_policy,
            predicted_switches.contains(&event.provider),
        );
    }
    if writable
        && runtime != before
        && let Err(error) = runtime.save(home, state_dir)
    {
        tracing::warn!(kind = ?error.kind(), "account policy state save failed");
    }
    let advanced = runtime.snapshot(
        file.advanced_policy,
        now_secs,
        policy::model_windows(home, &all),
        rate_limits,
    );
    // OWE-703 1단계: 계정별 상태 꼬리를 채운다. 판정에는 쓰지 않는다(표시·N-1용).
    // OWE-703 2단계 지시: 한도 기록은 감지 시각을 갱신 시각에 넣는다.
    let details = krill_proto::usage::ProviderDetails {
        supported: true,
        accounts: readings.into_values().collect(),
        handoffs: Vec::new(),
    };
    let statuses = all
        .iter()
        .map(|a| {
            let (mut error, mut detail, updated_at) = meta.remove(&a.id).unwrap_or_default();
            // 새 로그인 성공은 오래된 사용량 캐시의 인증 오류보다 우선한다.
            if auth
                .get(&(a.provider, PathBuf::from(&a.profile_dir)))
                .is_some_and(|s| s.logged_in)
                && error
                    .as_deref()
                    .is_some_and(|e| classify_error(e) == AccountStatus::NeedsLogin)
            {
                error = None;
                detail = None;
            }
            let detected = rate_limited_at(&limited, a.provider, &a.id);
            let (status, detected_at) = if auth
                .get(&(a.provider, PathBuf::from(&a.profile_dir)))
                .is_some_and(|status| !status.logged_in)
            {
                (AccountStatus::NeedsLogin, None)
            } else {
                let empty = details
                    .accounts
                    .iter()
                    .find(|m| m.account_id == a.id)
                    .is_none_or(|m| m.meters.is_empty());
                classify_account(error.as_deref(), a.stale, empty, detected)
            };
            AccountStatusInfo {
                status,
                account_id: a.id.clone(),
                updated_at: detected_at.or(updated_at),
                detail,
            }
        })
        .collect();

    let connections = krill_proto::usage::ConnectionsSnapshot {
        supported: true,
        direct_usage: file.direct_usage,
        accounts: all
            .iter()
            .map(|a| krill_proto::usage::AccountConnection {
                account_id: a.id.clone(),
                status: login::connection(a.provider, Path::new(&a.profile_dir)),
            })
            .collect(),
    };
    AccountsSnapshot {
        accounts: all,
        auto_switch: policy,
        switched,
        statuses,
        advanced,
        connections: Box::new(connections),
        details: Box::new(details),
    }
}

#[cfg(test)]
mod login_regression_tests {
    use super::*;

    #[test]
    fn auth_timeout_cache_error_is_not_a_login_request() {
        assert_eq!(
            classify_error("authentication probe timed out"),
            AccountStatus::Error
        );
        assert_eq!(classify_error("token lookup failed"), AccountStatus::Error);
        assert_eq!(classify_error("not logged in"), AccountStatus::NeedsLogin);
    }

    #[test]
    fn logged_in_probe_overrides_old_orca_login_error() {
        let root = std::env::temp_dir().join(format!("owe719-auth-{}", std::process::id()));
        let home = root.join("home");
        let state = root.join("state");
        let dir = home.join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(ORCA_CACHE),
            r#"{"accounts":[{"host":true,"error":"재로그인","detail":"token 400"}]}"#,
        )
        .unwrap();
        let auth = BTreeMap::from([(
            (ProviderKind::Claude, dir),
            krill_proto::account_login::AuthStatus {
                logged_in: true,
                identity: None,
            },
        )]);
        let snap =
            refresh_accounts_authenticated(&home, Some(&state), RefreshTrigger::ReadOnly, &auth);
        assert_ne!(
            snap.status_for("claude-host").unwrap().status,
            AccountStatus::NeedsLogin
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn login_required_overrides_cached_usage_and_excludes_candidate() {
        let root = std::env::temp_dir().join(format!("owe700-daemon-{}", std::process::id()));
        let home = root.join("home");
        let state = root.join("state");
        std::fs::create_dir_all(&home).unwrap();
        let host = host_default_dir(&home, ProviderKind::Claude);
        std::fs::create_dir_all(&host).unwrap();
        std::fs::write(
            host.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":95}}"#,
        )
        .unwrap();
        let snap = add_account(
            &home,
            Some(&state),
            ProviderKind::Claude,
            "Logged out",
            None,
            "",
        )
        .unwrap();
        let account = snap
            .accounts
            .iter()
            .find(|a| a.label == "Logged out")
            .unwrap();
        let dir = PathBuf::from(&account.profile_dir);
        std::fs::write(
            dir.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":10}}"#,
        )
        .unwrap();
        // 같은 경로를 지정한 다른 제공자의 로그인 상태가 Claude 판정을 덮지 않는다.
        let auth = BTreeMap::from([
            (
                (ProviderKind::Claude, dir.clone()),
                krill_proto::account_login::AuthStatus {
                    logged_in: false,
                    identity: None,
                },
            ),
            (
                (ProviderKind::Codex, dir),
                krill_proto::account_login::AuthStatus {
                    logged_in: true,
                    identity: None,
                },
            ),
        ]);
        let snap =
            refresh_accounts_authenticated(&home, Some(&state), RefreshTrigger::Automatic, &auth);
        assert!(snap.switched.is_empty());
        assert_eq!(
            snap.statuses
                .iter()
                .find(|s| s.account_id == account.id)
                .unwrap()
                .status,
            AccountStatus::NeedsLogin
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// 대표 사용량(`UsageRequest::Get` 응답용): 활성 Claude 계정 우선, 비었으면 활성 Codex 계정.
pub fn primary_snapshot(home: &Path, state_dir: Option<&Path>) -> Snapshot {
    let snap = refresh_accounts(home, state_dir);
    let claude = snap
        .accounts
        .iter()
        .find(|a| a.provider == ProviderKind::Claude && a.active && !a.usage.is_empty());
    let codex = snap
        .accounts
        .iter()
        .find(|a| a.provider == ProviderKind::Codex && a.active && !a.usage.is_empty());
    claude
        .or(codex)
        .or_else(|| snap.accounts.iter().find(|a| !a.usage.is_empty()))
        .map(|a| a.usage.clone())
        .unwrap_or_default()
}

/// 원클릭 계정 전환(MK-108).
pub fn switch_account(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    account_id: &str,
) -> io::Result<AccountsSnapshot> {
    if provider == ProviderKind::Codex {
        codex_home::migrate(home, state_dir)?;
    }
    // K-12: 존재 확인·최종 새로고침 모두 자동전환 없이 한다. 저장 직후 자동전환이
    // 사용자 선택을 덮어쓰고 응답만 고른 계정으로 위장하던 결함을 막는다.
    let snap = refresh_accounts_with(home, state_dir, false);
    let Some(target) = snap
        .accounts
        .iter()
        .find(|a| a.provider == provider && a.id == account_id)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("account not found: {account_id}"),
        ));
    };
    // OWE-696: 폴더 없는 계정(Orca 감지 등)을 활성으로 두면 실행은 호스트로 떠서 표시와 갈라진다.
    if !runnable(target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("account has no krill profile dir: {account_id}"),
        ));
    }
    let pinned_usage = target.usage.clone();
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        file.active
            .insert(provider.as_str().to_owned(), account_id.to_owned());
        save_inner(home, state_dir, &file)?;
        set_manual_pin(home, state_dir, provider, account_id, pinned_usage, &file);
    }

    Ok(refresh_accounts_with(home, state_dir, false))
}

/// 리셋 크레딧 1개를 소진해 해당 계정의 사용량 창을 초기화한다. Codex(banked reset)와
/// Claude(주간 세션 리셋·프로모션 grant)를 지원한다. 소진 성공(`Reset`·`AlreadyRedeemed`)에는
/// 로컬 한도 기록을 지워 자동전환이 다시 평가하게 하고, 소진 전 캐시를 버린 뒤 스냅샷을 새로고침한다.
/// 네트워크·자격증명 실패는 `Err`다(호출자가 자동 재시도하지 않는다: 멱등 키가 호출마다 새 값이라
/// 재시도가 새 소진이 된다. 타임아웃은 크레딧부터 다시 확인한다).
pub fn consume_reset_credit(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    account_id: &str,
) -> io::Result<(AccountsSnapshot, ResetConsumeOutcome, u64)> {
    if provider != ProviderKind::Codex && provider != ProviderKind::Claude {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reset credits are only available for codex and claude",
        ));
    }
    let snap = refresh_accounts_with(home, state_dir, false);
    let Some(target) = snap
        .accounts
        .iter()
        .find(|a| a.provider == provider && a.id == account_id)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("account not found: {account_id}"),
        ));
    };
    if !runnable(target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("account has no krill profile dir: {account_id}"),
        ));
    }
    let dir = PathBuf::from(&target.profile_dir);
    let outcome_windows = if provider == ProviderKind::Codex {
        super::direct::consume_reset_credit(provider, &dir, target.host_default, home, state_dir)
    } else {
        consume_claude_reset(home, state_dir, &dir, target.host_default)
    };
    let Some((outcome, windows)) = outcome_windows else {
        return Err(io::Error::other("사용량 초기화 실패(크레딧부터 다시 확인)"));
    };
    if matches!(
        outcome,
        ResetConsumeOutcome::Reset | ResetConsumeOutcome::AlreadyRedeemed
    ) {
        clear_rate_limited(provider, account_id);
    }
    super::direct::evict_profile(home, provider, &dir);
    Ok((refresh_accounts(home, state_dir), outcome, windows))
}

/// Claude 소진 한 걸음: 조직 조회 → 제공 프로그램 확인 → 소진. 제공 없음은 `NoCredit`으로,
/// 단계 실패는 `Err`로 돌려준다(재시도 유도).
fn consume_claude_reset(
    home: &Path,
    state_dir: Option<&Path>,
    dir: &Path,
    host: bool,
) -> Option<(ResetConsumeOutcome, u64)> {
    let token = super::direct::claude_token(dir, host, home, state_dir)?;
    let org = super::direct::claude_org_uuid(&token, super::direct::get_text)?;
    match super::direct::claude_offered_program(&token, super::direct::get_text)? {
        Some(program) => super::direct::claude_claim_reset(
            &token,
            &org,
            program,
            super::direct::post_json_status,
        ),
        None => Some((ResetConsumeOutcome::NoCredit, 0)),
    }
}

/// 새 계정 프로필 등록(MK-107). `profile_dir`이 비어 있으면 기본 프로필 경로를 자동 생성한다.
pub fn add_account(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    label: &str,
    email: Option<String>,
    profile_dir: &str,
) -> io::Result<AccountsSnapshot> {
    let clean_label = label.trim();
    if clean_label.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "account label cannot be empty",
        ));
    }
    // K-09: id 결정·디렉터리 생성·파일 저장을 같은 잠금 안에서 한다.
    let resolved = {
        let _guard = lock(&SAVE_LOCK);
        let file = load_writable(home, state_dir)?;
        // K-04: 비ASCII 라벨 충돌 시 접미사 id·디렉터리로 새 계정을 만든다(기존 id 유지).
        let (id, slug) = unique_id_and_slug(&file, provider, clean_label);
        let mut file = file;
        let resolved_dir = if profile_dir.trim().is_empty() {
            file.accounts.iter().find(|a| a.id == id).map_or_else(
                || krill_proto::usage::profiles::default_profile_dir(home, provider, &slug),
                |existing| {
                    let current = PathBuf::from(&existing.profile_dir);
                    if provider == ProviderKind::Codex {
                        krill_proto::usage::profiles::migrate_long_codex_profile_if_needed(
                            home, &slug, &current,
                        )
                    } else {
                        current
                    }
                },
            )
        } else if let Some(stripped) = profile_dir.trim().strip_prefix("~/") {
            home.join(stripped)
        } else {
            PathBuf::from(profile_dir.trim())
        };

        std::fs::create_dir_all(&resolved_dir)?;
        let resolved_dir = if provider == ProviderKind::Codex {
            codex_home::prepare(home, &resolved_dir)?
        } else {
            resolved_dir
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&resolved_dir, std::fs::Permissions::from_mode(0o700)).ok();
        }

        let stored = StoredAccount {
            id: id.clone(),
            provider: provider.as_str().to_owned(),
            label: clean_label.to_owned(),
            email: email.filter(|e| !e.trim().is_empty()),
            profile_dir: resolved_dir.display().to_string(),
        };
        if let Some(existing) = file.accounts.iter_mut().find(|a| a.id == id) {
            *existing = stored;
        } else {
            file.accounts.push(stored);
        }
        save_inner(home, state_dir, &file)?;
        resolved_dir
    };
    // OWE-699: 켜져 있으면 추가 직후 프로비저닝한다(실패는 경고만). 잠금 밖에서 프로필 폴더만 쓴다.
    if let Some(dir) = resolved.to_str() {
        super::provision::provision_if_enabled(home, state_dir, provider, dir);
    }
    Ok(refresh_accounts(home, state_dir))
}

/// 등록한 계정 삭제(MK-107). 호스트 기본 계정은 삭제할 수 없다.
pub fn remove_account(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    account_id: &str,
) -> io::Result<AccountsSnapshot> {
    if account_id == host_default_id(provider) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot remove host default account",
        ));
    }
    // K-09: 로드-수정-저장을 잠금 안에서 한다.
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        let before = file.accounts.len();
        file.accounts.retain(|a| a.id != account_id);
        if file.accounts.len() == before {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("account not found: {account_id}"),
            ));
        }
        if file.active.get(provider.as_str()).map(String::as_str) == Some(account_id) {
            file.active.remove(provider.as_str());
        }
        save_inner(home, state_dir, &file)?;
    }
    Ok(refresh_accounts(home, state_dir))
}

/// 라벨·이메일만 바꾼다. id·프로필 디렉터리·활성·규칙은 그대로다(OWE-705).
pub fn rename_account(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    account_id: &str,
    label: &str,
    email: Option<String>,
) -> io::Result<AccountsSnapshot> {
    let clean = label.trim();
    if clean.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "account label cannot be empty",
        ));
    }
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        let account = file
            .accounts
            .iter_mut()
            .find(|a| a.id == account_id && a.provider == provider.as_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("account not found: {account_id}"),
                )
            })?;
        clean.clone_into(&mut account.label);
        if let Some(email) = email {
            let email = email.trim();
            account.email = (!email.is_empty()).then(|| email.to_owned());
        }
        save_inner(home, state_dir, &file)?;
    }
    Ok(refresh_accounts(home, state_dir))
}

/// 등록을 지우고 Krill 관리 프로필 폴더를 휴지통으로 옮긴다(OWE-704). `in_use`(세션이 실행에 쓴
/// 프로필 폴더)에 있으면 `EBUSY`로 거부한다. 관리 루트 밖 폴더는 등록만 지운다. 휴지통 이동이 실패하면
/// 등록은 이미 지워졌으므로 폴더는 고아로 남아 [`purge_orphan_profiles`]가 다시 정리한다.
pub fn delete_account(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    account_id: &str,
    in_use: &[PathBuf],
) -> io::Result<AccountsSnapshot> {
    if account_id == host_default_id(provider) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot remove host default account",
        ));
    }
    let dir = {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        let Some(idx) = file
            .accounts
            .iter()
            .position(|a| a.id == account_id && a.provider == provider.as_str())
        else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("account not found: {account_id}"),
            ));
        };
        let dir = PathBuf::from(&file.accounts.remove(idx).profile_dir);
        if profile_in_use(&dir, in_use) {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        if file.active.get(provider.as_str()).map(String::as_str) == Some(account_id) {
            file.active.remove(provider.as_str());
        }
        save_inner(home, state_dir, &file)?;
        dir
    };
    if krill_proto::usage::profiles::is_managed(home, &dir)
        && let Err(e) = krill_proto::usage::profiles::move_to_trash(home, &dir)
    {
        tracing::warn!(error = %e, "account profile left as orphan");
    }
    Ok(refresh_accounts(home, state_dir))
}

// 세션이 같은 폴더를 symlink·정규화된 경로로 기록했어도 보호한다.
fn profile_in_use(dir: &Path, in_use: &[PathBuf]) -> bool {
    let resolved = dir.canonicalize().ok();
    in_use.iter().any(|other| {
        other == dir
            || resolved
                .as_ref()
                .is_some_and(|path| other.canonicalize().is_ok_and(|other| other == *path))
    })
}

/// 등록되지 않은 Krill 관리 프로필 폴더(OWE-704). `in_use` 폴더는 빼지 않는다(목록에는 보이되 정리에서 건너뜀).
pub fn orphan_profiles(home: &Path, state_dir: Option<&Path>) -> Vec<OrphanProfile> {
    let keep: Vec<PathBuf> = load_file(home, state_dir)
        .accounts
        .iter()
        .map(|a| PathBuf::from(&a.profile_dir))
        .collect();
    krill_proto::usage::profiles::orphans(home, &keep)
}

/// 고아 프로필 폴더를 휴지통으로 옮기고 남은 목록을 돌려준다. 세션이 쓰는 폴더는 건너뛴다.
/// 계정 추가가 같은 폴더를 만드는 중에 옮기지 않도록 `SAVE_LOCK` 안에서 한다.
pub fn purge_orphan_profiles(
    home: &Path,
    state_dir: Option<&Path>,
    in_use: &[PathBuf],
) -> io::Result<Vec<OrphanProfile>> {
    let _guard = lock(&SAVE_LOCK);
    let registered: Vec<PathBuf> = load_writable(home, state_dir)?
        .accounts
        .iter()
        .map(|a| PathBuf::from(&a.profile_dir))
        .collect();
    let keep: Vec<PathBuf> = registered.iter().chain(in_use).cloned().collect();
    for orphan in krill_proto::usage::profiles::orphans(home, &keep) {
        if profile_in_use(Path::new(&orphan.dir), in_use) {
            continue;
        }
        if let Err(e) = krill_proto::usage::profiles::move_to_trash(home, Path::new(&orphan.dir)) {
            tracing::warn!(error = %e, "orphan profile not moved");
        }
    }
    Ok(krill_proto::usage::profiles::orphans(home, &registered))
}

/// 자동전환 정책 변경(MK-108).
pub fn set_auto_switch(
    home: &Path,
    state_dir: Option<&Path>,
    policy: &AutoSwitchPolicy,
) -> io::Result<AccountsSnapshot> {
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        let mut stored = StoredAutoSwitch::from(policy);
        if stored.rules.is_empty() {
            stored.rules.clone_from(&file.auto_switch.rules);
        }
        file.auto_switch = stored;
        save_inner(home, state_dir, &file)?;
    }
    Ok(refresh_accounts(home, state_dir))
}

/// 계정별 규칙을 저장하고 즉시 평가한다.
pub fn upsert_auto_switch_rule(
    home: &Path,
    state_dir: Option<&Path>,
    rule: AutoSwitchRule,
) -> io::Result<AccountsSnapshot> {
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        let mut policy = AutoSwitchPolicy::from(file.auto_switch);
        policy.upsert_rule(rule);
        file.auto_switch = StoredAutoSwitch::from(&policy);
        save_inner(home, state_dir, &file)?;
    }
    Ok(refresh_accounts(home, state_dir))
}

/// 계정별 규칙을 삭제해 전역 정책으로 되돌린다.
pub fn remove_auto_switch_rule(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    from_account_id: &str,
) -> io::Result<AccountsSnapshot> {
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        file.auto_switch
            .rules
            .retain(|r| r.provider != provider || r.from_account_id != from_account_id);
        save_inner(home, state_dir, &file)?;
    }
    Ok(refresh_accounts(home, state_dir))
}

/// 에이전트 실행(`krilld::agent::launch_in`·`resume_in`) 시 활성 계정의 격리 프로필 환경 변수를 돌려준다.
/// `accounts.toml`에 비-기본 활성 계정이 지정되어 있을 때만 `Some((env_key, profile_dir))`을 반환해
/// 기존 기본 실행 테스트와 충돌하지 않게 한다.
pub fn active_profile_env(
    home: &Path,
    state_dir: Option<&Path>,
    program: &str,
) -> Option<(&'static str, String)> {
    let (provider, active) = active_account_info(home, state_dir, program)?;
    Some((provider.env_var(), active.profile_dir))
}

/// 에이전트 실행 시 비-기본 활성 계정의 `(ProviderKind, AccountInfo)`를 돌려준다.
pub fn active_account_info(
    home: &Path,
    state_dir: Option<&Path>,
    program: &str,
) -> Option<(ProviderKind, AccountInfo)> {
    let name = Path::new(program).file_name()?.to_str()?;
    let provider = ProviderKind::parse(name)?;
    let path = config_path(home, state_dir);
    if !path.exists() {
        return None;
    }
    let snap = refresh_accounts(home, state_dir);
    let active = snap
        .accounts
        .into_iter()
        .find(|a| a.provider == provider && a.active && !a.host_default && runnable(a))?;
    Some((provider, active))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_profile_usage_parses_usage_cache_json_and_antigravity_envelope() {
        let dir = std::env::temp_dir().join(format!("mk-usage-cache-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("usage_cache.json"),
            r#"{"five_hour":{"utilization":42,"resets_at":"2026-10-16T12:00:00Z"},"seven_day":{"utilization":67,"resets_at":"2026-10-20T00:00:00Z"}}"#,
        )
        .unwrap();

        for provider in [
            ProviderKind::Antigravity,
            ProviderKind::OpenCode,
            ProviderKind::Claude,
            ProviderKind::Codex,
        ] {
            let pu = read_profile_usage(&dir, provider, &dir, "test-profile", None, false);
            assert!(!pu.stale);
            assert!(pu.error.is_none());
            assert!(pu.updated_at.is_some());
            assert_eq!(pu.usage.five_hour.as_ref().map(|w| w.percent), Some(42));
            assert_eq!(pu.usage.seven_day.as_ref().map(|w| w.percent), Some(67));
        }

        // Antigravity native `agy -p /usage --output-format json` envelope
        let agy_dir = dir.join("agy-profile");
        std::fs::create_dir_all(&agy_dir).unwrap();
        std::fs::write(
            agy_dir.join("usage.json"),
            r#"{
              "status": "SUCCESS",
              "command": {
                "name": "usage",
                "data": {
                  "groups": [
                    {
                      "name": "Gemini Models",
                      "buckets": [
                        {"id": "g-5h", "window": "5h", "remaining_fraction": 0.65, "reset_time": "2026-10-16T15:00:00Z"},
                        {"id": "g-wk", "window": "weekly", "remaining_fraction": 0.20, "reset_time": "2026-10-22T00:00:00Z"},
                        {"id": "g-dis", "window": "5h", "remaining_fraction": 0.0, "disabled": true}
                      ]
                    }
                  ]
                }
              }
            }"#,
        )
        .unwrap();
        let pu = read_profile_usage(
            &dir,
            ProviderKind::Antigravity,
            &agy_dir,
            "agy-profile",
            None,
            false,
        );
        assert!(!pu.stale);
        assert!(pu.error.is_none());
        assert!(pu.updated_at.is_some());
        assert_eq!(pu.usage.five_hour.as_ref().map(|w| w.percent), Some(35));
        assert_eq!(pu.usage.seven_day.as_ref().map(|w| w.percent), Some(80));
        assert!(pu.usage.warn());

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
#[test]
fn consume_reset_credit_rejects_without_network() {
    use std::io::ErrorKind;
    let root = std::env::temp_dir().join(format!("reset-credit-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    let profile = home.join("codex-a");
    std::fs::create_dir_all(&profile).unwrap();
    // 프로바이더 제한·계정 확인은 네트워크 전에 일어난다.
    assert_eq!(
        consume_reset_credit(&home, Some(&state), ProviderKind::Claude, "x")
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        consume_reset_credit(&home, Some(&state), ProviderKind::Codex, "missing")
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("accounts.toml"),
        format!(
            "[[accounts]]\nid = 'codex-a'\nprovider = 'codex'\nlabel = 'a'\nprofile_dir = '{}'\n",
            profile.display()
        ),
    )
    .unwrap();
    // 자격증명이 없으면 소진 요청을 보내지 않고 실패한다(네트워크 금지).
    assert_eq!(
        consume_reset_credit(&home, Some(&state), ProviderKind::Codex, "codex-a")
            .unwrap_err()
            .kind(),
        ErrorKind::Other
    );
    std::fs::remove_dir_all(&root).ok();
}

#[cfg(test)]
#[test]
fn as12_daemon_accounts_save_preserves_future_fields() {
    let dir = std::env::temp_dir().join(format!("as12-daemon-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = config_path(&dir, Some(&dir));
    let raw = "# future writer\nfuture = 'root'\n[[accounts]]\nid = 'one'\nprovider = 'claude'\nlabel = 'old'\nprofile_dir = '/tmp/one'\nfuture = 'nested'\n[auto_switch]\nfuture = 'policy'\n";
    std::fs::write(&path, raw).unwrap();
    let mut file = load_file_result(&dir, Some(&dir)).unwrap();
    file.accounts[0].label = "edited".into();
    save_file(&dir, Some(&dir), &file).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert!(body.contains("# future writer"));
    assert_eq!(value["future"].as_str(), Some("root"));
    assert_eq!(value["accounts"][0]["future"].as_str(), Some("nested"));
    assert_eq!(value["auto_switch"]["future"].as_str(), Some("policy"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(test)]
mod policy_regressions {
    use super::*;

    #[test]
    fn old_rate_limit_expires_without_new_hook() {
        let root = std::env::temp_dir().join(format!("owe712-expiry-{}", std::process::id()));
        let home = root.join("home");
        let state = root.join("state");
        let profile = home.join("backup");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let mut file = AccountsFile::default();
        file.auto_switch.enabled = false;
        file.accounts.push(StoredAccount {
            id: "claude-expiry-regression".into(),
            provider: "claude".into(),
            label: "Expiry".into(),
            email: None,
            profile_dir: profile.display().to_string(),
        });
        save_file(&home, Some(&state), &file).unwrap();
        std::fs::write(
            profile.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":15}}"#,
        )
        .unwrap();
        lock(&RATE_LIMITED).insert(("claude".into(), "claude-expiry-regression".into()), 0);
        let snap = refresh_accounts_with(&home, Some(&state), false);
        clear_rate_limited(ProviderKind::Claude, "claude-expiry-regression");
        assert_eq!(
            snap.status_for("claude-expiry-regression").unwrap().status,
            AccountStatus::Ok
        );
        std::fs::remove_dir_all(root).ok();
    }
}

/// OWE-712: 기존 전역 정책·규칙과 독립적으로 고급 정책을 저장한다.
pub fn set_advanced_policy(
    home: &Path,
    state_dir: Option<&Path>,
    policy: &krill_proto::usage::AdvancedPolicy,
) -> io::Result<AccountsSnapshot> {
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state_dir)?;
        file.advanced_policy = policy.clone();
        save_inner(home, state_dir, &file)?;
    }
    Ok(refresh_accounts_with(home, state_dir, false))
}

/// 직접 사용량 조회만 켜거나 끈다. 프로바이더 인증 파일에는 쓰지 않는다.
pub fn set_direct_usage(
    home: &Path,
    state: Option<&Path>,
    enabled: bool,
) -> io::Result<AccountsSnapshot> {
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state)?;
        file.direct_usage = enabled;
        save_inner(home, state, &file)?;
    }
    Ok(refresh_accounts_with(home, state, false))
}

#[cfg(test)]
mod provisioning_regressions {
    use super::*;

    #[test]
    fn provisioning_toggle_preserves_other_and_unknown_fields() {
        let root = std::env::temp_dir().join(format!("owe722-preserve-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = config_path(&root, Some(&root));
        let raw = "# future writer\nfuture = 'root'\ndirect_usage = false\n[[accounts]]\nid = 'one'\nprovider = 'claude'\nlabel = 'unchanged'\nprofile_dir = '/tmp/one'\nfuture = 'nested'\n[auto_switch]\nenabled = false\nthreshold_percent = 77\nfuture = 'policy'\n[active]\nclaude = 'one'\n[advanced_policy]\npredict_exhaustion = true\n";
        std::fs::write(&path, raw).unwrap();
        let before: toml::Value = toml::from_str(raw).unwrap();
        // 키가 없으면 이미 켜짐이라, 끈 뒤 다시 켜는 순서로 두 값이 모두 파일에 적히는지 본다.
        for enabled in [false, true] {
            assert_eq!(
                set_provisioning(&root, Some(&root), enabled).unwrap(),
                enabled
            );
            assert_eq!(provisioning(&root, Some(&root)).unwrap(), enabled);
            let body = std::fs::read_to_string(&path).unwrap();
            let after: toml::Value = toml::from_str(&body).unwrap();
            assert!(body.contains("# future writer"));
            assert_eq!(after["provisioning"].as_bool(), Some(enabled));
            for key in [
                "future",
                "accounts",
                "active",
                "auto_switch",
                "advanced_policy",
                "direct_usage",
            ] {
                // 이미 알려진 기본 필드는 저장 때 채워질 수 있다. 입력에 있던 값만 비교한다.
                if let Some(table) = before[key].as_table() {
                    for (field, value) in table {
                        assert_eq!(&after[key][field], value);
                    }
                } else if let Some(accounts) = before[key].as_array() {
                    for (index, account) in accounts.iter().enumerate() {
                        for (field, value) in account.as_table().unwrap() {
                            assert_eq!(&after[key][index][field], value);
                        }
                    }
                } else {
                    assert_eq!(after[key], before[key]);
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provisioning_default_status_is_read_only_and_corruption_is_preserved() {
        let root = std::env::temp_dir().join(format!("owe722-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = config_path(&root, Some(&root));
        assert!(
            provisioning(&root, Some(&root)).unwrap(),
            "파일 없음 = 켜짐"
        );
        assert!(!path.exists());
        let raw = "provisioning = [broken";
        std::fs::write(&path, raw).unwrap();
        assert!(provisioning(&root, Some(&root)).is_err());
        assert!(set_provisioning(&root, Some(&root), true).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// 프로비저닝 opt-in만 바꾼다. 기존 계정·정책·미지 필드는 보존한다(OWE-722).
/// 설정 변경 자체는 프로필을 건드리지 않으며, 다음 계정 추가·실행 때 적용된다.
pub fn set_provisioning(home: &Path, state_dir: Option<&Path>, enabled: bool) -> io::Result<bool> {
    let _guard = lock(&SAVE_LOCK);
    let mut file = load_writable(home, state_dir)?;
    file.provisioning = enabled;
    save_inner(home, state_dir, &file)?;
    Ok(file.provisioning)
}

/// 파일 없음은 기본 켜짐으로 읽는다. 손상된 설정은 오류로 돌려주며 쓰지 않는다.
pub fn provisioning(home: &Path, state_dir: Option<&Path>) -> io::Result<bool> {
    let _guard = lock(&SAVE_LOCK);
    Ok(load_writable(home, state_dir)?.provisioning)
}

#[cfg(test)]
#[cfg(unix)]
mod removal_alias_tests {
    use super::*;

    #[test]
    fn removal_and_orphan_cleanup_preserve_in_use_profile_alias() {
        let home = std::env::temp_dir().join(format!("owe704-alias-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let state = home.join("state");
        let snap =
            add_account(&home, Some(&state), ProviderKind::Claude, "Work", None, "").unwrap();
        let account = snap.accounts.iter().find(|a| a.label == "Work").unwrap();
        let profile = PathBuf::from(&account.profile_dir);
        std::fs::write(profile.join("partial-login"), b"fake data").unwrap();
        let alias = home.join("running-profile");
        std::os::unix::fs::symlink(&profile, &alias).unwrap();
        let before = std::fs::read(config_path(&home, Some(&state))).unwrap();

        let result = delete_account(
            &home,
            Some(&state),
            ProviderKind::Claude,
            &account.id,
            std::slice::from_ref(&alias),
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EBUSY));
        assert_eq!(
            std::fs::read(config_path(&home, Some(&state))).unwrap(),
            before
        );
        assert!(profile.join("partial-login").is_file());

        remove_account(&home, Some(&state), ProviderKind::Claude, &account.id).unwrap();
        let left =
            purge_orphan_profiles(&home, Some(&state), std::slice::from_ref(&alias)).unwrap();
        assert_eq!(left.len(), 1);
        assert!(alias.join("partial-login").is_file());
        assert!(!home.join(".Trash").exists());
        assert!(
            purge_orphan_profiles(&home, Some(&state), &[])
                .unwrap()
                .is_empty()
        );
        assert!(
            home.join(".Trash/krill-claude-work/partial-login")
                .is_file()
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
