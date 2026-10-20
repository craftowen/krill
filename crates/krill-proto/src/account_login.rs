//! 프로바이더 로그인 명령과 비밀값 없는 로그인 상태 조회.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::control::Program;
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

/// `dir`이 `None`이면 호스트 기본 계정이다. env를 지워 실행한다: Claude는 `CLAUDE_CONFIG_DIR`이 있기만 해도
/// (값이 `~/.claude`여도) 다른 키체인 항목을 쓰는데, 호스트 세션은 env 없이 뜬다.
pub fn login_program(provider: ProviderKind, dir: Option<&Path>) -> Option<Program> {
    let tail: &[&str] = match provider {
        ProviderKind::Claude => &["claude", "auth", "login", "--claudeai"],
        ProviderKind::Codex => &["codex", "login"],
        // agy starts authentication in its interactive TUI; there is no `login` subcommand.
        ProviderKind::Antigravity => &[],
        ProviderKind::OpenCode => &["opencode", "auth", "login"],
        ProviderKind::Gemini => return None,
    };
    let mut args = if provider == ProviderKind::Antigravity {
        let mut args = vec![b"agy".to_vec()];
        if let Some(dir) = dir {
            if dir.as_os_str().is_empty() {
                return None;
            }
            args.insert(0, b"SSH_CONNECTION=krill".to_vec());
            args.push(format!("--gemini_dir={}", dir.join(".gemini").to_str()?).into_bytes());
        }
        args
    } else if provider == ProviderKind::OpenCode {
        match dir {
            Some(dir) if dir.as_os_str().is_empty() => return None,
            Some(dir) => vec![
                format!("XDG_DATA_HOME={}", dir.join("data").to_str()?).into_bytes(),
                format!("XDG_STATE_HOME={}", dir.join("state").to_str()?).into_bytes(),
                b"OPENCODE_DB=opencode.db".to_vec(),
            ],
            None => Vec::new(),
        }
    } else {
        match dir {
            Some(dir) if dir.as_os_str().is_empty() => return None,
            Some(dir) => vec![format!("{}={}", provider.env_var(), dir.to_str()?).into_bytes()],
            None => vec![b"-u".to_vec(), provider.env_var().as_bytes().to_vec()],
        }
    };
    let mut clean = Vec::new();
    for key in provider.credential_overrides() {
        clean.extend([b"-u".to_vec(), key.as_bytes().to_vec()]);
    }
    clean.extend(args);
    args = clean;
    args.extend(tail.iter().map(|s| s.as_bytes().to_vec()));
    Some(Program::Custom {
        program: b"/usr/bin/env".to_vec(),
        args,
    })
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

/// 비밀값을 출력하지 않는 CLI 상태 조회. stdout·stderr를 함께 읽되 결과에는 신원만 남긴다.
/// Codex는 상태를 stderr에 쓰며 로그아웃이면 비영(非零) 종료한다. `dir`이 `None`이면 호스트 기본 계정([`login_program`]).
pub fn probe(exe: &Path, provider: ProviderKind, dir: Option<&Path>) -> Option<AuthStatus> {
    if provider == ProviderKind::Antigravity {
        let dir = dir?;
        return Some(AuthStatus {
            logged_in: agy_token_logged_in(dir),
            identity: None,
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
    let mut cmd = Command::new(exe);
    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    for key in provider.credential_overrides() {
        cmd.env_remove(key);
    }
    match dir {
        Some(dir) if provider == ProviderKind::OpenCode => {
            cmd.env("XDG_DATA_HOME", dir.join("data"));
            cmd.env("XDG_STATE_HOME", dir.join("state"));
            cmd.env("OPENCODE_DB", "opencode.db");
            cmd.env_remove("OPENCODE_CONFIG_DIR");
        }
        Some(dir) => {
            cmd.env(provider.env_var(), dir);
        }
        None => {
            cmd.env_remove(provider.env_var());
        }
    }
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
            let mut text = String::new();
            let result = pipe
                .by_ref()
                .take(64 * 1024)
                .read_to_string(&mut text)
                .map(|_| text);
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
                child.kill().ok();
                child.wait().ok();
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
    Some(AuthStatus {
        logged_in,
        identity: None,
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
    fn login_uses_env_and_literal_profile_argument() {
        for (provider, expected) in [
            (
                ProviderKind::Claude,
                vec![
                    "CLAUDE_CONFIG_DIR=/tmp/a b;$x",
                    "claude",
                    "auth",
                    "login",
                    "--claudeai",
                ],
            ),
            (
                ProviderKind::Codex,
                vec!["CODEX_HOME=/tmp/a b;$x", "codex", "login"],
            ),
        ] {
            let mut expected_args = Vec::new();
            for key in provider.credential_overrides() {
                expected_args.extend([b"-u".to_vec(), key.as_bytes().to_vec()]);
            }
            expected_args.extend(expected.into_iter().map(|s| s.as_bytes().to_vec()));
            assert_eq!(
                login_program(provider, Some(Path::new("/tmp/a b;$x"))),
                Some(Program::Custom {
                    program: b"/usr/bin/env".to_vec(),
                    args: expected_args,
                })
            );
        }
        assert_eq!(
            login_program(ProviderKind::Claude, Some(Path::new(""))),
            None
        );
        assert_eq!(
            login_program(ProviderKind::Antigravity, Some(Path::new(""))),
            None
        );
        assert_eq!(
            login_program(ProviderKind::Antigravity, None),
            Some(Program::Custom {
                program: b"/usr/bin/env".to_vec(),
                args: vec![b"agy".to_vec()],
            })
        );
        let Some(Program::Custom { program, args }) =
            login_program(ProviderKind::OpenCode, Some(Path::new("/tmp/a")))
        else {
            panic!("OpenCode must support scoped account login");
        };
        assert_eq!(program, b"/usr/bin/env");
        assert!(args.contains(&b"XDG_DATA_HOME=/tmp/a/data".to_vec()));
        assert!(args.contains(&b"XDG_STATE_HOME=/tmp/a/state".to_vec()));
        assert!(args.contains(&b"OPENCODE_DB=opencode.db".to_vec()));
        assert!(args.ends_with(&[b"opencode".to_vec(), b"auth".to_vec(), b"login".to_vec()]));
        assert_eq!(
            login_program(ProviderKind::Antigravity, Some(Path::new("/tmp/a"))),
            Some(Program::Custom {
                program: b"/usr/bin/env".to_vec(),
                args: [
                    "SSH_CONNECTION=krill",
                    "agy",
                    &format!(
                        "--gemini_dir={}",
                        Path::new("/tmp/a").join(".gemini").to_string_lossy()
                    ),
                ]
                .into_iter()
                .map(|s| s.as_bytes().to_vec())
                .collect(),
            })
        );
    }

    /// 호스트 기본 계정은 env 없이 다룬다. Claude는 `CLAUDE_CONFIG_DIR`이 설정되기만 해도(값이 `~/.claude`여도)
    /// 키체인 항목 이름에 해시 접미사를 붙여 다른 로그인을 본다(claude 2.1.302 `D1`).
    #[test]
    #[cfg(unix)] // 가짜 CLI가 /bin/sh 스크립트다.
    fn host_account_login_and_probe_drop_config_env() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut clean = Vec::new();
        for key in ProviderKind::Claude.credential_overrides() {
            clean.extend([b"-u".to_vec(), key.as_bytes().to_vec()]);
        }
        clean.extend(
            [
                "-u",
                "CLAUDE_CONFIG_DIR",
                "claude",
                "auth",
                "login",
                "--claudeai",
            ]
            .into_iter()
            .map(|s| s.as_bytes().to_vec()),
        );
        assert_eq!(
            login_program(ProviderKind::Claude, None),
            Some(Program::Custom {
                program: b"/usr/bin/env".to_vec(),
                args: clean,
            })
        );
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
}
