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

pub(super) fn wrap(login: Launch, hint: &str, args: Vec<String>) -> Launch {
    // The running daemon is also the helper, so upgrades cannot select a different installation.
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("krilld.exe"));
    let mut argv = vec![
        executable.into_os_string().as_encoded_bytes().to_vec(),
        b"--agent-exec".to_vec(),
        hint.as_bytes().to_vec(),
    ];
    argv.extend(args.into_iter().map(String::into_bytes));
    Launch {
        argv,
        env: login.env,
    }
}

/// Internal `krilld --agent-exec <hint> <program> [args...]` entry point.
/// Inherit terminal handles and the holder's Job; never open another console or daemon socket.
#[cfg(windows)]
pub fn run(mut args: impl Iterator<Item = OsString>) -> ExitCode {
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
    if let Err(error) = crate::sys::protect_agent_launcher() {
        writeln!(io::stderr().lock(), "agent console setup failed: {error}").ok();
        return ExitCode::FAILURE;
    }
    match command.args(args).status() {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            writeln!(io::stderr().lock(), "agent launch failed: {error}").ok();
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
