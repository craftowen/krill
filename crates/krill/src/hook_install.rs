//! MK-048 consent and diff for shared hook installation and rollback.
//! Defaults to HOME; custom agent configuration roots are respected.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use krill_proto::agent::hooks::Agent;
use krill_proto::daemon;
use krill_proto::hook_install::{Action, Plan, TargetAgent};

const USAGE: &str =
    "usage: krill hook install|uninstall <claude|codex|agy|cursor|opencode|pi> [--dry-run] [--yes]";
const CONSENT: &str = "Apply the changes shown above? [y/N] ";

pub fn run(action: &str, args: &[&str]) -> ExitCode {
    if args.iter().any(|s| matches!(*s, "-h" | "--help" | "help")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match execute(action, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("krill hook {action}: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("krill hook {action}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
}

fn execute(action: &str, args: &[&str]) -> io::Result<()> {
    if !matches!(action, "install" | "uninstall") {
        return Err(usage_error(USAGE));
    }
    let agent = args
        .first()
        .and_then(|s| TargetAgent::parse(s))
        .ok_or_else(|| usage_error(USAGE))?;
    if args
        .iter()
        .skip(1)
        .any(|s| !matches!(*s, "--yes" | "--dry-run"))
    {
        return Err(usage_error(USAGE));
    }
    let home = canonicalize(Path::new(
        &home_dir().ok_or_else(|| io::Error::other("HOME is unset"))?,
    ))?;
    let state = absolute(
        daemon::state_dir().ok_or_else(|| io::Error::other("state directory unavailable"))?,
    )?;
    let operation = if action == "install" {
        Action::Install
    } else {
        Action::Uninstall
    };
    let path = if operation == Action::Uninstall {
        PathBuf::new() // Rollback follows receipt paths, independent of current environment overrides.
    } else if agent == TargetAgent::Other(Agent::Codex) {
        directory_override("CODEX_HOME", home.join(".codex"))?.join("config.toml")
    } else {
        config_path(agent, &home)?
    };
    let Some(plan) = Plan::prepare(operation, agent, &path, &state, &std::env::current_exe()?)?
    else {
        if operation == Action::Install {
            println!(
                "hooks already recorded; uninstall first to change or recover the installation"
            );
        } else {
            println!("no Krill hooks recorded for {}", agent.id());
        }
        return Ok(());
    };
    plan.write_diff(&mut io::stdout())?;
    if args.contains(&"--dry-run") {
        println!("preview only; no files written");
        return Ok(());
    }
    if !args.contains(&"--yes") && !consent()? {
        println!("cancelled; no files written");
        return Ok(());
    }
    plan.apply()?;
    println!("hooks {action} complete; pre-write backups use .krill-bak-* suffixes");
    Ok(())
}

fn consent() -> io::Result<bool> {
    print!("{CONSENT}");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// Resolve existing ancestor symlinks only for environment-supplied roots (e.g. /tmp on macOS).
/// The selected config file and descendants of this root still reject symlinks.
fn absolute(path: PathBuf) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestor = path.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(
            ancestor
                .file_name()
                .ok_or_else(|| io::Error::other("invalid configuration root"))?
                .to_owned(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| io::Error::other("missing configuration root"))?;
    }
    let mut result = canonicalize(ancestor)?;
    for component in tail.into_iter().rev() {
        result.push(component);
    }
    Ok(result)
}

/// 에이전트 설정이 놓이는 사용자 홈. Windows는 Claude Code·Cursor·Codex가 `~`로 쓰는 `%USERPROFILE%`이 우선이다
/// (Git Bash가 넘기는 `HOME`은 그 다음).
pub fn home_dir() -> Option<std::ffi::OsString> {
    #[cfg(windows)]
    if let Some(home) = std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()) {
        return Some(home);
    }
    std::env::var_os("HOME").filter(|h| !h.is_empty())
}

/// [`std::fs::canonicalize`]. Windows는 드라이브 경로의 `\\?\` 접두사를 뗀다: 접두사가 붙으면 `/`가 구분자로
/// 안 읽혀 `.cursor/hooks.json` 같은 하위 경로가 깨지고, 사용자에게 보이는 diff·receipt 경로도 낯설어진다.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(rest) = path.to_str().and_then(|p| p.strip_prefix(r"\\?\"))
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(rest));
    }
    Ok(path)
}

fn directory_override(variable: &str, default: PathBuf) -> io::Result<PathBuf> {
    absolute(std::env::var_os(variable).map_or(default, PathBuf::from))
}

fn config_path(agent: TargetAgent, home: &Path) -> io::Result<PathBuf> {
    Ok(match agent {
        TargetAgent::Claude => krill_proto::hook_install::claude_settings_path(
            &directory_override("CLAUDE_CONFIG_DIR", home.join(".claude"))?,
            home,
        ),
        TargetAgent::Other(Agent::Cursor) => home.join(".cursor/hooks.json"),
        TargetAgent::Other(Agent::Agy) => home.join(".gemini/config/hooks.json"),
        TargetAgent::Other(Agent::OpenCode) => {
            let xdg = directory_override("XDG_CONFIG_HOME", home.join(".config"))?;
            krill_proto::user_dirs::opencode_plugin(&directory_override(
                "OPENCODE_CONFIG_DIR",
                xdg.join("opencode"),
            )?)
        }
        TargetAgent::Other(Agent::Pi) => krill_proto::agent::hooks::pi_extension_path(
            &directory_override("PI_CODING_AGENT_DIR", home.join(".pi/agent"))?,
        ),
        TargetAgent::Other(Agent::Codex) => return Err(io::Error::other("Codex uses RPC trust")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pi_hook_install_uninstall_and_user_edits_use_receipts() {
        use krill_proto::agent::hooks;
        use std::fs;
        let root = std::env::temp_dir().join(format!("krill-pi-hooks-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let root = canonicalize(&root).unwrap();
        let path = hooks::pi_extension_path(&root.join("agent"));
        let state = root.join("state");
        let exe = root.join("krill with ' quotes");
        let agent = TargetAgent::parse("pi").unwrap();
        assert_eq!(agent, TargetAgent::Other(Agent::Pi));
        assert!(USAGE.contains("|pi>"));
        let plan = Plan::prepare(Action::Install, agent, &path, &state, &exe)
            .unwrap()
            .unwrap();
        assert!(!path.exists(), "preparation and preview never write");
        let mut diff = Vec::new();
        plan.write_diff(&mut diff).unwrap();
        assert!(String::from_utf8(diff).unwrap().contains("session_start"));
        let receipt = plan.apply().unwrap();
        assert!(receipt.complete);
        assert_eq!(receipt.agent, "pi");
        let installed = fs::read_to_string(&path).unwrap();
        assert_eq!(installed, hooks::pi_extension(&exe).unwrap());
        assert!(
            Plan::prepare(Action::Install, agent, &path, &state, &exe)
                .unwrap()
                .is_none()
        );
        Plan::prepare(Action::Uninstall, agent, &path, &state, &exe)
            .unwrap()
            .unwrap()
            .apply()
            .unwrap();
        assert!(!path.exists());
        assert!(!hooks::receipt(&state, Agent::Pi).exists());
        Plan::prepare(Action::Install, agent, &path, &state, &exe)
            .unwrap()
            .unwrap()
            .apply()
            .unwrap();
        let edited = installed + "// user edit\n";
        fs::write(&path, &edited).unwrap();
        Plan::prepare(Action::Uninstall, agent, &path, &state, &exe)
            .unwrap()
            .unwrap()
            .apply()
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        assert!(Plan::prepare(Action::Install, agent, &path, &state, &exe).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn help_returns_zero() {
        assert_eq!(run("install", &["--help"]), ExitCode::SUCCESS);
        assert_eq!(run("uninstall", &["-h"]), ExitCode::SUCCESS);
        // 도움말 플래그는 설치 경로와 겹치지 않는다.
        assert_eq!(run("install", &["--dry-run", "--help"]), ExitCode::SUCCESS);
    }
}
