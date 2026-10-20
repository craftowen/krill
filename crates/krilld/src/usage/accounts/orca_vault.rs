//! Read-only import of Orca's encrypted Antigravity vault. No credentials cross IPC.
//! Orca native-account-store.ts / native-credential-codec.ts: Copyright (c) 2026 Lovecast Inc., MIT.
//! v10 format: Chromium `components/os_crypt/sync/os_crypt_mac.mm`, BSD-3-Clause.

use std::io;
use std::path::{Path, PathBuf};

use aes::cipher::{BlockDecryptMut as _, KeyIvInit as _, block_padding::Pkcs7};
use krill_proto::usage::{AccountInfo, AccountsSnapshot, ProviderKind, Snapshot};
use zeroize::Zeroizing;

use super::{
    SAVE_LOCK, StoredAccount, load_writable, lock, refresh_accounts_with, save_inner,
    write_atomic_0600,
};

struct Account {
    id: String,
    email: Option<String>,
    credentials: Zeroizing<String>,
}

pub(crate) fn import(
    home: &Path,
    state: Option<&Path>,
    source: &Path,
    dry_run: bool,
) -> io::Result<AccountsSnapshot> {
    let path = source.join("antigravity-accounts/vault");
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    let encrypted = std::fs::read(&path)?;
    if encrypted
        .strip_prefix(b"v10")
        .is_none_or(|body| body.is_empty() || !body.len().is_multiple_of(16))
    {
        return Err(invalid());
    }
    let password = password()?;
    let raw = decrypt(&encrypted, &password)?;
    let (accounts, selected) = parse(&raw)?;
    apply(home, state, accounts, selected.as_deref(), dry_run)
}

fn apply(
    home: &Path,
    state: Option<&Path>,
    accounts: Vec<Account>,
    selected: Option<&str>,
    dry_run: bool,
) -> io::Result<AccountsSnapshot> {
    let mut preview = refresh_accounts_with(home, state, false);
    if dry_run {
        for account in accounts {
            let id = format!("agy-orca-{}", account.id);
            if preview.accounts.iter().any(|a| a.id == id) {
                continue;
            }
            preview.accounts.push(AccountInfo {
                id,
                provider: ProviderKind::Antigravity,
                label: account
                    .email
                    .clone()
                    .unwrap_or_else(|| "Antigravity".into()),
                email: account.email,
                profile_dir: profile(home, &account.id).display().to_string(),
                active: selected == Some(&account.id),
                host_default: false,
                stale: false,
                usage: Snapshot::default(),
            });
        }
        return Ok(preview);
    }
    {
        let _guard = lock(&SAVE_LOCK);
        let mut file = load_writable(home, state)?;
        for account in accounts {
            let id = format!("agy-orca-{}", account.id);
            let dir = profile(home, &account.id);
            let native = dir.join(".gemini/antigravity-cli");
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&native)?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(&native)?;
            // The native blob includes refresh credentials; preserve its unknown fields verbatim.
            write_atomic_0600(
                &native.join("antigravity-oauth-token"),
                account.credentials.as_bytes(),
            )?;
            let label = account
                .email
                .clone()
                .unwrap_or_else(|| "Antigravity".into());
            if let Some(existing) = file.accounts.iter_mut().find(|a| a.id == id) {
                existing.email = account.email;
                existing.profile_dir = dir.display().to_string();
            } else {
                file.accounts.push(StoredAccount {
                    id: id.clone(),
                    provider: "agy".into(),
                    label,
                    email: account.email,
                    profile_dir: dir.display().to_string(),
                });
            }
            if selected == Some(&account.id) {
                file.active.insert("agy".into(), id);
            }
        }
        save_inner(home, state, &file)?;
    }
    Ok(refresh_accounts_with(home, state, false))
}

fn profile(home: &Path, id: &str) -> PathBuf {
    krill_proto::usage::profiles::default_profile_dir(
        home,
        ProviderKind::Antigravity,
        &format!("orca-{id}"),
    )
}

#[cfg(target_os = "macos")]
fn password() -> io::Result<Zeroizing<Vec<u8>>> {
    // Read only Orca's own safeStorage item. Do not change Orca's current native account.
    security_framework::passwords::get_generic_password("Orca Safe Storage", "Orca Key")
        .or_else(|_| {
            security_framework::passwords::get_generic_password("orca Safe Storage", "orca Key")
        })
        .or_else(|_| {
            security_framework::passwords::get_generic_password("Orca Safe Storage", "Orca")
        })
        .or_else(|_| {
            security_framework::passwords::get_generic_password("orca Safe Storage", "orca")
        })
        .map(Zeroizing::new)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Orca account vault is locked or its Keychain key is unavailable",
            )
        })
}

#[cfg(not(target_os = "macos"))]
fn password() -> io::Result<Zeroizing<Vec<u8>>> {
    Err(io::ErrorKind::Unsupported.into())
}

fn decrypt(encrypted: &[u8], password: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let body = encrypted.strip_prefix(b"v10").ok_or_else(invalid)?;
    let iterations = std::num::NonZeroU32::new(1003).ok_or_else(invalid)?;
    let mut key = Zeroizing::new([0_u8; 16]);
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA1,
        iterations,
        b"saltysalt",
        password,
        key.as_mut(),
    );
    let mut raw = Zeroizing::new(body.to_vec());
    let len = cbc::Decryptor::<aes::Aes128>::new_from_slices(key.as_ref(), &[b' '; 16])
        .map_err(|_| invalid())?
        .decrypt_padded_mut::<Pkcs7>(&mut raw)
        .map_err(|_| invalid())?
        .len();
    raw.truncate(len);
    Ok(raw)
}

fn parse(raw: &[u8]) -> io::Result<(Vec<Account>, Option<String>)> {
    let mut json: serde_json::Value = serde_json::from_slice(raw).map_err(|_| invalid())?;
    let selected = json
        .get("selectedAccountId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let rows = json
        .get_mut("accounts")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(invalid)?;
    if rows.len() > 64 {
        return Err(invalid());
    }
    let mut accounts = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| id.len() <= 128 && crate::agent::control::valid_session_id(id))
            .ok_or_else(invalid)?
            .to_owned();
        if !ids.insert(id.clone()) {
            return Err(invalid());
        }
        let email = row
            .get("email")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let credentials = match row.get_mut("credentials").map(serde_json::Value::take) {
            Some(serde_json::Value::String(s)) if s.len() <= 64 * 1024 => Zeroizing::new(s),
            _ => return Err(invalid()),
        };
        let native: serde_json::Value =
            serde_json::from_str(&credentials).map_err(|_| invalid())?;
        if native
            .get("auth_method")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
            || native
                .pointer("/token/access_token")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(invalid());
        }
        accounts.push(Account {
            id,
            email,
            credentials,
        });
    }
    if selected.as_ref().is_some_and(|id| !ids.contains(id)) {
        return Err(invalid());
    }
    Ok((accounts, selected))
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Orca Antigravity account vault could not be decoded; source preserved",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn arbitrary_vault_documents_are_rejected_or_bounded(raw in ".{0,4096}") {
            if let Ok((accounts, selected)) = parse(raw.as_bytes()) {
                proptest::prop_assert!(accounts.len() <= 64);
                proptest::prop_assert!(accounts.iter().all(|a| a.id.len() <= 128 && a.credentials.len() <= 64 * 1024));
                proptest::prop_assert!(selected.is_none_or(|id| accounts.iter().any(|a| a.id == id)));
            }
        }
    }
    #[test]
    fn native_vault_rejects_path_traversal_invalid_tokens_and_foreign_selection() {
        let good = br#"{"accounts":[{"id":"fixture-id","email":"fixture@example.test","credentials":"{\"auth_method\":\"oauth\",\"token\":{\"access_token\":\"test-only\",\"future\":true}}"}],"selectedAccountId":"fixture-id"}"#;
        let (accounts, selected) = parse(good).unwrap();
        assert_eq!(selected.as_deref(), Some("fixture-id"));
        assert!(accounts[0].credentials.contains("future"));
        for raw in [
            br#"{"accounts":[{"id":"../escape","credentials":"{}"}]}"#.as_slice(),
            br#"{"accounts":[],"selectedAccountId":"other"}"#.as_slice(),
        ] {
            assert!(parse(raw).is_err());
        }
        assert!(decrypt(b"plaintext", b"fixture").is_err());
    }

    #[test]
    fn vault_import_is_idempotent_private_and_dry_run_does_not_write() {
        let root = std::env::temp_dir().join(format!("krill-vault-import-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state = root.join("state");
        let raw = br#"{"accounts":[{"id":"fixture-account","email":"fixture@example.test","credentials":"{\"auth_method\":\"oauth\",\"token\":{\"access_token\":\"test-only\",\"unknown\":true}}"}],"selectedAccountId":"fixture-account"}"#;
        let (accounts, selected) = parse(raw).unwrap();
        let preview = apply(&home, Some(&state), accounts, selected.as_deref(), true).unwrap();
        assert!(
            preview
                .accounts
                .iter()
                .any(|a| a.id == "agy-orca-fixture-account")
        );
        assert!(
            !root.exists(),
            "preview must not create credentials or account state"
        );
        for _ in 0..2 {
            let (accounts, selected) = parse(raw).unwrap();
            let snapshot =
                apply(&home, Some(&state), accounts, selected.as_deref(), false).unwrap();
            assert_eq!(
                snapshot
                    .accounts
                    .iter()
                    .filter(|a| a.id == "agy-orca-fixture-account" && a.active)
                    .count(),
                1
            );
        }
        let token = profile(&home, "fixture-account")
            .join(".gemini/antigravity-cli/antigravity-oauth-token");
        assert!(std::fs::read_to_string(&token).unwrap().contains("unknown"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn chromium_v10_independent_fixture_uses_scoped_key_and_rejects_wrong_key() {
        let password = b"fixture-only-password";
        let plaintext = b"{\"accounts\":[],\"selectedAccountId\":null}";
        // Generated independently with Python hashlib.pbkdf2_hmac and OpenSSL AES-128-CBC.
        let fixture = "763130326ca27b5053fbf5f4e5b8bba9615f9657a63013879c34f94cdc654f7a5b8024dff1fe52de1f68fbd935922c1034029d";
        let sealed: Vec<_> = fixture
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(decrypt(&sealed, password).unwrap().as_slice(), plaintext);
        assert!(decrypt(&sealed, b"another-account-key").is_err());
    }
}
