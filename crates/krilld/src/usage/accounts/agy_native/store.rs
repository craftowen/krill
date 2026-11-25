// Adapted from Orca `src/main/antigravity/native-account-store.ts`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Encrypted Antigravity (`agy`) account vault (`<state_dir>/antigravity-accounts/vault`, mode `0o600`).
//!
//! Master key protection mirrors `crate::bots::secrets`:
//! - macOS (non-test): OS Keychain (`com.krill.antigravity-accounts-key.v1`) with UI disabled.
//! - Windows (non-test): DPAPI current-user sealed key file (`crate::sys::protect`/`unprotect`).
//! - Linux / tests: `0o600` key file in `<vault_dir>/vault.key`.
//!
//! Payloads are authenticated with AES-256-GCM + per-write random 256-bit HKDF salt + 96-bit nonce.

use std::collections::BTreeSet;
use std::fs;
use std::io;
#[cfg(all(target_os = "macos", not(test)))]
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::usage::{AntigravityAccountSummary, AntigravityStoredUsage};
use ring::{
    aead, hkdf,
    rand::{SecureRandom as _, SystemRandom},
};
#[cfg(all(target_os = "macos", not(test)))]
use security_framework::os::macos::keychain::SecKeychain;
use serde::{Deserialize, Serialize};
#[cfg(all(target_os = "macos", not(test)))]
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::codec::parse_antigravity_native_credential;
use crate::session::lock;
use crate::usage::accounts::write_atomic_0600;

const MAGIC: &[u8; 4] = b"KAV1";
const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
pub(crate) const MAX_VAULT_BYTES: usize = 4 * 1024 * 1024;
const VAULT_DIR_NAME: &str = "antigravity-accounts";
const VAULT_FILE_NAME: &str = "vault";
const READ_ERR: &str =
    "Antigravity account snapshots could not be read; the existing vault was preserved.";
const WRITE_ERR: &str = "Antigravity account snapshots could not be saved.";

#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_SERVICE: &str = "com.krill.antigravity-accounts-key.v1";
#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_NOT_FOUND: i32 = -25300;
#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_DUPLICATE: i32 = -25299;
#[cfg(all(target_os = "macos", not(test)))]
static KEYCHAIN_GATE: Mutex<()> = Mutex::new(());

#[cfg(any(test, target_os = "linux", windows))]
const KEY_FILE: &str = "vault.key";
#[cfg(any(test, target_os = "linux", windows))]
static KEY_FILE_LOCK: Mutex<()> = Mutex::new(());

static VAULT_IO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredAntigravityAccount {
    pub id: String,
    pub email: Option<String>,
    pub subject: Option<String>,
    pub auth_method: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(with = "zeroizing_string")]
    pub credentials: Zeroizing<String>,
    #[serde(default)]
    pub usage: Option<AntigravityStoredUsage>,
}

impl std::fmt::Debug for StoredAntigravityAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredAntigravityAccount")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("subject", &self.subject)
            .field("auth_method", &self.auth_method)
            .finish_non_exhaustive()
    }
}

impl StoredAntigravityAccount {
    pub(crate) fn to_summary(&self) -> AntigravityAccountSummary {
        AntigravityAccountSummary {
            id: self.id.clone(),
            email: self.email.clone(),
            subject: self.subject.clone(),
            auth_method: self.auth_method.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            usage: self.usage.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AntigravityAccountVault {
    #[serde(default)]
    pub accounts: Vec<StoredAntigravityAccount>,
    #[serde(default)]
    pub selected_account_id: Option<String>,
}

pub(crate) trait AntigravityAccountStore: Send + Sync {
    fn read(&self) -> Result<AntigravityAccountVault, String>;
    fn write(&self, vault: &AntigravityAccountVault) -> Result<(), String>;
}

pub(crate) fn vault_path(home: &Path, state_dir: Option<&Path>) -> PathBuf {
    crate::usage::accounts::config_path(home, state_dir)
        .with_file_name(VAULT_DIR_NAME)
        .join(VAULT_FILE_NAME)
}

pub(crate) struct EncryptedAntigravityAccountStore {
    path: PathBuf,
}

impl EncryptedAntigravityAccountStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl AntigravityAccountStore for EncryptedAntigravityAccountStore {
    fn read(&self) -> Result<AntigravityAccountVault, String> {
        let _guard = lock(&VAULT_IO_LOCK);
        let meta = match fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(AntigravityAccountVault::default());
            }
            Err(_) => return Err(READ_ERR.to_owned()),
        };
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() > u64::try_from(MAX_VAULT_BYTES).unwrap_or(u64::MAX)
        {
            return Err(READ_ERR.to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(READ_ERR.to_owned());
            }
        }
        let dir = self.path.parent().ok_or_else(|| READ_ERR.to_owned())?;
        let key = MasterKey::load_or_create(dir, false).map_err(|_| {
            "Protected secret storage is unavailable; Antigravity accounts were not changed."
                .to_owned()
        })?;
        let sealed = fs::read(&self.path).map_err(|_| READ_ERR.to_owned())?;
        let plaintext = key.decrypt(&sealed).map_err(|_| READ_ERR.to_owned())?;
        let vault = parse_and_validate_vault(&plaintext).map_err(|_| READ_ERR.to_owned())?;
        Ok(vault)
    }

    fn write(&self, vault: &AntigravityAccountVault) -> Result<(), String> {
        let _guard = lock(&VAULT_IO_LOCK);
        validate_vault(vault).map_err(|_| WRITE_ERR.to_owned())?;
        let dir = self.path.parent().ok_or_else(|| WRITE_ERR.to_owned())?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|_| WRITE_ERR.to_owned())?;
        let key = MasterKey::load_or_create(dir, true).map_err(|_| {
            "Protected secret storage is unavailable; Antigravity accounts were not changed."
                .to_owned()
        })?;
        let json = Zeroizing::new(serde_json::to_vec(vault).map_err(|_| WRITE_ERR.to_owned())?);
        let encrypted = key.encrypt(&json).map_err(|_| WRITE_ERR.to_owned())?;
        if encrypted.len() > MAX_VAULT_BYTES {
            return Err(WRITE_ERR.to_owned());
        }
        write_atomic_0600(&self.path, &encrypted).map_err(|_| WRITE_ERR.to_owned())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&self.path)
                .map_err(|_| WRITE_ERR.to_owned())?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                return Err(WRITE_ERR.to_owned());
            }
        }
        Ok(())
    }
}

fn parse_and_validate_vault(plaintext: &[u8]) -> Result<AntigravityAccountVault, String> {
    // Parse loosely for `usage` so a malformed cached reading is dropped without rejecting the vault
    // (mirrors Orca `native-account-store.ts:577`).
    let mut raw: serde_json::Value =
        serde_json::from_slice(plaintext).map_err(|_| "invalid vault".to_owned())?;
    if let Some(accounts) = raw
        .get_mut("accounts")
        .and_then(serde_json::Value::as_array_mut)
    {
        for account in accounts {
            if let Some(usage) = account.get("usage")
                && !usage.is_null()
                && serde_json::from_value::<AntigravityStoredUsage>(usage.clone()).is_err()
                && let Some(obj) = account.as_object_mut()
            {
                obj.insert("usage".to_owned(), serde_json::Value::Null);
            }
        }
    }
    let vault: AntigravityAccountVault =
        serde_json::from_value(raw).map_err(|_| "invalid vault".to_owned())?;
    validate_vault(&vault)?;
    Ok(vault)
}

fn validate_vault(vault: &AntigravityAccountVault) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for account in &vault.accounts {
        if account.id.is_empty() || !ids.insert(account.id.as_str()) {
            return Err("invalid account id".to_owned());
        }
        let credential = parse_antigravity_native_credential(&account.credentials)?;
        if credential.auth_method != account.auth_method {
            return Err("inconsistent auth_method".to_owned());
        }
        if let Some(identity) = credential.identity
            && account.subject.as_deref() != Some(identity.subject.as_str())
        {
            return Err("inconsistent identity".to_owned());
        }
    }
    if let Some(sel) = vault.selected_account_id.as_deref()
        && !ids.contains(sel)
    {
        return Err("invalid selection".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Master key + AEAD encryption (AES-256-GCM + HKDF-SHA256)
// ---------------------------------------------------------------------------

struct MasterKey(Zeroizing<[u8; 32]>);

impl MasterKey {
    fn from_slice(bytes: &[u8]) -> io::Result<Self> {
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid vault key length"))?;
        Ok(Self(Zeroizing::new(arr)))
    }

    fn derive_aead(&self, salt: &[u8; SALT_LEN]) -> io::Result<aead::LessSafeKey> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(self.0.as_ref());
        let okm = prk
            .expand(&[b"krill antigravity accounts v1"], &aead::AES_256_GCM)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "vault key derivation failed")
            })?;
        Ok(aead::LessSafeKey::new(aead::UnboundKey::from(okm)))
    }

    fn encrypt(&self, plaintext: &[u8]) -> io::Result<Vec<u8>> {
        let rng = SystemRandom::new();
        let mut salt = [0u8; SALT_LEN];
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill(&mut salt)
            .map_err(|_| io::Error::other("vault RNG failure"))?;
        rng.fill(&mut nonce_bytes)
            .map_err(|_| io::Error::other("vault RNG failure"))?;
        let key = self.derive_aead(&salt)?;
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let mut buf = plaintext.to_vec();
        key.seal_in_place_append_tag(nonce, aead::Aad::from(MAGIC.as_slice()), &mut buf)
            .map_err(|_| io::Error::other("vault seal failed"))?;
        let mut out = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + buf.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&buf);
        Ok(out)
    }

    fn decrypt(&self, sealed: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        let header_len = MAGIC.len() + SALT_LEN + NONCE_LEN;
        let Some(magic) = sealed.get(..MAGIC.len()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad vault header",
            ));
        };
        if sealed.len() < header_len || magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad vault header",
            ));
        }
        let salt: [u8; SALT_LEN] = sealed
            .get(MAGIC.len()..MAGIC.len() + SALT_LEN)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad salt"))?
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad salt"))?;
        let nonce_bytes: [u8; NONCE_LEN] = sealed
            .get(MAGIC.len() + SALT_LEN..header_len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad nonce"))?
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad nonce"))?;
        let key = self.derive_aead(&salt)?;
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let ciphertext = sealed
            .get(header_len..)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad vault body"))?;
        let mut buf = Zeroizing::new(ciphertext.to_vec());
        let plain_len = key
            .open_in_place(nonce, aead::Aad::from(MAGIC.as_slice()), &mut buf)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "vault open failed"))?
            .len();
        buf.truncate(plain_len);
        Ok(buf)
    }

    #[cfg(all(target_os = "macos", not(test)))]
    fn load_or_create(root: &Path, allow_create: bool) -> io::Result<Arc<Self>> {
        ensure_no_symlink(root)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        let _gate = lock(&KEYCHAIN_GATE);
        let _ui = if SecKeychain::user_interaction_allowed().map_err(keychain_err)? {
            Some(SecKeychain::disable_user_interaction().map_err(keychain_err)?)
        } else {
            None
        };
        let keychain = SecKeychain::default().map_err(keychain_err)?;
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let account =
            crate::mobile::devices::hex(&Sha256::digest(canonical.as_os_str().as_bytes()));
        match keychain.find_generic_password(KEYCHAIN_SERVICE, &account) {
            Ok((bytes, _)) => Self::from_slice(bytes.as_ref()).map(Arc::new),
            Err(err) if err.code() == KEYCHAIN_NOT_FOUND && allow_create => {
                let mut bytes = Zeroizing::new([0u8; 32]);
                SystemRandom::new()
                    .fill(bytes.as_mut())
                    .map_err(|_| io::Error::other("vault RNG failure"))?;
                match keychain.add_generic_password(KEYCHAIN_SERVICE, &account, bytes.as_ref()) {
                    Ok(()) => Ok(Arc::new(Self(bytes))),
                    Err(dup) if dup.code() == KEYCHAIN_DUPLICATE => {
                        let (existing, _) = keychain
                            .find_generic_password(KEYCHAIN_SERVICE, &account)
                            .map_err(keychain_err)?;
                        Self::from_slice(existing.as_ref()).map(Arc::new)
                    }
                    Err(add_err) => Err(keychain_err(add_err)),
                }
            }
            Err(err) => Err(keychain_err(err)),
        }
    }

    #[cfg(any(test, target_os = "linux", windows))]
    fn load_or_create(root: &Path, allow_create: bool) -> io::Result<Arc<Self>> {
        let _guard = lock(&KEY_FILE_LOCK);
        ensure_no_symlink(root)?;
        let path = root.join(KEY_FILE);
        ensure_no_symlink(&path)?;
        match fs::read(&path) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                return unseal_key(&bytes).map(Arc::new);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound && allow_create => {}
            Err(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("vault key unavailable ({err})"),
                ));
            }
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        let mut bytes = Zeroizing::new([0u8; 32]);
        SystemRandom::new()
            .fill(bytes.as_mut())
            .map_err(|_| io::Error::other("vault RNG failure"))?;
        let sealed = seal_key(bytes.as_ref())?;
        write_atomic_0600(&path, &sealed)?;
        ensure_no_symlink(&path)?;
        let readback = Zeroizing::new(fs::read(&path)?);
        unseal_key(&readback).map(Arc::new)
    }
}

fn ensure_no_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "symlink not allowed in vault path",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn keychain_err(err: security_framework::base::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("vault key unavailable (keychain status {})", err.code()),
    )
}

#[cfg(all(windows, not(test)))]
fn seal_key(raw: &[u8]) -> io::Result<Vec<u8>> {
    crate::sys::protect(raw)
}

#[cfg(all(windows, not(test)))]
fn unseal_key(sealed: &[u8]) -> io::Result<MasterKey> {
    let raw = crate::sys::unprotect(sealed)?;
    MasterKey::from_slice(raw.as_ref())
}

#[cfg(any(test, target_os = "linux"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches Windows DPAPI seal_key Result signature"
)]
fn seal_key(raw: &[u8]) -> io::Result<Vec<u8>> {
    Ok(raw.to_vec())
}

#[cfg(any(test, target_os = "linux"))]
fn unseal_key(raw: &[u8]) -> io::Result<MasterKey> {
    MasterKey::from_slice(raw)
}

mod zeroizing_string {
    use serde::{Deserialize, Deserializer, Serializer};
    use zeroize::Zeroizing;

    pub fn serialize<S: Serializer>(val: &Zeroizing<String>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(val.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Zeroizing<String>, D::Error> {
        String::deserialize(d).map(Zeroizing::new)
    }
}
