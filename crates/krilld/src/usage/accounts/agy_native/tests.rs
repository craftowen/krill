use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use krill_proto::usage::{
    AntigravityAccountOp, AntigravityStoredUsage, ProviderKind, Snapshot, Window,
};
use zeroize::Zeroizing;

use super::backend::{
    AntigravityCredentialBackend, FileCredentialBackend, force_file_token_storage_env,
    forces_antigravity_file_store,
};
use super::codec::{
    AntigravityCredentialIdentity, AntigravityNativeCredential, GOOGLE_ISSUER,
    decode_antigravity_keychain_value, encode_antigravity_keychain_value,
    parse_antigravity_native_credential,
};
use super::service::{
    AntigravityAccountService, AntigravityUsageReadFinish, migrate_legacy_agy_profiles_once,
};
use super::store::{
    AntigravityAccountStore, AntigravityAccountVault, EncryptedAntigravityAccountStore, VaultError,
};
use super::*;
use crate::usage::accounts::{AccountsFile, StoredAccount};

fn encode_b64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let (mut bits, mut count) = (0_u32, 0_u32);
    let mut out = String::new();
    for &byte in bytes {
        bits = (bits << 8) | u32::from(byte);
        count += 8;
        while count >= 6 {
            count -= 6;
            let idx = usize::try_from((bits >> count) & 63).unwrap();
            out.push(char::from(*TABLE.get(idx).unwrap()));
        }
    }
    if count > 0 {
        let idx = usize::try_from((bits << (6 - count)) & 63).unwrap();
        out.push(char::from(*TABLE.get(idx).unwrap()));
    }
    out
}

fn google_jwt(sub: &str, email: &str) -> String {
    let payload = serde_json::json!({
        "iss": "https://accounts.google.com",
        "sub": sub,
        "email": email,
        "email_verified": true,
    });
    format!(
        "e30.{}.sig",
        encode_b64url(&serde_json::to_vec(&payload).unwrap())
    )
}

fn agy_credential_json(
    sub: &str,
    email: &str,
    access: &str,
    refresh: &str,
    include_id_token: bool,
) -> String {
    if include_id_token {
        serde_json::json!({
            "auth_method": "consumer",
            "id_token": google_jwt(sub, email),
            "id_token_email": email,
            "token": {
                "access_token": access,
                "refresh_token": refresh,
                "token_type": "Bearer",
            }
        })
        .to_string()
    } else {
        serde_json::json!({
            "auth_method": "consumer",
            "id_token_email": email,
            "token": {
                "access_token": access,
                "refresh_token": refresh,
                "token_type": "Bearer",
            }
        })
        .to_string()
    }
}

struct MemoryBackend {
    raw: Arc<Mutex<Option<String>>>,
}

impl AntigravityCredentialBackend for MemoryBackend {
    fn read(&self) -> Result<Option<AntigravityNativeCredential>, String> {
        let guard = self.raw.lock().unwrap();
        match guard.as_deref() {
            None => Ok(None),
            Some(s) => parse_antigravity_native_credential(s).map(Some),
        }
    }

    fn write(&self, contents: &str, expected: Option<&str>) -> Result<(), String> {
        parse_antigravity_native_credential(contents)?;
        let mut guard = self.raw.lock().unwrap();
        if guard.as_deref() != expected {
            return Err(super::backend::CONFLICT_MSG.to_owned());
        }
        *guard = Some(contents.to_owned());
        Ok(())
    }
}

struct MemoryStore {
    vault: Arc<Mutex<AntigravityAccountVault>>,
}

impl AntigravityAccountStore for MemoryStore {
    fn read(&self) -> Result<AntigravityAccountVault, VaultError> {
        Ok(self.vault.lock().unwrap().clone())
    }

    fn write(&self, vault: &AntigravityAccountVault) -> Result<(), String> {
        *self.vault.lock().unwrap() = vault.clone();
        Ok(())
    }

    fn reset_locked_vault(&self) -> Result<(), String> {
        *self.vault.lock().unwrap() = AntigravityAccountVault::default();
        Ok(())
    }
}

#[test]
fn codec_roundtrips_go_keyring_base64_hex_modern_and_legacy_tokens() {
    let raw = agy_credential_json("sub-1", "alice@example.test", "acc-1", "ref-1", true);
    let encoded = encode_antigravity_keychain_value(&raw).unwrap();
    assert!(encoded.starts_with("go-keyring-base64:"));
    assert_eq!(
        decode_antigravity_keychain_value(&encoded)
            .unwrap()
            .as_str(),
        raw
    );

    let hex_encoded = format!(
        "go-keyring-encoded:{}",
        crate::mobile::devices::hex(raw.as_bytes())
    );
    assert_eq!(
        decode_antigravity_keychain_value(&hex_encoded)
            .unwrap()
            .as_str(),
        raw
    );
    assert!(decode_antigravity_keychain_value("go-keyring-base64:").is_err());
    assert!(decode_antigravity_keychain_value("go-keyring-encoded:abc").is_err());

    let parsed = parse_antigravity_native_credential(&raw).unwrap();
    assert_eq!(parsed.auth_method, "consumer");
    assert_eq!(parsed.identity.as_ref().unwrap().subject, "sub-1");
    assert_eq!(
        parsed.identity.as_ref().unwrap().email.as_deref(),
        Some("alice@example.test")
    );
    assert_eq!(
        parsed.refresh_token.as_deref().map(String::as_str),
        Some("ref-1")
    );

    let legacy = r#"{"access_token":"legacy-acc","refresh_token":"legacy-ref"}"#;
    let parsed_legacy = parse_antigravity_native_credential(legacy).unwrap();
    assert_eq!(parsed_legacy.auth_method, "");
    assert_eq!(parsed_legacy.access_token.as_str(), "legacy-acc");
    assert_eq!(
        parsed_legacy.refresh_token.as_deref().map(String::as_str),
        Some("legacy-ref")
    );
}

#[test]
fn force_file_token_storage_env_matches_agy_1_5_16_policy() {
    let mut env = BTreeMap::new();
    assert!(!force_file_token_storage_env(&env));
    assert!(!forces_antigravity_file_store(&env));

    env.insert("AGY_CLI_FORCE_FILE_TOKEN_STORAGE".into(), "1".into());
    assert!(force_file_token_storage_env(&env));
    assert!(forces_antigravity_file_store(&env));

    env.insert("AGY_CLI_FORCE_FILE_TOKEN_STORAGE".into(), "TRUE".into());
    assert!(force_file_token_storage_env(&env));

    env.insert("AGY_CLI_FORCE_FILE_TOKEN_STORAGE".into(), "0".into());
    assert!(!force_file_token_storage_env(&env));
}

#[test]
fn service_add_select_refresh_rewritten_remove_and_usage_fencing() {
    let native = Arc::new(Mutex::new(Some(agy_credential_json(
        "sub-a",
        "alice@example.test",
        "acc-a1",
        "ref-a",
        true,
    ))));
    let vault_mem = Arc::new(Mutex::new(AntigravityAccountVault::default()));
    let lookup_calls = Arc::new(Mutex::new(0u32));
    let lookup_counter = Arc::clone(&lookup_calls);
    let svc = AntigravityAccountService::new(
        Box::new(MemoryStore {
            vault: Arc::clone(&vault_mem),
        }),
        Box::new(MemoryBackend {
            raw: Arc::clone(&native),
        }),
        Box::new(|| 1_700_000_000_000),
        Box::new(move |token| {
            *lookup_counter.lock().unwrap() += 1;
            if token == "acc-a-rewritten" {
                Ok(AntigravityCredentialIdentity {
                    issuer: GOOGLE_ISSUER,
                    subject: "sub-a".into(),
                    email: Some("alice@example.test".into()),
                })
            } else {
                Err("unexpected lookup".into())
            }
        }),
    );

    // 1. Current unsaved login is reported with active_account_id = None.
    let s0 = svc.list_accounts().unwrap();
    assert!(s0.accounts.is_empty());
    assert_eq!(s0.active_account_id, None);
    assert_eq!(
        s0.current_account.as_ref().and_then(|c| c.email.as_deref()),
        Some("alice@example.test")
    );

    // 2. Save account A (idempotent).
    let s1 = svc.add_current_account().unwrap();
    assert_eq!(s1.accounts.len(), 1);
    let id_a = s1.accounts[0].id.clone();
    assert_eq!(s1.active_account_id.as_deref(), Some(id_a.as_str()));
    assert_eq!(svc.add_current_account().unwrap().accounts.len(), 1);

    // 3. Switch native externally to account B and save account B.
    *native.lock().unwrap() = Some(agy_credential_json(
        "sub-b",
        "bob@example.test",
        "acc-b1",
        "ref-b",
        true,
    ));
    let s2 = svc.add_current_account().unwrap();
    assert_eq!(s2.accounts.len(), 2);
    let id_b = s2.accounts[1].id.clone();
    assert_eq!(s2.active_account_id.as_deref(), Some(id_b.as_str()));

    // 4. Select account A -> writes A's credential back to native store and sets selected_account_id.
    let s3 = svc.select_account(&id_a).unwrap();
    assert_eq!(s3.active_account_id.as_deref(), Some(id_a.as_str()));
    assert_eq!(s3.selected_account_id.as_deref(), Some(id_a.as_str()));
    assert!(
        native
            .lock()
            .unwrap()
            .as_deref()
            .unwrap()
            .contains("acc-a1")
    );
    assert!(svc.prepare_for_launch().is_ok());

    // 5. Token refresh drops id_token while keeping refresh_token -> still matches A & updates vault!
    let refreshed_a = agy_credential_json("sub-a", "alice@example.test", "acc-a2", "ref-a", false);
    *native.lock().unwrap() = Some(refreshed_a.clone());
    let s4 = svc.list_accounts().unwrap();
    assert_eq!(s4.active_account_id.as_deref(), Some(id_a.as_str()));
    assert_eq!(
        vault_mem.lock().unwrap().accounts[0].credentials.as_str(),
        refreshed_a
    );
    assert_eq!(*lookup_calls.lock().unwrap(), 0);

    // 6. Rewritten login (new refresh token, no id_token) -> uses cached userinfo lookup once!
    let rewritten_a = agy_credential_json(
        "sub-a",
        "alice@example.test",
        "acc-a-rewritten",
        "ref-a-new",
        false,
    );
    *native.lock().unwrap() = Some(rewritten_a.clone());
    let s5 = svc.list_accounts().unwrap();
    assert_eq!(s5.active_account_id.as_deref(), Some(id_a.as_str()));
    let _ = svc.list_accounts().unwrap();
    assert_eq!(
        *lookup_calls.lock().unwrap(),
        1,
        "userinfo cached per access_token"
    );

    // 7. Usage read fencing: committed when unchanged, dropped when switched mid-read.
    let start = svc.begin_usage_read().unwrap().unwrap();
    let login = start.login.unwrap();
    assert_eq!(login.account_id.as_deref(), Some(id_a.as_str()));
    let reading = AntigravityStoredUsage {
        usage: Snapshot {
            five_hour: Some(Window {
                percent: 18,
                resets_at: None,
            }),
            seven_day: Some(Window {
                percent: 42,
                resets_at: None,
            }),
        },
        meters: vec![],
        updated_at: 1_700_000_050,
    };
    let fin = svc
        .finish_usage_read(&login, Some(reading.clone()))
        .unwrap();
    match fin {
        AntigravityUsageReadFinish::Committed { account_id, email } => {
            assert_eq!(account_id.as_deref(), Some(id_a.as_str()));
            assert_eq!(email.as_deref(), Some("alice@example.test"));
        }
        AntigravityUsageReadFinish::Switched => panic!("expected committed usage read"),
    }
    let parsed_a = parse_antigravity_native_credential(&agy_credential_json(
        "sub-a",
        "alice@example.test",
        "acc-a1",
        "ref-a",
        true,
    ))
    .unwrap();
    let parsed_refreshed = parse_antigravity_native_credential(&refreshed_a).unwrap();
    let parsed_b = parse_antigravity_native_credential(&agy_credential_json(
        "sub-b",
        "bob@example.test",
        "acc-b1",
        "ref-b",
        true,
    ))
    .unwrap();
    assert!(super::service::is_same_antigravity_login(
        &parsed_a,
        &parsed_refreshed
    ));
    assert!(!super::service::is_same_antigravity_login(
        &parsed_a, &parsed_b
    ));
    assert_eq!(
        svc.list_accounts().unwrap().accounts[0].usage,
        Some(reading.clone())
    );

    let start2 = svc.begin_usage_read().unwrap().unwrap();
    let login2 = start2.login.unwrap();
    svc.select_account(&id_b).unwrap();
    let fin2 = svc.finish_usage_read(&login2, Some(reading)).unwrap();
    assert!(matches!(fin2, AntigravityUsageReadFinish::Switched));

    // 8. Cannot remove currently active account B; can remove inactive account A.
    assert!(svc.remove_account(&id_b).is_err());
    let s6 = svc.remove_account(&id_a).unwrap();
    assert_eq!(s6.accounts.len(), 1);
    assert_eq!(s6.accounts[0].id, id_b);

    // 9. Native change outside Krill makes prepare_for_launch fail.
    *native.lock().unwrap() = None;
    let s7 = svc.list_accounts().unwrap();
    assert!(s7.native_account_changed());
    assert!(svc.prepare_for_launch().is_err());
}

#[test]
fn encrypted_vault_persists_with_0600_and_preserves_corrupt_file() {
    let root = std::env::temp_dir().join(format!("krill-agy-vault-enc-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let path = root.join("antigravity-accounts/vault");
    let store = EncryptedAntigravityAccountStore::new(path.clone());
    assert!(store.read().unwrap().accounts.is_empty());

    let raw = agy_credential_json(
        "sub-enc",
        "enc@example.test",
        "secret-tok-xyz",
        "ref-enc",
        true,
    );
    let vault = AntigravityAccountVault {
        accounts: vec![StoredAntigravityAccount {
            id: "acct-enc".into(),
            email: Some("enc@example.test".into()),
            subject: Some("sub-enc".into()),
            auth_method: "consumer".into(),
            created_at: 1000,
            updated_at: 1000,
            credentials: Zeroizing::new(raw.clone()),
            usage: None,
        }],
        selected_account_id: Some("acct-enc".into()),
    };
    store.write(&vault).unwrap();

    // Encrypted bytes must never contain plaintext token!
    let disk_bytes = std::fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&disk_bytes).contains("secret-tok-xyz"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    let loaded = store.read().unwrap();
    assert_eq!(loaded.accounts.len(), 1);
    assert_eq!(loaded.accounts[0].credentials.as_str(), raw);
    assert_eq!(loaded.selected_account_id.as_deref(), Some("acct-enc"));

    // Corrupt vault is rejected and existing bytes are preserved.
    let corrupt = b"corrupt-bytes";
    std::fs::write(&path, corrupt).unwrap();
    assert!(store.read().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn legacy_agy_profiles_migrate_valid_tokens_without_touching_profile_files() {
    let root = std::env::temp_dir().join(format!("krill-agy-legacy-mig-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    let prof1 = home.join(".config/krill/profiles/agy/antigravity-profile-1");
    let prof2 = home.join(".config/krill/profiles/agy/antigravity-profile-2");
    std::fs::create_dir_all(prof1.join(".gemini/antigravity-cli")).unwrap();
    std::fs::create_dir_all(prof2.join(".gemini/antigravity-cli")).unwrap();

    // User settings & history in legacy profiles must remain untouched.
    std::fs::write(
        prof1.join(".gemini/antigravity-cli/settings.json"),
        r#"{"keep":1}"#,
    )
    .unwrap();
    std::fs::write(
        prof2.join(".gemini/antigravity-cli/history.jsonl"),
        "{\"h\":2}\n",
    )
    .unwrap();

    let tok1 = agy_credential_json("sub-1", "one@example.test", "acc-1", "ref-1", true);
    let tok1_path = prof1.join(".gemini/antigravity-cli/antigravity-oauth-token");
    write_atomic_0600(&tok1_path, tok1.as_bytes()).unwrap();
    // prof2 simulates a wiped token (like after agy 1.5.16 purgeTokenStorage).

    let mut file = AccountsFile {
        accounts: vec![
            StoredAccount {
                id: "agy-antigravity-profile-1".into(),
                provider: "agy".into(),
                label: "Antigravity Profile #1".into(),
                email: None,
                profile_dir: prof1.display().to_string(),
            },
            StoredAccount {
                id: "agy-antigravity-profile-2".into(),
                provider: "agy".into(),
                label: "Antigravity Profile #2".into(),
                email: None,
                profile_dir: prof2.display().to_string(),
            },
        ],
        ..AccountsFile::default()
    };
    file.active
        .insert("agy".into(), "agy-antigravity-profile-1".into());
    crate::usage::accounts::save_file(&home, Some(&state), &file).unwrap();

    migrate_legacy_agy_profiles_once(&home, Some(&state));

    let state_view = handle_op(&home, Some(&state), &AntigravityAccountOp::List).unwrap();
    assert_eq!(state_view.accounts.len(), 1);
    assert_eq!(state_view.accounts[0].id, "agy-antigravity-profile-1");
    assert_eq!(
        state_view.accounts[0].email.as_deref(),
        Some("one@example.test")
    );
    assert_eq!(
        state_view.selected_account_id.as_deref(),
        Some("agy-antigravity-profile-1")
    );
    // Legacy user files untouched!
    assert_eq!(
        std::fs::read_to_string(prof1.join(".gemini/antigravity-cli/settings.json")).unwrap(),
        r#"{"keep":1}"#
    );
    assert_eq!(
        std::fs::read_to_string(prof2.join(".gemini/antigravity-cli/history.jsonl")).unwrap(),
        "{\"h\":2}\n"
    );

    // File backend CAS switch works end-to-end against home token file.
    let home_tok = antigravity_token_file_path(&home);
    std::fs::remove_file(&home_tok).ok();
    assert_eq!(
        super::backend::agy_credential_presence(&home),
        super::backend::AgyCredentialPresence::Absent
    );
    let file_backend = FileCredentialBackend::new(home_tok);
    assert!(file_backend.read().unwrap().is_none());
    let selected = handle_op(
        &home,
        Some(&state),
        &AntigravityAccountOp::Select {
            account_id: "agy-antigravity-profile-1".into(),
        },
    )
    .unwrap();
    assert_eq!(
        super::backend::agy_credential_presence(&home),
        super::backend::AgyCredentialPresence::Present
    );
    assert_eq!(
        selected.active_account_id.as_deref(),
        Some("agy-antigravity-profile-1")
    );
    assert_eq!(
        file_backend.read().unwrap().unwrap().contents.as_str(),
        tok1
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn switching_agy_account_swaps_host_token_and_preserves_refreshed_host_token() {
    let root = std::env::temp_dir().join(format!("krill-agy-native-swap-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let prof1 = root.join("prof1");
    let prof2 = root.join("prof2");
    for dir in [&home, &prof1, &prof2] {
        std::fs::create_dir_all(dir.join(".gemini/antigravity-cli")).unwrap();
    }
    let tok1_initial = br#"{"auth_method":"consumer","id_token_email":"one@example.test","token":{"access_token":"tok1-old"}}"#;
    let tok1_refreshed = br#"{"auth_method":"consumer","id_token_email":"one@example.test","token":{"access_token":"tok1-refreshed"}}"#;
    let tok2 = br#"{"auth_method":"consumer","id_token_email":"two@example.test","token":{"access_token":"tok2"}}"#;
    std::fs::write(
        prof1.join(".gemini/antigravity-cli/antigravity-oauth-token"),
        tok1_initial,
    )
    .unwrap();
    std::fs::write(
        prof2.join(".gemini/antigravity-cli/antigravity-oauth-token"),
        tok2,
    )
    .unwrap();

    let mut file = AccountsFile {
        accounts: vec![
            StoredAccount {
                id: "agy-1".into(),
                provider: "agy".into(),
                label: "One".into(),
                email: Some("one@example.test".into()),
                profile_dir: prof1.display().to_string(),
            },
            StoredAccount {
                id: "agy-2".into(),
                provider: "agy".into(),
                label: "Two".into(),
                email: Some("two@example.test".into()),
                profile_dir: prof2.display().to_string(),
            },
        ],
        ..AccountsFile::default()
    };

    file.active.insert("agy".into(), "agy-1".into());
    sync_active(&home, &file);
    assert_eq!(
        std::fs::read(home.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
        tok1_initial
    );

    let host_dir = crate::usage::accounts::host_default_dir(&home, ProviderKind::Antigravity);
    crate::usage::meters::remember(
        &home,
        ProviderKind::Antigravity,
        &host_dir,
        krill_proto::usage::AccountMeters {
            account_id: "agy-host".into(),
            plan: None,
            meters: vec![krill_proto::usage::UsageMeter {
                key: "gemini_weekly".into(),
                label: "Gemini · 7d".into(),
                value: None,
                window: None,
            }],
        },
    );
    assert!(crate::usage::meters::has_cached(
        &home,
        ProviderKind::Antigravity,
        &host_dir
    ));
    std::fs::write(
        home.join(".gemini/antigravity-cli/antigravity-oauth-token"),
        tok1_refreshed,
    )
    .unwrap();
    file.active.insert("agy".into(), "agy-2".into());
    sync_active(&home, &file);
    assert!(!crate::usage::meters::has_cached(
        &home,
        ProviderKind::Antigravity,
        &host_dir
    ));
    assert_eq!(
        std::fs::read(prof1.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
        tok1_refreshed
    );
    assert_eq!(
        std::fs::read(home.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
        tok2
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn switching_claude_codex_and_opencode_accounts_keeps_the_default_login() {
    let root =
        std::env::temp_dir().join(format!("krill-default-login-kept-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let mut cases = Vec::new();
    for (provider, host, profile) in [
        (
            ProviderKind::Claude,
            ".claude/.credentials.json",
            ".credentials.json",
        ),
        (ProviderKind::Codex, ".codex/auth.json", "auth.json"),
        (
            ProviderKind::OpenCode,
            ".local/share/opencode/auth.json",
            "data/opencode/auth.json",
        ),
    ] {
        let dir = root.join(provider.as_str());
        let (host, profile) = (home.join(host), dir.join(profile));
        for path in [&host, &profile] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        std::fs::write(&host, br#"{"default":"login"}"#).unwrap();
        std::fs::write(&profile, br#"{"profile":"login"}"#).unwrap();
        cases.push((provider, dir, host));
    }
    let mut file = AccountsFile::default();
    for (provider, dir, host) in &cases {
        sync_provider_account_dir(&home, None, *provider, dir);
        file.accounts.push(StoredAccount {
            id: provider.as_str().into(),
            provider: provider.as_str().into(),
            label: provider.as_str().into(),
            email: None,
            profile_dir: dir.display().to_string(),
        });
        file.active
            .insert(provider.as_str().into(), provider.as_str().into());
        assert_eq!(std::fs::read(host).unwrap(), br#"{"default":"login"}"#);
    }
    sync_active(&home, &file);
    for (_, _, host) in &cases {
        assert_eq!(std::fs::read(host).unwrap(), br#"{"default":"login"}"#);
    }
    std::fs::remove_dir_all(root).ok();
}

fn codex_jwt(payload: &serde_json::Value) -> String {
    let encoded = encode_b64url(&serde_json::to_vec(payload).unwrap());
    format!("eyJhbGciOiJub25lIn0.{encoded}.sig")
}

fn codex_auth_payload(
    last_refresh: &str,
    access_iat: i64,
    access_exp: i64,
    id_iat: i64,
    id_exp: i64,
) -> String {
    serde_json::json!({
        "last_refresh": last_refresh,
        "tokens": {
            "access_token": codex_jwt(&serde_json::json!({
                "iat": access_iat,
                "exp": access_exp,
            })),
            "id_token": codex_jwt(&serde_json::json!({
                "email": "alpha@example.test",
                "iat": id_iat,
                "exp": id_exp,
            })),
            "account_id": "acc-relogin",
        }
    })
    .to_string()
}

#[test]
fn codex_migration_moves_only_a_newer_same_identity_default_token_and_never_writes_default() {
    let root =
        std::env::temp_dir().join(format!("krill-codex-host-migration-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let prof = root.join("prof");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(&prof).unwrap();
    let older = codex_auth_payload(
        "2026-10-28T11:02:37Z",
        1_793_185_356,
        1_794_049_356,
        1_793_185_356,
        1_793_188_956,
    );
    let newer = codex_auth_payload(
        "2026-11-08T14:24:24Z",
        1_794_147_864,
        1_795_011_864,
        1_794_147_864,
        1_794_151_464,
    );
    let accounts = vec![StoredAccount {
        id: "codex-alpha".into(),
        provider: "codex".into(),
        label: "alpha@example.test".into(),
        email: Some("alpha@example.test".into()),
        profile_dir: prof.display().to_string(),
    }];
    let read = |path: &Path| std::fs::read(path).unwrap();

    std::fs::write(prof.join("auth.json"), &newer).unwrap();
    std::fs::write(home.join(".codex/auth.json"), &older).unwrap();
    preserve_codex_host(&home, &accounts);
    assert_eq!(read(&prof.join("auth.json")), newer.as_bytes());
    assert_eq!(read(&home.join(".codex/auth.json")), older.as_bytes());

    std::fs::write(prof.join("auth.json"), &older).unwrap();
    std::fs::write(home.join(".codex/auth.json"), &newer).unwrap();
    preserve_codex_host(&home, &accounts);
    assert_eq!(read(&prof.join("auth.json")), newer.as_bytes());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
#[cfg(unix)]
fn keychain_command_timeout_kills_hanging_process() {
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", "sleep 30"]);
    let started = Instant::now();
    let out = backend::run_helper_with_timeout(
        &mut cmd,
        None,
        true,
        false,
        1024,
        Duration::from_millis(80),
    );
    assert!(out.is_some_and(|o| o.timed_out));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "hanging helper must be killed promptly on timeout"
    );
}

#[test]
fn preflight_auth_backoff_handles_present_absent_and_transient_locked() {
    use backend::{
        AgyAuthPreflightError, AgyCredentialPresence, preflight_auth_with_backoff_using,
    };

    let delays = [Duration::from_millis(1), Duration::from_millis(1)];

    // Immediate Present: single probe, succeeds.
    let mut calls = 0;
    let res = preflight_auth_with_backoff_using(&delays, || {
        calls += 1;
        AgyCredentialPresence::Present
    });
    assert_eq!(res, Ok(()));
    assert_eq!(calls, 1);

    // Immediate Absent: never waits or retries.
    let mut calls = 0;
    let res = preflight_auth_with_backoff_using(&delays, || {
        calls += 1;
        AgyCredentialPresence::Absent
    });
    assert_eq!(res, Err(AgyAuthPreflightError::Absent));
    assert_eq!(calls, 1);

    // Transient Locked after reboot -> Present on 3rd attempt.
    let mut calls = 0;
    let res = preflight_auth_with_backoff_using(&delays, || {
        calls += 1;
        if calls < 3 {
            AgyCredentialPresence::Locked
        } else {
            AgyCredentialPresence::Present
        }
    });
    assert_eq!(res, Ok(()));
    assert_eq!(calls, 3);

    // Persistent Locked -> exhausts backoff and fails with Locked.
    let mut calls = 0;
    let res = preflight_auth_with_backoff_using(&delays, || {
        calls += 1;
        AgyCredentialPresence::Locked
    });
    assert_eq!(res, Err(AgyAuthPreflightError::Locked));
    assert_eq!(calls, 3);
}

#[test]
fn update_snapshot_backfills_subject_for_migrated_account_without_validation_error() {
    let root =
        std::env::temp_dir().join(format!("krill-agy-subject-backfill-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    // Seed host token WITHOUT id_token (just id_token_email, matching real agy 1.5.17 consumer OAuth).
    let initial = serde_json::json!({
        "auth_method": "consumer",
        "id_token_email": "owner@example.test",
        "token": {
            "access_token": "tok-1",
            "refresh_token": "ref-1",
            "token_type": "Bearer"
        }
    })
    .to_string();
    let token_path = antigravity_token_file_path(&home);
    write_atomic_0600(&token_path, initial.as_bytes()).unwrap();

    let svc = AntigravityAccountService::for_host(&home, Some(&state));
    // Simulate a legacy migrated snapshot whose stored credential lacked an OIDC id_token (`subject: None`).
    svc.import_snapshots(
        vec![StoredAntigravityAccount {
            id: "agy-migrated-1".into(),
            email: Some("owner@example.test".into()),
            subject: None,
            auth_method: "consumer".into(),
            created_at: 1,
            updated_at: 1,
            credentials: Zeroizing::new(initial),
            usage: None,
        }],
        Some("agy-migrated-1"),
    )
    .unwrap();
    let before = svc.list_accounts().unwrap();
    assert_eq!(before.accounts.len(), 1);
    assert_eq!(before.accounts[0].subject, None);

    // Now simulate agy refreshing the token with an OIDC id_token containing `sub`.
    let id_token = codex_jwt(&serde_json::json!({
        "sub": "google-sub-123",
        "email": "owner@example.test",
        "iss": "https://accounts.google.com"
    }));
    let refreshed = serde_json::json!({
        "auth_method": "consumer",
        "id_token_email": "owner@example.test",
        "token": {
            "access_token": "tok-2",
            "refresh_token": "ref-1",
            "id_token": id_token,
            "token_type": "Bearer"
        }
    })
    .to_string();
    write_atomic_0600(&token_path, refreshed.as_bytes()).unwrap();

    // Listing reconciles and writes the updated snapshot — must backfill `subject` instead of
    // tripping `validate_vault`'s "inconsistent identity" error.
    let listed = svc.list_accounts().unwrap();
    assert_eq!(listed.accounts.len(), 1);
    assert_eq!(
        listed.accounts[0].subject.as_deref(),
        Some("google-sub-123")
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn migrate_and_prune_removes_legacy_agy_rows_while_preserving_other_providers() {
    let root = std::env::temp_dir().join(format!("krill-agy-legacy-prune-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    let raw_toml = r#"# User comment must survive pruning
[active]
claude = "claude-work"
agy = "agy-antigravity-profile-1"

[[accounts]]
id = "claude-work"
provider = "claude"
label = "Claude Work"
email = "work@example.test"
profile_dir = "/tmp/claude-work"

[[accounts]]
id = "codex-personal"
provider = "codex"
label = "Codex Personal"
profile_dir = "/tmp/codex-personal"

[[accounts]]
id = "agy-antigravity-profile-1"
provider = "agy"
label = "Antigravity Profile #1"
profile_dir = "/tmp/missing-agy-1"

[[accounts]]
id = "agy-antigravity-profile-2"
provider = "agy"
label = "Antigravity Profile #2"
profile_dir = "/tmp/missing-agy-2"
"#;
    let config_path = crate::usage::accounts::config_path(&home, Some(&state));
    std::fs::write(&config_path, raw_toml).unwrap();

    service::migrate_and_prune_legacy_agy_profiles(&home, Some(&state), true);

    let after = crate::usage::accounts::load_file(&home, Some(&state));
    assert!(
        after
            .accounts
            .iter()
            .all(|a| ProviderKind::parse(&a.provider) != Some(ProviderKind::Antigravity)),
        "legacy agy rows must be pruned: {:?}",
        after.accounts
    );
    assert_eq!(after.accounts.len(), 2);
    assert!(after.accounts.iter().any(|a| a.id == "claude-work"));
    assert!(after.accounts.iter().any(|a| a.id == "codex-personal"));
    assert_eq!(
        after.active.get("claude").map(String::as_str),
        Some("claude-work")
    );
    assert!(!after.active.contains_key("agy"));
    let saved_text = std::fs::read_to_string(&config_path).unwrap();
    assert!(saved_text.contains("# User comment must survive pruning"));
    assert!(
        vault_path(&home, Some(&state))
            .parent()
            .unwrap()
            .join(".legacy-pruned-v2")
            .is_file()
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn refresh_accounts_with_native_login_hides_legacy_ghost_rows_without_vault() {
    let root = std::env::temp_dir().join(format!("krill-agy-no-ghosts-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    let host_token = serde_json::json!({
        "auth_method": "consumer",
        "id_token_email": "native@example.test",
        "token": {
            "access_token": "tok-native",
            "refresh_token": "ref-native",
            "token_type": "Bearer"
        }
    })
    .to_string();
    write_atomic_0600(&antigravity_token_file_path(&home), host_token.as_bytes()).unwrap();

    let file = AccountsFile {
        accounts: vec![StoredAccount {
            id: "agy-antigravity-profile-1".into(),
            provider: "agy".into(),
            label: "Antigravity Profile #1".into(),
            email: None,
            profile_dir: root.join("missing-profile").display().to_string(),
        }],
        ..AccountsFile::default()
    };
    crate::usage::accounts::save_file(&home, Some(&state), &file).unwrap();
    assert!(!vault_path(&home, Some(&state)).is_file());

    let snap = crate::usage::accounts::refresh_accounts(&home, Some(&state));
    let agy_rows: Vec<_> = snap
        .accounts
        .iter()
        .filter(|a| a.provider == ProviderKind::Antigravity)
        .collect();
    assert_eq!(
        agy_rows.len(),
        1,
        "ghost rows must not appear: {agy_rows:?}"
    );
    assert_eq!(agy_rows[0].id, "agy-host");
    assert_eq!(agy_rows[0].email.as_deref(), Some("native@example.test"));
    assert_eq!(
        snap.connection_for("agy-host"),
        krill_proto::usage::ConnectionStatus::Connected
    );

    std::fs::remove_dir_all(root).ok();
}

struct LockableMemoryStore {
    vault: Arc<Mutex<AntigravityAccountVault>>,
    locked_status: Arc<Mutex<Option<i32>>>,
    reset_calls: Arc<Mutex<usize>>,
}

impl AntigravityAccountStore for LockableMemoryStore {
    fn read(&self) -> Result<AntigravityAccountVault, VaultError> {
        if let Some(status) = *self.locked_status.lock().unwrap() {
            return Err(VaultError::LegacyAclLocked { status });
        }
        Ok(self.vault.lock().unwrap().clone())
    }

    fn write(&self, vault: &AntigravityAccountVault) -> Result<(), String> {
        if let Some(status) = *self.locked_status.lock().unwrap() {
            return Err(VaultError::LegacyAclLocked { status }.into_message());
        }
        *self.vault.lock().unwrap() = vault.clone();
        Ok(())
    }

    fn reset_locked_vault(&self) -> Result<(), String> {
        *self.reset_calls.lock().unwrap() += 1;
        *self.locked_status.lock().unwrap() = None;
        *self.vault.lock().unwrap() = AntigravityAccountVault::default();
        Ok(())
    }
}

#[test]
fn locked_legacy_vault_preserves_host_login_and_recovers_cleanly() {
    let host_cred = agy_credential_json(
        "sub-host",
        "owner@example.test",
        "acc-host",
        "ref-host",
        true,
    );
    let backend_raw = Arc::new(Mutex::new(Some(host_cred.clone())));
    let locked_status = Arc::new(Mutex::new(Some(-25308)));
    let reset_calls = Arc::new(Mutex::new(0));
    let vault = Arc::new(Mutex::new(AntigravityAccountVault::default()));
    let svc = AntigravityAccountService::new(
        Box::new(LockableMemoryStore {
            vault: vault.clone(),
            locked_status: locked_status.clone(),
            reset_calls: reset_calls.clone(),
        }),
        Box::new(MemoryBackend {
            raw: backend_raw.clone(),
        }),
        Box::new(|| 1_700_000_000_000),
        Box::new(|_| Err("offline".to_owned())),
    );

    // 1. Listing while locked returns Ok with vault_locked=true and real host login.
    let listed = svc.list_accounts().unwrap();
    assert!(listed.vault_locked);
    assert!(listed.accounts.is_empty());
    assert_eq!(
        listed
            .current_account
            .as_ref()
            .and_then(|c| c.email.as_deref()),
        Some("owner@example.test")
    );

    // 2. Usage read and launch preflight do NOT error while vault is locked.
    assert!(svc.begin_usage_read().unwrap().is_none());
    svc.prepare_for_launch().unwrap();

    // 3. Recovery resets locked vault, re-saves host login, and leaves host credential untouched.
    let recovered = svc.reset_locked_vault().unwrap();
    assert!(!recovered.vault_locked);
    assert_eq!(*reset_calls.lock().unwrap(), 1);
    assert_eq!(recovered.accounts.len(), 1);
    assert_eq!(
        recovered.accounts[0].email.as_deref(),
        Some("owner@example.test")
    );
    assert_eq!(
        recovered.active_account_id.as_deref(),
        Some(recovered.accounts[0].id.as_str())
    );
    assert_eq!(
        backend_raw.lock().unwrap().as_deref(),
        Some(host_cred.as_str())
    );

    // 4. Calling recover again on a healthy unlocked vault is a safe no-op and keeps saved accounts.
    let idempotent = svc.reset_locked_vault().unwrap();
    assert!(!idempotent.vault_locked);
    assert_eq!(*reset_calls.lock().unwrap(), 1);
    assert_eq!(idempotent.accounts.len(), 1);
}

#[test]
fn encrypted_store_reset_locked_vault_backs_up_unreadable_file_and_preserves_other_accounts() {
    let root =
        std::env::temp_dir().join(format!("krill-agy-recover-backup-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    // Seed Claude/Codex accounts.toml and host agy token — none may be modified by recovery.
    let accounts_file = AccountsFile {
        accounts: vec![
            StoredAccount {
                id: "claude-work".into(),
                provider: "claude".into(),
                label: "Work".into(),
                email: Some("work@example.test".into()),
                profile_dir: String::new(),
            },
            StoredAccount {
                id: "codex-personal".into(),
                provider: "codex".into(),
                label: "Personal".into(),
                email: Some("codex@example.test".into()),
                profile_dir: String::new(),
            },
        ],
        ..AccountsFile::default()
    };
    crate::usage::accounts::save_file(&home, Some(&state), &accounts_file).unwrap();
    let host_cred = agy_credential_json("sub-1", "alice@example.test", "acc-1", "ref-1", true);
    let host_token_path = antigravity_token_file_path(&home);
    write_atomic_0600(&host_token_path, host_cred.as_bytes()).unwrap();

    // Write an unreadable/corrupted vault file (simulating a vault encrypted under an unreachable key).
    let vpath = vault_path(&home, Some(&state));
    std::fs::create_dir_all(vpath.parent().unwrap()).unwrap();
    write_atomic_0600(&vpath, b"unreadable-legacy-ciphertext").unwrap();

    let store = EncryptedAntigravityAccountStore::new(vpath.clone());
    assert!(store.read().is_err());

    let recovered =
        handle_op(&home, Some(&state), &AntigravityAccountOp::ResetLockedVault).unwrap();
    assert!(!recovered.vault_locked);
    assert_eq!(recovered.accounts.len(), 1);
    assert_eq!(
        recovered.accounts[0].email.as_deref(),
        Some("alice@example.test")
    );

    // Verify backup file exists with exact old ciphertext.
    let backups: Vec<_> = std::fs::read_dir(vpath.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n.starts_with("vault.locked-")
                    && std::path::Path::new(n)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("bak"))
            })
        })
        .collect();
    assert_eq!(backups.len(), 1, "expected exactly one vault backup");
    assert_eq!(
        std::fs::read(backups[0].path()).unwrap(),
        b"unreadable-legacy-ciphertext"
    );

    // Verify host agy token and Claude/Codex accounts.toml are completely untouched.
    assert_eq!(
        std::fs::read_to_string(&host_token_path).unwrap(),
        host_cred
    );
    let after_accounts = crate::usage::accounts::load_file(&home, Some(&state));
    assert_eq!(after_accounts.accounts.len(), 2);
    assert!(
        after_accounts
            .accounts
            .iter()
            .any(|a| a.id == "claude-work")
    );
    assert!(
        after_accounts
            .accounts
            .iter()
            .any(|a| a.id == "codex-personal")
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn for_host_interns_service_instance_sharing_op_lock_and_rewritten_cache() {
    let root = std::env::temp_dir().join(format!("krill-agy-intern-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    let svc1 = AntigravityAccountService::for_host(&home, Some(&state));
    let svc2 = AntigravityAccountService::for_host(&home, Some(&state));
    assert!(
        std::sync::Arc::ptr_eq(&svc1, &svc2),
        "for_host must intern Arc<AntigravityAccountService> per canonical (home, state_dir)"
    );

    // Concurrent read-modify-write via two `for_host()` handles must serialize cleanly under op_lock.
    let home_a = home.clone();
    let state_a = state.clone();
    let home_b = home.clone();
    let state_b = state.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let b1 = std::sync::Arc::clone(&barrier);
    let b2 = std::sync::Arc::clone(&barrier);

    let cred_a = serde_json::json!({
        "auth_method": "consumer",
        "id_token_email": "alice@example.test",
        "token": {
            "access_token": "tok-a",
            "refresh_token": "ref-a",
            "id_token": codex_jwt(&serde_json::json!({
                "sub": "sub-a",
                "email": "alice@example.test",
                "iss": "https://accounts.google.com"
            })),
            "token_type": "Bearer"
        }
    })
    .to_string();
    let cred_b = serde_json::json!({
        "auth_method": "consumer",
        "id_token_email": "bob@example.test",
        "token": {
            "access_token": "tok-b",
            "refresh_token": "ref-b",
            "id_token": codex_jwt(&serde_json::json!({
                "sub": "sub-b",
                "email": "bob@example.test",
                "iss": "https://accounts.google.com"
            })),
            "token_type": "Bearer"
        }
    })
    .to_string();

    let t1 = std::thread::spawn(move || {
        b1.wait();
        AntigravityAccountService::for_host(&home_a, Some(&state_a))
            .import_snapshots(
                vec![StoredAntigravityAccount {
                    id: "acc-a".into(),
                    email: Some("alice@example.test".into()),
                    subject: Some("sub-a".into()),
                    auth_method: "consumer".into(),
                    created_at: 1,
                    updated_at: 1,
                    credentials: Zeroizing::new(cred_a),
                    usage: None,
                }],
                None,
            )
            .unwrap();
    });
    let t2 = std::thread::spawn(move || {
        b2.wait();
        AntigravityAccountService::for_host(&home_b, Some(&state_b))
            .import_snapshots(
                vec![StoredAntigravityAccount {
                    id: "acc-b".into(),
                    email: Some("bob@example.test".into()),
                    subject: Some("sub-b".into()),
                    auth_method: "consumer".into(),
                    created_at: 2,
                    updated_at: 2,
                    credentials: Zeroizing::new(cred_b),
                    usage: None,
                }],
                None,
            )
            .unwrap();
    });
    t1.join().unwrap();
    t2.join().unwrap();

    let listed = AntigravityAccountService::for_host(&home, Some(&state))
        .list_accounts()
        .unwrap();
    assert_eq!(
        listed.accounts.len(),
        2,
        "concurrent for_host() vault writes must both survive under shared op_lock"
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn rewritten_login_identity_lookup_hits_cache_on_shared_service() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let store = MemoryStore {
        vault: Arc::new(Mutex::new(AntigravityAccountVault::default())),
    };
    let raw_slot = Arc::new(Mutex::new(Some(agy_credential_json(
        "sub-initial",
        "initial@example.test",
        "tok-initial",
        "ref-initial",
        true,
    ))));
    let backend = MemoryBackend {
        raw: Arc::clone(&raw_slot),
    };
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let calls_bg = std::sync::Arc::clone(&calls);
    let svc = std::sync::Arc::new(AntigravityAccountService::new(
        Box::new(store),
        Box::new(backend),
        Box::new(|| 100),
        Box::new(move |_| {
            calls_bg.fetch_add(1, Ordering::AcqRel);
            Ok(AntigravityCredentialIdentity {
                subject: "sub-rewritten".into(),
                email: Some("rewritten@example.test".into()),
                issuer: "https://accounts.google.com",
            })
        }),
    ));
    // Register initial account with a subject into the vault.
    let _ = svc.add_current_account().unwrap();
    // Simulate a rewritten login without an embedded id_token JWT.
    *raw_slot.lock().unwrap() = Some(agy_credential_json(
        "sub-initial",
        "initial@example.test",
        "tok-rewritten-1",
        "ref-rewritten",
        false,
    ));
    let _ = svc.list_accounts().unwrap();
    let _ = svc.list_accounts().unwrap();
    assert_eq!(
        calls.load(Ordering::Acquire),
        1,
        "shared service must cache rewritten login identity across polls"
    );
}

#[test]
fn prepare_for_launch_handles_legacy_acl_locked_silently_after_first_warn() {
    struct LockedStore;
    impl AntigravityAccountStore for LockedStore {
        fn read(&self) -> Result<AntigravityAccountVault, VaultError> {
            Err(VaultError::LegacyAclLocked { status: -25293 })
        }
        fn write(&self, _vault: &AntigravityAccountVault) -> Result<(), String> {
            Err("locked".into())
        }
        fn reset_locked_vault(&self) -> Result<(), String> {
            Ok(())
        }
    }
    let svc = AntigravityAccountService::new(
        Box::new(LockedStore),
        Box::new(MemoryBackend {
            raw: Arc::new(Mutex::new(None)),
        }),
        Box::new(|| 1),
        Box::new(|_| Err("offline".into())),
    );
    // Repeated launch preflights must succeed (skip preflight gracefully) on locked legacy vault.
    assert!(svc.prepare_for_launch().is_ok());
    assert!(svc.prepare_for_launch().is_ok());
}

#[test]
fn launch_restore_restores_selected_saved_account_and_defers_to_active_sign_in() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let cred_a = agy_credential_json("sub-a", "alice@example.test", "acc-a1", "ref-a", true);
    let cred_b = agy_credential_json("sub-b", "bob@example.test", "acc-b1", "ref-b", true);
    let cred_b_refreshed =
        agy_credential_json("sub-b", "bob@example.test", "acc-b2", "ref-b", false);
    let cred_external = agy_credential_json("sub-c", "ext@example.test", "acc-c1", "ref-c", true);

    let vault_mem = Arc::new(Mutex::new(AntigravityAccountVault::default()));
    let native = Arc::new(Mutex::new(Some(cred_a.clone())));
    let sign_in_active = Arc::new(AtomicBool::new(false));
    let guard_flag = Arc::clone(&sign_in_active);

    let svc = AntigravityAccountService::new_with_launch_restore(
        Box::new(MemoryStore {
            vault: vault_mem.clone(),
        }),
        Box::new(MemoryBackend {
            raw: native.clone(),
        }),
        Box::new(|| 1_700_000_000_000),
        Box::new(|_| Err("offline".to_owned())),
        Some(Box::new(move || guard_flag.load(Ordering::Acquire))),
    );

    let s1 = svc.add_current_account().unwrap();
    assert!(s1.restores_selection_at_launch);
    let id_a = s1.accounts[0].id.clone();

    *native.lock().unwrap() = Some(cred_b);
    let s2 = svc.add_current_account().unwrap();
    let id_b = s2.accounts[1].id.clone();

    svc.select_account(&id_a).unwrap();

    // Simulate a still-running agy on saved account B refreshing its token over the native slot.
    *native.lock().unwrap() = Some(cred_b_refreshed.clone());
    let moved = svc.list_accounts().unwrap();
    assert_eq!(moved.active_account_id.as_deref(), Some(id_b.as_str()));
    assert_eq!(moved.selected_account_id.as_deref(), Some(id_a.as_str()));
    assert_eq!(moved.launches_use_account(), Some(id_a.as_str()));

    // While an in-app sign-in holds the native slot, launch must refuse rather than overwrite it.
    sign_in_active.store(true, Ordering::Release);
    let err = svc.prepare_for_launch().unwrap_err();
    assert!(err.contains("sign-in is in progress"), "{err}");
    sign_in_active.store(false, Ordering::Release);

    // Once idle, launch preflight preserves B's refreshed token in the vault and restores A natively.
    svc.prepare_for_launch().unwrap();
    let after = svc.list_accounts().unwrap();
    assert_eq!(after.active_account_id.as_deref(), Some(id_a.as_str()));
    assert_eq!(after.launches_use_account(), None);
    let stored_b = vault_mem
        .lock()
        .unwrap()
        .accounts
        .iter()
        .find(|a| a.id == id_b)
        .unwrap()
        .credentials
        .as_str()
        .to_owned();
    assert_eq!(stored_b, cred_b_refreshed);

    // Unsaved external login still refuses launch so the unsaved credential is never overwritten.
    *native.lock().unwrap() = Some(cred_external);
    let ext_err = svc.prepare_for_launch().unwrap_err();
    assert!(
        ext_err.contains("native Antigravity account changed"),
        "{ext_err}"
    );
    // Even if the native slot is cleared or replaced by an unsaved credential during sign-in,
    // `holds_login` takes precedence and reports sign-in-in-progress.
    *native.lock().unwrap() = None;
    sign_in_active.store(true, Ordering::Release);
    let active_err = svc.prepare_for_launch().unwrap_err();
    assert!(
        active_err.contains("sign-in is in progress"),
        "{active_err}"
    );
}

#[cfg(unix)]
#[test]
fn in_app_sign_in_add_account_saves_existing_completes_oauth_and_selects_new_account() {
    use krill_proto::usage::{AntigravitySignInIntent, AntigravitySignInPhase};
    use std::os::unix::fs::PermissionsExt as _;

    let root = std::env::temp_dir().join(format!("krill-agy-signin-e2e-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    // Seed existing unsaved host login (Account A).
    let cred_a = agy_credential_json("sub-a", "first@example.test", "acc-a", "ref-a", true);
    let cred_b = agy_credential_json("sub-b", "second@example.test", "acc-b", "ref-b", true);
    let token_path = antigravity_token_file_path(&home);
    write_atomic_0600(&token_path, cred_a.as_bytes()).unwrap();

    let fake_agy = root.join("fake-agy");
    let cred_b_path = root.join("cred-b.json");
    std::fs::write(&cred_b_path, &cred_b).unwrap();
    let script = format!(
        r#"#!/bin/sh
if [ "$1" != "acp" ] || [ "$2" != "--login" ]; then
  echo "unexpected args: $@" >&2
  exit 9
fi
echo "Open this link in a browser to log in with Google:"
echo "  https://accounts.google.com/o/oauth2/auth?response_type=code&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&state=test-state-1"
echo "Then paste the authorization code:"
read -r line
if [ "$line" != "https://antigravity.google/oauth-callback?code=good-code&state=test-state-1" ]; then
  echo 'token exchange failed: oauth2: "invalid_grant"' >&2
  exit 1
fi
cp "{}" "{}"
chmod 600 "{}"
exit 0
"#,
        cred_b_path.display(),
        token_path.display(),
        token_path.display(),
    );
    std::fs::write(&fake_agy, script).unwrap();
    std::fs::set_permissions(&fake_agy, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Start AddAccount: must save existing Account A into vault BEFORE replacing it.
    let flow_id = sign_in::start_with_exe(
        &home,
        Some(&state),
        AntigravitySignInIntent::AddAccount,
        &fake_agy,
        Duration::from_secs(10),
    )
    .unwrap();

    // Wait for Google OAuth URL.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let waiting = loop {
        let status = sign_in::status_for(&home, Some(&state), Some(&flow_id)).unwrap();
        if status.phase == AntigravitySignInPhase::Waiting {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for URL: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        waiting
            .url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://accounts.google.com/o/oauth2/auth?"))
    );

    // Another caller without the capability flow_id must NOT see flow_id or url, and cannot act.
    let redacted = sign_in::status_for(&home, Some(&state), None).unwrap();
    assert_eq!(redacted.flow_id, None);
    assert_eq!(redacted.url, None);
    assert_eq!(redacted.phase, AntigravitySignInPhase::Waiting);
    assert!(sign_in::complete(&home, Some(&state), "wrong-id", "good-code").is_err());
    assert!(sign_in::cancel(&home, Some(&state), "wrong-id").is_err());
    assert!(
        sign_in::start_with_exe(
            &home,
            Some(&state),
            AntigravitySignInIntent::SignIn,
            &fake_agy,
            Duration::from_secs(10),
        )
        .is_err()
    );

    // Invalid answer is rejected cleanly while keeping the flow waiting.
    assert!(sign_in::complete(&home, Some(&state), &flow_id, "not a valid answer").is_err());
    assert_eq!(
        sign_in::status_for(&home, Some(&state), Some(&flow_id))
            .unwrap()
            .phase,
        AntigravitySignInPhase::Waiting
    );

    // Account mutations are fenced while sign-in owns the native credential slot.
    let fenced_err = handle_op(
        &home,
        Some(&state),
        &AntigravityAccountOp::Select {
            account_id: "any".into(),
        },
    )
    .unwrap_err();
    assert!(
        fenced_err.contains("sign-in is in progress"),
        "{fenced_err}"
    );

    // Deliver the callback URL; flow verifies, saves & selects Account B.
    sign_in::complete(
        &home,
        Some(&state),
        &flow_id,
        "https://antigravity.google/oauth-callback?code=good-code&state=test-state-1",
    )
    .unwrap();

    // Polling via `handle_op(SignInStatus)` observes `SignedIn` paired with the freshly saved
    // 2-account list (no stale pre-save snapshot).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let final_state = loop {
        let st = handle_op(
            &home,
            Some(&state),
            &AntigravityAccountOp::SignInStatus {
                flow_id: Some(flow_id.clone()),
            },
        )
        .unwrap();
        if st.sign_in.as_ref().map(|s| s.phase) == Some(AntigravitySignInPhase::SignedIn) {
            break st;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "did not reach SignedIn: {st:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!sign_in::holds_native_login(&home, Some(&state)));
    assert!(final_state.sign_in_supported);
    assert_eq!(final_state.accounts.len(), 2);
    let emails: Vec<_> = final_state
        .accounts
        .iter()
        .filter_map(|a| a.email.as_deref())
        .collect();
    assert!(emails.contains(&"first@example.test"), "{emails:?}");
    assert!(emails.contains(&"second@example.test"), "{emails:?}");
    let id_b = final_state
        .accounts
        .iter()
        .find(|a| a.email.as_deref() == Some("second@example.test"))
        .unwrap()
        .id
        .clone();
    assert_eq!(
        final_state.active_account_id.as_deref(),
        Some(id_b.as_str())
    );
    assert_eq!(
        final_state.selected_account_id.as_deref(),
        Some(id_b.as_str())
    );
    assert!(!final_state.native_account_changed());

    std::fs::remove_dir_all(root).ok();
}

#[cfg(unix)]
#[test]
fn in_app_sign_in_cancel_and_failure_classification_work_cleanly() {
    use krill_proto::usage::{
        ANTIGRAVITY_INVALID_GRANT, AntigravitySignInIntent, AntigravitySignInPhase,
    };
    use std::os::unix::fs::PermissionsExt as _;

    let root = std::env::temp_dir().join(format!(
        "krill-agy-signin-cancel-fail-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    // 1. Cancel kills waiting process and releases host lock.
    let fake_wait = root.join("fake-wait");
    std::fs::write(
        &fake_wait,
        "#!/bin/sh\necho 'https://accounts.google.com/o/oauth2/v2/auth?response_type=code&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&state=cancel-me'\nread -r _\nsleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_wait, std::fs::Permissions::from_mode(0o700)).unwrap();

    let flow_id = sign_in::start_with_exe(
        &home,
        Some(&state),
        AntigravitySignInIntent::SignIn,
        &fake_wait,
        Duration::from_secs(10),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while sign_in::status_for(&home, Some(&state), Some(&flow_id))
        .unwrap()
        .phase
        != AntigravitySignInPhase::Waiting
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    sign_in::cancel(&home, Some(&state), &flow_id).unwrap();
    let cancelled = sign_in::status_for(&home, Some(&state), Some(&flow_id)).unwrap();
    assert_eq!(cancelled.phase, AntigravitySignInPhase::Cancelled);
    assert!(!sign_in::holds_native_login(&home, Some(&state)));

    // 2. Rejected code surfaces friendly invalid_grant failure.
    let fake_fail = root.join("fake-fail");
    std::fs::write(
        &fake_fail,
        r#"#!/bin/sh
echo 'https://accounts.google.com/o/oauth2/auth?response_type=code&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&state=fail-me'
read -r _
printf '%s\n' 'could not log in: token exchange failed: oauth2: "invalid_grant"' >&2
exit 1
"#,
    )
    .unwrap();
    std::fs::set_permissions(&fake_fail, std::fs::Permissions::from_mode(0o700)).unwrap();

    let flow_id = sign_in::start_with_exe(
        &home,
        Some(&state),
        AntigravitySignInIntent::SignIn,
        &fake_fail,
        Duration::from_secs(10),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while sign_in::status_for(&home, Some(&state), Some(&flow_id))
        .unwrap()
        .phase
        != AntigravitySignInPhase::Waiting
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    sign_in::complete(&home, Some(&state), &flow_id, "bad-code").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let failed = loop {
        let st = sign_in::status_for(&home, Some(&state), Some(&flow_id)).unwrap();
        if st.phase == AntigravitySignInPhase::Failed {
            break st;
        }
        assert!(std::time::Instant::now() < deadline, "{st:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(failed.error.as_deref(), Some(ANTIGRAVITY_INVALID_GRANT));

    // 3. Exit 0 without writing native credentials reports Failed rather than masking as SignedIn.
    let fake_noop = root.join("fake-noop");
    std::fs::write(
        &fake_noop,
        r"#!/bin/sh
echo 'https://accounts.google.com/o/oauth2/auth?response_type=code&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&state=noop-me'
read -r _
exit 0
",
    )
    .unwrap();
    std::fs::set_permissions(&fake_noop, std::fs::Permissions::from_mode(0o700)).unwrap();

    let flow_id = sign_in::start_with_exe(
        &home,
        Some(&state),
        AntigravitySignInIntent::SignIn,
        &fake_noop,
        Duration::from_secs(10),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while sign_in::status_for(&home, Some(&state), Some(&flow_id))
        .unwrap()
        .phase
        != AntigravitySignInPhase::Waiting
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    sign_in::complete(&home, Some(&state), &flow_id, "good-looking-code").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let failed_save = loop {
        let st = sign_in::status_for(&home, Some(&state), Some(&flow_id)).unwrap();
        if st.phase == AntigravitySignInPhase::Failed {
            break st;
        }
        assert!(std::time::Instant::now() < deadline, "{st:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(failed_save.error.is_some());

    std::fs::remove_dir_all(root).ok();
}
