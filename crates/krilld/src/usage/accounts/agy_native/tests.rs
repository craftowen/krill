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
    AntigravityAccountStore, AntigravityAccountVault, EncryptedAntigravityAccountStore,
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
    fn read(&self) -> Result<AntigravityAccountVault, String> {
        Ok(self.vault.lock().unwrap().clone())
    }

    fn write(&self, vault: &AntigravityAccountVault) -> Result<(), String> {
        *self.vault.lock().unwrap() = vault.clone();
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
