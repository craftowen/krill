//! 프로바이더 로그인 명령과 비밀값 없는 로그인 상태 조회.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::usage::ProviderKind;

/// OWE-705 신원 정보. 토큰·키체인 값은 포함하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub email: String,
    pub org_id: Option<String>,
    pub org_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub identity: Option<Identity>,
}

/// Build a native login process without a POSIX `env` executable or shell quoting.
/// The caller resolves `exe` with the same PATH used for discovery and verification.
/// Interactive callers retain their console; background callers use `os::configure_background`.
pub fn login_command(exe: &Path, provider: ProviderKind, dir: Option<&Path>) -> Option<Command> {
    if dir.is_some_and(|dir| dir.as_os_str().is_empty()) {
        return None;
    }
    let args: &[&str] = match provider {
        ProviderKind::Claude => &["auth", "login", "--claudeai"],
        ProviderKind::Codex => &["login"],
        ProviderKind::OpenCode => &["auth", "login"],
        ProviderKind::Antigravity => &[],
        ProviderKind::Gemini => return None,
    };
    let mut command = Command::new(exe);
    command.args(args);
    if provider == ProviderKind::Antigravity {
        if let Some(dir) = dir {
            command.env("SSH_CONNECTION", "krill");
            command.env("AGY_CLI_FORCE_FILE_TOKEN_STORAGE", "1");
            let mut arg = std::ffi::OsString::from("--gemini_dir=");
            arg.push(dir.join(".gemini"));
            command.arg(arg);
        } else {
            command.env_remove("SSH_CONNECTION");
            command.env_remove("AGY_CLI_FORCE_FILE_TOKEN_STORAGE");
        }
    } else {
        configure_account(&mut command, provider, dir);
    }
    Some(command)
}

fn configure_account(command: &mut Command, provider: ProviderKind, dir: Option<&Path>) {
    for key in provider.credential_overrides() {
        command.env_remove(key);
    }
    match dir {
        Some(dir) if provider == ProviderKind::OpenCode => {
            command.env("XDG_DATA_HOME", dir.join("data"));
            command.env("XDG_STATE_HOME", dir.join("state"));
            command.env("OPENCODE_DB", "opencode.db");
        }
        Some(dir) => {
            command.env(provider.env_var(), dir);
        }
        None => {
            command.env_remove(provider.env_var());
        }
    }
}

pub fn parse_claude_status(json: &str) -> Option<AuthStatus> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let logged_in = v.get("loggedIn")?.as_bool()?;
    let text = |key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let identity = if logged_in {
        text("email").map(|email| Identity {
            email,
            org_id: text("orgId"),
            org_name: text("orgName"),
        })
    } else {
        None
    };
    Some(AuthStatus {
        logged_in,
        identity,
    })
}

/// agy 프로필 로그인 판정(OWE-901). 토큰 파일 존재·크기만 보고 내용은 읽지 않는다.
/// `krilld::usage::direct::antigravity_auth`와 같은 판정이라 여기로 모았다.
#[must_use]
pub fn agy_token_logged_in(dir: &Path) -> bool {
    let token = dir.join(".gemini/antigravity-cli/antigravity-oauth-token");
    std::fs::symlink_metadata(&token)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0)
}

#[must_use]
pub fn agy_identity_from_credentials(raw: &[u8]) -> Option<Identity> {
    if raw.is_empty() || raw.len() > 64 * 1024 {
        return None;
    }
    let data: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let token = match data
        .get("id_token")
        .or_else(|| data.pointer("/token/id_token"))
    {
        Some(token) => token.as_str()?,
        None => return agy_identity_fields(&data),
    };
    let claims = jwt_claims(token)?;
    if !matches!(
        claims.get("iss").and_then(serde_json::Value::as_str),
        Some("accounts.google.com" | "https://accounts.google.com")
    ) || claims
        .get("sub")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|s| s.trim().is_empty())
        || claims
            .get("email_verified")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return None;
    }
    let email = claims.get("email")?.as_str()?.trim();
    if email.is_empty() || email.len() > 320 || email.chars().any(char::is_control) {
        return None;
    }
    Some(Identity {
        email: email.to_owned(),
        org_id: None,
        org_name: None,
    })
}

fn agy_identity_fields(data: &serde_json::Value) -> Option<Identity> {
    if data.get("id_token_email").is_some()
        && data
            .get("auth_method")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
        && data
            .pointer("/token/access_token")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    {
        if data
            .get("email_verified")
            .is_some_and(|v| v != &serde_json::Value::Bool(true))
        {
            return None;
        }
        return agy_email(data.get("id_token_email")?);
    }
    [
        Some(data),
        data.get("token"),
        data.get("account"),
        data.get("user"),
    ]
    .into_iter()
    .flatten()
    .find_map(|object| {
        let valid_text = |value: &serde_json::Value| {
            value.as_str().is_some_and(|s| {
                !s.trim().is_empty() && s.len() <= 320 && !s.chars().any(char::is_control)
            })
        };
        if !["sub", "account_id"]
            .iter()
            .any(|key| object.get(key).is_some_and(valid_text))
            || object
                .get("email_verified")
                .is_some_and(|v| v != &serde_json::Value::Bool(true))
        {
            return None;
        }
        ["id_token_email", "email", "account_email"]
            .iter()
            .find_map(|key| agy_email(object.get(key)?))
    })
}

fn agy_email(value: &serde_json::Value) -> Option<Identity> {
    let raw = value.as_str()?;
    if raw.len() > 320 || raw.chars().any(char::is_control) {
        return None;
    }
    let email = raw.trim();
    let (local, domain) = email.split_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || email.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(Identity {
        email: email.to_owned(),
        org_id: None,
        org_name: None,
    })
}

#[must_use]
pub fn agy_identity_from_dir(dir: &Path) -> Option<Identity> {
    let path = dir.join(".gemini/antigravity-cli/antigravity-oauth-token");
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| {
        m.is_file() && !m.file_type().is_symlink() && m.len() > 0 && m.len() <= 64 * 1024
    }) {
        return None;
    }
    let mut bytes = std::fs::read(&path).ok()?;
    let id = agy_identity_from_credentials(&bytes);
    bytes.fill(0);
    id
}

#[must_use]
pub fn codex_identity_from_credentials(raw: &[u8]) -> Option<Identity> {
    if raw.is_empty() || raw.len() > 64 * 1024 {
        return None;
    }
    let data: serde_json::Value = serde_json::from_slice(raw).ok()?;
    if data.get("auth_mode").and_then(serde_json::Value::as_str) == Some("apikey")
        || data
            .get("OPENAI_API_KEY")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    {
        return None;
    }
    let token = data
        .pointer("/tokens/id_token")
        .or_else(|| data.pointer("/tokens/idToken"))?
        .as_str()?;
    let claims = jwt_claims(token)?;
    let email = claims
        .get("email")
        .or_else(|| claims.pointer("/https:~1~1api.openai.com~1profile/email"))?
        .as_str()?
        .trim();
    if email.is_empty() || email.len() > 320 || email.chars().any(char::is_control) {
        return None;
    }
    Some(Identity {
        email: email.to_owned(),
        org_id: None,
        org_name: None,
    })
}

#[must_use]
pub fn codex_identity_from_dir(dir: &Path) -> Option<Identity> {
    let path = dir.join("auth.json");
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| {
        m.is_file() && !m.file_type().is_symlink() && m.len() > 0 && m.len() <= 64 * 1024
    }) {
        return None;
    }
    let mut bytes = std::fs::read(&path).ok()?;
    let id = codex_identity_from_credentials(&bytes);
    bytes.fill(0);
    id
}

#[must_use]
pub fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    parts.next()?;
    let payload = parts.next()?;
    parts.next()?;
    if parts.next().is_some() || payload.is_empty() || payload.len() > 16 * 1024 {
        return None;
    }
    let (mut bits, mut count) = (0_u32, 0_u32);
    let mut decoded = Vec::new();
    for byte in payload.bytes() {
        let n = match byte {
            b'A'..=b'Z' => byte.saturating_sub(b'A'),
            b'a'..=b'z' => byte.saturating_sub(b'a').saturating_add(26),
            b'0'..=b'9' => byte.saturating_sub(b'0').saturating_add(52),
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = bits.checked_shl(6)? | u32::from(n);
        count = count.saturating_add(6);
        if count >= 8 {
            count = count.saturating_sub(8);
            let shifted = bits.checked_shr(count)?;
            decoded.push(u8::try_from(shifted & 255).ok()?);
        }
    }
    if count >= 6 {
        return None;
    }
    let mask = 1_u32.checked_shl(count)?.checked_sub(1)?;
    if bits & mask != 0 {
        return None;
    }
    serde_json::from_slice(&decoded).ok()
}

/// 비밀값을 출력하지 않는 CLI 상태 조회. stdout·stderr를 함께 읽되 결과에는 신원만 남긴다.
/// Codex는 상태를 stderr에 쓰며 로그아웃이면 비영(非零) 종료한다. `dir`이 `None`이면 호스트 기본 계정([`login_command`]).
pub fn probe(exe: &Path, provider: ProviderKind, dir: Option<&Path>) -> Option<AuthStatus> {
    if provider == ProviderKind::Antigravity {
        let dir = dir?;
        let logged_in = agy_token_logged_in(dir);
        return Some(AuthStatus {
            logged_in,
            identity: logged_in.then(|| agy_identity_from_dir(dir)).flatten(),
        });
    }
    probe_with_environment(exe, provider, dir, None, None)
}

/// 데몬 로그인 셸 PATH와 대상 HOME을 명시한다. 부모 환경은 바꾸지 않는다.
pub fn probe_with_environment(
    exe: &Path,
    provider: ProviderKind,
    dir: Option<&Path>,
    home: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Option<AuthStatus> {
    let args: &[&str] = match provider {
        ProviderKind::Claude => &["auth", "status", "--json"],
        ProviderKind::Codex => &["login", "status"],
        ProviderKind::OpenCode => &["auth", "list"],
        _ => return None,
    };
    let mut cmd = crate::os::background_command(exe);
    if let Some(home) = home {
        cmd.env("HOME", home);
        #[cfg(windows)]
        cmd.env("USERPROFILE", home);
    }
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    configure_account(&mut cmd, provider, dir);
    let mut child = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let pipes: [Box<dyn std::io::Read + Send>; 2] = [Box::new(stdout), Box::new(stderr)];
    for mut pipe in pipes {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let result = pipe
                .by_ref()
                .take(64 * 1024)
                .read_to_end(&mut buf)
                .map(|_| {
                    std::io::copy(&mut pipe, &mut std::io::sink()).ok();
                    String::from_utf8_lossy(&buf).into_owned()
                });
            tx.send(result).ok();
        });
    }
    drop(tx);
    let deadline = Instant::now().checked_add(Duration::from_secs(5))?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                crate::os::kill_tree(&mut child);
                break None;
            }
        }
    }?;
    let mut outputs = Vec::new();
    for _ in 0..2 {
        outputs.push(
            rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok()?
                .ok()?,
        );
    }
    if provider == ProviderKind::Claude {
        // Claude JSON은 stdout, 진단은 stderr. 결합 순서에 기대지 않는다.
        return outputs.iter().find_map(|output| {
            parse_claude_status(output).or_else(|| output.lines().find_map(parse_claude_status))
        });
    }
    let output = outputs.join("\n");
    let lower = strip_ansi(&output).to_ascii_lowercase();
    let logged_in = if provider == ProviderKind::OpenCode {
        if !status.success() {
            return None;
        }
        // auth list는 토큰 대신 저장된 자격증명 건수만 반환한다.
        let count = lower
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find_map(|pair| {
                pair.first()?.parse::<u64>().ok().filter(|_| {
                    pair.get(1)
                        .is_some_and(|s| s.starts_with("credential") || s.starts_with("provider"))
                })
            })?;
        count > 0
    } else if lower.contains("not logged in") {
        false
    } else if status.success() && lower.contains("logged in") {
        true
    } else {
        return None;
    };
    let identity = if logged_in && provider == ProviderKind::Codex {
        let codex_dir = dir.map(Path::to_path_buf).or_else(|| {
            home.map(Path::to_path_buf)
                .or_else(crate::os::home_dir)
                .map(|h| h.join(".codex"))
        });
        codex_dir.as_deref().and_then(codex_identity_from_dir)
    } else {
        None
    };
    Some(AuthStatus {
        logged_in,
        identity,
    })
}

fn strip_ansi(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    let mut escape = false;
                    for c in chars.by_ref() {
                        if c == '\u{7}' || (escape && c == '\\') {
                            break;
                        }
                        escape = c == '\u{1b}';
                    }
                }
                _ => {}
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// 실패·취소 시 이번에 만든 빈 폴더만 제거한다. 내용·심볼릭 링크는 절대 지우지 않는다.
pub fn cleanup_empty_profile(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        && std::fs::remove_dir(dir).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_login_scopes_profiles_without_shell_syntax() {
        let dir = Path::new("profile 한글 with spaces;$x&data");
        for provider in ProviderKind::ALL {
            let command = login_command(Path::new("provider.cmd"), provider, Some(dir)).unwrap();
            assert_eq!(command.get_program(), "provider.cmd");
            let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
            for key in provider.credential_overrides() {
                if *key != "OPENCODE_DB" {
                    assert_eq!(env.get(std::ffi::OsStr::new(key)), Some(&None));
                }
            }
            match provider {
                ProviderKind::Antigravity => {
                    assert_eq!(command.get_args().count(), 1);
                    assert_eq!(
                        env[std::ffi::OsStr::new("SSH_CONNECTION")],
                        Some(std::ffi::OsStr::new("krill"))
                    );
                    assert_eq!(
                        env[std::ffi::OsStr::new("AGY_CLI_FORCE_FILE_TOKEN_STORAGE")],
                        Some(std::ffi::OsStr::new("1"))
                    );
                }
                ProviderKind::OpenCode => {
                    assert_eq!(
                        env[std::ffi::OsStr::new("XDG_DATA_HOME")],
                        Some(dir.join("data").as_os_str())
                    );
                    assert_eq!(
                        env[std::ffi::OsStr::new("XDG_STATE_HOME")],
                        Some(dir.join("state").as_os_str())
                    );
                }
                _ => assert_eq!(
                    env[std::ffi::OsStr::new(provider.env_var())],
                    Some(dir.as_os_str())
                ),
            }
        }
        assert!(
            login_command(
                Path::new("claude"),
                ProviderKind::Claude,
                Some(Path::new(""))
            )
            .is_none()
        );
        assert!(login_command(Path::new("gemini"), ProviderKind::Gemini, None).is_none());
        for provider in [ProviderKind::Claude, ProviderKind::Codex] {
            let command = login_command(Path::new(provider.as_str()), provider, None).unwrap();
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == provider.env_var() && value.is_none())
            );
        }
    }

    /// 호스트 기본 계정은 env 없이 다룬다. Claude는 `CLAUDE_CONFIG_DIR`이 설정되기만 해도(값이 `~/.claude`여도)
    /// 키체인 항목 이름에 해시 접미사를 붙여 다른 로그인을 본다(claude 2.1.302 `D1`).
    #[test]
    #[cfg(unix)] // 가짜 CLI가 /bin/sh 스크립트다.
    fn host_account_login_and_probe_drop_config_env() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("owe700-host-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("fake-cli");
        std::fs::write(
            &exe,
            "#!/bin/sh\n[ -z \"${CLAUDE_CONFIG_DIR+x}\" ] || { echo '{\"loggedIn\":false}'; exit 0; }\necho '{\"loggedIn\":true}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(probe(&exe, ProviderKind::Claude, None).unwrap().logged_in);
        assert!(
            !probe(&exe, ProviderKind::Claude, Some(&root))
                .unwrap()
                .logged_in
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn logged_out_is_distinct_from_missing_email_or_bad_output() {
        assert_eq!(
            parse_claude_status(r#"{"loggedIn":false}"#),
            Some(AuthStatus {
                logged_in: false,
                identity: None
            })
        );
        assert_eq!(
            parse_claude_status(r#"{"loggedIn":true}"#),
            Some(AuthStatus {
                logged_in: true,
                identity: None
            })
        );
        assert_eq!(parse_claude_status("invalid"), None);
    }

    #[test]
    #[cfg(unix)] // 가짜 CLI가 /bin/sh 스크립트다.
    fn fake_cli_probes_isolated_profile_and_codex_stderr() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("owe700-probe-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("fake-cli");
        let profile = root.join("profile with spaces");
        std::fs::create_dir_all(&profile).unwrap();
        let write = |script: &str| {
            std::fs::write(&exe, script).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        write(
            "#!/bin/sh\n[ \"$1 $2 $3\" = 'auth status --json' ] || exit 9\n[ -d \"$CLAUDE_CONFIG_DIR\" ] || exit 9\nprintf '%s\\n' '{\"loggedIn\":true,\"email\":\"test@example.invalid\",\"orgName\":\"Test\"}'\n",
        );
        let status = probe(&exe, ProviderKind::Claude, Some(&profile)).unwrap();
        assert!(status.logged_in);
        assert_eq!(status.identity.unwrap().email, "test@example.invalid");
        write(
            "#!/bin/sh\necho 'diagnostic' >&2\nprintf '{\\n  \"loggedIn\": true,\\n  \"email\": \"pretty@example.invalid\"\\n}\\n'\n",
        );
        assert_eq!(
            probe(&exe, ProviderKind::Claude, Some(&profile))
                .unwrap()
                .identity
                .unwrap()
                .email,
            "pretty@example.invalid"
        );
        write(
            "#!/bin/sh\n[ \"$1 $2\" = 'login status' ] || exit 9\n[ -d \"$CODEX_HOME\" ] || exit 9\necho 'Not logged in' >&2\nexit 1\n",
        );
        assert!(
            !probe(&exe, ProviderKind::Codex, Some(&profile))
                .unwrap()
                .logged_in
        );
        write("#!/bin/sh\necho 'Logged in using ChatGPT' >&2\n");
        assert!(
            probe(&exe, ProviderKind::Codex, Some(&profile))
                .unwrap()
                .logged_in
        );
        write("#!/bin/sh\necho 'unknown option' >&2\nexit 2\n");
        assert_eq!(probe(&exe, ProviderKind::Codex, Some(&profile)), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)] // 가짜 CLI가 /bin/sh 스크립트다.
    fn opencode_probe_uses_profile_xdg_paths() {
        use std::os::unix::fs::PermissionsExt as _;

        let root =
            std::env::temp_dir().join(format!("owe706-opencode-probe-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("opencode");
        let profile = root.join("profile");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\ntest \"$1 $2\" = 'auth list' && test \"$XDG_DATA_HOME\" = '{}/data' && test \"$XDG_STATE_HOME\" = '{}/state' && test \"$OPENCODE_DB\" = opencode.db && test -z \"${{OPENCODE_CONFIG_DIR+x}}\" || exit 9\nprintf '1 credential\\n'\n",
                profile.display(), profile.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let status = probe(&exe, ProviderKind::OpenCode, Some(&profile)).unwrap();
        assert!(status.logged_in);
        assert_eq!(status.identity, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agy_profile_login_state_comes_from_its_token_file() {
        let root = std::env::temp_dir().join(format!("owe706-agy-auth-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let token = root.join(".gemini/antigravity-cli/antigravity-oauth-token");
        std::fs::create_dir_all(token.parent().unwrap()).unwrap();
        std::fs::write(&token, b"fixture token contents").unwrap();
        assert!(
            probe(Path::new("/unused"), ProviderKind::Antigravity, Some(&root))
                .unwrap()
                .logged_in
        );
        // 토큰 파일이 없으면 모름이 아니라 로그인 필요다(OWE-901: 프로필 판정 통일).
        std::fs::remove_file(&token).unwrap();
        assert!(
            !probe(Path::new("/unused"), ProviderKind::Antigravity, Some(&root))
                .unwrap()
                .logged_in
        );
        assert!(!agy_token_logged_in(&root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)] // 가짜 CLI가 /bin/sh 스크립트다.
    fn provider_probe_uses_requested_home_path_and_distinguishes_cli_failures() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("owe719-env-probe-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("fake-cli");
        let write = |script: &str| {
            std::fs::write(&exe, script).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        write(
            "#!/bin/sh\n[ \"$HOME\" = \"$CLAUDE_CONFIG_DIR\" ] || exit 5\n[ \"$PATH\" = /fixture/bin ] || exit 6\necho '{\"loggedIn\":true}'\n",
        );
        assert!(
            probe_with_environment(
                &exe,
                ProviderKind::Claude,
                Some(&root),
                Some(&root),
                Some(std::ffi::OsStr::new("/fixture/bin"))
            )
            .unwrap()
            .logged_in
        );
        let runtime_name = format!("owe719-runtime-{}", std::process::id());
        let runtime = root.join(&runtime_name);
        std::fs::write(&runtime, "#!/bin/sh\necho '{\"loggedIn\":true}'\n").unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        write(&format!("#!/usr/bin/env {runtime_name}\n"));
        assert!(
            probe(&exe, ProviderKind::Claude, Some(&root)).is_none(),
            "데몬의 축소된 PATH에서는 런처 실행 실패"
        );
        assert!(
            probe_with_environment(
                &exe,
                ProviderKind::Claude,
                Some(&root),
                Some(&root),
                Some(root.as_os_str())
            )
            .unwrap()
            .logged_in
        );
        write("#!/bin/sh\nprintf '\\033[90m1 credentials\\033[0m\\n'\n");
        assert!(probe(&exe, ProviderKind::OpenCode, None).unwrap().logged_in);
        write("#!/bin/sh\necho '0 credentials'\n");
        assert!(!probe(&exe, ProviderKind::OpenCode, None).unwrap().logged_in);
        write("#!/bin/sh\necho 'authentication timed out' >&2\nexit 9\n");
        assert!(probe(&exe, ProviderKind::Codex, None).is_none());
        assert!(probe(&root.join("missing"), ProviderKind::Claude, None).is_none());
        write("#!/bin/sh\nexec /bin/sleep 10\n");
        let started = Instant::now();
        assert!(probe(&exe, ProviderKind::Claude, None).is_none());
        assert!(started.elapsed() < Duration::from_secs(8));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn cleanup_preserves_existing_data_and_symlinks() {
        let root = std::env::temp_dir().join(format!("owe700-cleanup-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let empty = root.join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(cleanup_empty_profile(&empty));
        let data = root.join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("credentials"), "keep").unwrap();
        assert!(!cleanup_empty_profile(&data));
        let link = root.join("link");
        std::os::unix::fs::symlink(&data, &link).unwrap();
        assert!(!cleanup_empty_profile(&link));
        assert_eq!(
            std::fs::read_to_string(data.join("credentials")).unwrap(),
            "keep"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_identity_extracts_email_and_rejects_apikey_or_symlinks() {
        let encode = |bytes: &[u8]| {
            const TABLE: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let (mut bits, mut count) = (0_u32, 0_u32);
            let mut out = String::new();
            for &byte in bytes {
                bits = bits.checked_shl(8).unwrap() | u32::from(byte);
                count = count.saturating_add(8);
                while count >= 6 {
                    count = count.saturating_sub(6);
                    let idx = usize::try_from(bits.checked_shr(count).unwrap() & 63).unwrap();
                    out.push(char::from(*TABLE.get(idx).unwrap()));
                }
            }
            if count > 0 {
                let rem = 6_u32.saturating_sub(count);
                let idx = usize::try_from(bits.checked_shl(rem).unwrap() & 63).unwrap();
                out.push(char::from(*TABLE.get(idx).unwrap()));
            }
            out
        };
        let jwt = |claims: serde_json::Value| {
            format!("e30.{}.sig", encode(&serde_json::to_vec(&claims).unwrap()))
        };
        let raw = serde_json::to_vec(&serde_json::json!({
            "tokens": { "id_token": jwt(serde_json::json!({ "email": " user@example.test " })) }
        }))
        .unwrap();
        assert_eq!(
            codex_identity_from_credentials(&raw),
            Some(Identity {
                email: "user@example.test".into(),
                org_id: None,
                org_name: None,
            })
        );
        let alt = serde_json::to_vec(&serde_json::json!({
            "tokens": {
                "idToken": jwt(serde_json::json!({
                    "https://api.openai.com/profile": { "email": "profile@example.test" }
                }))
            }
        }))
        .unwrap();
        assert_eq!(
            codex_identity_from_credentials(&alt).unwrap().email,
            "profile@example.test"
        );
        let apikey = serde_json::to_vec(&serde_json::json!({
            "OPENAI_API_KEY": "sk-test",
            "tokens": { "id_token": jwt(serde_json::json!({ "email": "user@example.test" })) }
        }))
        .unwrap();
        assert_eq!(codex_identity_from_credentials(&apikey), None);

        let root = std::env::temp_dir().join(format!("codex-id-dir-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(codex_identity_from_dir(&root), None);
        std::fs::write(root.join("auth.json"), &raw).unwrap();
        assert_eq!(
            codex_identity_from_dir(&root).unwrap().email,
            "user@example.test"
        );
        #[cfg(unix)]
        {
            let linked = root.join("linked");
            std::fs::create_dir_all(&linked).unwrap();
            std::os::unix::fs::symlink(root.join("auth.json"), linked.join("auth.json")).unwrap();
            assert_eq!(codex_identity_from_dir(&linked), None);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn probe_with_environment_handles_multibyte_boundary_and_large_output() {
        let dir = std::env::temp_dir().join(format!("krill-probe-boundary-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        let filler = "a".repeat(65_534);
        let body = format!(
            "#!/bin/sh\nprintf '%s' '{filler}가'\nprintf '\\377bad\\n' >&2\nprintf '{{\"loggedIn\":true}}'\n"
        );
        std::fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Check a second script where the valid JSON fits before 64KiB and invalid UTF-8 / >64KiB follows.
        let script2 = dir.join("codex");
        let tail = "b".repeat(70 * 1024);
        let body2 = format!(
            "#!/bin/sh\nprintf 'Logged in\\n%s가{tail}' '{filler}'\nprintf '\\377\\376' >&2\n"
        );
        std::fs::write(&script2, body2).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script2, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_env = dir.to_string_lossy().into_owned();
        let res = probe_with_environment(
            &script2,
            ProviderKind::Codex,
            None,
            Some(&dir),
            Some(std::ffi::OsStr::new(&path_env)),
        );
        assert_eq!(res.map(|s| s.logged_in), Some(true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agy_login_command_forces_file_storage_for_isolated_and_clears_for_host_default() {
        let dir = Path::new("/tmp/krill-agy-isolated");
        let isolated = login_command(Path::new("agy"), ProviderKind::Antigravity, Some(dir))
            .expect("isolated agy login command");
        let iso_env: std::collections::BTreeMap<_, _> = isolated.get_envs().collect();
        assert_eq!(
            iso_env.get(std::ffi::OsStr::new("SSH_CONNECTION")),
            Some(&Some(std::ffi::OsStr::new("krill")))
        );
        assert_eq!(
            iso_env.get(std::ffi::OsStr::new("AGY_CLI_FORCE_FILE_TOKEN_STORAGE")),
            Some(&Some(std::ffi::OsStr::new("1")))
        );

        let host = login_command(Path::new("agy"), ProviderKind::Antigravity, None)
            .expect("host default agy login command");
        let host_env: std::collections::BTreeMap<_, _> = host.get_envs().collect();
        assert_eq!(
            host_env.get(std::ffi::OsStr::new("SSH_CONNECTION")),
            Some(&None)
        );
        assert_eq!(
            host_env.get(std::ffi::OsStr::new("AGY_CLI_FORCE_FILE_TOKEN_STORAGE")),
            Some(&None)
        );
    }

    #[test]
    fn agy_probe_extracts_email_from_isolated_token_file() {
        let root =
            std::env::temp_dir().join(format!("krill-agy-probe-identity-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let token_dir = root.join(".gemini/antigravity-cli");
        std::fs::create_dir_all(&token_dir).unwrap();
        let envelope = serde_json::json!({
            "auth_method": "consumer",
            "id_token_email": "isolated@example.test",
            "token": { "access_token": "synthetic-access" }
        });
        std::fs::write(
            token_dir.join("antigravity-oauth-token"),
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();

        let status = probe(Path::new("agy"), ProviderKind::Antigravity, Some(&root))
            .expect("agy probe status");
        assert!(status.logged_in);
        assert_eq!(
            status.identity.map(|i| i.email),
            Some("isolated@example.test".to_owned())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests;
