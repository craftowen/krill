//! Windows agent trampoline: Rust quotes argv, including npm batch launchers, without PowerShell parsing.
//! The holder owns its ConPTY/Job; this process waits for the agent and shares that lifetime.

use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::Command;
#[cfg(windows)]
use std::process::ExitCode;

use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::usage::ProviderKind;

use crate::shell::Launch;

/// Batch files need Rust's `Command` batch quoting rather than the holder's CRT argv encoding.
pub(crate) fn wrap_shell(mut login: Launch) -> Launch {
    let batch = login.argv.first().is_some_and(|program| {
        std::path::Path::new(OsStr::from_bytes(program))
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
    });
    if !batch {
        return login;
    }
    let args = std::mem::take(&mut login.argv);
    let mut launch = wrap(login, "command not found", Vec::new(), false);
    launch.argv.extend(args);
    launch
}

/// `then_shell`: 에이전트가 끝나면 같은 `ConPTY`에서 로그인 셸을 띄운다(Unix `PREFLIGHT`와 같은 약속).
/// 사용자 지정 배치 셸([`wrap_shell`])은 그 자체가 셸이라 끄고 쓴다.
pub(super) fn wrap(login: Launch, hint: &str, args: Vec<String>, then_shell: bool) -> Launch {
    // The running daemon is also the helper, so upgrades cannot select a different installation.
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("krilld.exe"));
    let flag: &[u8] = if then_shell {
        AGENT_THEN_SHELL
    } else {
        b"--agent-exec"
    };
    let mut argv = vec![
        executable.into_os_string().as_encoded_bytes().to_vec(),
        flag.to_vec(),
        hint.as_bytes().to_vec(),
    ];
    argv.extend(args.into_iter().map(String::into_bytes));
    Launch {
        argv,
        env: login.env,
    }
}

/// [`wrap`]의 `then_shell` 진입 플래그. 인자 모양은 `--agent-exec`와 같다.
pub const AGENT_THEN_SHELL: &[u8] = b"--agent-exec-then-shell";

/// Internal `krilld --agent-exec <hint> <program> [args...]` entry point.
/// Inherit terminal handles and the holder's Job; never open another console or daemon socket.
/// `then_shell`이면 에이전트가 끝난 뒤(설치 안 됨 127은 제외) 로그인 셸을 띄우고 그 종료 코드를 돌려준다.
#[cfg(windows)]
pub fn run(mut args: impl Iterator<Item = OsString>, then_shell: bool) -> ExitCode {
    let Some(hint) = args.next() else {
        return ExitCode::from(2);
    };
    let Some(program) = args.next() else {
        return ExitCode::from(2);
    };
    let env = std::env::vars_os()
        .map(|(k, v)| (k.as_encoded_bytes().to_vec(), v.as_encoded_bytes().to_vec()))
        .collect();
    let Some(mut command) = command(&program, env) else {
        writeln!(io::stderr().lock(), "{}", hint.to_string_lossy()).ok();
        return ExitCode::from(127);
    };
    let args: Vec<_> = args.collect();
    // agy needs SSH mode with --gemini_dir even when no account marker is present.
    if std::path::Path::new(&program)
        .file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|name| matches!(name, "agy" | "antigravity"))
        && args
            .iter()
            .any(|arg| arg.to_string_lossy().starts_with("--gemini_dir="))
    {
        command.env("SSH_CONNECTION", "krill");
    }
    crate::sys::console_utf8();
    if let Err(error) = crate::sys::protect_agent_launcher() {
        writeln!(io::stderr().lock(), "agent console setup failed: {error}").ok();
        return ExitCode::FAILURE;
    }
    let code = match command.args(args).status() {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            writeln!(io::stderr().lock(), "agent launch failed: {error}").ok();
            ExitCode::FAILURE
        }
    };
    if !then_shell {
        return code;
    }
    // 셸은 에이전트 계정 env 없이, 세션 env(`MK_PANE_ID` 등)는 그대로 받는다.
    let login = crate::shell::Shell::Login.launch();
    let Some((program, shell_args)) = login.argv.split_first() else {
        return code;
    };
    let mut shell = Command::new(OsStr::from_bytes(program));
    shell
        .args(shell_args.iter().map(|a| OsStr::from_bytes(a)))
        .env_remove("KRILL_ACCOUNT_PROVIDER")
        .env_remove("KRILL_ACCOUNT_DIR");
    match shell.status() {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            writeln!(io::stderr().lock(), "shell launch failed: {error}").ok();
            ExitCode::FAILURE
        }
    }
}

pub(super) fn command(program: &OsStr, mut env: Vec<(Vec<u8>, Vec<u8>)>) -> Option<Command> {
    let value = |key: &[u8]| {
        env.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let provider =
        ProviderKind::parse(std::str::from_utf8(&value(b"KRILL_ACCOUNT_PROVIDER")).ok()?);
    let dir = value(b"KRILL_ACCOUNT_DIR");
    if let Some(provider) = provider {
        env.retain(|(key, _)| {
            !key.eq_ignore_ascii_case(provider.env_var().as_bytes())
                && !provider
                    .credential_overrides()
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name.as_bytes()))
        });
        if !dir.is_empty() {
            match provider {
                ProviderKind::OpenCode => {
                    env.retain(|(key, _)| {
                        !key.eq_ignore_ascii_case(b"XDG_DATA_HOME")
                            && !key.eq_ignore_ascii_case(b"XDG_STATE_HOME")
                    });
                    for (key, suffix) in [
                        (b"XDG_DATA_HOME".as_slice(), "data"),
                        (b"XDG_STATE_HOME", "state"),
                    ] {
                        let path = PathBuf::from(OsStr::from_bytes(&dir)).join(suffix);
                        env.push((
                            key.to_vec(),
                            path.into_os_string().as_encoded_bytes().to_vec(),
                        ));
                    }
                    env.push((b"OPENCODE_DB".to_vec(), b"opencode.db".to_vec()));
                }
                ProviderKind::Antigravity => {
                    env.retain(|(key, _)| !key.eq_ignore_ascii_case(b"SSH_CONNECTION"));
                    env.push((b"SSH_CONNECTION".to_vec(), b"krill".to_vec()));
                }
                _ => env.push((provider.env_var().as_bytes().to_vec(), dir)),
            }
        }
    }
    env.retain(|(key, _)| {
        !key.eq_ignore_ascii_case(b"KRILL_ACCOUNT_PROVIDER")
            && !key.eq_ignore_ascii_case(b"KRILL_ACCOUNT_DIR")
    });
    let path = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(b"PATH"))
        .map(|(_, v)| OsStr::from_bytes(v))?;
    let executable = krill_proto::agent::which(program.to_str()?, path)?;
    let mut command = Command::new(executable);
    command.env_clear().envs(
        env.iter()
            .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
    );
    Some(command)
}

#[cfg(test)]
mod tests;
