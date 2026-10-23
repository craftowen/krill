//! 프로바이더 다계정·사용량(5h/7d)·원클릭 전환 클라이언트 및 읽기 전용 로컬 폴백(MK-107·MK-108).
//!
//! - 데몬(`krilld`)이 연결되어 있으면 `FEAT_USAGE` IPC(`UsageRequest`/`UsageReply`)로 요청한다.
//! - 자동전환 판정·기록은 krilld 하나만 한다(OWE-697). 로컬 폴백은 `accounts.toml`과 캐시를 읽기만 하고
//!   자동전환을 평가하지 않는다. 로컬 변이(`request_local`)는 데몬 없는 임시 상태 디렉터리(테스트)용이다.
//!
//! OAuth process lifecycle reference: Orca claude-login-session.ts / codex-login-session.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

pub mod display;
pub mod installed;
mod login_browser;
mod login_output;
pub mod opencode_go;

#[cfg(test)]
#[cfg(unix)]
mod login_tests;

use std::collections::BTreeMap;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{
    AccountInfo, AccountsSnapshot, AutoSwitchPolicy, AutoSwitchRule, FEAT_USAGE, OrphanProfile,
    ProviderKind, ResetConsumeOutcome, Snapshot, UsageReply, UsageRequest, Window,
};

/// [`UsageRequest::ConsumeResetCredit`] 결과. 스냅샷과 소진 결과를 함께 들고 온다.
#[derive(Debug, Clone)]
pub struct ResetConsumed {
    pub snapshot: AccountsSnapshot,
    pub outcome: ResetConsumeOutcome,
    pub windows_reset: u64,
}
use serde::{Deserialize, Serialize};

const ACCOUNTS_FILE: &str = "accounts.toml";
const CLAUDE_CACHE: &str = "statusline_cache.json";
const ORCA_CACHE: &str = "orca-usage-cache.json";
const TAIL_MAX: u64 = 256 * 1024;
const FIVE_HOUR_MINUTES: u64 = 300;
const SEVEN_DAY_MINUTES: u64 = 10_080;
const ORCA_ID_PREFIX: &str = "claude-orca-";

#[cfg(test)]
mod status_negotiation_tests {
    use super::*;
    use krill_proto::ipc::{ClientHello, ServerHello};
    use krill_proto::usage::{AccountStatus, AccountStatusInfo, FEAT_USAGE_STATUS};

    #[test]
    fn ipc_keeps_status_tail_after_negotiation() {
        let root = std::env::temp_dir().join(format!("acct-status-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = Paths {
            socket: root.join("daemon.sock"),
            holder: root.join("unused-holder"),
            krilld: root.join("unused-daemon"),
        };
        let listener = krill_proto::os::net::UnixListener::bind(&paths.socket).unwrap();
        let worker = std::thread::spawn(move || {
            let server = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: FEAT_USAGE | FEAT_USAGE_STATUS,
                build: "test".to_owned(),
                instance_id: 1,
            };
            // connect는 holder 감시 Hello 뒤에 계정 요청 연결을 새로 연다.
            let (mut watch, _) = listener.accept().unwrap();
            watch.write_all(&server.encode().unwrap()).unwrap();
            ipc::read_frame(&mut watch).unwrap().unwrap();
            drop(watch);
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream.write_all(&server.encode().unwrap()).unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            let client = ClientHello::decode(kind, &body).unwrap();
            let features = ipc::negotiate(&server, &client).unwrap().features;
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            assert_eq!(
                UsageRequest::decode(kind, &body).unwrap(),
                UsageRequest::ListAccounts
            );
            let snapshot = AccountsSnapshot {
                statuses: vec![AccountStatusInfo {
                    account_id: "claude-host".to_owned(),
                    status: AccountStatus::NeedsLogin,
                    updated_at: Some(1),
                    detail: Some("login required".to_owned()),
                }],
                ..AccountsSnapshot::default()
            };
            let reply = UsageReply::Accounts(snapshot).for_features(features);
            stream.write_all(&reply.encode().unwrap()).unwrap();
            features
        });
        let snap = request_ipc(&paths, &UsageRequest::ListAccounts).unwrap();
        let features = worker.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert_ne!(features & FEAT_USAGE_STATUS, 0);
        assert_eq!(snap.statuses.len(), 1);
        assert_eq!(snap.statuses[0].status, AccountStatus::NeedsLogin);
    }
}

/// OWE-696: Orca가 감지·관리하는 계정. Krill이 아는 프로필 폴더가 없어 이 계정으로 실행·전환·제거할 수 없다.
#[must_use]
pub fn orca_managed(acc: &AccountInfo) -> bool {
    acc.id.starts_with(ORCA_ID_PREFIX)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredAccount {
    pub id: String,
    pub provider: String,
    pub label: String,
    #[serde(default)]
    pub email: Option<String>,
    pub profile_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountsFile {
    #[serde(default)]
    pub auto_switch: StoredAutoSwitch,
    #[serde(default)]
    pub active: BTreeMap<String, String>,
    #[serde(default)]
    pub accounts: Vec<StoredAccount>,
    /// OWE-699: 계정 프로필 프로비저닝(키가 없으면 켜짐, krilld와 같은 규칙). 프로비저닝은 krilld가 한다 — 앱은 칩 노출만 본다.
    #[serde(default = "provisioning_default")]
    pub provisioning: bool,
    /// Krill 목록에서 지운 Orca 감지 계정 id(krilld와 같은 필드).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_accounts: Vec<String>,
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
            provisioning: provisioning_default(),
            hidden_accounts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAutoSwitch {
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
    krill_proto::usage::DEFAULT_AUTO_SWITCH_PERCENT
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

/// `krilld`에 `FEAT_USAGE` 요청을 보내고, 읽기 요청만 로컬 폴백으로 처리한다.
/// 변이 요청의 데몬 거부·연결 실패는 그대로 돌려준다. 로컬 쓰기로 덮으면
/// 데몬 상태와 갈라지는 split-brain이 된다(AS-03).
pub fn request(paths: Option<&Paths>, req: &UsageRequest) -> Result<AccountsSnapshot, String> {
    if let Some(paths) = paths {
        match request_ipc(paths, req) {
            Ok(snap) => return Ok(snap),
            Err(e) if !req.is_read() => return Err(e),
            Err(_) => {}
        }
    }
    let home = default_home();
    let state_dir = paths.and_then(|p| {
        let parent = p.socket.parent()?;
        if parent.file_name().is_some_and(|n| n == "run") {
            parent.parent()
        } else {
            Some(parent)
        }
    });
    request_local(&home, state_dir, req).map_err(|e| e.to_string())
}

/// 데몬에만 묻는다(로컬 폴백 없음). 설정 화면이 데몬 연결 여부를 가릴 때 쓴다(OWE-697).
pub fn request_ipc(paths: &Paths, req: &UsageRequest) -> Result<AccountsSnapshot, String> {
    match exchange(paths, req)? {
        UsageReply::Accounts(snap) | UsageReply::ResetConsumed { snapshot: snap, .. } => Ok(snap),
        UsageReply::Current(_) | UsageReply::Orphans(_) | UsageReply::Provisioning(_) => {
            Ok(AccountsSnapshot::default())
        }
        UsageReply::Failed(e) => Err(failure_text(e)),
    }
}

/// Codex banked reset 크레딧 소진. 데몬에만 묻는다(로컬 폴백 없음).
pub fn consume_reset_credit_ipc(
    paths: &Paths,
    req: &UsageRequest,
) -> Result<ResetConsumed, String> {
    match exchange(paths, req)? {
        UsageReply::ResetConsumed {
            snapshot,
            outcome,
            windows_reset,
        } => Ok(ResetConsumed {
            snapshot,
            outcome,
            windows_reset,
        }),
        UsageReply::Failed(e) => Err(failure_text(e)),
        _ => Err("unexpected account reply".to_owned()),
    }
}

/// 고아 프로필 목록·정리 요청(OWE-704). 데몬에만 묻는다.
pub fn orphans_ipc(paths: &Paths, req: &UsageRequest) -> Result<Vec<OrphanProfile>, String> {
    match exchange(paths, req)? {
        UsageReply::Orphans(list) => Ok(list),
        UsageReply::Failed(e) => Err(failure_text(e)),
        UsageReply::Accounts(_)
        | UsageReply::Current(_)
        | UsageReply::Provisioning(_)
        | UsageReply::ResetConsumed { .. } => Ok(Vec::new()),
    }
}

/// 사용 중 세션 때문에 거부된 삭제(`EBUSY`)는 사용자 문구로 바꾼다(OWE-704).
fn failure_text(e: krill_proto::control::Failure) -> String {
    #[cfg(unix)]
    let busy = e == krill_proto::control::Failure::Os(rustix::io::Errno::BUSY.raw_os_error());
    #[cfg(not(unix))]
    let busy = false;
    if busy {
        crate::i18n::catalog::settings::ACCOUNT_DELETE_BUSY
            .get()
            .to_owned()
    } else {
        e.to_string()
    }
}

fn exchange(paths: &Paths, req: &UsageRequest) -> Result<UsageReply, String> {
    let mut hello = daemon::client_hello(ClientRole::App, "krill-app accounts");
    hello.features = FEAT_USAGE
        | krill_proto::usage::FEAT_PROVIDER_DETAILS
        | krill_proto::usage::FEAT_ACCOUNT_CANCEL
        | krill_proto::usage::FEAT_USAGE_AGY
        | krill_proto::usage::FEAT_USAGE_RULES
        | krill_proto::usage::FEAT_USAGE_STATUS
        | krill_proto::usage::FEAT_USAGE_RATELIMIT
        | krill_proto::usage::FEAT_USAGE_MANAGE
        | krill_proto::usage::FEAT_ACCOUNT_POLICY
        | krill_proto::usage::FEAT_ACCOUNT_CONNECTION
        | krill_proto::usage::FEAT_USAGE_RESET;
    let mut conn = daemon::connect(paths, &hello).map_err(|e| e.to_string())?;
    if conn.negotiated.features & FEAT_USAGE == 0 {
        return Err("FEAT_USAGE not negotiated".to_owned());
    }
    let required = req.required_features();
    let required = required
        | if matches!(req, UsageRequest::SwitchAccount { .. }) {
            krill_proto::usage::FEAT_PROVIDER_DETAILS
        } else {
            0
        };
    if conn.negotiated.features & required != required {
        return Err("daemon does not support this account extension; restart daemon".to_owned());
    }
    let timeout = Some(Duration::from_secs(5));
    conn.stream.set_read_timeout(timeout).ok();
    conn.stream.set_write_timeout(timeout).ok();
    conn.stream
        .write_all(&req.encode().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "daemon closed usage stream".to_owned())?;
    UsageReply::decode(kind, &body).map_err(|e| e.to_string())
}

pub fn request_local(
    home: &Path,
    state_dir: Option<&Path>,
    req: &UsageRequest,
) -> io::Result<AccountsSnapshot> {
    match req {
        UsageRequest::CancelAccountSwitch { .. } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "account handoff cancellation requires the daemon",
        )),
        UsageRequest::Get | UsageRequest::ListAccounts | UsageRequest::ListOrphanProfiles => {
            Ok(refresh_local(home, state_dir))
        }
        UsageRequest::SwitchAccount {
            provider,
            account_id,
        } => switch_account(home, state_dir, *provider, account_id),
        UsageRequest::AddAccount {
            provider,
            label,
            email,
            profile_dir,
        } => {
            let clean = label.trim();
            if clean.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "label cannot be empty",
                ));
            }
            // id를 확정한 뒤 기본 프로필 경로에도 같은 접미사를 적용한다.
            // 다른 라벨은 분리하고, 접미사가 붙은 라벨 재등록도 멱등 처리한다(K-04·AS-13).
            let mut file = load_file_result(home, state_dir)?;
            let base_slug = slugify(clean);
            let mut slug = base_slug.clone();
            let mut suffix = 2u32;
            let id = loop {
                let id = format!("{}-{slug}", provider.as_str());
                match file.accounts.iter().find(|a| a.id == id) {
                    None => break id,
                    Some(existing)
                        if existing.label == clean && existing.provider == provider.as_str() =>
                    {
                        break id;
                    }
                    Some(_) => {
                        slug = format!("{base_slug}-{suffix}");
                        suffix = suffix.saturating_add(1);
                    }
                }
            };
            let resolved = if profile_dir.trim().is_empty() {
                // 같은 라벨 재등록(멱등)이면 기존 프로필 디렉터리를 유지한다(krilld K-04와 동일).
                // 수동 지정 디렉터리를 기본값으로 덮어쓰지 않기 위해서다.
                if let Some(existing) = file.accounts.iter().find(|a| a.id == id) {
                    let current = PathBuf::from(&existing.profile_dir);
                    if *provider == ProviderKind::Codex {
                        krill_proto::usage::profiles::migrate_long_codex_profile_if_needed(
                            home, &slug, &current,
                        )
                    } else {
                        current
                    }
                } else {
                    krill_proto::usage::profiles::default_profile_dir(home, *provider, &slug)
                }
            } else if let Some(rest) = profile_dir.trim().strip_prefix("~/") {
                home.join(rest)
            } else {
                PathBuf::from(profile_dir.trim())
            };
            std::fs::create_dir_all(&resolved)?;
            let stored = StoredAccount {
                id: id.clone(),
                provider: provider.as_str().to_owned(),
                label: clean.to_owned(),
                email: email.clone().filter(|e| !e.trim().is_empty()),
                profile_dir: resolved.display().to_string(),
            };
            if let Some(ex) = file.accounts.iter_mut().find(|a| a.id == id) {
                *ex = stored;
            } else {
                file.accounts.push(stored);
            }
            save_file(home, state_dir, &file)?;
            Ok(refresh_local(home, state_dir))
        }
        UsageRequest::RemoveAccount {
            provider,
            account_id,
        } => {
            let mut file = load_file_result(home, state_dir)?;
            file.accounts.retain(|a| a.id != *account_id);
            if file.active.get(provider.as_str()) == Some(account_id) {
                file.active.remove(provider.as_str());
            }
            save_file(home, state_dir, &file)?;
            Ok(refresh_local(home, state_dir))
        }
        UsageRequest::RenameAccount {
            provider,
            account_id,
            label,
            email,
        } => {
            let clean = label.trim();
            if clean.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "label cannot be empty",
                ));
            }
            let mut file = load_file_result(home, state_dir)?;
            let account = file
                .accounts
                .iter_mut()
                .find(|a| a.id == *account_id && a.provider == provider.as_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "account not found"))?;
            clean.clone_into(&mut account.label);
            if let Some(email) = email {
                let email = email.trim();
                account.email = (!email.is_empty()).then(|| email.to_owned());
            }
            save_file(home, state_dir, &file)?;
            Ok(refresh_local(home, state_dir))
        }
        // 로컬 경로(데몬 없는 임시 상태 디렉터리)에는 세션이 없으므로 사용 중 확인이 없다.
        UsageRequest::DeleteAccount {
            provider,
            account_id,
        } => {
            let mut file = load_file_result(home, state_dir)?;
            if account_id.starts_with(ORCA_ID_PREFIX) {
                if !file.hidden_accounts.contains(account_id) {
                    file.hidden_accounts.push(account_id.clone());
                }
                if file.active.get(provider.as_str()) == Some(account_id) {
                    file.active.remove(provider.as_str());
                }
                save_file(home, state_dir, &file)?;
                return Ok(refresh_local(home, state_dir));
            }
            let idx = file
                .accounts
                .iter()
                .position(|a| a.id == *account_id && a.provider == provider.as_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "account not found"))?;
            let dir = PathBuf::from(&file.accounts.remove(idx).profile_dir);
            if file.active.get(provider.as_str()) == Some(account_id) {
                file.active.remove(provider.as_str());
            }
            save_file(home, state_dir, &file)?;
            if krill_proto::usage::profiles::is_managed(home, &dir) {
                krill_proto::usage::profiles::move_to_trash(home, &dir).ok();
            }
            Ok(refresh_local(home, state_dir))
        }
        UsageRequest::PurgeOrphanProfiles => {
            for orphan in orphans_local(home, state_dir) {
                krill_proto::usage::profiles::move_to_trash(home, Path::new(&orphan.dir)).ok();
            }
            Ok(refresh_local(home, state_dir))
        }
        UsageRequest::SetAdvancedPolicy(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "advanced policy requires daemon",
        )),
        UsageRequest::SetDirectUsage(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "direct usage requires daemon",
        )),
        UsageRequest::ImportOrcaAntigravity { .. } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Orca account vault import requires daemon",
        )),
        UsageRequest::SetProvisioning(_) | UsageRequest::GetProvisioning => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "provisioning requires daemon",
        )),
        UsageRequest::ConsumeResetCredit { .. } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "reset credits require daemon",
        )),
        UsageRequest::SetAutoSwitch(policy) => set_auto_switch_local(home, state_dir, policy),
        UsageRequest::UpsertAutoSwitchRule(rule) => {
            let mut file = load_file_result(home, state_dir)?;
            let mut policy = AutoSwitchPolicy {
                rules: file.auto_switch.rules,
                ..AutoSwitchPolicy::default()
            };
            policy.upsert_rule(rule.clone());
            file.auto_switch.rules = policy.rules;
            save_file(home, state_dir, &file)?;
            Ok(refresh_local(home, state_dir))
        }
        UsageRequest::RemoveAutoSwitchRule {
            provider,
            from_account_id,
        } => {
            let mut file = load_file_result(home, state_dir)?;
            file.auto_switch
                .rules
                .retain(|r| r.provider != *provider || r.from_account_id != *from_account_id);
            save_file(home, state_dir, &file)?;
            Ok(refresh_local(home, state_dir))
        }
    }
}

/// `claude auth status --json`이 알려 준 로그인(OWE-705). 비밀값은 없다.
pub use krill_proto::account_login::Identity;

/// OWE-705와 로그인 완료 확인이 같은 파서를 쓴다.
#[must_use]
pub fn parse_claude_identity(json: &str) -> Option<Identity> {
    krill_proto::account_login::parse_claude_status(json)?.identity
}

/// `CLAUDE_CONFIG_DIR=<dir> claude auth status --json`(1회 0.29초 실측, 보고서 C5). `dir`이 `None`이면 호스트 기본
/// 로그인을 묻는다. 실패·시간 초과·미로그인은 `None`(호출자는 무시한다). 백그라운드에서만 부른다.
pub fn claude_identity(exe: &Path, dir: Option<&Path>) -> Option<Identity> {
    krill_proto::account_login::probe(exe, ProviderKind::Claude, dir)?.identity
}

/// 로그인 CLI를 백그라운드에서 실행하고, 허용된 인증 링크가 준비되면 브라우저를 연다.
/// 취소 손잡이(`Arc<AtomicBool>`)를 돌려준다. UI의 [취소]가 그것을 켜면
/// 스레드가 자식 CLI를 죽이고 정리한 뒤 [`LoginOutcome::Cancelled`]를 기록한다(OWE-881).
pub fn start_login(
    paths: &Paths,
    account: &AccountInfo,
    newly_added: bool,
) -> Result<std::sync::Arc<std::sync::atomic::AtomicBool>, String> {
    // agy needs a TTY for its account picker and the SSH OAuth code prompt.
    if account.provider == ProviderKind::Antigravity {
        return Err(crate::i18n::catalog::settings::AGY_LOGIN_GUIDE
            .get()
            .to_owned());
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let path = crate::onboarding::login_path(&shell)
        .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let exe = krill_proto::agent::which(account.provider.as_str(), &path)
        .ok_or_else(|| "provider executable not found".to_owned())?;
    let mut command =
        krill_proto::account_login::login_command(&exe, account.provider, login_dir(account))
            .ok_or_else(|| {
                crate::i18n::catalog::settings::ACCOUNT_LOGIN_UNSUPPORTED
                    .get()
                    .to_owned()
            })?;
    command.env("PATH", path);
    let (paths, account) = (paths.clone(), account.clone());
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_for_thread = cancel.clone();
    std::thread::Builder::new()
        .name("account-login-browser".into())
        .spawn(move || {
            run_browser_login(&paths, &account, newly_added, command, &cancel_for_thread);
        })
        .map_err(|e| e.to_string())?;
    Ok(cancel)
}

/// Interactive providers need a terminal for their account picker and OAuth code prompt.
pub fn terminal_login_command(account: &AccountInfo) -> String {
    #[cfg(windows)]
    {
        let provider = account.provider.as_str();
        let id = account.id.replace('\'', "''");
        let exe = krill_proto::daemon::sibling_exe("krill")
            .unwrap_or_else(|_| PathBuf::from("krill.exe"));
        let exe = exe.to_string_lossy().replace('\'', "''");
        // The terminal's default shell is PowerShell. Let the native CLI own profile scoping.
        format!("& '{exe}' account login {provider} '{id}'")
    }
    #[cfg(not(windows))]
    terminal_login_command_unix(account)
}

#[cfg(not(windows))]
fn terminal_login_command_unix(account: &AccountInfo) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    if account.provider == ProviderKind::Antigravity {
        return if account.host_default {
            "agy".into()
        } else {
            format!(
                "env SSH_CONNECTION=krill agy --gemini_dir={}",
                quote(
                    &Path::new(&account.profile_dir)
                        .join(".gemini")
                        .to_string_lossy()
                )
            )
        };
    }
    let unsets = ProviderKind::OpenCode
        .credential_overrides()
        .iter()
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>()
        .join(" ");
    if account.host_default {
        return format!("env {unsets} opencode auth login");
    }
    let dir = Path::new(&account.profile_dir);
    format!(
        "env {unsets} XDG_DATA_HOME={} XDG_STATE_HOME={} OPENCODE_DB=opencode.db opencode auth login",
        quote(&dir.join("data").to_string_lossy()),
        quote(&dir.join("state").to_string_lossy())
    )
}

/// 백그라운드 브라우저 로그인의 종료 사유(OWE-881). 설정 탭의 모니터가
/// [`take_login_outcome`]으로 꺼내 진행 표시·실패 안내에 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginOutcome {
    Succeeded,
    Failed(LoginFailReason),
    Cancelled,
}

/// 로그인 실패 사유(OWE-881).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFailReason {
    /// 로그인 자식 프로세스를 띄우지 못했거나 임시 작업대를 만들지 못했다.
    SpawnFailed,
    /// 600초 안에 브라우저 인증이 끝나지 않았다.
    TimedOut,
    /// CLI가 비정상 종료했거나 로그인 상태를 확인하지 못했다.
    Unsuccessful,
}

fn login_outcomes() -> &'static std::sync::Mutex<std::collections::HashMap<String, LoginOutcome>> {
    static OUTCOMES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, LoginOutcome>>,
    > = std::sync::OnceLock::new();
    OUTCOMES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn lock_login_outcomes()
-> std::sync::MutexGuard<'static, std::collections::HashMap<String, LoginOutcome>> {
    login_outcomes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn record_login_outcome(account_id: &str, outcome: LoginOutcome) {
    lock_login_outcomes().insert(account_id.to_owned(), outcome);
}

#[cfg(test)]
#[cfg(unix)]
pub(crate) fn record_login_outcome_for_test(account_id: &str, outcome: LoginOutcome) {
    record_login_outcome(account_id, outcome);
}

/// 백그라운드 로그인 스레드가 남긴 종료 사유를 꺼낸다. 없으면 아직 진행 중이다(OWE-881).
#[must_use]
pub fn take_login_outcome(account_id: &str) -> Option<LoginOutcome> {
    match login_outcomes().try_lock() {
        Ok(mut outcomes) => outcomes.remove(account_id),
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().remove(account_id),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn run_browser_login(
    paths: &Paths,
    account: &AccountInfo,
    newly_added: bool,
    mut command: std::process::Command,
    cancel: &std::sync::atomic::AtomicBool,
) {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::Ordering;
    // 실패 기록+정리를 한 번에 한다. 취소는 별도 경로(호출자가 구분해야 한다).
    let fail = |reason: LoginFailReason| {
        record_login_outcome(&account.id, LoginOutcome::Failed(reason));
        cleanup_failed_login(paths, account, newly_added);
    };
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    let temp = std::env::temp_dir().join(format!("krill-login-{}-{nonce}", std::process::id()));
    if std::fs::create_dir(&temp).is_err() {
        fail(LoginFailReason::SpawnFailed);
        return;
    }
    #[cfg(unix)]
    if std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o700)).is_err() {
        std::fs::remove_dir_all(&temp).ok();
        fail(LoginFailReason::SpawnFailed);
        return;
    }
    let link_file = temp.join("authorize-url");
    let Ok(browser) = login_browser::write_helper(&temp) else {
        std::fs::remove_dir_all(&temp).ok();
        fail(LoginFailReason::SpawnFailed);
        return;
    };
    krill_proto::os::configure_background(&mut command);
    command
        .env("BROWSER", &browser)
        .env("KRILL_LOGIN_URL_FILE", &link_file)
        // Claude's OAuth flow treats stdin EOF as cancellation. Keep the pipe alive until exit.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let Ok(mut child) = command.spawn() else {
        std::fs::remove_dir_all(&temp).ok();
        fail(LoginFailReason::SpawnFailed);
        return;
    };
    let links = login_output::watch(&mut child, account.provider);
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    let mut opened = false;
    let mut timed_out = false;
    let mut cancelled = false;
    let success = loop {
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            child.wait().ok();
            cancelled = true;
            break false;
        }
        let browser_link = login_browser::read_link(&link_file);
        let printed_link = links.try_iter().last();
        if !opened
            && let Some(raw) = browser_link.or(printed_link)
            && let Some(url) = authorize_url(account.provider, &raw)
        {
            opened = if cfg!(target_os = "macos") {
                crate::platform::background_command("open")
                    .arg(url)
                    .status()
                    .is_ok_and(|status| status.success())
            } else {
                crate::platform::permissions::open_url(url)
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                child.kill().ok();
                child.wait().ok();
                // 데드라인을 넘겼으면 타임아웃, 자식이 일찍 죽었으면 비정상 종료다.
                timed_out = std::time::Instant::now() >= deadline;
                break false;
            }
        }
    };
    let dir = login_dir(account);
    let path = command
        .get_envs()
        .find(|(key, _)| *key == "PATH")
        .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned))
        .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let status = if account.provider == ProviderKind::Antigravity && account.host_default && success
    {
        // The native host account may live in Keychain. The daemon owns that probe.
        std::fs::write(
            Path::new(&account.profile_dir).join(".krill-login-completed"),
            b"",
        )
        .ok();
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        loop {
            let connected = request_ipc(paths, &UsageRequest::ListAccounts).is_ok_and(|s| {
                s.connection_for(&account.id) == krill_proto::usage::ConnectionStatus::Connected
            });
            if connected {
                break Some(krill_proto::account_login::AuthStatus {
                    logged_in: true,
                    identity: None,
                });
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    } else {
        krill_proto::account_login::probe_with_environment(
            Path::new(command.get_program()),
            account.provider,
            dir,
            None,
            Some(&path),
        )
    };
    if (success || newly_added)
        && let Some(status) = status.filter(|s| s.logged_in)
    {
        // A late cancel/CLI error must not delete a newly authenticated account.
        std::fs::write(
            Path::new(&account.profile_dir).join(".krill-login-completed"),
            b"",
        )
        .ok();
        if let Some(identity) = status.identity {
            let label = if auto_label(account) {
                identity.org_name.unwrap_or_else(|| identity.email.clone())
            } else {
                account.label.clone()
            };
            request_ipc(
                paths,
                &UsageRequest::RenameAccount {
                    provider: account.provider,
                    account_id: account.id.clone(),
                    label,
                    email: Some(identity.email),
                },
            )
            .ok();
        }
        record_login_outcome(&account.id, LoginOutcome::Succeeded);
    } else if cancelled {
        cleanup_failed_login(paths, account, newly_added);
        record_login_outcome(&account.id, LoginOutcome::Cancelled);
    } else {
        let reason = if timed_out {
            LoginFailReason::TimedOut
        } else {
            LoginFailReason::Unsuccessful
        };
        fail(reason);
    }
    std::fs::remove_dir_all(temp).ok();
}

pub(super) fn authorize_url(provider: ProviderKind, raw: &str) -> Option<&str> {
    let url = raw.trim();
    let parsed = gpui::http_client::Url::parse(url).ok()?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || url.chars().any(char::is_whitespace)
    {
        return None;
    }
    let authority = parsed.host_str()?;
    let allowed = match provider {
        ProviderKind::Claude => {
            // Claude prints a pasted-code link as well. Only BROWSER's localhost callback can
            // finish unattended. Orca claude-sign-in-browser.ts, Lovecast Inc. (2026), MIT.
            let callback = parsed
                .query_pairs()
                .find(|(key, _)| key == "redirect_uri")?
                .1;
            let callback = gpui::http_client::Url::parse(&callback).ok()?;
            ["claude.com", "claude.ai", "console.anthropic.com"].contains(&authority)
                && callback.scheme() == "http"
                && matches!(callback.host_str(), Some("localhost" | "127.0.0.1"))
                && callback.username().is_empty()
                && callback.password().is_none()
        }
        ProviderKind::Codex => ["auth.openai.com", "chatgpt.com"].contains(&authority),
        ProviderKind::Antigravity => ["accounts.google.com"].contains(&authority),
        _ => false,
    };
    allowed.then_some(url)
}

/// 로그인·조회 대상 프로필. 호스트 기본 계정은 `None`(env 없이 실행, [`krill_proto::account_login::login_command`]).
fn login_dir(account: &AccountInfo) -> Option<&Path> {
    (!account.host_default).then(|| Path::new(&account.profile_dir))
}

fn cleanup_failed_login(paths: &Paths, account: &AccountInfo, newly_added: bool) {
    if newly_added {
        // DeleteAccount owns the atomic accounts.toml mutation and moves the complete,
        // app-managed profile (including files written during login) to the user trash.
        request_ipc(
            paths,
            &UsageRequest::DeleteAccount {
                provider: account.provider,
                account_id: account.id.clone(),
            },
        )
        .ok();
    }
}

/// 자동 라벨(`"Claude Code Profile #n"`, 계정 추가 버튼이 붙임)인가. 사용자가 바꾼 라벨은 덮어쓰지 않는다.
#[must_use]
pub fn auto_label(acc: &AccountInfo) -> bool {
    acc.label
        .strip_prefix(acc.provider.display_name())
        .and_then(|rest| rest.strip_prefix(" Profile #"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// 같은 로그인(이메일+조직)을 가진 다른 계정(OWE-705, Orca `claude-duplicate-account` 패리티).
/// 호스트 기본 계정이 있으면 그것을 먼저 돌려준다.
#[must_use]
pub fn duplicate_of<'a>(
    acc: &AccountInfo,
    accounts: &'a [AccountInfo],
    identities: &BTreeMap<String, Identity>,
) -> Option<&'a AccountInfo> {
    let mine = identities.get(&acc.id)?;
    let same = |other: &&AccountInfo| {
        other.id != acc.id
            && other.provider == acc.provider
            && identities.get(&other.id).is_some_and(|id| {
                id.email.eq_ignore_ascii_case(&mine.email) && id.org_id == mine.org_id
            })
    };
    accounts
        .iter()
        .filter(same)
        .min_by_key(|other| !other.host_default)
}

/// 로컬 `accounts.toml`에 없는 Krill 관리 프로필 폴더(OWE-704, 데몬 없는 경로).
pub fn orphans_local(home: &Path, state_dir: Option<&Path>) -> Vec<OrphanProfile> {
    let keep: Vec<PathBuf> = load_file(home, state_dir)
        .accounts
        .iter()
        .map(|a| PathBuf::from(&a.profile_dir))
        .collect();
    krill_proto::usage::profiles::orphans(home, &keep)
}

/// 데몬 연결을 우선하고 로컬 파일로 폴백한다.
pub fn upsert_auto_switch_rule(rule: AutoSwitchRule) -> Result<AccountsSnapshot, String> {
    let paths = Paths::dev().ok();
    request(paths.as_ref(), &UsageRequest::UpsertAutoSwitchRule(rule))
}

pub fn remove_auto_switch_rule(
    provider: ProviderKind,
    from_account_id: &str,
) -> Result<AccountsSnapshot, String> {
    let paths = Paths::dev().ok();
    request(
        paths.as_ref(),
        &UsageRequest::RemoveAutoSwitchRule {
            provider,
            from_account_id: from_account_id.to_owned(),
        },
    )
}

pub fn set_auto_switch_local(
    home: &Path,
    state_dir: Option<&Path>,
    policy: &AutoSwitchPolicy,
) -> io::Result<AccountsSnapshot> {
    let mut file = load_file_result(home, state_dir)?;
    let rules = if policy.rules.is_empty() {
        file.auto_switch.rules
    } else {
        policy.rules.clone()
    };
    file.auto_switch = StoredAutoSwitch {
        enabled: policy.enabled,
        threshold_percent: policy.threshold_percent.clamp(
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MIN,
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MAX,
        ),
        switch_on_rate_limit: policy.switch_on_rate_limit,
        rules,
    };
    save_file(home, state_dir, &file)?;
    Ok(refresh_local(home, state_dir))
}

pub fn default_home() -> PathBuf {
    // 단위 테스트가 실제 사용자 홈(~/.config/krill/profiles 등)을 읽거나
    // 고아 프로필 정리로 휴지통에 옮기지 않게 프로세스별 임시 홈으로 격리한다.
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("krill-app-test-home-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        krill_proto::os::home_dir().unwrap_or_else(std::env::temp_dir)
    }
}

pub fn config_path(home: &Path, state_dir: Option<&Path>) -> PathBuf {
    if let Some(state) = state_dir {
        state.join(ACCOUNTS_FILE)
    } else {
        krill_proto::user_dirs::config_dir(home).join(ACCOUNTS_FILE)
    }
}

fn load_file(home: &Path, state_dir: Option<&Path>) -> AccountsFile {
    load_file_result(home, state_dir).unwrap_or_default()
}

fn load_file_result(home: &Path, state_dir: Option<&Path>) -> io::Result<AccountsFile> {
    let path = config_path(home, state_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AccountsFile::default()),
        Err(e) => Err(e),
    }
}

/// OWE-699: `accounts.toml`의 `provisioning`(키가 없으면 켜짐). 깨진 파일은 기본값으로 본다.
pub fn provisioning_enabled(home: &Path, state_dir: Option<&Path>) -> bool {
    load_file(home, state_dir).provisioning
}

/// `accounts.toml`이 깨졌거나 읽을 수 없으면(없는 파일 제외) 사유를 돌려준다.
/// 읽기 경로는 기본값으로 계속 그리되 이 메시지를 오류 배너에 보인다(AS-04).
pub fn load_problem(home: &Path, state_dir: Option<&Path>) -> Option<String> {
    match load_file_result(home, state_dir) {
        Ok(_) => None,
        Err(e) => Some(format!("{}: {e}", config_path(home, state_dir).display())),
    }
}

fn save_file(home: &Path, state_dir: Option<&Path>, file: &AccountsFile) -> io::Result<()> {
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let path = config_path(home, state_dir);
    let text = krill_proto::config_document::save_toml(&path, file)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 크래시·ENOSPC 타이밍의 잘린 파일을 막는 원자 교체(0600 — 이메일·프로필 경로 포함, AS-04).
    let tmp = path.with_extension(format!(
        "toml.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

fn host_default_dir(home: &Path, provider: ProviderKind) -> PathBuf {
    match provider {
        ProviderKind::Claude => home.join(".claude"),
        ProviderKind::Codex => home.join(".codex"),
        ProviderKind::Gemini => PathBuf::new(),
        ProviderKind::OpenCode => home.join(".config").join("opencode"),
        ProviderKind::Antigravity => home.join(".gemini").join("antigravity-cli"),
    }
}

/// 수동 계정 전환(MK-108): 자동전환을 즉시 실행하지 않아 고사용률 계정으로의 명시적 선택이 덮어쓰이지 않게 한다.
pub fn switch_account(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    account_id: &str,
) -> io::Result<AccountsSnapshot> {
    let mut file = load_file_result(home, state_dir)?;
    file.active
        .insert(provider.as_str().to_owned(), account_id.to_owned());
    save_file(home, state_dir, &file)?;
    Ok(refresh_local(home, state_dir))
}

/// 읽기 전용 스냅샷. 자동전환을 평가하지도 `accounts.toml`을 쓰지도 않는다(OWE-697) — 판정은 krilld만.
/// 앱이 데몬이 쓴 파일을 1초 감시로 다시 읽어도 왕복 전환이 생기지 않는다.
pub fn refresh_local(home: &Path, state_dir: Option<&Path>) -> AccountsSnapshot {
    let file = load_file(home, state_dir);
    let policy = AutoSwitchPolicy {
        enabled: file.auto_switch.enabled,
        threshold_percent: file.auto_switch.threshold_percent.clamp(
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MIN,
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MAX,
        ),
        switch_on_rate_limit: file.auto_switch.switch_on_rate_limit,
        rules: file.auto_switch.rules,
    };

    let mut all = Vec::new();
    for provider in ProviderKind::ALL {
        let id = format!("{}-host", provider.as_str());
        let dir = host_default_dir(home, provider);
        let label = format!(
            "Host ({})",
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default")
        );
        let usage = read_profile_usage(home, provider, &dir);
        all.push(AccountInfo {
            id,
            provider,
            label,
            email: None,
            profile_dir: dir.display().to_string(),
            active: false,
            host_default: true,
            stale: false,
            usage,
        });
    }

    if let Ok(s) = std::fs::read_to_string(home.join(".claude").join(ORCA_CACHE))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
        && let Some(rows) = v.get("accounts").and_then(serde_json::Value::as_array)
    {
        for (idx, row) in rows.iter().enumerate() {
            let label = row
                .get("label")
                .or_else(|| row.get("email"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let host = row
                .get("host")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let stale = row
                .get("stale")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let usage = Snapshot {
                five_hour: parse_window(row, &["five_hour"]),
                seven_day: parse_window(row, &["seven_day"]),
            };
            if host {
                if let Some(h) = all
                    .iter_mut()
                    .find(|a| a.provider == ProviderKind::Claude && a.host_default)
                {
                    if !label.is_empty() {
                        h.email = Some(label);
                    }
                    if h.usage.is_empty() && !usage.is_empty() {
                        h.usage = usage;
                        h.stale = stale;
                    }
                }
                continue;
            }
            // krilld와 같은 규칙(OWE-696): 캐시 행 uuid로 안정 id, Krill 프로필 폴더는 없음(빈 경로).
            let id = match row.get("id").and_then(serde_json::Value::as_str) {
                Some(uuid) if !uuid.is_empty() => format!("{ORCA_ID_PREFIX}{}", slugify(uuid)),
                _ => format!("{ORCA_ID_PREFIX}{idx}-{}", slugify(&label)),
            };
            if file.hidden_accounts.contains(&id) {
                continue;
            }
            all.push(AccountInfo {
                id,
                provider: ProviderKind::Claude,
                label: if label.is_empty() {
                    format!("Claude #{idx}")
                } else {
                    label.clone()
                },
                email: (!label.is_empty()).then_some(label),
                profile_dir: String::new(),
                active: false,
                host_default: false,
                stale,
                usage,
            });
        }
    }

    for stored in &file.accounts {
        let Some(provider) = ProviderKind::parse(&stored.provider) else {
            continue;
        };
        let usage = read_profile_usage(home, provider, Path::new(&stored.profile_dir));
        if let Some(ex) = all.iter_mut().find(|a| a.id == stored.id) {
            ex.label.clone_from(&stored.label);
            ex.email.clone_from(&stored.email);
            ex.profile_dir.clone_from(&stored.profile_dir);
            if !usage.is_empty() {
                ex.usage = usage;
            }
        } else {
            all.push(AccountInfo {
                id: stored.id.clone(),
                provider,
                label: stored.label.clone(),
                email: stored.email.clone(),
                profile_dir: stored.profile_dir.clone(),
                active: false,
                host_default: false,
                stale: false,
                usage,
            });
        }
    }

    for provider in ProviderKind::ALL {
        let active_id = file
            .active
            .get(provider.as_str())
            .filter(|id| all.iter().any(|a| a.provider == provider && a.id == **id))
            .cloned()
            .unwrap_or_else(|| format!("{}-host", provider.as_str()));
        for a in all.iter_mut().filter(|a| a.provider == provider) {
            a.active = a.id == active_id;
        }
    }

    AccountsSnapshot {
        accounts: all,
        auto_switch: policy,
        switched: Vec::new(),
        // OWE-703 1단계: 상태 꼬리는 데몬 스냅샷에만 있다(앱 로컬 폴백은 모름).
        statuses: Vec::new(),
        advanced: krill_proto::usage::AccountPolicyState::default(),
        // OWE-719: 연결 상태도 데몬만 안다. 앱 폴백은 `supported: false`라 화면이 사용량 상태로 대신 그린다.
        connections: Box::default(),
        details: Box::default(),
    }
}

pub fn read_profile_usage(_home: &Path, provider: ProviderKind, dir: &Path) -> Snapshot {
    if let Ok(s) = std::fs::read_to_string(dir.join(CLAUDE_CACHE)) {
        let snap = parse_claude_json(&s);
        if !snap.is_empty() {
            return snap;
        }
        let agy = krill_proto::usage::parse_antigravity_usage(&s);
        if !agy.is_empty() {
            return agy;
        }
    }
    for name in [
        "usage.json",
        "usage_cache.json",
        "quota.json",
        "stats-cache.json",
        "rate_limits.json",
    ] {
        if let Ok(s) = std::fs::read_to_string(dir.join(name)) {
            let snap = parse_claude_json(&s);
            if !snap.is_empty() {
                return snap;
            }
            let agy = krill_proto::usage::parse_antigravity_usage(&s);
            if !agy.is_empty() {
                return agy;
            }
        }
    }
    if provider == ProviderKind::Codex
        && let Some(snap) =
            read_codex_rollout(&dir.join("sessions")).or_else(|| read_codex_rollout(dir))
    {
        return snap;
    }
    Snapshot::default()
}

fn parse_claude_json(s: &str) -> Snapshot {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return Snapshot::default();
    };
    let body = v.get("rate_limits").unwrap_or(&v);
    Snapshot {
        five_hour: parse_window(body, &["five_hour", "5_hour"]),
        seven_day: parse_window(body, &["seven_day", "7_day"]),
    }
}

fn parse_window(v: &serde_json::Value, keys: &[&str]) -> Option<Window> {
    let w = keys.iter().find_map(|k| v.get(*k))?;
    let pct = parse_percent(
        w.get("utilization")
            .or_else(|| w.get("used_percentage"))
            .or_else(|| w.get("used_percent"))
            .or_else(|| w.get("percent"))?,
    )?;
    let resets_at = match w.get("resets_at") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    Some(Window {
        percent: pct,
        resets_at,
    })
}

fn parse_percent(v: &serde_json::Value) -> Option<u8> {
    if let Some(n) = v.as_u64() {
        return Some(u8::try_from(n).unwrap_or(100).min(100));
    }
    let f = v.as_f64()?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped 0..=100"
    )]
    let n = f.clamp(0.0, 100.0).round() as u8;
    Some(n)
}

fn read_codex_rollout(sessions: &Path) -> Option<Snapshot> {
    let mut stack = vec![sessions.to_path_buf()];
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Some(d) = stack.pop() {
        let Ok(rows) = std::fs::read_dir(&d) else {
            continue;
        };
        for row in rows.flatten() {
            let Ok(ft) = row.file_type() else {
                continue;
            };
            let p = row.path();
            if ft.is_dir() {
                if row
                    .file_name()
                    .to_str()
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    stack.push(p);
                }
            } else if ft.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rollout-"))
                && p.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                && let Ok(m) = row.metadata().and_then(|m| m.modified())
                && best.as_ref().is_none_or(|(t, _)| m >= *t)
            {
                best = Some((m, p));
            }
        }
    }
    let (_, path) = best?;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_MAX);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let slice = if start > 0 {
        bytes
            .iter()
            .position(|&b| b == b'\n')
            .and_then(|i| bytes.get(i + 1..))
            .unwrap_or_default()
    } else {
        &bytes[..]
    };
    let text = String::from_utf8_lossy(slice);
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(rl) = v
            .get("rate_limits")
            .or_else(|| v.get("payload").and_then(|p| p.get("rate_limits")))
        else {
            continue;
        };
        let mut snap = Snapshot::default();
        for w in [rl.get("primary"), rl.get("secondary")]
            .into_iter()
            .flatten()
        {
            let mins = w.get("window_minutes").and_then(serde_json::Value::as_u64);
            let slot = match mins {
                Some(FIVE_HOUR_MINUTES) => &mut snap.five_hour,
                Some(SEVEN_DAY_MINUTES) => &mut snap.seven_day,
                _ => continue,
            };
            if let Some(pct) = w.get("used_percent").and_then(parse_percent) {
                let resets_at = match w.get("resets_at") {
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    Some(serde_json::Value::Number(n)) => Some(n.to_string()),
                    _ => None,
                };
                *slot = Some(Window {
                    percent: pct,
                    resets_at,
                });
            }
        }
        if !snap.is_empty() {
            return Some(snap);
        }
    }
    None
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    if t.is_empty() {
        "account".to_owned()
    } else {
        t.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_switch_account_to_high_usage_stays_on_selected_account() {
        let root = std::env::temp_dir().join(format!("mk-acc-switch-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state = root.join("state");
        let host_claude = home.join(".claude");
        let high_profile = root.join("high-profile");
        std::fs::create_dir_all(&host_claude).unwrap();
        std::fs::create_dir_all(&high_profile).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        // Host account has low usage (15%), while secondary account has 95% in usage_cache.json.
        std::fs::write(
            host_claude.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":15,"resets_at":1760000000}}"#,
        )
        .unwrap();
        std::fs::write(
            high_profile.join("usage_cache.json"),
            r#"{"five_hour":{"utilization":95,"resets_at":1760003600},"seven_day":{"utilization":88}}"#,
        )
        .unwrap();

        let _ = request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "High Max".to_owned(),
                email: Some("high@example.com".to_owned()),
                profile_dir: high_profile.display().to_string(),
            },
        )
        .unwrap();

        let _ = request_local(
            &home,
            Some(&state),
            &UsageRequest::SetAutoSwitch(AutoSwitchPolicy {
                enabled: true,
                threshold_percent: 80,
                switch_on_rate_limit: true,
                rules: Vec::new(),
            }),
        )
        .unwrap();

        // Manual switch to the 95% account must NOT be immediately overridden by auto-switch.
        let snap =
            switch_account(&home, Some(&state), ProviderKind::Claude, "claude-high-max").unwrap();
        assert!(snap.switched.is_empty());
        let active = snap
            .accounts
            .iter()
            .find(|a| a.provider == ProviderKind::Claude && a.active)
            .unwrap();
        assert_eq!(active.id, "claude-high-max");
        assert_eq!(active.usage.max_percent(), 95);

        let saved = load_file(&home, Some(&state));
        assert_eq!(
            saved.active.get("claude").map(String::as_str),
            Some("claude-high-max")
        );

        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
#[cfg(unix)]
mod as_regression_tests {
    use super::*;

    fn dead_paths(root: &Path) -> krill_proto::daemon::Paths {
        krill_proto::daemon::Paths {
            socket: root.join("run/krilld.sock"),
            holder: root.join("krill-holder"),
            krilld: root.join("krilld"),
        }
    }

    fn sandbox(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("mk-acc-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state = root.join("state");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        (root, home, state)
    }

    /// AS-03: 데몬 거부가 로컬 성공으로 둔갑하면 안 된다. 변이 요청은 오류를
    /// 그대로 돌려주고 파일을 건드리지 않으며, 조회는 로컬 폴백을 유지한다.
    #[test]
    fn as03_daemon_refusal_is_not_masked_by_local_fallback() {
        let (root, home, state) = sandbox("as03");
        let paths = dead_paths(&root);
        let err = request(
            Some(&paths),
            &UsageRequest::SwitchAccount {
                provider: ProviderKind::Claude,
                account_id: "claude-x".into(),
            },
        )
        .expect_err("mutation must surface daemon error, not local fallback");
        assert!(!err.is_empty());
        assert!(
            !config_path(&home, Some(&state)).exists(),
            "failed mutation must not write a fallback file"
        );
        request(Some(&paths), &UsageRequest::Get).expect("reads keep local fallback");
        std::fs::remove_dir_all(&root).ok();
    }

    /// AS-04: 계정 파일은 이메일·프로필 경로를 담으므로 0600 + 원자 교체.
    #[test]
    fn as04_accounts_save_is_atomic_and_restricted() {
        use std::os::unix::fs::PermissionsExt as _;
        let (root, home, state) = sandbox("as04");
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "Work".into(),
                email: Some("w@example.com".into()),
                profile_dir: home.join("p").display().to_string(),
            },
        )
        .unwrap();
        let path = config_path(&home, Some(&state));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "accounts.toml holds emails and profile paths");
        assert_eq!(load_file(&home, Some(&state)).accounts.len(), 1);
        for entry in std::fs::read_dir(&state).unwrap() {
            let path = entry.unwrap().path();
            assert!(
                path.extension()
                    .is_none_or(|e| !e.eq_ignore_ascii_case("tmp")),
                "leftover tmp file: {}",
                path.display()
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// AS-04: 깨진 파일은 무통보 기본값 대체가 아니라 오류 보고 + 변이 거부.
    #[test]
    fn as04_corrupt_file_blocks_mutation_and_reports() {
        let (root, home, state) = sandbox("as04c");
        let path = config_path(&home, Some(&state));
        std::fs::write(&path, "not = [valid toml\n").unwrap();
        assert!(
            load_problem(&home, Some(&state)).is_some(),
            "corruption must be reported, not silently defaulted"
        );
        let before = std::fs::read(&path).unwrap();
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "Work".into(),
                email: None,
                profile_dir: home.join("p").display().to_string(),
            },
        )
        .expect_err("mutation on corrupt file must fail, not reset the file");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "failed mutation must preserve original bytes"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// AS-13: 대소문자만 다른 라벨은 기존 계정을 덮어쓰면 안 된다.
    #[test]
    fn as13_case_only_label_collision_gets_unique_id() {
        let (root, home, state) = sandbox("as13");
        let profile_a = home.join("a").display().to_string();
        let profile_b = home.join("b").display().to_string();
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "Work".into(),
                email: Some("a@example.com".into()),
                profile_dir: profile_a.clone(),
            },
        )
        .unwrap();
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "work".into(),
                email: Some("b@example.com".into()),
                profile_dir: profile_b.clone(),
            },
        )
        .unwrap();
        let file = load_file(&home, Some(&state));
        assert_eq!(file.accounts.len(), 2, "distinct labels must not overwrite");
        assert_ne!(file.accounts[0].id, file.accounts[1].id);
        assert_eq!(
            file.accounts
                .iter()
                .find(|a| a.label == "Work")
                .unwrap()
                .profile_dir,
            profile_a
        );
        // 같은 라벨 재등록은 멱등 덮어쓰기를 유지한다.
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "Work".into(),
                email: Some("a@example.com".into()),
                profile_dir: profile_a.clone(),
            },
        )
        .unwrap();
        assert_eq!(load_file(&home, Some(&state)).accounts.len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    /// OWE-697: 데몬에서 고친 K-10·K-11·K-14·K-27 시나리오 모두 앱은 전환하지 않고 파일도 쓰지 않는다.
    /// (옛 앱 엔진은 네 경우 모두 전환해 `accounts.toml`에 기록했다.)
    #[test]
    fn app_refresh_never_auto_switches_or_writes() {
        let cases = [
            // K-10: 사용량을 모르는 계정으로 전환.
            ("k10", r#"{"five_hour":{"utilization":95}}"#, None, true),
            // K-11: 리셋 시각이 지난 창의 수치로 전환.
            (
                "k11",
                r#"{"five_hour":{"utilization":95,"resets_at":"2000-01-01T00:00:00Z"}}"#,
                Some(r#"{"five_hour":{"utilization":10}}"#),
                true,
            ),
            // K-14: 히스테리시스 없이 1%p 차이로 전환.
            (
                "k14",
                r#"{"five_hour":{"utilization":91}}"#,
                Some(r#"{"five_hour":{"utilization":90}}"#),
                true,
            ),
            // K-27: switch_on_rate_limit=false인데 100%에서 전환.
            (
                "k27",
                r#"{"five_hour":{"utilization":100}}"#,
                Some(r#"{"five_hour":{"utilization":10}}"#),
                false,
            ),
        ];
        for (name, host_usage, backup_usage, on_rate_limit) in cases {
            let (root, home, state) = sandbox(&format!("owe697-{name}"));
            std::fs::create_dir_all(home.join(".claude")).unwrap();
            std::fs::write(home.join(".claude").join(CLAUDE_CACHE), host_usage).unwrap();
            let backup = root.join("backup");
            std::fs::create_dir_all(&backup).unwrap();
            if let Some(usage) = backup_usage {
                std::fs::write(backup.join("usage_cache.json"), usage).unwrap();
            }
            let file = AccountsFile {
                auto_switch: StoredAutoSwitch {
                    enabled: true,
                    threshold_percent: 90,
                    switch_on_rate_limit: on_rate_limit,
                    rules: Vec::new(),
                },
                active: BTreeMap::new(),
                accounts: vec![StoredAccount {
                    id: "claude-backup".into(),
                    provider: "claude".into(),
                    label: "Backup".into(),
                    email: None,
                    profile_dir: backup.display().to_string(),
                }],
                provisioning: false,
                hidden_accounts: Vec::new(),
            };
            save_file(&home, Some(&state), &file).unwrap();
            let path = config_path(&home, Some(&state));
            let before = std::fs::read(&path).unwrap();
            let snap = refresh_local(&home, Some(&state));
            assert!(snap.switched.is_empty(), "{name}: {:?}", snap.switched);
            let active = snap
                .accounts
                .iter()
                .find(|a| a.provider == ProviderKind::Claude && a.active)
                .unwrap();
            assert_eq!(active.id, "claude-host", "{name}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{name}: app wrote");
            std::fs::remove_dir_all(&root).ok();
        }
    }

    /// OWE-697: 데몬이 전환해 `accounts.toml`을 쓴 직후 1초 감시가 다시 읽어도 앱은 되돌리거나
    /// 추가로 전환하지 않는다(옛 엔진은 자기 규칙으로 재판정해 왕복 전환할 수 있었다).
    #[test]
    fn watch_refresh_after_daemon_switch_does_not_switch_again() {
        let (root, home, state) = sandbox("owe697-watch");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":20}}"#,
        )
        .unwrap();
        let backup = root.join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        // 데몬이 고른 계정이 이후 임계치를 넘었다: 옛 앱 엔진이면 호스트(20%)로 되돌렸다.
        std::fs::write(
            backup.join("usage_cache.json"),
            r#"{"five_hour":{"utilization":95}}"#,
        )
        .unwrap();
        let daemon_written = "[auto_switch]\nenabled = true\nthreshold_percent = 90\nswitch_on_rate_limit = true\n\n[active]\nclaude = \"claude-backup\"\n\n[[accounts]]\nid = \"claude-backup\"\nprovider = \"claude\"\nlabel = \"Backup\"\nprofile_dir = \"PROFILE\"\n"
            .replace("PROFILE", &backup.display().to_string());
        let path = config_path(&home, Some(&state));
        std::fs::write(&path, &daemon_written).unwrap();
        for _ in 0..3 {
            let snap = refresh_local(&home, Some(&state));
            assert!(snap.switched.is_empty(), "{:?}", snap.switched);
            assert!(
                snap.accounts
                    .iter()
                    .any(|a| a.id == "claude-backup" && a.active)
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), daemon_written);
        std::fs::remove_dir_all(&root).ok();
    }

    /// OWE-696: Orca 감지 계정은 캐시 행 uuid로 id를 만들고(순번 무관) Krill 프로필 경로가 없다.
    #[test]
    fn orca_rows_get_stable_uuid_ids_without_profile_dir() {
        let (root, home, state) = sandbox("owe696-orca");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let write = |rows: &str| {
            std::fs::write(
                home.join(".claude").join(ORCA_CACHE),
                format!(r#"{{"accounts":[{rows}]}}"#),
            )
            .unwrap();
        };
        let a = r#"{"id":"96f8c826-8b89-4470-a327-f4918efb8177","label":"a@example.com"}"#;
        let b = r#"{"id":"432d5922-d39e-4df0-9255-6de3443b3df0","label":"b@example.com"}"#;
        let b_id = "claude-orca-432d5922-d39e-4df0-9255-6de3443b3df0";
        let mut file = AccountsFile::default();
        file.active.insert("claude".into(), b_id.into());
        save_file(&home, Some(&state), &file).unwrap();
        for rows in [format!("{a},{b}"), format!("{b},{a}")] {
            write(&rows);
            let snap = refresh_local(&home, Some(&state));
            let active = snap
                .accounts
                .iter()
                .find(|x| x.provider == ProviderKind::Claude && x.active)
                .unwrap();
            assert_eq!(active.id, b_id);
            assert_eq!(active.label, "b@example.com");
            assert!(orca_managed(active));
            assert!(active.profile_dir.is_empty());
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// 교차 검수: 충돌 접미사는 기본 프로필 경로에도 적용하고 재등록 시 id를 유지한다.
    #[test]
    fn colliding_labels_keep_distinct_profiles_and_stable_ids() {
        let (root, home, state) = sandbox("cross-profile-collision");
        for label in ["업무", "개인", "개인"] {
            request_local(
                &home,
                Some(&state),
                &UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: label.into(),
                    email: None,
                    profile_dir: String::new(),
                },
            )
            .unwrap();
        }
        let file = load_file(&home, Some(&state));
        assert_eq!(
            file.accounts.len(),
            2,
            "suffix account registration must be idempotent"
        );
        let work = file.accounts.iter().find(|a| a.label == "업무").unwrap();
        let personal = file.accounts.iter().find(|a| a.label == "개인").unwrap();
        assert_eq!(work.id, "claude-account");
        assert_eq!(personal.id, "claude-account-2");
        assert_ne!(work.profile_dir, personal.profile_dir);
        assert!(Path::new(&personal.profile_dir).ends_with("account-2"));
        std::fs::remove_dir_all(root).ok();
    }
    /// 교차 검수(K-04): 수동 지정 프로필 디렉터리는 같은 라벨 재등록 때 유지된다.
    #[test]
    fn re_add_with_empty_dir_keeps_custom_profile_dir() {
        let (root, home, state) = sandbox("cross-profile-keep-custom");
        let custom = root.join("custom-profile");
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "업무".into(),
                email: None,
                profile_dir: custom.display().to_string(),
            },
        )
        .unwrap();
        // 빈 디렉터리로 재등록해도 기존 디렉터리를 덮어쓰지 않는다(krilld와 동일).
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "업무".into(),
                email: None,
                profile_dir: String::new(),
            },
        )
        .unwrap();
        let file = load_file(&home, Some(&state));
        assert_eq!(file.accounts.len(), 1, "re-add must stay idempotent");
        assert_eq!(
            file.accounts[0].profile_dir,
            custom.display().to_string(),
            "custom profile dir must survive re-add with empty dir"
        );
        std::fs::remove_dir_all(root).ok();
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    /// 규칙 저장·삭제와 전역 정책 갱신은 로컬 파일에 그대로 남는다. 평가는 데몬 몫이라 `switched`는 비어 있다.
    #[test]
    fn per_account_rules_persist_and_survive_global_updates() {
        use krill_proto::usage::{AutoSwitchMetric, AutoSwitchPolicy, AutoSwitchRule};

        let root =
            std::env::temp_dir().join(format!("krill-app-rule-behavior-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state = root.join("state");
        for provider in [ProviderKind::Claude, ProviderKind::Codex] {
            let rule = AutoSwitchRule {
                provider,
                from_account_id: format!("{}-host", provider.as_str()),
                enabled: true,
                threshold_percent: 90,
                metric: AutoSwitchMetric::SevenDay,
                target_account_id: Some(format!("{}-target", provider.as_str())),
            };
            let snap = request_local(
                &home,
                Some(&state),
                &UsageRequest::UpsertAutoSwitchRule(rule.clone()),
            )
            .unwrap();
            assert!(snap.switched.is_empty());
            assert!(snap.auto_switch.rules.contains(&rule));
            let policy = AutoSwitchPolicy {
                enabled: false,
                ..AutoSwitchPolicy::default()
            };
            let updated = set_auto_switch_local(&home, Some(&state), &policy).unwrap();
            assert_eq!(updated.auto_switch.rules, vec![rule.clone()]);
            assert_eq!(load_file(&home, Some(&state)).auto_switch.rules, vec![rule]);
            let removed = request_local(
                &home,
                Some(&state),
                &UsageRequest::RemoveAutoSwitchRule {
                    provider,
                    from_account_id: format!("{}-host", provider.as_str()),
                },
            )
            .unwrap();
            assert!(removed.auto_switch.rules.is_empty());
            assert!(load_file(&home, Some(&state)).auto_switch.rules.is_empty());
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn legacy_accounts_toml_defaults_rules_to_empty() {
        let file: AccountsFile = toml::from_str(
            "[auto_switch]\nenabled = true\nthreshold_percent = 90\nswitch_on_rate_limit = true\n",
        )
        .unwrap();
        assert!(file.auto_switch.rules.is_empty());
    }
}

#[cfg(test)]
#[test]
fn as12_accounts_save_preserves_future_nested_fields() {
    let dir = std::env::temp_dir().join(format!("as12-app-accounts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(ACCOUNTS_FILE);
    let raw = "# newer accounts\nfuture = 'root'\n[[accounts]]\nid = 'one'\nprovider = 'claude'\nlabel = 'old'\nprofile_dir = '/tmp/one'\nfuture = { nested = true } # preserve\n[auto_switch]\nenabled = false\nthreshold_percent = 90\nswitch_on_rate_limit = true\nfuture = 'policy'\n";
    std::fs::write(&path, raw).unwrap();
    let mut file = load_file_result(&dir, Some(&dir)).unwrap();
    file.accounts[0].label = "edited".into();
    save_file(&dir, Some(&dir), &file).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert_eq!(value["future"].as_str(), Some("root"));
    assert_eq!(value["auto_switch"]["future"].as_str(), Some("policy"));
    assert_eq!(
        value["accounts"][0]["future"]["nested"].as_bool(),
        Some(true)
    );
    assert!(body.contains("# preserve"));
    std::fs::remove_dir_all(dir).unwrap();
}

/// OWE-699: 프로비저닝은 키가 없으면 켜짐이고, 명시한 값은 앱이 계정 파일을 다시 써도 유지된다.
#[cfg(test)]
#[test]
fn owe699_provisioning_on_by_default_and_survives_app_save() {
    let dir = std::env::temp_dir().join(format!("owe699-app-accounts-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    assert!(provisioning_enabled(&dir, Some(&dir)), "파일 없음 = 켜짐");
    std::fs::write(dir.join(ACCOUNTS_FILE), "[auto_switch]\nenabled = false\n").unwrap();
    assert!(provisioning_enabled(&dir, Some(&dir)), "키 없음 = 켜짐");
    std::fs::write(dir.join(ACCOUNTS_FILE), "provisioning = false\n").unwrap();
    assert!(!provisioning_enabled(&dir, Some(&dir)));
    let mut file = load_file_result(&dir, Some(&dir)).unwrap();
    file.auto_switch.enabled = false;
    save_file(&dir, Some(&dir), &file).unwrap();
    assert!(!provisioning_enabled(&dir, Some(&dir)), "명시한 꺼짐 유지");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(test)]
mod manage_tests {
    use super::*;

    fn acc(id: &str, label: &str, host: bool) -> AccountInfo {
        AccountInfo {
            id: id.into(),
            provider: ProviderKind::Claude,
            label: label.into(),
            email: None,
            profile_dir: String::new(),
            active: false,
            host_default: host,
            stale: false,
            usage: Snapshot::default(),
        }
    }

    /// OWE-705: `claude auth status --json`에서 로그인·이메일·조직만 읽는다(미로그인·이메일 없음은 None).
    #[test]
    fn claude_identity_parses_logged_in_email_and_org() {
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"a@x.io","orgId":"o1","orgName":"Acme","subscriptionType":"max"}"#;
        assert_eq!(
            parse_claude_identity(json),
            Some(Identity {
                email: "a@x.io".into(),
                org_id: Some("o1".into()),
                org_name: Some("Acme".into()),
            })
        );
        assert_eq!(
            parse_claude_identity(r#"{"loggedIn":false,"authMethod":"none"}"#),
            None
        );
        assert_eq!(parse_claude_identity(r#"{"loggedIn":true}"#), None);
        assert_eq!(parse_claude_identity("not json"), None);
    }

    /// 자동 라벨만 조직 이름으로 바꾼다. 사용자가 고친 라벨은 그대로.
    #[test]
    fn auto_label_matches_only_add_button_labels() {
        assert!(auto_label(&acc(
            "claude-p1",
            "Claude Code Profile #1",
            false
        )));
        assert!(auto_label(&acc(
            "claude-p12",
            "Claude Code Profile #12",
            false
        )));
        assert!(!auto_label(&acc("claude-w", "Work", false)));
        assert!(!auto_label(&acc(
            "claude-p",
            "Claude Code Profile #",
            false
        )));
        assert!(!auto_label(&acc(
            "claude-p",
            "Claude Code Profile #1 work",
            false
        )));
    }

    /// OWE-705: 같은 이메일+조직이면 중복, 호스트 기본 계정을 먼저 가리킨다. 조직이 다르면 다른 계정.
    #[test]
    fn duplicate_detects_same_email_and_org_preferring_host() {
        let accounts = vec![
            acc("claude-a", "A", false),
            acc("claude-host", "Host", true),
            acc("claude-b", "B", false),
            acc("claude-c", "C", false),
        ];
        let id = |email: &str, org: &str| Identity {
            email: email.into(),
            org_id: Some(org.into()),
            org_name: None,
        };
        let identities: BTreeMap<String, Identity> = [
            ("claude-a".to_owned(), id("me@x.io", "o1")),
            ("claude-host".to_owned(), id("ME@x.io", "o1")),
            ("claude-b".to_owned(), id("me@x.io", "o1")),
            ("claude-c".to_owned(), id("me@x.io", "o2")),
        ]
        .into();
        let dup =
            |i: usize| duplicate_of(&accounts[i], &accounts, &identities).map(|a| a.id.as_str());
        assert_eq!(dup(0), Some("claude-host"));
        assert_eq!(dup(2), Some("claude-host"));
        assert_eq!(dup(1), Some("claude-a"));
        assert_eq!(dup(3), None);
        let unknown = acc("claude-z", "Z", false);
        assert!(duplicate_of(&unknown, &accounts, &identities).is_none());
    }

    /// OWE-704 로컬 경로: 이름 변경은 id·폴더를 유지하고, 삭제는 관리 루트 밖 폴더를 남긴다.
    #[test]
    fn local_rename_keeps_id_and_delete_keeps_custom_dir() {
        let root = std::env::temp_dir().join(format!("mk-acc-manage-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let (home, state, custom) = (root.join("home"), root.join("state"), root.join("mine"));
        std::fs::create_dir_all(&state).unwrap();
        request_local(
            &home,
            Some(&state),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: "Claude Profile #1".into(),
                email: None,
                profile_dir: custom.display().to_string(),
            },
        )
        .unwrap();
        let snap = request_local(
            &home,
            Some(&state),
            &UsageRequest::RenameAccount {
                provider: ProviderKind::Claude,
                account_id: "claude-claude-profile-1".into(),
                label: "Acme".into(),
                email: Some("a@x.io".into()),
            },
        )
        .unwrap();
        let renamed = snap
            .accounts
            .iter()
            .find(|a| a.id == "claude-claude-profile-1")
            .unwrap();
        assert_eq!(renamed.label, "Acme");
        assert_eq!(renamed.email.as_deref(), Some("a@x.io"));
        assert_eq!(renamed.profile_dir, custom.display().to_string());
        let snap = request_local(
            &home,
            Some(&state),
            &UsageRequest::DeleteAccount {
                provider: ProviderKind::Claude,
                account_id: "claude-claude-profile-1".into(),
            },
        )
        .unwrap();
        assert!(snap.accounts.iter().all(|a| a.host_default));
        assert!(custom.is_dir());
        // 관리 루트의 고아 폴더는 목록에 보이고 정리하면 휴지통으로 간다.
        let orphan = krill_proto::usage::profiles::managed_root(&home)
            .join("opencode")
            .join("opencode-profile-2");
        std::fs::create_dir_all(&orphan).unwrap();
        assert_eq!(orphans_local(&home, Some(&state)).len(), 1);
        request_local(&home, Some(&state), &UsageRequest::PurgeOrphanProfiles).unwrap();
        assert!(orphans_local(&home, Some(&state)).is_empty());
        assert!(
            home.join(".Trash")
                .join("krill-opencode-opencode-profile-2")
                .is_dir()
        );
        std::fs::remove_dir_all(root).ok();
    }
}

#[test]
fn async_login_poll_never_waits_for_outcome_writer() {
    let guard = lock_login_outcomes();
    assert_eq!(take_login_outcome("contended-poll"), None);
    drop(guard);
}
