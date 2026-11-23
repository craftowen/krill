// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Encrypted secret vault for Krill Bots (`<state_dir>/bots/secrets.enc`, mode `0o600`).
//!
//! Master key protection follows `crate::store::crypto`:
//! - macOS (non-test): OS Keychain (`com.krill.bots-secret-key.v1`) with user interaction disabled.
//! - Windows (non-test): DPAPI current-user sealed key file (`crate::sys::protect`/`unprotect`).
//! - Linux / tests: `0o600` atomic key file in `<bots_root>/secrets.key`.
//!
//! Secret payloads are authenticated with AES-256-GCM + per-write random 256-bit HKDF salt +
//! 96-bit random nonce. Raw secret values are always returned inside [`Zeroizing<String>`] and
//! are never logged or serialized into `bot.json`.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
#[cfg(all(target_os = "macos", not(test)))]
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use krill_proto::bots::BotId;
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::write_atomic_0600;
use ring::{
    aead, hkdf,
    rand::{SecureRandom as _, SystemRandom},
};
#[cfg(all(target_os = "macos", not(test)))]
use security_framework::os::macos::keychain::SecKeychain;
#[cfg(all(target_os = "macos", not(test)))]
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::session::lock;

const MAGIC: &[u8; 4] = b"KBS1";
const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const MAX_SECRET_REF_BYTES: usize = 256;
const MAX_VAULT_BYTES: usize = 4 * 1024 * 1024;
const VAULT_FILE: &str = "secrets.enc";

#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_SERVICE: &str = "com.krill.bots-secret-key.v1";
#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_NOT_FOUND: i32 = -25300;
#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_DUPLICATE: i32 = -25299;
#[cfg(all(target_os = "macos", not(test)))]
static KEYCHAIN_GATE: Mutex<()> = Mutex::new(());

#[cfg(any(test, target_os = "linux", windows))]
const KEY_FILE: &str = "secrets.key";
#[cfg(any(test, target_os = "linux", windows))]
static KEY_FILE_LOCK: Mutex<()> = Mutex::new(());

struct MasterKey(Zeroizing<[u8; 32]>);

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey([redacted])")
    }
}

impl MasterKey {
    fn from_slice(bytes: &[u8]) -> io::Result<Self> {
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid bot vault key length")
        })?;
        Ok(Self(Zeroizing::new(arr)))
    }

    fn derive_aead(&self, salt: &[u8; SALT_LEN]) -> io::Result<aead::LessSafeKey> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(self.0.as_ref());
        let okm = prk
            .expand(&[b"krill bots secrets v1"], &aead::AES_256_GCM)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "vault key derivation failed")
            })?;
        Ok(aead::LessSafeKey::new(aead::UnboundKey::from(okm)))
    }

    #[cfg(all(target_os = "macos", not(test)))]
    fn load_or_create(root: &Path, allow_create: bool) -> io::Result<Arc<Self>> {
        ensure_no_symlink_path(root)?;
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
        use krill_proto::os::fs::OpenOptionsExt as _;
        use std::io::Write as _;

        let _guard = lock(&KEY_FILE_LOCK);
        ensure_no_symlink_path(root)?;
        let path = root.join(KEY_FILE);
        ensure_no_symlink_path(&path)?;
        match fs::read(&path) {
            Ok(bytes) => return unseal_key(bytes).map(Arc::new),
            Err(err) if err.kind() == io::ErrorKind::NotFound && allow_create => {}
            Err(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("bot vault key unavailable ({err})"),
                ));
            }
        }

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;

        let mut bytes = Zeroizing::new([0u8; 32]);
        let rng = SystemRandom::new();
        rng.fill(bytes.as_mut())
            .map_err(|_| io::Error::other("vault RNG failure"))?;
        let sealed = seal_key(bytes.as_ref())?;
        let mut suffix = [0u8; 16];
        rng.fill(&mut suffix)
            .map_err(|_| io::Error::other("vault RNG failure"))?;
        let tmp = root.join(format!(
            ".{KEY_FILE}.{}.{}",
            std::process::id(),
            crate::mobile::devices::hex(&suffix)
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        if let Err(err) = file.write_all(&sealed).and_then(|()| file.sync_all()) {
            drop(file);
            fs::remove_file(&tmp).ok();
            return Err(err);
        }
        drop(file);
        place_new_key_file(&path, &tmp, &sealed, |src, dst| fs::hard_link(src, dst))?;
        ensure_no_symlink_path(&path)?;
        unseal_key(fs::read(&path)?).map(Arc::new)
    }
}

#[cfg(any(test, target_os = "linux", windows))]
fn place_new_key_file(
    path: &Path,
    tmp: &Path,
    sealed: &[u8],
    linker: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::io::Write as _;

    let placed = linker(tmp, path);
    fs::remove_file(tmp).ok();
    match placed {
        Ok(()) => ensure_no_symlink_path(path),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => ensure_no_symlink_path(path),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            ensure_no_symlink_path(path)?;
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                Ok(mut file) => {
                    if let Err(write_err) = file.write_all(sealed).and_then(|()| file.sync_all()) {
                        drop(file);
                        fs::remove_file(path).ok();
                        return Err(write_err);
                    }
                    Ok(())
                }
                Err(open_err) if open_err.kind() == io::ErrorKind::AlreadyExists => {
                    ensure_no_symlink_path(path)
                }
                Err(open_err) => Err(open_err),
            }
        }
        Err(err) => Err(err),
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn keychain_err(err: security_framework::base::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("bot vault key unavailable (keychain status {})", err.code()),
    )
}

#[cfg(all(windows, not(test)))]
fn seal_key(raw: &[u8]) -> io::Result<Vec<u8>> {
    crate::sys::protect(raw)
}

#[cfg(any(test, target_os = "linux"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches Windows DPAPI sealing signature"
)]
fn seal_key(raw: &[u8]) -> io::Result<Vec<u8>> {
    Ok(raw.to_vec())
}

#[cfg(all(windows, not(test)))]
fn unseal_key(bytes: Vec<u8>) -> io::Result<MasterKey> {
    let bytes = Zeroizing::new(bytes);
    let plain = crate::sys::unprotect(&bytes)?;
    MasterKey::from_slice(&plain)
}

#[cfg(any(test, target_os = "linux"))]
fn unseal_key(bytes: Vec<u8>) -> io::Result<MasterKey> {
    let bytes = Zeroizing::new(bytes);
    MasterKey::from_slice(&bytes)
}

/// Lazily initialized encrypted secret vault stored at `<bots_root>/secrets.enc`.
pub struct SecretVault {
    root: PathBuf,
    vault_path: PathBuf,
    io_lock: Mutex<()>,
    key: OnceLock<Arc<MasterKey>>,
}

impl fmt::Debug for SecretVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretVault")
            .field("initialized", &self.key.get().is_some())
            .finish_non_exhaustive()
    }
}

impl SecretVault {
    /// Creates a lazy vault handle. Performs zero filesystem or keychain I/O until first use.
    #[must_use]
    pub fn new(bots_root: PathBuf) -> Self {
        let vault_path = bots_root.join(VAULT_FILE);
        Self {
            root: bots_root,
            vault_path,
            io_lock: Mutex::new(()),
            key: OnceLock::new(),
        }
    }

    /// Creates a vault with an explicit master key (used in deterministic tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_static_key(bots_root: PathBuf, raw_key: [u8; 32]) -> Self {
        let vault_path = bots_root.join(VAULT_FILE);
        let key = OnceLock::new();
        key.set(Arc::new(MasterKey(Zeroizing::new(raw_key)))).ok();
        Self {
            root: bots_root,
            vault_path,
            io_lock: Mutex::new(()),
            key,
        }
    }

    /// Canonical scoped reference key for `(bot_id, key)`.
    #[must_use]
    pub fn scoped_ref(bot_id: &BotId, key: &str) -> String {
        format!("{}:{key}", bot_id.as_str())
    }

    /// Stores a secret under an arbitrary validated `secret_ref`.
    pub fn set_ref(&self, secret_ref: &str, value: Zeroizing<String>) -> io::Result<()> {
        validate_secret_ref(secret_ref)?;
        let _guard = lock(&self.io_lock);
        let mut map = self.load_map_locked()?;
        map.insert(secret_ref.to_owned(), value);
        self.save_map_locked(&map)
    }

    /// Reads a secret by exact `secret_ref`. Returns `Ok(None)` if vault or key does not exist.
    pub fn get_ref(&self, secret_ref: &str) -> io::Result<Option<Zeroizing<String>>> {
        validate_secret_ref(secret_ref)?;
        let _guard = lock(&self.io_lock);
        ensure_no_symlink_path(&self.root)?;
        ensure_no_symlink_path(&self.vault_path)?;
        if !self.vault_path.exists() {
            return Ok(None);
        }
        let mut map = self.load_map_locked()?;
        Ok(map.remove(secret_ref))
    }

    /// Deletes a secret by exact `secret_ref`. Returns `true` if a value existed.
    pub fn delete_ref(&self, secret_ref: &str) -> io::Result<bool> {
        validate_secret_ref(secret_ref)?;
        let _guard = lock(&self.io_lock);
        ensure_no_symlink_path(&self.root)?;
        ensure_no_symlink_path(&self.vault_path)?;
        if !self.vault_path.exists() {
            return Ok(false);
        }
        let mut map = self.load_map_locked()?;
        let removed = map.remove(secret_ref).is_some();
        if removed {
            self.save_map_locked(&map)?;
        }
        Ok(removed)
    }

    /// Checks whether a secret exists by exact `secret_ref` without returning its plaintext.
    pub fn has_ref(&self, secret_ref: &str) -> io::Result<bool> {
        Ok(self.get_ref(secret_ref)?.is_some())
    }

    /// Stores a bot-scoped secret under `<bot_id>:<key>`.
    pub fn set_secret(
        &self,
        bot_id: &BotId,
        key: &str,
        value: Zeroizing<String>,
    ) -> io::Result<()> {
        self.set_ref(&Self::scoped_ref(bot_id, key), value)
    }

    /// Retrieves a bot-scoped secret stored under `<bot_id>:<key>`.
    pub fn get_secret(&self, bot_id: &BotId, key: &str) -> io::Result<Option<Zeroizing<String>>> {
        self.get_ref(&Self::scoped_ref(bot_id, key))
    }

    /// Deletes a bot-scoped secret stored under `<bot_id>:<key>`.
    pub fn delete_secret(&self, bot_id: &BotId, key: &str) -> io::Result<bool> {
        self.delete_ref(&Self::scoped_ref(bot_id, key))
    }

    /// Checks whether a bot-scoped secret exists under `<bot_id>:<key>`.
    pub fn has_secret(&self, bot_id: &BotId, key: &str) -> io::Result<bool> {
        self.has_ref(&Self::scoped_ref(bot_id, key))
    }

    /// Resolves a secret checking `explicit_ref`, `<bot_id>:<explicit_ref>`, and `<bot_id>:<fallback_key>`.
    pub fn resolve_secret(
        &self,
        bot_id: &BotId,
        explicit_ref: Option<&str>,
        fallback_key: &str,
    ) -> io::Result<Option<Zeroizing<String>>> {
        if let Some(raw_ref) = explicit_ref.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(found) = self.get_ref(raw_ref)? {
                return Ok(Some(found));
            }
            if !raw_ref.contains(':') {
                let scoped = Self::scoped_ref(bot_id, raw_ref);
                if let Some(found) = self.get_ref(&scoped)? {
                    return Ok(Some(found));
                }
            }
        }
        if !fallback_key.is_empty() {
            return self.get_secret(bot_id, fallback_key);
        }
        Ok(None)
    }

    /// Deletes all secrets prefixed with `<bot_id>:` plus any explicit refs owned by that bot.
    pub fn delete_bot_secrets(&self, bot_id: &BotId, extra_refs: &[String]) -> io::Result<usize> {
        let _guard = lock(&self.io_lock);
        ensure_no_symlink_path(&self.root)?;
        ensure_no_symlink_path(&self.vault_path)?;
        if !self.vault_path.exists() {
            return Ok(0);
        }
        let mut map = self.load_map_locked()?;
        let prefix = format!("{}:", bot_id.as_str());
        let before = map.len();
        map.retain(|k, _| !k.starts_with(&prefix) && !extra_refs.iter().any(|r| r == k));
        let removed = before.saturating_sub(map.len());
        if removed > 0 {
            self.save_map_locked(&map)?;
        }
        Ok(removed)
    }

    fn key(&self, allow_create: bool) -> io::Result<Arc<MasterKey>> {
        if let Some(existing) = self.key.get() {
            return Ok(Arc::clone(existing));
        }
        let loaded = MasterKey::load_or_create(&self.root, allow_create)?;
        self.key.set(Arc::clone(&loaded)).ok();
        Ok(loaded)
    }

    fn load_map_locked(&self) -> io::Result<BTreeMap<String, Zeroizing<String>>> {
        ensure_no_symlink_path(&self.root)?;
        ensure_no_symlink_path(&self.vault_path)?;
        let raw = match fs::read(&self.vault_path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => return Err(err),
        };
        if raw.len() > MAX_VAULT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bot secrets vault exceeds size limit",
            ));
        }
        let min_len = MAGIC.len() + SALT_LEN + NONCE_LEN + aead::AES_256_GCM.tag_len();
        if raw.len() < min_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated bot secrets vault",
            ));
        }
        if raw.get(..MAGIC.len()) != Some(MAGIC.as_slice()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid bot secrets vault header",
            ));
        }
        let salt_start = MAGIC.len();
        let nonce_start = salt_start + SALT_LEN;
        let ct_start = nonce_start + NONCE_LEN;
        let salt: [u8; SALT_LEN] = raw
            .get(salt_start..nonce_start)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing vault salt"))?;
        let nonce_bytes: [u8; NONCE_LEN] = raw
            .get(nonce_start..ct_start)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing vault nonce"))?;
        let aad = raw.get(..ct_start).unwrap_or_default();
        let mut buf = Zeroizing::new(raw.get(ct_start..).unwrap_or_default().to_vec());

        let master = self.key(false)?;
        let aead_key = master.derive_aead(&salt)?;
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let plain = aead_key
            .open_in_place(nonce, aead::Aad::from(aad), &mut buf)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "bot secrets vault authentication failed",
                )
            })?;
        let map: BTreeMap<String, String> = serde_json::from_slice(plain).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid bot secrets vault JSON")
        })?;
        Ok(map
            .into_iter()
            .map(|(k, v)| (k, Zeroizing::new(v)))
            .collect())
    }

    fn save_map_locked(&self, map: &BTreeMap<String, Zeroizing<String>>) -> io::Result<()> {
        ensure_no_symlink_path(&self.root)?;
        ensure_no_symlink_path(&self.vault_path)?;
        let master = self.key(true)?;
        let rng = SystemRandom::new();
        let mut salt = [0u8; SALT_LEN];
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill(&mut salt)
            .map_err(|_| io::Error::other("vault salt RNG failure"))?;
        rng.fill(&mut nonce_bytes)
            .map_err(|_| io::Error::other("vault nonce RNG failure"))?;

        let mut aad = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN);
        aad.extend_from_slice(MAGIC);
        aad.extend_from_slice(&salt);
        aad.extend_from_slice(&nonce_bytes);

        let view: BTreeMap<&str, &str> =
            map.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut plain = Zeroizing::new(
            serde_json::to_vec(&view)
                .map_err(|_| io::Error::other("failed to serialize bot secrets vault"))?,
        );
        drop(view);
        let projected_len = aad
            .len()
            .saturating_add(plain.len())
            .saturating_add(aead::AES_256_GCM.tag_len());
        if projected_len > MAX_VAULT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bot secrets vault exceeds size limit",
            ));
        }
        let aead_key = master.derive_aead(&salt)?;
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        aead_key
            .seal_in_place_append_tag(nonce, aead::Aad::from(&aad), &mut *plain)
            .map_err(|_| io::Error::other("bot secrets vault encryption failed"))?;

        let mut out = Vec::with_capacity(aad.len().saturating_add(plain.len()));
        out.extend_from_slice(&aad);
        out.extend_from_slice(&plain);
        if out.len() > MAX_VAULT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bot secrets vault exceeds size limit",
            ));
        }
        write_atomic_0600(&self.vault_path, &out)
    }
}

pub(crate) fn ensure_no_symlink_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to follow symlink in bot path",
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn validate_secret_ref(secret_ref: &str) -> io::Result<()> {
    let trimmed = secret_ref.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_SECRET_REF_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid secret reference",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-secrets-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn vault_is_lazy_encrypts_at_rest_and_detects_tampering() {
        let dir = temp_dir("enc");
        let vault = SecretVault::with_static_key(dir.clone(), [0x42; 32]);
        let bot = BotId::new("bot-alpha").unwrap();

        // Lazy: vault file does not exist until first write.
        assert!(!dir.join(VAULT_FILE).exists());
        assert!(!vault.has_secret(&bot, "telegram").unwrap());
        assert!(!dir.join(VAULT_FILE).exists());

        let token = "super-secret-bot-token-999";
        vault
            .set_secret(&bot, "telegram", Zeroizing::new(token.to_owned()))
            .unwrap();
        assert!(dir.join(VAULT_FILE).exists());

        // Ciphertext on disk must never contain plaintext secret.
        let disk = fs::read(dir.join(VAULT_FILE)).unwrap();
        assert!(!disk.windows(token.len()).any(|w| w == token.as_bytes()));

        // Debug output must never leak secrets.
        let dbg = format!("{vault:?}");
        assert!(!dbg.contains(token));

        // Roundtrip read & resolve.
        assert_eq!(
            vault
                .get_secret(&bot, "telegram")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some(token)
        );
        assert_eq!(
            vault
                .resolve_secret(&bot, Some("telegram"), "fallback")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some(token)
        );

        // Tampering fails authentication.
        let mut tampered = disk;
        if let Some(last) = tampered.last_mut() {
            *last ^= 0xff;
        }
        fs::write(dir.join(VAULT_FILE), tampered).unwrap();
        assert!(vault.get_secret(&bot, "telegram").is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_and_delete_bot_secrets_clean_up_entries() {
        let dir = temp_dir("del");
        let vault = SecretVault::new(dir.clone());
        let bot_a = BotId::new("bot-a").unwrap();
        let bot_b = BotId::new("bot-b").unwrap();

        vault
            .set_secret(&bot_a, "k1", Zeroizing::new("v1".to_owned()))
            .unwrap();
        vault
            .set_ref("custom-ref-a", Zeroizing::new("v2".to_owned()))
            .unwrap();
        vault
            .set_secret(&bot_b, "k1", Zeroizing::new("v3".to_owned()))
            .unwrap();

        let removed = vault
            .delete_bot_secrets(&bot_a, &["custom-ref-a".to_owned()])
            .unwrap();
        assert_eq!(removed, 2);
        assert!(!vault.has_secret(&bot_a, "k1").unwrap());
        assert!(!vault.has_ref("custom-ref-a").unwrap());
        assert!(vault.has_secret(&bot_b, "k1").unwrap());

        assert!(vault.delete_secret(&bot_b, "k1").unwrap());
        assert!(!vault.delete_secret(&bot_b, "k1").unwrap());

        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn vault_rejects_symlink_vault_and_key_paths() {
        let dir = temp_dir("symlink");
        let target = dir.join("outside.txt");
        fs::write(&target, b"not-a-vault").unwrap();
        std::os::unix::fs::symlink(&target, dir.join(VAULT_FILE)).unwrap();

        let vault = SecretVault::with_static_key(dir.clone(), [0x11; 32]);
        assert!(vault.get_ref("tok").is_err());
        assert!(
            vault
                .set_ref("tok", Zeroizing::new("secret".to_owned()))
                .is_err()
        );
        assert!(vault.delete_ref("tok").is_err());

        fs::remove_file(dir.join(VAULT_FILE)).unwrap();
        std::os::unix::fs::symlink(&target, dir.join(KEY_FILE)).unwrap();
        let vault2 = SecretVault::new(dir.clone());
        assert!(
            vault2
                .set_ref("tok", Zeroizing::new("secret".to_owned()))
                .is_err()
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn key_placement_falls_back_when_hard_link_unsupported() {
        let dir = temp_dir("hardlink-fallback");
        let key_path = dir.join(KEY_FILE);
        let tmp_path = dir.join(".tmp.key");
        let sealed = seal_key(&[0x77; 32]).unwrap();
        fs::write(&tmp_path, &sealed).unwrap();

        place_new_key_file(&key_path, &tmp_path, &sealed, |_, _| {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links"))
        })
        .unwrap();
        assert_eq!(fs::read(&key_path).unwrap(), sealed);
        assert!(!tmp_path.exists());

        // Subsequent AlreadyExists is harmless.
        fs::write(&tmp_path, &sealed).unwrap();
        place_new_key_file(&key_path, &tmp_path, &sealed, |_, _| {
            Err(io::Error::new(io::ErrorKind::AlreadyExists, "exists"))
        })
        .unwrap();

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_rejects_vault_exceeding_max_bytes_without_corrupting_existing_vault() {
        let dir = temp_dir("max-vault-size");
        let vault = SecretVault::with_static_key(dir.clone(), [0x22; 32]);
        vault
            .set_ref("existing", Zeroizing::new("keep-me".to_owned()))
            .unwrap();

        // Fill multiple entries near MAX_VAULT_BYTES; the write that crosses the cap must fail
        // cleanly without leaving an unreadable oversized vault on disk.
        let big_val = "A".repeat(60 * 1024);
        let mut hit_cap = false;
        for idx in 0..80 {
            let res = vault.set_ref(&format!("k-{idx}"), Zeroizing::new(big_val.clone()));
            if let Err(err) = res {
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
                hit_cap = true;
                break;
            }
        }
        assert!(
            hit_cap,
            "expected vault write to reject exceeding MAX_VAULT_BYTES"
        );
        // Vault must remain readable after the rejected write.
        assert_eq!(
            vault
                .get_ref("existing")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("keep-me")
        );

        fs::remove_dir_all(&dir).ok();
    }
}
