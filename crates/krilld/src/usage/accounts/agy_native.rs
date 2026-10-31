//! 프로바이더별 호스트 기본 경로 자격증명 동기화 및 Antigravity(`agy`) 단일 `~/.gemini` 네이티브 자격증명 교체.
//!
//! - Antigravity(`agy`): `--gemini_dir`로 `.gemini` 루트를 분리하지 않고 호스트 기본 `~/.gemini`를 그대로 쓰면서,
//!   계정 전환·실행 시 선택된 계정의 `antigravity-oauth-token`을 호스트 파일과 macOS 키체인(`gemini`/`antigravity`)에
//!   원자적으로 반영한다. 교체 전 호스트에서 갱신된 토큰은 동일 이메일의 프로필 슬롯에 먼저 되돌려 저장한다.
//! - Codex(`codex`): 선택된 프로필의 `auth.json`을 호스트 `~/.codex/auth.json`에도 동기화하고, 호스트에서 갱신된
//!   `auth.json`은 동일 신원의 프로필 슬롯으로 되돌린다.
//! - Claude(`claude`): 선택된 프로필의 키체인 자격증명(`Claude Code-credentials-<hash>`) 및 `.credentials.json`을
//!   호스트 기본 자격증명(`Claude Code-credentials`, `~/.claude/.credentials.json`)에도 동기화한다.
//! - `OpenCode`(`opencode`): 선택된 프로필의 `data/opencode/auth.json`을 `~/.local/share/opencode/auth.json`에 동기화한다.

#[cfg(target_os = "macos")]
use std::io::Write as _;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};

use krill_proto::usage::ProviderKind;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
use super::profile_name::Vault as _;
use super::{AccountsFile, StoredAccount, identity_from_credentials, write_atomic_0600};
#[cfg(target_os = "macos")]
use crate::usage::direct::claude_service;

const MAX_TOKEN_BYTES: u64 = 64 * 1024;

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

/// `accounts.toml` 저장 시 활성 계정의 자격증명을 호스트 기본 경로와 동기화한다.
pub(crate) fn sync_active(home: &Path, file: &AccountsFile) {
    sync_account(
        home,
        &file.accounts,
        active_dir_for(file, ProviderKind::Antigravity),
    );
    if use_real_keychain(home) {
        sync_codex_account(
            home,
            &file.accounts,
            active_dir_for(file, ProviderKind::Codex),
        );
        sync_claude_account(home, active_dir_for(file, ProviderKind::Claude), true);
        sync_opencode_account(home, active_dir_for(file, ProviderKind::OpenCode));
    }
}

/// 명시된 Antigravity 계정 프로필 폴더(`dir`)의 자격증명을 호스트 `~/.gemini`로 활성화한다.
pub(crate) fn sync_account_dir(home: &Path, state_dir: Option<&Path>, dir: &Path) {
    sync_provider_account_dir(home, state_dir, ProviderKind::Antigravity, dir);
}

/// 명시된 프로바이더 계정 프로필 폴더(`dir`)의 자격증명을 호스트 기본 경로로 동기화한다.
pub(crate) fn sync_provider_account_dir(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    dir: &Path,
) {
    if dir.as_os_str().is_empty() {
        return;
    }
    match provider {
        ProviderKind::Antigravity => {
            let file = super::load_file(home, state_dir);
            sync_account(home, &file.accounts, Some(dir));
        }
        ProviderKind::Codex => {
            let file = super::load_file(home, state_dir);
            sync_codex_account(home, &file.accounts, Some(dir));
        }
        ProviderKind::Claude => {
            sync_claude_account(home, Some(dir), use_real_keychain(home));
        }
        ProviderKind::OpenCode => {
            sync_opencode_account(home, Some(dir));
        }
        ProviderKind::Gemini => {}
    }
}

/// 호스트 기본 경로(및 macOS 키체인)에서 갱신된 토큰을 동일 신원의 프로필 슬롯으로 되돌린다.
pub(crate) fn reconcile_host_to_profiles(home: &Path, accounts: &[StoredAccount]) {
    sync_account(home, accounts, None);
    if use_real_keychain(home) {
        sync_codex_account(home, accounts, None);
    }
}

fn codex_account_id(raw: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()?
        .pointer("/tokens/account_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(crate) fn sync_codex_account(
    home: &Path,
    accounts: &[StoredAccount],
    target_dir: Option<&Path>,
) {
    if home.as_os_str().is_empty() {
        return;
    }
    let host_auth_path = home.join(".codex/auth.json");
    if target_dir
        .is_some_and(|dir| dir == home.join(".codex") || dir.join("auth.json") == host_auth_path)
    {
        return;
    }
    let current_host = read_token_file(&host_auth_path);

    // 1. 호스트 `~/.codex/auth.json`에서 갱신된 토큰이 있으면 동일 신원의 프로필 슬롯에 보존한다.
    if let Some(host_raw) = current_host.as_ref() {
        let host_email = identity_from_credentials(ProviderKind::Codex, host_raw).map(|i| i.email);
        let host_acc_id = codex_account_id(host_raw);
        if host_email.is_some() || host_acc_id.is_some() {
            for acc in accounts {
                if ProviderKind::parse(&acc.provider) != Some(ProviderKind::Codex)
                    || acc.profile_dir.is_empty()
                {
                    continue;
                }
                let prof_dir = Path::new(&acc.profile_dir);
                if !prof_dir.is_dir() {
                    continue;
                }
                let prof_auth_path = prof_dir.join("auth.json");
                let prof_raw = read_token_file(&prof_auth_path);
                let prof_email = prof_raw
                    .as_deref()
                    .and_then(|r| identity_from_credentials(ProviderKind::Codex, r))
                    .map(|i| i.email)
                    .or_else(|| acc.email.clone());
                let prof_acc_id = prof_raw.as_deref().and_then(|r| codex_account_id(r));
                let same_identity = match (&host_email, &prof_email) {
                    (Some(he), Some(pe)) => he.eq_ignore_ascii_case(pe),
                    _ => host_acc_id.is_some() && host_acc_id == prof_acc_id,
                };
                if same_identity && prof_raw.as_ref() != Some(host_raw) {
                    write_atomic_0600(&prof_auth_path, host_raw).ok();
                }
            }
        }
    }

    // 2. 선택된 프로필의 `auth.json`을 호스트 `~/.codex/auth.json`에 반영한다.
    let Some(target) = target_dir.filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let Some(target_raw) = read_token_file(&target.join("auth.json")) else {
        return;
    };
    if current_host.as_ref() != Some(&target_raw) {
        write_atomic_0600(&host_auth_path, &target_raw).ok();
    }
}

pub(crate) fn sync_claude_account(home: &Path, target_dir: Option<&Path>, allow_keychain: bool) {
    if home.as_os_str().is_empty() {
        return;
    }
    let Some(target) = target_dir.filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let host_claude = home.join(".claude");
    if target == host_claude {
        return;
    }
    let target_creds = target.join(".credentials.json");
    let host_creds = host_claude.join(".credentials.json");
    if target_creds != host_creds
        && let Some(raw) = read_token_file(&target_creds)
        && read_token_file(&host_creds).as_ref() != Some(&raw)
    {
        write_atomic_0600(&host_creds, &raw).ok();
    }
    #[cfg(target_os = "macos")]
    if allow_keychain
        && let (Some(src_service), Some(dst_service)) =
            (claude_service(Some(target)), claude_service(None))
        && src_service != dst_service
    {
        super::Keychain.copy(&src_service, &dst_service).ok();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = allow_keychain;
}

pub(crate) fn sync_opencode_account(home: &Path, target_dir: Option<&Path>) {
    if home.as_os_str().is_empty() {
        return;
    }
    let Some(target) = target_dir.filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let target_auth = target.join("data/opencode/auth.json");
    let host_auth = home.join(".local/share/opencode/auth.json");
    if target_auth == host_auth {
        return;
    }
    if let Some(raw) = read_token_file(&target_auth)
        && read_token_file(&host_auth).as_ref() != Some(&raw)
    {
        write_atomic_0600(&host_auth, &raw).ok();
    }
}

fn sync_account(home: &Path, accounts: &[StoredAccount], target_dir: Option<&Path>) {
    if home.as_os_str().is_empty() {
        return;
    }
    let host_token_path = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
    if target_dir.is_some_and(|dir| {
        dir == home
            || dir.join(".gemini/antigravity-cli/antigravity-oauth-token") == host_token_path
    }) {
        return;
    }
    let real_keychain = use_real_keychain(home);
    let current_keychain = if real_keychain { read_keychain() } else { None };
    let current_file = read_token_file(&host_token_path);
    let current_host = current_keychain.as_ref().or(current_file.as_ref());

    // 1. 호스트에서 갱신된 토큰이 있으면 같은 이메일을 가진 프로필 슬롯에 먼저 보존한다.
    if let Some(host_raw) = current_host
        && let Some(host_id) = identity_from_credentials(ProviderKind::Antigravity, host_raw)
    {
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
            let prof_raw = read_token_file(&prof_token_path);
            let prof_email = prof_raw
                .as_deref()
                .and_then(|r| identity_from_credentials(ProviderKind::Antigravity, r))
                .map(|i| i.email)
                .or_else(|| acc.email.clone());
            if prof_email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(&host_id.email))
                && prof_raw.as_ref() != Some(host_raw)
            {
                write_atomic_0600(&prof_token_path, host_raw).ok();
            }
        }
    }

    // 2. 선택된 프로필의 토큰을 호스트 파일 및 macOS 키체인에 반영한다.
    let Some(target) = target_dir.filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let target_token_path = target.join(".gemini/antigravity-cli/antigravity-oauth-token");
    let Some(target_raw) = read_token_file(&target_token_path) else {
        return;
    };
    if current_file.as_ref() != Some(&target_raw) {
        write_atomic_0600(&host_token_path, &target_raw).ok();
    }
    if real_keychain && current_keychain.as_ref() != Some(&target_raw) {
        write_keychain(&target_raw);
    }
}

fn use_real_keychain(home: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        if cfg!(test) {
            return false;
        }
        if std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0") {
            return false;
        }
        let Some(real_home) = krill_proto::os::home_dir() else {
            return false;
        };
        let same_home = home == real_home
            || home
                .canonicalize()
                .ok()
                .zip(real_home.canonicalize().ok())
                .is_some_and(|(a, b)| a == b);
        same_home
            && !home
                .join(".gemini/antigravity-cli/cache/antigravity-keyring-unavailable")
                .exists()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        false
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

#[cfg(target_os = "macos")]
fn read_keychain() -> Option<Zeroizing<Vec<u8>>> {
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args([
        "find-generic-password",
        "-s",
        "gemini",
        "-a",
        "antigravity",
        "-w",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let output = cmd.output().ok()?;
    if !output.status.success() || u64::try_from(output.stdout.len()).ok()? > MAX_TOKEN_BYTES {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?.trim();
    let decoded = if let Some(b64) = text.strip_prefix("go-keyring-base64:") {
        Zeroizing::new(base64_decode(b64)?)
    } else if let Some(hex) = text.strip_prefix("go-keyring-encoded:") {
        let (pairs, rem) = hex.as_bytes().as_chunks::<2>();
        if !rem.is_empty() {
            return None;
        }
        let bytes = pairs
            .iter()
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
            .collect::<Option<Vec<_>>>()?;
        Zeroizing::new(bytes)
    } else {
        Zeroizing::new(text.as_bytes().to_vec())
    };
    valid_token(&decoded).then_some(decoded)
}

#[cfg(not(target_os = "macos"))]
fn read_keychain() -> Option<Zeroizing<Vec<u8>>> {
    None
}

#[cfg(target_os = "macos")]
fn write_keychain(raw: &[u8]) -> bool {
    let encoded = format!("go-keyring-base64:{}", base64_encode(raw));
    let line = Zeroizing::new(format!(
        "add-generic-password -U -s \"gemini\" -a \"antigravity\" -w {encoded}\n"
    ));
    if line.len() > 16 * 1024 {
        return false;
    }
    let mut cmd = Command::new("/usr/bin/security");
    cmd.arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(line.as_bytes()).is_ok());
    if !wrote {
        child.kill().ok();
        child.wait().ok();
        return false;
    }
    child.wait().is_ok_and(|s| s.success())
}

#[cfg(not(target_os = "macos"))]
fn write_keychain(_raw: &[u8]) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let char_at = |idx: u8| char::from(TABLE.get(usize::from(idx & 0x3f)).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let Some(&b0) = chunk.first() else {
            continue;
        };
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(char_at(b0 >> 2));
        out.push(char_at(((b0 & 0x03) << 4) | (b1 >> 4)));
        if chunk.len() > 1 {
            out.push(char_at(((b1 & 0x0f) << 2) | (b2 >> 6)));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char_at(b2 & 0x3f));
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim().trim_end_matches('=');
    let mut decoded = Vec::with_capacity(trimmed.len() * 3 / 4);
    let (mut bits, mut count) = (0_u32, 0_u32);
    for byte in trimmed.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let n = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(n);
        count += 6;
        if count >= 8 {
            count -= 8;
            decoded.push(u8::try_from((bits >> count) & 255).ok()?);
        }
    }
    if count >= 6 || bits & ((1 << count) - 1) != 0 {
        return None;
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::accounts::{AccountsFile, StoredAccount};

    #[test]
    fn switching_agy_account_swaps_host_token_and_preserves_refreshed_host_token() {
        let root =
            std::env::temp_dir().join(format!("krill-agy-native-swap-{}", std::process::id()));
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

        // 1. agy-1 활성화 시 호스트 토큰이 agy-1 토큰으로 채워진다.
        file.active.insert("agy".into(), "agy-1".into());
        sync_active(&home, &file);
        assert_eq!(
            std::fs::read(home.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
            tok1_initial
        );

        // 2. 호스트에서 agy가 토큰을 갱신한 뒤 agy-2로 전환하면, 갱신된 토큰이 prof1에 보존되고 호스트는 agy-2가 된다.
        std::fs::write(
            home.join(".gemini/antigravity-cli/antigravity-oauth-token"),
            tok1_refreshed,
        )
        .unwrap();
        file.active.insert("agy".into(), "agy-2".into());
        sync_active(&home, &file);
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
    fn switching_codex_claude_and_opencode_accounts_syncs_host_credentials() {
        let root =
            std::env::temp_dir().join(format!("krill-all-native-swap-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let codex1 = root.join("codex1");
        let codex2 = root.join("codex2");
        let claude1 = root.join("claude1");
        let opencode1 = root.join("opencode1");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".local/share/opencode")).unwrap();
        std::fs::create_dir_all(&codex1).unwrap();
        std::fs::create_dir_all(&codex2).unwrap();
        std::fs::create_dir_all(&claude1).unwrap();
        std::fs::create_dir_all(opencode1.join("data/opencode")).unwrap();

        let cx1_old = br#"{"tokens":{"access_token":"cx1-old","account_id":"acc-1"}}"#;
        let cx1_new = br#"{"tokens":{"access_token":"cx1-new","account_id":"acc-1"}}"#;
        let cx2 = br#"{"tokens":{"access_token":"cx2","account_id":"acc-2"}}"#;
        std::fs::write(codex1.join("auth.json"), cx1_old).unwrap();
        std::fs::write(codex2.join("auth.json"), cx2).unwrap();

        let accounts = vec![
            StoredAccount {
                id: "cx-1".into(),
                provider: "codex".into(),
                label: "CX1".into(),
                email: Some("cx1@example.test".into()),
                profile_dir: codex1.display().to_string(),
            },
            StoredAccount {
                id: "cx-2".into(),
                provider: "codex".into(),
                label: "CX2".into(),
                email: Some("cx2@example.test".into()),
                profile_dir: codex2.display().to_string(),
            },
        ];

        sync_codex_account(&home, &accounts, Some(&codex1));
        assert_eq!(
            std::fs::read(home.join(".codex/auth.json")).unwrap(),
            cx1_old
        );

        std::fs::write(home.join(".codex/auth.json"), cx1_new).unwrap();
        sync_codex_account(&home, &accounts, Some(&codex2));
        assert_eq!(std::fs::read(codex1.join("auth.json")).unwrap(), cx1_new);
        assert_eq!(std::fs::read(home.join(".codex/auth.json")).unwrap(), cx2);

        let cl_creds = br#"{"claudeAiOauth":{"accessToken":"cl-1"}}"#;
        std::fs::write(claude1.join(".credentials.json"), cl_creds).unwrap();
        sync_claude_account(&home, Some(&claude1), false);
        assert_eq!(
            std::fs::read(home.join(".claude/.credentials.json")).unwrap(),
            cl_creds
        );

        let oc_creds = br#"{"anthropic":{"type":"oauth","access":"oc-1"}}"#;
        std::fs::write(opencode1.join("data/opencode/auth.json"), oc_creds).unwrap();
        sync_opencode_account(&home, Some(&opencode1));
        assert_eq!(
            std::fs::read(home.join(".local/share/opencode/auth.json")).unwrap(),
            oc_creds
        );

        std::fs::remove_dir_all(root).ok();
    }
}
