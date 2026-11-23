//! Antigravity (`agy`) native account vault, credential backends, and account service.
//!
//! 1:1 port of Orca's `src/main/antigravity/native-*` modules (`stablyai/orca` `cb35acc25`):
//! - `codec`: `go-keyring-base64:`/`go-keyring-encoded:`/JSON codec + Google OIDC identity projection
//! - `backend`: macOS Keychain, Windows Credential Manager, Linux Secret Service (+ KDE `secret-tool search`),
//!   file fallback, `AGY_CLI_FORCE_FILE_TOKEN_STORAGE=1` (`agy 1.5.16+`), keyring fallback marker, CAS + readback
//! - `store`: encrypted `<state_dir>/antigravity-accounts/vault` (AES-256-GCM + OS-protected master key, mode 0600)
//! - `service`: `AntigravityAccountService` (`list_accounts`, `add_current_account`, `select_account`,
//!   `remove_account`, `prepare_for_launch`, `begin_usage_read`, `finish_usage_read`, Google `userinfo` lookup)
//! - Also preserves `migrate_codex_host_once` and compatibility shims for legacy Krill callers.

pub(crate) mod backend;
pub(crate) mod codec;
pub(crate) mod service;
pub(crate) mod store;

#[cfg(unix)]
use std::io::Read as _;
#[cfg(unix)]
use std::io::Write as _;
use std::path::Path;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::Mutex;
#[cfg(unix)]
use std::time::{Duration, Instant};

use krill_proto::usage::{AntigravityAccountOp, AntigravityAccountState, ProviderKind};
use zeroize::Zeroizing;

use super::{AccountsFile, StoredAccount, identity_from_credentials, write_atomic_0600};

pub(crate) use backend::{antigravity_token_file_path, is_antigravity_file_storage_host};
pub(crate) use service::{
    AntigravityAccountService, AntigravityUsageReadFinish, migrate_legacy_agy_profiles_once,
    prepare_antigravity_account_for_launch,
};
use store::AntigravityAccountStore as _;
pub(crate) use store::{StoredAntigravityAccount, vault_path};

const MAX_TOKEN_BYTES: u64 = 64 * 1024;
#[cfg(unix)]
pub(crate) const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);
static SYNC_LOCK: Mutex<()> = Mutex::new(());

/// Dispatch one `UsageRequest::AntigravityAccounts(op)` against the host's native Antigravity service.
pub(crate) fn handle_op(
    home: &Path,
    state_dir: Option<&Path>,
    op: &AntigravityAccountOp,
) -> Result<AntigravityAccountState, String> {
    migrate_legacy_agy_profiles_once(home, state_dir);
    let svc = AntigravityAccountService::for_host(home, state_dir);
    match op {
        AntigravityAccountOp::List => svc.list_accounts(),
        AntigravityAccountOp::AddCurrent => svc.add_current_account(),
        AntigravityAccountOp::Select { account_id } => {
            let state = svc.select_account(account_id)?;
            evict_host_agy_caches(home);
            Ok(state)
        }
        AntigravityAccountOp::Remove { account_id } => svc.remove_account(account_id),
    }
}

/// Read-only projection of native Antigravity vault state for `AccountsSnapshot`.
/// Returns `Ok(None)` if neither a vault nor a native login exists.
pub(crate) fn list_state_for_snapshot(
    home: &Path,
    state_dir: Option<&Path>,
) -> Option<AntigravityAccountState> {
    migrate_legacy_agy_profiles_once(home, state_dir);
    let path = vault_path(home, state_dir);
    let has_vault = path.is_file();
    if !has_vault {
        return None;
    }
    AntigravityAccountService::for_host(home, state_dir)
        .list_accounts()
        .ok()
}

/// Returns true if the native Antigravity vault currently has any saved account with `id`.
pub(crate) fn vault_has_account(home: &Path, state_dir: Option<&Path>, id: &str) -> bool {
    let path = vault_path(home, state_dir);
    if !path.is_file() {
        return false;
    }
    store::EncryptedAntigravityAccountStore::new(path)
        .read()
        .is_ok_and(|v| v.accounts.iter().any(|a| a.id == id))
}

fn evict_host_agy_caches(home: &Path) {
    let host_dir = super::host_default_dir(home, ProviderKind::Antigravity);
    crate::usage::direct::evict_profile(home, ProviderKind::Antigravity, &host_dir);
    super::login::evict_profile(ProviderKind::Antigravity, &host_dir);
}

/// `accounts.toml` 저장 시 활성 agy 계정의 자격증명을 호스트 기본 경로에 반영한다.
#[cfg(test)]
pub(crate) fn sync_active(home: &Path, file: &AccountsFile) {
    sync_account(
        home,
        &file.accounts,
        active_dir_for(file, ProviderKind::Antigravity),
    );
}

fn active_dir_for(file: &AccountsFile, provider: ProviderKind) -> Option<&Path> {
    file.active
        .get(provider.as_str())
        .and_then(|active_id| {
            file.accounts.iter().find(|a| {
                a.id == *active_id
                    && ProviderKind::parse(&a.provider) == Some(provider)
                    && !a.profile_dir.is_empty()
            })
        })
        .map(|a| Path::new(&a.profile_dir))
}

/// Legacy compatibility shim: if the active Antigravity account is in the native vault,
/// `select_account` already synced it; otherwise preserve legacy profile sync for tests/legacy dirs.
pub(crate) fn sync_active_from_disk(home: &Path, state_dir: Option<&Path>) {
    if home.as_os_str().is_empty() {
        return;
    }
    let path = vault_path(home, state_dir);
    if path.is_file() {
        return;
    }
    let host_token_path = antigravity_token_file_path(home);
    crate::usage::direct::agy::run_account_op(|| {
        let _sync_guard = crate::session::lock(&SYNC_LOCK);
        let file = super::load_file(home, state_dir);
        let target_dir = active_dir_for(&file, ProviderKind::Antigravity);
        if target_dir.is_some_and(|dir| is_host_target(home, &host_token_path, dir)) {
            return;
        }
        sync_account_gated(home, &host_token_path, &file.accounts, target_dir);
    });
}

/// Legacy compatibility shim for explicit profile dir sync.
pub(crate) fn sync_account_dir(home: &Path, state_dir: Option<&Path>, dir: &Path) {
    sync_provider_account_dir(home, state_dir, ProviderKind::Antigravity, dir);
}

/// Legacy compatibility shim for provider profile dir sync.
pub(crate) fn sync_provider_account_dir(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    dir: &Path,
) {
    if dir.as_os_str().is_empty() || provider != ProviderKind::Antigravity {
        return;
    }
    if home.as_os_str().is_empty() || vault_path(home, state_dir).is_file() {
        return;
    }
    let host_token_path = antigravity_token_file_path(home);
    if is_host_target(home, &host_token_path, dir) {
        return;
    }
    crate::usage::direct::agy::run_account_op(|| {
        let _sync_guard = crate::session::lock(&SYNC_LOCK);
        let file = super::load_file(home, state_dir);
        sync_account_gated(home, &host_token_path, &file.accounts, Some(dir));
    });
}

/// Serializes host reconciliation with subsequent usage check (`after`).
pub(crate) fn reconcile_host_to_profiles_then<T>(
    home: &Path,
    accounts: &[StoredAccount],
    after: impl FnOnce() -> T,
) -> T {
    if home.as_os_str().is_empty() {
        return after();
    }
    let host_token_path = antigravity_token_file_path(home);
    crate::usage::direct::agy::run_account_op(|| {
        let _sync_guard = crate::session::lock(&SYNC_LOCK);
        sync_account_gated(home, &host_token_path, accounts, None);
        after()
    })
}

fn is_host_target(home: &Path, host_token_path: &Path, dir: &Path) -> bool {
    dir == home || dir.join(".gemini/antigravity-cli/antigravity-oauth-token") == host_token_path
}

// ---------------------------------------------------------------------------
// Codex legacy host-auth migration (preserved unchanged)
// ---------------------------------------------------------------------------

const CODEX_MIGRATION_MARKER: &str = "codex-host-auth-migrated-v1";

pub(crate) fn migrate_codex_host_once(
    home: &Path,
    state_dir: Option<&Path>,
    accounts: &[StoredAccount],
) {
    if !backend::is_real_home(home) {
        return;
    }
    let marker = super::config_path(home, state_dir).with_file_name(CODEX_MIGRATION_MARKER);
    if marker.exists() {
        return;
    }
    preserve_codex_host(home, accounts);
    std::fs::write(&marker, b"").ok();
}

fn codex_account_id(raw: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()?
        .pointer("/tokens/account_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn preserve_codex_host(home: &Path, accounts: &[StoredAccount]) {
    let host_auth_path = home.join(".codex/auth.json");
    let Some(host_raw) = read_token_file(&host_auth_path) else {
        return;
    };
    let host_email = identity_from_credentials(ProviderKind::Codex, &host_raw).map(|i| i.email);
    let host_acc_id = codex_account_id(&host_raw);
    if host_email.is_none() && host_acc_id.is_none() {
        return;
    }
    for acc in accounts {
        if ProviderKind::parse(&acc.provider) != Some(ProviderKind::Codex)
            || acc.profile_dir.is_empty()
        {
            continue;
        }
        let prof_dir = Path::new(&acc.profile_dir);
        let prof_auth_path = prof_dir.join("auth.json");
        if prof_auth_path == host_auth_path {
            continue;
        }
        let Some(prof_raw) = read_token_file(&prof_auth_path) else {
            continue;
        };
        let prof_email = identity_from_credentials(ProviderKind::Codex, &prof_raw)
            .map(|i| i.email)
            .or_else(|| acc.email.clone());
        let same_identity = match (&host_email, &prof_email) {
            (Some(he), Some(pe)) => he.eq_ignore_ascii_case(pe),
            _ => host_acc_id.is_some() && host_acc_id == codex_account_id(&prof_raw),
        };
        if same_identity
            && prof_raw != host_raw
            && token_is_newer(
                ProviderKind::Codex,
                &host_raw,
                Some(&host_auth_path),
                &prof_raw,
                Some(&prof_auth_path),
                true,
            )
        {
            write_atomic_0600(&prof_auth_path, &host_raw).ok();
            crate::usage::direct::evict_profile(home, ProviderKind::Codex, prof_dir);
            super::login::evict_profile(ProviderKind::Codex, prof_dir);
        }
    }
}

#[cfg(test)]
fn sync_account(home: &Path, accounts: &[StoredAccount], target_dir: Option<&Path>) {
    if home.as_os_str().is_empty() {
        return;
    }
    let host_token_path = antigravity_token_file_path(home);
    if target_dir.is_some_and(|dir| is_host_target(home, &host_token_path, dir)) {
        return;
    }
    crate::usage::direct::agy::run_account_op(|| {
        let _sync_guard = crate::session::lock(&SYNC_LOCK);
        sync_account_gated(home, &host_token_path, accounts, target_dir);
    });
}

fn sync_account_gated(
    home: &Path,
    host_token_path: &Path,
    accounts: &[StoredAccount],
    target_dir: Option<&Path>,
) {
    let mut current_file = read_token_file(host_token_path);
    if current_file.is_some() {
        for acc in accounts {
            if ProviderKind::parse(&acc.provider) != Some(ProviderKind::Antigravity)
                || acc.profile_dir.is_empty()
            {
                continue;
            }
            let prof_dir = Path::new(&acc.profile_dir);
            if !prof_dir.is_dir() {
                continue;
            }
            let prof_token_path = prof_dir.join(".gemini/antigravity-cli/antigravity-oauth-token");
            let Some(prof_raw) = read_token_file(&prof_token_path) else {
                continue;
            };
            let Some(host_raw) = current_file.as_ref() else {
                break;
            };
            let Some(host_id) = identity_from_credentials(ProviderKind::Antigravity, host_raw)
            else {
                break;
            };
            let prof_email = identity_from_credentials(ProviderKind::Antigravity, &prof_raw)
                .map(|i| i.email)
                .or_else(|| acc.email.clone());
            let same_identity = prof_email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(&host_id.email));
            if !same_identity || &prof_raw == host_raw {
                continue;
            }
            let is_target = target_dir == Some(prof_dir);
            if token_is_newer(
                ProviderKind::Antigravity,
                host_raw,
                Some(host_token_path),
                &prof_raw,
                Some(&prof_token_path),
                is_target,
            ) {
                write_atomic_0600(&prof_token_path, host_raw).ok();
                crate::usage::direct::evict_profile(home, ProviderKind::Antigravity, prof_dir);
                super::login::evict_profile(ProviderKind::Antigravity, prof_dir);
            } else if (target_dir.is_none() || is_target)
                && token_is_newer(
                    ProviderKind::Antigravity,
                    &prof_raw,
                    Some(&prof_token_path),
                    host_raw,
                    Some(host_token_path),
                    false,
                )
            {
                write_atomic_0600(host_token_path, &prof_raw).ok();
                current_file = Some(prof_raw);
                evict_host_agy_caches(home);
            }
        }
    }
    let Some(target) = target_dir.filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let target_token_path = target.join(".gemini/antigravity-cli/antigravity-oauth-token");
    let mut host_changed = false;
    if let Some(target_raw) = read_token_file(&target_token_path) {
        if current_file.as_ref() != Some(&target_raw) {
            write_atomic_0600(host_token_path, &target_raw).ok();
            host_changed = true;
        }
    } else if std::fs::symlink_metadata(host_token_path).is_ok()
        && std::fs::remove_file(host_token_path).is_ok()
    {
        host_changed = true;
    }
    if host_changed {
        evict_host_agy_caches(home);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TokenStamp {
    issued_ns: i128,
    expiry_ns: i128,
}

fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i128::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

fn rfc3339_ns(s: &str) -> Option<i128> {
    let secs = i128::from(krill_proto::time::parse_rfc3339_epoch(s)?);
    let (_, rest) = s.split_once(['T', 't', ' '])?;
    let clock = rest
        .strip_suffix(['Z', 'z'])
        .or_else(|| rest.rsplit_once(['+', '-']).map(|(clock, _)| clock))?;
    let nanos = match clock.split_once(['.', ',']) {
        None => 0_i128,
        Some((_, frac)) => {
            let mut acc = 0_i128;
            let mut scale = 100_000_000_i128;
            for b in frac.bytes().take(9) {
                if !b.is_ascii_digit() {
                    return None;
                }
                acc += i128::from(b - b'0') * scale;
                scale /= 10;
            }
            acc
        }
    };
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

fn jwt_times_ns(jwt: &str) -> (Option<i128>, Option<i128>) {
    let Some(claims) = krill_proto::account_login::jwt_claims(jwt) else {
        return (None, None);
    };
    let ts = |key: &str| {
        claims
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .and_then(|v| i128::from(v).checked_mul(1_000_000_000))
    };
    (ts("iat"), ts("exp"))
}

fn codex_token_stamp(raw: &[u8]) -> Option<TokenStamp> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let last_refresh = v
        .get("last_refresh")
        .and_then(serde_json::Value::as_str)
        .and_then(rfc3339_ns);
    let (acc_iat, acc_exp) = v
        .pointer("/tokens/access_token")
        .and_then(serde_json::Value::as_str)
        .map_or((None, None), jwt_times_ns);
    let (id_iat, id_exp) = v
        .pointer("/tokens/id_token")
        .or_else(|| v.pointer("/tokens/idToken"))
        .and_then(serde_json::Value::as_str)
        .map_or((None, None), jwt_times_ns);
    let issued = [last_refresh, acc_iat, id_iat].into_iter().flatten().max();
    let expiry = [acc_exp, id_exp].into_iter().flatten().max();
    if issued.is_none() && expiry.is_none() {
        return None;
    }
    Some(TokenStamp {
        issued_ns: issued.unwrap_or(i128::MIN),
        expiry_ns: expiry.unwrap_or(i128::MIN),
    })
}

fn agy_token_stamp(raw: &[u8]) -> Option<TokenStamp> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let expiry_rfc = v
        .pointer("/token/expiry")
        .or_else(|| v.get("expiry"))
        .and_then(serde_json::Value::as_str)
        .and_then(rfc3339_ns);
    let (id_iat, id_exp) = v
        .pointer("/token/id_token")
        .or_else(|| v.get("id_token"))
        .and_then(serde_json::Value::as_str)
        .map_or((None, None), jwt_times_ns);
    let issued = id_iat;
    let expiry = [expiry_rfc, id_exp].into_iter().flatten().max();
    if issued.is_none() && expiry.is_none() {
        return None;
    }
    Some(TokenStamp {
        issued_ns: issued.unwrap_or(i128::MIN),
        expiry_ns: expiry.unwrap_or(i128::MIN),
    })
}

fn token_is_newer(
    provider: ProviderKind,
    cand_raw: &[u8],
    cand_path: Option<&Path>,
    curr_raw: &[u8],
    curr_path: Option<&Path>,
    strict_mtime: bool,
) -> bool {
    let stamp = match provider {
        ProviderKind::Codex => codex_token_stamp,
        ProviderKind::Antigravity => agy_token_stamp,
        _ => return false,
    };
    match (stamp(cand_raw), stamp(curr_raw)) {
        (Some(cs), Some(us)) => cs > us,
        (Some(cs), None) => cs.expiry_ns == i128::MIN || cs.expiry_ns > now_ns(),
        (None, Some(us)) => us.expiry_ns != i128::MIN && us.expiry_ns <= now_ns(),
        (None, None) => {
            let mtime = |p: Option<&Path>| std::fs::metadata(p?).ok()?.modified().ok();
            match (mtime(cand_path), mtime(curr_path)) {
                (Some(cm), Some(um)) => {
                    if strict_mtime {
                        cm > um
                    } else {
                        cm >= um
                    }
                }
                _ => !strict_mtime,
            }
        }
    }
}

fn read_token_file(path: &Path) -> Option<Zeroizing<Vec<u8>>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() == 0
        || meta.len() > MAX_TOKEN_BYTES
    {
        return None;
    }
    let bytes = Zeroizing::new(std::fs::read(path).ok()?);
    valid_token(&bytes).then_some(bytes)
}

fn valid_token(raw: &[u8]) -> bool {
    !raw.is_empty()
        && u64::try_from(raw.len()).is_ok_and(|len| len <= MAX_TOKEN_BYTES)
        && serde_json::from_slice::<serde_json::Value>(raw).is_ok_and(|v| v.is_object())
}

#[cfg(unix)]
pub(crate) fn run_command_with_timeout(
    cmd: &mut Command,
    stdin_bytes: Option<&[u8]>,
    capture_stdout: bool,
    max_stdout: u64,
    timeout: Duration,
) -> Option<(std::process::ExitStatus, Zeroizing<Vec<u8>>)> {
    cmd.stdin(if stdin_bytes.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let reader = if capture_stdout {
        let stdout = child.stdout.take()?;
        let handle = std::thread::Builder::new()
            .name("keychain-read".into())
            .spawn(move || {
                let mut buf = Zeroizing::new(Vec::new());
                let limit = max_stdout.saturating_add(1);
                stdout.take(limit).read_to_end(&mut buf).ok()?;
                (u64::try_from(buf.len()).ok()? <= max_stdout).then_some(buf)
            })
            .ok();
        let Some(handle) = handle else {
            child.kill().ok();
            child.wait().ok();
            return None;
        };
        Some(handle)
    } else {
        None
    };
    if let Some(bytes) = stdin_bytes {
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(bytes).is_ok());
        if !wrote {
            child.kill().ok();
            child.wait().ok();
            if let Some(handle) = reader {
                drop(handle.join());
            }
            return None;
        }
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
        }
    };
    let stdout = match reader {
        Some(handle) => handle.join().ok().flatten(),
        None => Some(Zeroizing::new(Vec::new())),
    };
    let status = status?;
    let stdout = stdout?;
    Some((status, stdout))
}


#[cfg(test)]
mod tests;
