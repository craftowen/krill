// Adapted from Orca:
// - `src/main/antigravity/native-account-service.ts`
// - `src/main/antigravity/native-account-rewritten-login.ts`
// - `src/main/antigravity/google-userinfo-identity.ts`
// - `src/main/antigravity/native-account-usage-reading.ts`
// - `src/main/antigravity/native-account-launch.ts`
// - `src/main/antigravity/native-account-host.ts`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Native Antigravity (`agy`) account service: list, save current, select, remove,
//! launch preflight verification, and switch-fenced usage attribution.

use std::collections::BTreeMap;
use std::path::Path;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::usage::{
    AntigravityAccountState, AntigravityCurrentAccount, AntigravityStoredUsage,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use zeroize::Zeroizing;

use super::backend::{AntigravityCredentialBackend, HostCredentialBackend};
#[cfg(not(target_os = "windows"))]
use super::backend::{current_process_env, is_antigravity_file_storage_host};
use super::codec::{
    AntigravityCredentialIdentity, AntigravityNativeCredential,
    parse_antigravity_native_credential, read_google_identity_claims,
};
use super::store::{
    AntigravityAccountStore, AntigravityAccountVault, EncryptedAntigravityAccountStore,
    StoredAntigravityAccount, VaultError, vault_path,
};
use crate::bots::http_client::{HttpMethod, HttpRequest, SsrfPolicy};
use crate::session::lock;

const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const USERINFO_REJECTED: &str = "Google rejected the current Antigravity credential. Run agy once to refresh it, then save again.";
const USERINFO_UNCONFIRMED: &str =
    "Google did not confirm the signed-in Antigravity account; nothing was saved.";
const USERINFO_UNREACHABLE: &str =
    "Could not reach Google to identify the Antigravity account; nothing was saved.";

pub(crate) type IdentityLookupFn =
    Box<dyn Fn(&str) -> Result<AntigravityCredentialIdentity, String> + Send + Sync>;

/// Mirrors Orca `fetchGoogleIdentity(accessToken)` (`google-userinfo-identity.ts`).
pub(crate) fn fetch_google_identity(
    access_token: &str,
) -> Result<AntigravityCredentialIdentity, String> {
    if cfg!(test)
        || std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0")
    {
        return Err(USERINFO_UNREACHABLE.to_owned());
    }
    let mut req = HttpRequest::new(HttpMethod::Get, USERINFO_URL);
    req.timeout = Duration::from_secs(10);
    req.max_response_bytes = 16 * 1024;
    req.ssrf_policy = SsrfPolicy::StrictExternal;
    req.headers
        .push(("Authorization".to_owned(), format!("Bearer {access_token}")));
    let response =
        crate::bots::http_client::send(&req).map_err(|_| USERINFO_UNREACHABLE.to_owned())?;
    if !response.is_success() {
        return Err(if response.status == 401 {
            USERINFO_REJECTED.to_owned()
        } else {
            USERINFO_UNCONFIRMED.to_owned()
        });
    }
    let claims: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|_| USERINFO_UNCONFIRMED.to_owned())?;
    read_google_identity_claims(&claims).ok_or_else(|| USERINFO_UNCONFIRMED.to_owned())
}

/// Stable login fingerprint across `agy` token refreshes (`native-account-usage-reading.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AntigravityUsageLogin {
    pub key: String,
    pub account_id: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AntigravityUsageReadStart {
    pub login: Option<AntigravityUsageLogin>,
}

#[derive(Debug, Clone)]
pub(crate) enum AntigravityUsageReadFinish {
    Switched,
    Committed {
        account_id: Option<String>,
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "inspected by usage-reading attribution tests")
        )]
        email: Option<String>,
    },
}

/// `agy`'s token refresh drops the `id_token` but keeps the grant's `refresh_token`.
#[cfg(test)]
pub(crate) fn is_same_antigravity_login(
    left: &AntigravityNativeCredential,
    right: &AntigravityNativeCredential,
) -> bool {
    if left.auth_method != right.auth_method {
        return false;
    }
    if let (Some(li), Some(ri)) = (&left.identity, &right.identity) {
        return li.subject == ri.subject;
    }
    if let (Some(lr), Some(rr)) = (&left.refresh_token, &right.refresh_token) {
        return lr.as_str() == rr.as_str();
    }
    left.contents.as_str() == right.contents.as_str()
}

fn account_matches(
    account: &StoredAntigravityAccount,
    current: &AntigravityNativeCredential,
) -> bool {
    if let (Some(identity), Some(subject)) = (&current.identity, account.subject.as_deref()) {
        return subject == identity.subject && account.auth_method == current.auth_method;
    }
    // agy's token refresh stores no id_token but keeps the grant's refresh token.
    if let Some(current_refresh) = current.refresh_token.as_deref()
        && account.auth_method == current.auth_method
        && parse_antigravity_native_credential(&account.credentials)
            .ok()
            .and_then(|c| c.refresh_token)
            .as_deref()
            == Some(current_refresh)
    {
        return true;
    }
    account.credentials.as_str() == current.contents.as_str()
}

fn antigravity_usage_login(
    account: Option<&StoredAntigravityAccount>,
    current: &AntigravityNativeCredential,
) -> AntigravityUsageLogin {
    let subject = current
        .identity
        .as_ref()
        .map(|i| i.subject.as_str())
        .or_else(|| account.and_then(|a| a.subject.as_deref()));
    let key_part = subject
        .or(current.refresh_token.as_deref().map(String::as_str))
        .unwrap_or(current.contents.as_str());
    AntigravityUsageLogin {
        key: format!("{}\n{key_part}", current.auth_method),
        account_id: account.map(|a| a.id.clone()),
        email: current
            .identity
            .as_ref()
            .and_then(|i| i.email.clone())
            .or_else(|| account.and_then(|a| a.email.clone()))
            .or_else(|| current.email_hint.clone()),
    }
}

/// Caches one Google userinfo lookup per `access_token` (`native-account-rewritten-login.ts`).
struct RewrittenLoginCache {
    access_token: Zeroizing<String>,
    identity: Option<AntigravityCredentialIdentity>,
}

enum ReconcileError {
    LegacyVaultLocked { status: i32 },
    Other(String),
}

impl ReconcileError {
    fn into_message(self) -> String {
        match self {
            Self::LegacyVaultLocked { status } => {
                VaultError::LegacyAclLocked { status }.into_message()
            }
            Self::Other(msg) => msg,
        }
    }
}

impl From<VaultError> for ReconcileError {
    fn from(err: VaultError) -> Self {
        match err {
            VaultError::LegacyAclLocked { status } => Self::LegacyVaultLocked { status },
            VaultError::Other(msg) => Self::Other(msg),
        }
    }
}

impl From<String> for ReconcileError {
    fn from(msg: String) -> Self {
        Self::Other(msg)
    }
}

/// Mirrors Orca's `AntigravityAccountService` 1:1.
pub(crate) struct AntigravityAccountService {
    store: Box<dyn AntigravityAccountStore>,
    backend: Box<dyn AntigravityCredentialBackend>,
    now_ms: Box<dyn Fn() -> u64 + Send + Sync>,
    lookup_identity: IdentityLookupFn,
    op_lock: Mutex<()>,
    rewritten_cache: Mutex<Option<RewrittenLoginCache>>,
}

impl AntigravityAccountService {
    pub(crate) fn new(
        store: Box<dyn AntigravityAccountStore>,
        backend: Box<dyn AntigravityCredentialBackend>,
        now_ms: Box<dyn Fn() -> u64 + Send + Sync>,
        lookup_identity: IdentityLookupFn,
    ) -> Self {
        Self {
            store,
            backend,
            now_ms,
            lookup_identity,
            op_lock: Mutex::new(()),
            rewritten_cache: Mutex::new(None),
        }
    }

    pub(crate) fn for_host(home: &Path, state_dir: Option<&Path>) -> Self {
        let path = vault_path(home, state_dir);
        Self::new(
            Box::new(EncryptedAntigravityAccountStore::new(path)),
            Box::new(HostCredentialBackend::new(home)),
            Box::new(default_now_ms),
            Box::new(fetch_google_identity),
        )
    }

    pub(crate) fn list_accounts(&self) -> Result<AntigravityAccountState, String> {
        let _guard = lock(&self.op_lock);
        match self.reconcile(None) {
            Ok((vault, current)) => Ok(Self::build_state(&vault, current.as_ref(), false)),
            Err(ReconcileError::LegacyVaultLocked { status }) => {
                tracing::warn!(
                    status,
                    "Antigravity vault is locked by a previous macOS ad-hoc Keychain ACL; returning host login with vault_locked=true"
                );
                let current = self.backend.read()?;
                Ok(Self::build_state(
                    &AntigravityAccountVault::default(),
                    current.as_ref(),
                    true,
                ))
            }
            Err(ReconcileError::Other(msg)) => Err(msg),
        }
    }

    pub(crate) fn reset_locked_vault(&self) -> Result<AntigravityAccountState, String> {
        let _guard = lock(&self.op_lock);
        // Never wipe a healthy, already-readable vault.
        if self.store.read().is_ok() {
            let (vault, current) = self.reconcile(None).map_err(ReconcileError::into_message)?;
            return Ok(Self::build_state(&vault, current.as_ref(), false));
        }
        self.store.reset_locked_vault()?;
        let current = self.backend.read()?;
        let mut vault = self.store.read().map_err(VaultError::into_message)?;
        let Some(current) = current else {
            return Ok(Self::build_state(&vault, None, false));
        };
        let identity = if let Some(id) = current.identity.clone() {
            Some(id)
        } else if let Ok(id) = (self.lookup_identity)(&current.access_token) {
            let reread = self.backend.read()?;
            if reread.as_ref().map(|c| c.contents.as_str()) == Some(current.contents.as_str()) {
                Some(id)
            } else {
                None
            }
        } else {
            tracing::warn!(
                "Antigravity locked vault was reset, but Google userinfo lookup failed; leaving vault unlocked for manual save"
            );
            None
        };
        if let Some(identity) = identity {
            let mut identified = current;
            identified.identity = Some(identity.clone());
            let timestamp = (self.now_ms)();
            vault.accounts.push(StoredAntigravityAccount {
                id: generate_uuid_v4(),
                email: identity.email.or_else(|| identified.email_hint.clone()),
                subject: Some(identity.subject),
                auth_method: identified.auth_method.clone(),
                created_at: timestamp,
                updated_at: timestamp,
                credentials: identified.contents.clone(),
                usage: None,
            });
            self.store.write(&vault)?;
            return Ok(Self::build_state(&vault, Some(&identified), false));
        }
        Ok(Self::build_state(&vault, Some(&current), false))
    }

    pub(crate) fn add_current_account(&self) -> Result<AntigravityAccountState, String> {
        let _guard = lock(&self.op_lock);
        let (vault, current) = self.reconcile(None).map_err(ReconcileError::into_message)?;
        let Some(current) = current else {
            return Err(
                "Sign in with agy on this execution host, then save the current account."
                    .to_owned(),
            );
        };
        if vault
            .accounts
            .iter()
            .any(|entry| account_matches(entry, &current))
        {
            return Ok(Self::build_state(&vault, Some(&current), false));
        }
        let identity = if let Some(id) = current.identity.clone() {
            id
        } else {
            let id = (self.lookup_identity)(&current.access_token)?;
            let reread = self.backend.read()?;
            if reread.as_ref().map(|c| c.contents.as_str()) != Some(current.contents.as_str()) {
                return Err(
                    "The native Antigravity account changed. Save the current account again."
                        .to_owned(),
                );
            }
            id
        };
        let mut identified = current;
        identified.identity = Some(identity.clone());
        let mut latest = self.store.read().map_err(VaultError::into_message)?;
        if !latest
            .accounts
            .iter()
            .any(|entry| account_matches(entry, &identified))
        {
            let timestamp = (self.now_ms)();
            latest.accounts.push(StoredAntigravityAccount {
                id: generate_uuid_v4(),
                email: identity.email.or_else(|| identified.email_hint.clone()),
                subject: Some(identity.subject),
                auth_method: identified.auth_method.clone(),
                created_at: timestamp,
                updated_at: timestamp,
                credentials: identified.contents.clone(),
                usage: None,
            });
        }
        self.update_snapshot(&mut latest, &identified);
        self.store.write(&latest)?;
        Ok(Self::build_state(&latest, Some(&identified), false))
    }

    pub(crate) fn select_account(&self, id: &str) -> Result<AntigravityAccountState, String> {
        crate::usage::direct::agy::run_account_op(|| {
            let _guard = lock(&self.op_lock);
            let (vault, current) = self
                .reconcile(Some(id))
                .map_err(ReconcileError::into_message)?;
            let selected = vault
                .accounts
                .iter()
                .find(|a| a.id == id)
                .ok_or_else(|| "Antigravity account was not found.".to_owned())?;
            if current
                .as_ref()
                .is_none_or(|curr| !account_matches(selected, curr))
            {
                self.backend.write(
                    &selected.credentials,
                    current.as_ref().map(|c| c.contents.as_str()),
                )?;
            }
            let readback = self.backend.read()?;
            let Some(readback) = readback.filter(|rb| account_matches(selected, rb)) else {
                return Err(
                    "Antigravity account switching could not be verified; refresh before retrying."
                        .to_owned(),
                );
            };
            let mut latest = self.store.read().map_err(VaultError::into_message)?;
            if !latest
                .accounts
                .iter()
                .any(|a| a.id == id && account_matches(a, &readback))
            {
                return Err(
                    "Antigravity snapshots changed during selection; refresh before retrying."
                        .to_owned(),
                );
            }
            latest.selected_account_id = Some(id.to_owned());
            self.update_snapshot(&mut latest, &readback);
            self.store.write(&latest)?;
            Ok(Self::build_state(&latest, Some(&readback), false))
        })
    }

    pub(crate) fn remove_account(&self, id: &str) -> Result<AntigravityAccountState, String> {
        let _guard = lock(&self.op_lock);
        let (mut vault, current) = self.reconcile(None).map_err(ReconcileError::into_message)?;
        let account = vault
            .accounts
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| "Antigravity account was not found.".to_owned())?;
        if current
            .as_ref()
            .is_some_and(|curr| account_matches(account, curr))
        {
            return Err(
                "Switch agy to another account before removing the one it is signed in to."
                    .to_owned(),
            );
        }
        vault.accounts.retain(|entry| entry.id != id);
        if vault.selected_account_id.as_deref() == Some(id) {
            vault.selected_account_id = None;
        }
        self.store.write(&vault)?;
        Ok(Self::build_state(&vault, current.as_ref(), false))
    }

    #[cfg(any(not(target_os = "windows"), test))]
    pub(crate) fn prepare_for_launch(&self) -> Result<(), String> {
        let _guard = lock(&self.op_lock);
        let (vault, current) = match self.reconcile(None) {
            Ok(pair) => pair,
            Err(ReconcileError::LegacyVaultLocked { status }) => {
                tracing::warn!(
                    status,
                    "skipping Antigravity account launch check because legacy vault is ACL-locked"
                );
                return Ok(());
            }
            Err(ReconcileError::Other(msg)) => return Err(msg),
        };
        let Some(selected_id) = vault.selected_account_id.as_deref() else {
            return Ok(());
        };
        let selected = vault.accounts.iter().find(|a| a.id == selected_id);
        let matches = selected
            .zip(current.as_ref())
            .is_some_and(|(sel, curr)| account_matches(sel, curr));
        if !matches {
            return Err("The native Antigravity account changed. Select the account again in Accounts before launching agy.".to_owned());
        }
        Ok(())
    }

    pub(crate) fn begin_usage_read(&self) -> Result<Option<AntigravityUsageReadStart>, String> {
        let _guard = lock(&self.op_lock);
        match self.store.read() {
            Ok(vault) if vault.accounts.is_empty() => return Ok(None),
            Ok(_) => {}
            Err(VaultError::LegacyAclLocked { status }) => {
                tracing::warn!(
                    status,
                    "skipping Antigravity vault usage attribution because legacy vault is ACL-locked"
                );
                return Ok(None);
            }
            Err(err) => return Err(err.into_message()),
        }
        let (vault, current) = match self.reconcile(None) {
            Ok(pair) => pair,
            Err(ReconcileError::LegacyVaultLocked { .. }) => return Ok(None),
            Err(ReconcileError::Other(msg)) => return Err(msg),
        };
        let login = current.as_ref().map(|curr| {
            let acc = vault.accounts.iter().find(|e| account_matches(e, curr));
            antigravity_usage_login(acc, curr)
        });
        Ok(Some(AntigravityUsageReadStart { login }))
    }

    pub(crate) fn finish_usage_read(
        &self,
        login: &AntigravityUsageLogin,
        reading: Option<AntigravityStoredUsage>,
    ) -> Result<AntigravityUsageReadFinish, String> {
        let _guard = lock(&self.op_lock);
        let (mut vault, current) = self.reconcile(None).map_err(ReconcileError::into_message)?;
        let now_login = current.as_ref().map(|curr| {
            let acc = vault.accounts.iter().find(|e| account_matches(e, curr));
            antigravity_usage_login(acc, curr)
        });
        let Some(now_login) = now_login.filter(|nl| nl.key == login.key) else {
            return Ok(AntigravityUsageReadFinish::Switched);
        };
        if let Some(usage) = reading
            && now_login.account_id == login.account_id
            && let Some(acc_id) = now_login.account_id.as_deref()
            && let Some(account) = vault.accounts.iter_mut().find(|e| e.id == acc_id)
        {
            account.usage = Some(usage);
            self.store.write(&vault)?;
        }
        Ok(AntigravityUsageReadFinish::Committed {
            account_id: now_login.account_id,
            email: now_login.email,
        })
    }

    /// Upsert pre-parsed snapshots (used by Orca vault import and legacy Krill profile migration).
    pub(crate) fn import_snapshots(
        &self,
        imported: Vec<StoredAntigravityAccount>,
        selected_id: Option<&str>,
    ) -> Result<AntigravityAccountState, String> {
        let _guard = lock(&self.op_lock);
        let mut vault = self.store.read().map_err(VaultError::into_message)?;
        for item in imported {
            let parsed = parse_antigravity_native_credential(&item.credentials)?;
            if let Some(existing) = vault
                .accounts
                .iter_mut()
                .find(|a| a.id == item.id || account_matches(a, &parsed))
            {
                if existing.email.is_none() {
                    existing.email = item.email;
                }
                if existing.subject.is_none() {
                    existing.subject = item.subject;
                }
                existing.credentials = item.credentials;
                existing.updated_at = item.updated_at;
            } else {
                vault.accounts.push(item);
            }
        }
        if let Some(sel) = selected_id
            && vault.accounts.iter().any(|a| a.id == sel)
        {
            vault.selected_account_id = Some(sel.to_owned());
        }
        self.store.write(&vault)?;
        let current = self.backend.read().ok().flatten();
        Ok(Self::build_state(&vault, current.as_ref(), false))
    }

    fn identify_rewritten_login(
        &self,
        current: Option<AntigravityNativeCredential>,
        accounts: &[StoredAntigravityAccount],
    ) -> Option<AntigravityNativeCredential> {
        let mut current = current?;
        if current.identity.is_some()
            || !accounts.iter().any(|a| a.subject.is_some())
            || accounts.iter().any(|a| account_matches(a, &current))
        {
            return Some(current);
        }
        let mut cache = lock(&self.rewritten_cache);
        let identity = if let Some(cached) = cache
            .as_ref()
            .filter(|c| c.access_token.as_str() == current.access_token.as_str())
        {
            cached.identity.clone()
        } else {
            let looked = (self.lookup_identity)(&current.access_token).ok();
            *cache = Some(RewrittenLoginCache {
                access_token: current.access_token.clone(),
                identity: looked.clone(),
            });
            looked
        };
        if let Some(id) = identity {
            current.identity = Some(id);
        }
        Some(current)
    }

    fn reconcile(
        &self,
        selected_id: Option<&str>,
    ) -> Result<(AntigravityAccountVault, Option<AntigravityNativeCredential>), ReconcileError>
    {
        let initial_accounts = self.store.read()?.accounts;
        let raw_current = self.backend.read()?;
        let current = self.identify_rewritten_login(raw_current, &initial_accounts);
        // Re-read after native I/O so a delayed read never restores an older vault.
        let mut vault = self.store.read()?;
        if let Some(sel_id) = selected_id {
            let selected = vault
                .accounts
                .iter()
                .find(|a| a.id == sel_id)
                .ok_or_else(|| "Antigravity account was not found.".to_owned())?;
            parse_antigravity_native_credential(&selected.credentials)?;
        }
        if let Some(curr) = current.as_ref()
            && self.update_snapshot(&mut vault, curr)
        {
            self.store.write(&vault)?;
        }
        Ok((vault, current))
    }

    fn update_snapshot(
        &self,
        vault: &mut AntigravityAccountVault,
        current: &AntigravityNativeCredential,
    ) -> bool {
        let Some(account) = vault
            .accounts
            .iter_mut()
            .find(|entry| account_matches(entry, current))
        else {
            return false;
        };
        if account.credentials.as_str() == current.contents.as_str() {
            return false;
        }
        account.credentials.clone_from(&current.contents);
        if account.subject.is_none()
            && let Some(identity) = current.identity.as_ref()
        {
            account.subject = Some(identity.subject.clone());
        }
        if let Some(email) = current
            .identity
            .as_ref()
            .and_then(|i| i.email.clone())
            .or_else(|| current.email_hint.clone())
        {
            account.email = Some(email);
        }
        account.updated_at = (self.now_ms)();
        true
    }

    fn build_state(
        vault: &AntigravityAccountVault,
        current: Option<&AntigravityNativeCredential>,
        vault_locked: bool,
    ) -> AntigravityAccountState {
        let active =
            current.and_then(|curr| vault.accounts.iter().find(|e| account_matches(e, curr)));
        let subject = current
            .and_then(|c| c.identity.as_ref().map(|i| i.subject.clone()))
            .or_else(|| active.and_then(|a| a.subject.clone()));
        AntigravityAccountState {
            accounts: vault
                .accounts
                .iter()
                .map(StoredAntigravityAccount::to_summary)
                .collect(),
            active_account_id: active.map(|a| a.id.clone()),
            selected_account_id: vault.selected_account_id.clone(),
            current_account: current.map(|c| {
                let email = c
                    .identity
                    .as_ref()
                    .and_then(|i| i.email.clone())
                    .or_else(|| active.and_then(|a| a.email.clone()))
                    .or_else(|| c.email_hint.clone());
                let identity_known = subject.is_some();
                AntigravityCurrentAccount {
                    email,
                    subject: subject.clone(),
                    auth_method: c.auth_method.clone(),
                    identity_known,
                }
            }),
            vault_locked,
        }
    }
}

/// Mirrors Orca `prepareAntigravityAccountForLaunch` (`native-account-launch.ts`).
#[cfg_attr(
    target_os = "windows",
    expect(clippy::unnecessary_wraps, reason = "matches Unix Result signature")
)]
pub(crate) fn prepare_antigravity_account_for_launch(
    home: &Path,
    state_dir: Option<&Path>,
    launch_env: &BTreeMap<String, String>,
) -> Result<(), String> {
    // Windows account selection applies the host default; the shell and agent own each launch.
    #[cfg(target_os = "windows")]
    {
        let _ = (home, state_dir, launch_env);
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let path = vault_path(home, state_dir);
        if !path.is_file() {
            return Ok(());
        }
        let store = EncryptedAntigravityAccountStore::new(path);
        let vault = match store.read() {
            Ok(v) => v,
            Err(VaultError::LegacyAclLocked { status }) => {
                tracing::warn!(
                    status,
                    "skipping Antigravity launch preflight because legacy vault is ACL-locked"
                );
                return Ok(());
            }
            Err(err) => return Err(err.into_message()),
        };
        if vault.selected_account_id.is_none() {
            return Ok(());
        }
        let env_home = launch_env
            .get("HOME")
            .or_else(|| launch_env.get("USERPROFILE"))
            .map(PathBuf::from);
        let host_env = current_process_env();
        let home_mismatch = env_home.is_some_and(|eh| {
            let ca = std::fs::canonicalize(&eh).unwrap_or(eh);
            let cb = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            ca != cb
        });
        if home_mismatch
            || is_antigravity_file_storage_host(launch_env)
                != is_antigravity_file_storage_host(&host_env)
        {
            return Err("This agy launch uses a different credential authority from the selected Antigravity account.".to_owned());
        }
        AntigravityAccountService::for_host(home, state_dir).prepare_for_launch()
    }
}

/// Safe one-time migration of legacy Krill `accounts.toml` `agy` profiles whose token file still
/// exists on disk into the encrypted native vault, followed by pruning obsolete isolated `agy`
/// rows from `accounts.toml` (never modifying Claude/Codex/OpenCode entries or legacy profile
/// directories on disk).
pub(crate) fn migrate_legacy_agy_profiles_once(home: &Path, state_dir: Option<&Path>) {
    migrate_and_prune_legacy_agy_profiles(home, state_dir, !cfg!(test));
}

pub(crate) fn migrate_and_prune_legacy_agy_profiles(
    home: &Path,
    state_dir: Option<&Path>,
    prune_accounts_toml: bool,
) {
    let vault_file = vault_path(home, state_dir);
    let Some(vault_dir) = vault_file.parent() else {
        return;
    };
    let migrated_marker = vault_dir.join(".legacy-migrated-v1");
    let pruned_marker = vault_dir.join(".legacy-pruned-v2");
    if migrated_marker.exists() && (!prune_accounts_toml || pruned_marker.exists()) {
        return;
    }
    let file = crate::usage::accounts::load_file(home, state_dir);
    let legacy_agy: Vec<_> = file
        .accounts
        .iter()
        .filter(|a| {
            krill_proto::usage::ProviderKind::parse(&a.provider)
                == Some(krill_proto::usage::ProviderKind::Antigravity)
        })
        .collect();
    if legacy_agy.is_empty() {
        if prune_accounts_toml && vault_dir.is_dir() {
            std::fs::write(&pruned_marker, b"1").ok();
        }
        return;
    }
    if !migrated_marker.exists() {
        let active_id = file.active.get("agy").map(String::as_str);
        let now = default_now_ms();
        let mut imported = Vec::new();
        let mut selected = None;
        for acc in &legacy_agy {
            if acc.profile_dir.is_empty() {
                continue;
            }
            let token_path = Path::new(&acc.profile_dir)
                .join(".gemini")
                .join("antigravity-cli")
                .join("antigravity-oauth-token");
            let Ok(Some(raw)) = super::backend::read_antigravity_credential_file(&token_path)
            else {
                continue;
            };
            let Ok(parsed) = parse_antigravity_native_credential(&raw) else {
                continue;
            };
            let email = parsed
                .identity
                .as_ref()
                .and_then(|i| i.email.clone())
                .or_else(|| parsed.email_hint.clone())
                .or_else(|| acc.email.clone());
            let subject = parsed.identity.as_ref().map(|i| i.subject.clone());
            if active_id == Some(acc.id.as_str()) {
                selected = Some(acc.id.clone());
            }
            imported.push(StoredAntigravityAccount {
                id: acc.id.clone(),
                email,
                subject,
                auth_method: parsed.auth_method,
                created_at: now,
                updated_at: now,
                credentials: parsed.contents,
                usage: None,
            });
        }
        if !imported.is_empty() {
            let svc = AntigravityAccountService::for_host(home, state_dir);
            if svc.import_snapshots(imported, selected.as_deref()).is_ok() {
                std::fs::write(&migrated_marker, b"1").ok();
            } else {
                // Keep legacy accounts.toml rows intact if vault import failed.
                return;
            }
        } else if vault_dir.is_dir() {
            std::fs::write(&migrated_marker, b"1").ok();
        }
    }
    if prune_accounts_toml && !pruned_marker.exists() {
        match crate::usage::accounts::prune_legacy_agy_accounts(home, state_dir) {
            Ok(_) => {
                #[cfg(unix)]
                {
                    use krill_proto::os::fs::DirBuilderExt as _;
                    std::fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(vault_dir)
                        .ok();
                }
                #[cfg(not(unix))]
                std::fs::create_dir_all(vault_dir).ok();
                std::fs::write(&migrated_marker, b"1").ok();
                std::fs::write(&pruned_marker, b"1").ok();
            }
            Err(err) => {
                tracing::warn!(
                    kind = ?err.kind(),
                    "skipped pruning legacy agy rows from accounts.toml"
                );
            }
        }
    }
}

pub(crate) fn default_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn generate_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0_u128, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}
