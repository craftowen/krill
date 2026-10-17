//! OWE-719: 계정별 읽기 전용 직접 사용량. 토큰·응답 원문은 캐시·로그·IPC에 넣지 않는다.
//! Orca(MIT)의 rate-limits/claude-oauth-usage-request.ts, codex-backend-usage-client.ts,
//! claude-accounts/keychain.ts의 엔드포인트·저장소 규약을 참고한 독립 Rust 구현이다.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::account_login::AuthStatus;
use krill_proto::usage::ProviderKind;
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::{ProfileUsage, Usage, Window, accounts};
use crate::session::lock;

type Key = (PathBuf, ProviderKind, PathBuf);
static CACHE: Mutex<BTreeMap<Key, (Instant, ProfileUsage)>> = Mutex::new(BTreeMap::new());
static PENDING: Mutex<BTreeSet<Key>> = Mutex::new(BTreeSet::new());
const INTERVAL: Duration = Duration::from_secs(60);
const MAX_BYTES: u64 = 512 * 1024;

pub(super) fn cached(home: &Path, provider: ProviderKind, dir: &Path) -> Option<ProfileUsage> {
    lock(&CACHE)
        .get(&(home.to_path_buf(), provider, dir.to_path_buf()))
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(180))
        .map(|(_, usage)| usage.clone())
}

/// OWE-702의 틱과 `ListAccounts`가 호출한다. 네트워크·자격증명 IO는 별도 스레드에서만 수행한다.
pub fn refresh(home: &Path, state: Option<&Path>) {
    let file = accounts::load_file(home, state);
    if !file.direct_usage {
        lock(&CACHE).retain(|(h, _, _), _| h != home);
        return;
    }
    if cfg!(test) || (cfg!(debug_assertions) && std::env::var_os("KRILL_TEST_OFFLINE").is_some()) {
        return;
    } // 실제 자격증명·네트워크는 단위 테스트에서 금지.
    let profiles = [ProviderKind::Claude, ProviderKind::Codex]
        .into_iter()
        .map(|p| (p, accounts::host_default_dir(home, p), true))
        .chain(file.accounts.iter().filter_map(|a| {
            Some((
                ProviderKind::parse(&a.provider)?,
                PathBuf::from(&a.profile_dir),
                false,
            ))
        }));
    for (provider, dir, host) in profiles {
        if !matches!(provider, ProviderKind::Claude | ProviderKind::Codex) || !dir.is_dir() {
            continue;
        }
        let key = (home.to_path_buf(), provider, dir);
        if lock(&CACHE)
            .get(&key)
            .is_some_and(|(at, _)| at.elapsed() < INTERVAL)
            || !lock(&PENDING).insert(key.clone())
        {
            continue;
        }
        spawn(&key, host, state.map(Path::to_path_buf));
    }
}

fn spawn(key: &Key, host: bool, state: Option<PathBuf>) {
    let job = key.clone();
    let launched = std::thread::Builder::new()
        .name("account-usage".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(|| fetch(job.1, &job.2, host))
                .ok()
                .flatten();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            let usage = match result {
                Some(usage) => ProfileUsage {
                    usage,
                    updated_at: now,
                    ..ProfileUsage::default()
                },
                None => ProfileUsage {
                    error: Some("갱신 실패".into()),
                    detail: Some("사용량 갱신 실패".into()),
                    updated_at: now,
                    ..ProfileUsage::default()
                },
            };
            // 조회 중 사용자가 끈 경우에는 결과도 버린다.
            if accounts::load_file(&job.0, state.as_deref()).direct_usage {
                let mut cache = lock(&CACHE);
                cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(180));
                cache.insert(job.clone(), (Instant::now(), usage));
            }
            lock(&PENDING).remove(&job);
        });
    if launched.is_err() {
        lock(&PENDING).remove(key);
    }
}

/// 단위 테스트는 네트워크·실제 키체인을 절대 읽지 않는다.
#[cfg(test)]
fn fetch(_: ProviderKind, _: &Path, _: bool) -> Option<Usage> {
    None
}

#[cfg(not(test))]
fn fetch(provider: ProviderKind, dir: &Path, host: bool) -> Option<Usage> {
    let (token, account) = credentials(provider, dir, host)?;
    query(provider, &token, account.as_deref(), |config| {
        pipe_command(
            "/usr/bin/curl",
            &[
                "-q",
                "--config",
                "-",
                "--silent",
                "--fail",
                "--max-time",
                "10",
                "--max-filesize",
                "524288",
            ],
            config.as_bytes(),
        )
    })
}

fn query(
    provider: ProviderKind,
    token: &str,
    account: Option<&str>,
    send: impl FnOnce(&str) -> Option<Zeroizing<Vec<u8>>>,
) -> Option<Usage> {
    let config = request_config(provider, token, account)?;
    let body = send(&config)?;
    let text = std::str::from_utf8(&body).ok()?;
    let usage = if provider == ProviderKind::Claude {
        super::parse_claude(text)
    } else {
        parse_codex_backend(text)
    };
    (!usage.is_empty()).then_some(usage)
}

/// 토큰이 명령행·환경 변수에 노출되지 않도록 curl 설정을 stdin으로 전한다.
fn request_config(
    provider: ProviderKind,
    token: &str,
    account: Option<&str>,
) -> Option<Zeroizing<String>> {
    if token.is_empty()
        || !token
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\'))
    {
        return None;
    }
    if account.is_some_and(|a| {
        !a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    }) {
        return None;
    }
    let (url, headers) = match provider {
        ProviderKind::Claude => (
            "https://api.anthropic.com/api/oauth/usage",
            "header = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"User-Agent: claude-code/2.1.0\"\n",
        ),
        ProviderKind::Codex => (
            "https://chatgpt.com/backend-api/wham/usage",
            "header = \"User-Agent: codex-cli\"\nheader = \"OpenAI-Beta: codex-1\"\nheader = \"originator: Codex Desktop\"\n",
        ),
        _ => return None,
    };
    let mut config = Zeroizing::new(format!(
        "url = \"{url}\"\nrequest = \"GET\"\nheader = \"Authorization: Bearer {token}\"\n{headers}"
    ));
    if let Some(account) = account {
        config.push_str("header = \"ChatGPT-Account-Id: ");
        config.push_str(account);
        config.push_str("\"\n");
    }
    Some(config)
}

fn read_file(path: &Path) -> Option<Zeroizing<Vec<u8>>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_BYTES).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn credentials(
    provider: ProviderKind,
    dir: &Path,
    host: bool,
) -> Option<(Zeroizing<String>, Option<String>)> {
    let raw = if provider == ProviderKind::Claude {
        let user = std::env::var("USER")
            .ok()
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            })
            .unwrap_or_else(|| "claude-code-user".into());
        let mut dirs = vec![dir.to_path_buf()];
        if let Ok(canonical) = dir.canonicalize()
            && canonical != dir
        {
            dirs.push(canonical);
        }
        dirs.iter()
            .find_map(|dir| keychain(&claude_service((!host).then_some(dir.as_path()))?, &user))
            .or_else(|| read_file(&dir.join(".credentials.json")))?
    } else {
        read_file(&dir.join("auth.json"))?
    };
    parse_credentials(provider, &raw)
}

fn parse_credentials(
    provider: ProviderKind,
    raw: &[u8],
) -> Option<(Zeroizing<String>, Option<String>)> {
    let mut json: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let (token_path, account_path) = if provider == ProviderKind::Claude {
        ("/claudeAiOauth/accessToken", "")
    } else {
        ("/tokens/access_token", "/tokens/account_id")
    };
    let account = json
        .pointer(account_path)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let token = json.pointer_mut(token_path)?.take();
    let serde_json::Value::String(token) = token else {
        return None;
    };
    (!token.is_empty()).then(|| (Zeroizing::new(token), account))
}

fn claude_service(dir: Option<&Path>) -> Option<String> {
    let Some(dir) = dir else {
        return Some("Claude Code-credentials".into());
    };
    let path = dir.to_str()?;
    // macOS 파일명은 분해형일 수 있어 Claude의 NFC 해시 규약에 맞춘다.
    let normalized = if path.is_ascii() {
        path.as_bytes().to_vec()
    } else {
        pipe_command(
            "/usr/bin/iconv",
            &["-f", "UTF-8-MAC", "-t", "UTF-8"],
            path.as_bytes(),
        )?
        .to_vec()
    };
    let digest = format!("{:x}", Sha256::digest(&normalized));
    Some(format!(
        "Claude Code-credentials-{}",
        digest.chars().take(8).collect::<String>()
    ))
}

// Claude Code·agy(go-keyring)는 `/usr/bin/security`로 항목을 만들어 ACL이 그 도구만 믿는다.
// krilld가 SecItem으로 직접 읽으면 macOS가 매번 로그인 키체인 암호를 묻는다 — Orca처럼 같은 도구로 읽는다.
#[cfg(not(test))]
fn keychain(service: &str, account: &str) -> Option<Zeroizing<Vec<u8>>> {
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let stdout = Zeroizing::new(output.stdout);
    (output.status.success() && u64::try_from(stdout.len()).ok()? <= MAX_BYTES).then_some(stdout)
}
#[cfg(test)]
fn keychain(_: &str, _: &str) -> Option<Zeroizing<Vec<u8>>> {
    None
}

fn pipe_command(exe: &str, args: &[&str], input: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let write = child.stdin.take()?.write_all(input);
    if write.is_err() {
        child.kill().ok();
        child.wait().ok();
        return None;
    }
    let output = child.wait_with_output().ok()?;
    (output.status.success() && u64::try_from(output.stdout.len()).ok()? <= MAX_BYTES)
        .then(|| Zeroizing::new(output.stdout))
}

pub(super) fn antigravity_auth(dir: &Path, host: bool) -> Option<AuthStatus> {
    if !host {
        let token = dir.join(".gemini/antigravity-cli/antigravity-oauth-token");
        let metadata = std::fs::symlink_metadata(token).ok()?;
        return Some(AuthStatus {
            logged_in: metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() > 0,
            identity: None,
        });
    }
    let raw = keychain("gemini", "antigravity")?;
    parse_antigravity_auth(&raw)
}

fn parse_antigravity_auth(raw: &[u8]) -> Option<AuthStatus> {
    let text = std::str::from_utf8(raw).ok()?.trim();
    let decoded = if let Some(encoded) = text.strip_prefix("go-keyring-base64:") {
        pipe_command("/usr/bin/base64", &["-D"], encoded.as_bytes())?
    } else if let Some(encoded) = text.strip_prefix("go-keyring-encoded:") {
        let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
        if !remainder.is_empty() {
            return None;
        }
        let bytes = pairs
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
            .collect::<Option<Vec<_>>>()?;
        Zeroizing::new(bytes)
    } else {
        Zeroizing::new(text.as_bytes().to_vec())
    };
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let valid = json
        .get("auth_method")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty())
        && json
            .pointer("/token/access_token")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.is_empty());
    Some(AuthStatus {
        logged_in: valid,
        identity: None,
    })
}

fn parse_codex_backend(text: &str) -> Usage {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Usage::default();
    };
    let mut usage = Usage::default();
    for name in ["primary_window", "secondary_window"] {
        let Some(raw) = json.get("rate_limit").and_then(|v| v.get(name)) else {
            continue;
        };
        let slot = match raw
            .get("limit_window_seconds")
            .and_then(serde_json::Value::as_u64)
        {
            Some(18_000) => &mut usage.five_hour,
            Some(604_800) => &mut usage.seven_day,
            _ => continue,
        };
        let Some(percent) = raw.get("used_percent").and_then(super::percent) else {
            continue;
        };
        *slot = Some(Window {
            percent,
            resets_at: raw.get("reset_at").and_then(super::resets),
        });
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_file_fallback_reads_only_the_selected_profile() {
        let root = std::env::temp_dir().join(format!("owe719-credentials-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(".credentials.json");
        let raw = br#"{"claudeAiOauth":{"accessToken":"fixture-only"}}"#;
        std::fs::write(&path, raw).unwrap();
        let (token, _) = credentials(ProviderKind::Claude, &root, false).unwrap();
        assert_eq!(&*token, "fixture-only");
        assert_eq!(std::fs::read(&path).unwrap(), raw);
        assert!(credentials(ProviderKind::Codex, &root, false).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provider_credentials_and_headers_stay_scoped_and_read_only() {
        let (token, account) = parse_credentials(
            ProviderKind::Codex,
            br#"{"tokens":{"access_token":"test-only","account_id":"acct-1"}}"#,
        )
        .unwrap();
        assert_eq!(&*token, "test-only");
        assert_eq!(account.as_deref(), Some("acct-1"));
        assert!(
            parse_credentials(
                ProviderKind::Claude,
                br#"{"tokens":{"access_token":"wrong"}}"#
            )
            .is_none()
        );
        let config = request_config(ProviderKind::Codex, &token, account.as_deref()).unwrap();
        assert!(config.contains("request = \"GET\""));
        assert!(config.contains("ChatGPT-Account-Id: acct-1"));
        for invalid in ["token\r\ninjected", "token\"", "token\\"] {
            assert!(request_config(ProviderKind::Claude, invalid, None).is_none());
        }
        assert_eq!(
            claude_service(None).as_deref(),
            Some("Claude Code-credentials")
        );
        assert_ne!(
            claude_service(Some(Path::new("/tmp/a"))),
            claude_service(None)
        );
    }
    #[test]
    fn antigravity_native_metadata_is_validated_without_a_model_turn() {
        let raw = br#"{"auth_method":"oauth","token":{"access_token":"fixture-only"}}"#;
        assert!(parse_antigravity_auth(raw).unwrap().logged_in);
        assert!(
            !parse_antigravity_auth(br#"{"auth_method":"oauth","token":{}}"#)
                .unwrap()
                .logged_in
        );
        assert!(parse_antigravity_auth(b"invalid").is_none());
        assert!(parse_antigravity_auth(b"go-keyring-encoded:7").is_none());
        let encoded = pipe_command("/usr/bin/base64", &[], raw).unwrap();
        let mut wrapped = b"go-keyring-base64:".to_vec();
        wrapped.extend_from_slice(&encoded);
        assert!(parse_antigravity_auth(&wrapped).unwrap().logged_in);
        assert_eq!(
            claude_service(Some(Path::new("/tmp/é"))),
            claude_service(Some(Path::new("/tmp/e\u{301}")))
        );
    }

    #[test]
    fn injected_http_reads_quota_and_maps_failures_to_unknown() {
        let usage = query(ProviderKind::Claude, "fixture-only", None, |config| {
            assert!(config.contains("request = \"GET\""));
            assert!(config.contains("anthropic-beta: oauth-2025-04-20"));
            Some(Zeroizing::new(
                br#"{"five_hour":{"utilization":42},"seven_day":{"utilization":8}}"#.to_vec(),
            ))
        })
        .unwrap();
        assert_eq!(usage.five_hour.unwrap().percent, 42);
        assert_eq!(usage.seven_day.unwrap().percent, 8);
        assert!(query(ProviderKind::Claude, "fixture-only", None, |_| None).is_none());
        assert!(
            query(ProviderKind::Codex, "fixture-only", None, |_| Some(
                Zeroizing::new(b"invalid response".to_vec())
            ))
            .is_none()
        );
    }

    #[test]
    fn backend_windows_use_duration_not_primary_order() {
        let usage = parse_codex_backend(
            r#"{"rate_limit":{"primary_window":{"limit_window_seconds":604800,"used_percent":12.4,"reset_at":1900000000},"secondary_window":{"limit_window_seconds":18000,"used_percent":76}}}"#,
        );
        assert_eq!(usage.five_hour.unwrap().percent, 76);
        assert_eq!(usage.seven_day.unwrap().percent, 12);
        assert!(parse_codex_backend("bad").is_empty());
        assert!(parse_codex_backend(r#"{"rate_limit":{"primary_window":{"limit_window_seconds":60,"used_percent":100}}}"#).is_empty());
    }
    #[test]
    fn disabled_setting_drops_cached_usage_without_reading_credentials() {
        let root = std::env::temp_dir().join(format!("owe719-direct-{}", std::process::id()));
        let home = root.join("home");
        let state = root.join("state");
        let key = (home.clone(), ProviderKind::Claude, home.join(".claude"));
        lock(&CACHE).insert(key.clone(), (Instant::now(), ProfileUsage::default()));
        let file = accounts::AccountsFile {
            direct_usage: false,
            ..accounts::AccountsFile::default()
        };
        accounts::save_file(&home, Some(&state), &file).unwrap();
        refresh(&home, Some(&state));
        assert!(!lock(&CACHE).contains_key(&key));
        std::fs::remove_dir_all(root).ok();
    }
}
