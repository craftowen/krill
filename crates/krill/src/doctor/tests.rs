//! 판정 순수 함수 경계 테스트(OWE-689 완료 조건).

use std::path::PathBuf;

use super::checks::{Agent, CliPath, Daemon, Facts, Helpers, History, State};
use krill_proto::doctor::Status;

fn ok_facts() -> Facts {
    Facts {
        helpers: Helpers {
            ok: true,
            missing: Vec::new(),
            detail: "krill-holder + krilld executable".to_owned(),
        },
        cli_build: "0.1.1+dev.abc".to_owned(),
        daemon_build: Some("0.1.1+dev.abc".to_owned()),
        state: State {
            ok: true,
            dir: PathBuf::from("/tmp/state"),
        },
        daemon: Daemon::Up {
            build: "0.1.1+dev.abc".to_owned(),
            instance: 7,
            proto: 1,
            launched: false,
        },
        holder_orphan: Some(false),
        history: History {
            warning: false,
            recording: true,
        },
        shell_path_ok: true,
        git_version: Some("git version 2.49.0".to_owned()),
        agents: vec![
            Agent {
                id: "claude".to_owned(),
                name: "Claude Code".to_owned(),
                exe: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                version: Some("2.1.0".to_owned()),
                install: "curl -fsSL https://claude.ai/install.sh | bash".to_owned(),
            },
            Agent {
                id: "codex".to_owned(),
                name: "Codex".to_owned(),
                exe: Some(PathBuf::from("/opt/homebrew/bin/codex")),
                version: Some("0.163.0".to_owned()),
                install: "npm install -g @openai/codex".to_owned(),
            },
        ],
        agent_install_hint: "curl -fsSL https://claude.ai/install.sh | bash".to_owned(),
        hook_receipts: vec![
            "codex".to_owned(),
            "opencode".to_owned(),
            "cursor".to_owned(),
        ],
        krill_exe: PathBuf::from("/tmp/bin/krill"),
        cli: CliPath::Installed,
    }
}

fn status_of(checks: &[krill_proto::doctor::Check], id: &str) -> Status {
    checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("missing check {id}"))
        .status
}

#[test]
fn all_ok_when_facts_ok() {
    let checks = super::checks::decide(&ok_facts());
    for c in &checks {
        // 미설치 에이전트의 hook은 skip이다.
        let ok = c.status == Status::Ok || (c.status == Status::Skip && c.id.starts_with("hook."));
        assert!(ok, "{}: {:?}", c.id, c.status);
    }
    assert!(checks.iter().any(|c| c.id == "agent.claude"));
    assert!(checks.iter().any(|c| c.id == "hook.codex"));
}

#[test]
fn missing_helpers_fail() {
    let mut f = ok_facts();
    f.helpers.ok = false;
    f.helpers.missing = vec!["krill-holder".to_owned()];
    let checks = super::checks::decide(&f);
    let c = checks.iter().find(|c| c.id == "bin.helpers").unwrap();
    assert_eq!(c.status, Status::Fail);
    assert!(matches!(
        c.fix.kind,
        krill_proto::doctor::FixKind::Reinstall
    ));
}

#[test]
fn version_mismatch_fails_and_unreachable_skips() {
    let mut f = ok_facts();
    f.daemon_build = Some("0.2.0+dev.other".to_owned());
    assert_eq!(
        status_of(&super::checks::decide(&f), "bin.version"),
        Status::Fail
    );
    f.daemon_build = None;
    assert_eq!(
        status_of(&super::checks::decide(&f), "bin.version"),
        Status::Skip
    );
}

#[test]
fn same_version_different_dev_hash_is_ok() {
    let mut f = ok_facts();
    f.daemon_build = Some("0.1.1+dev.other".to_owned());
    assert_eq!(
        status_of(&super::checks::decide(&f), "bin.version"),
        Status::Ok
    );
}

#[test]
fn unwritable_state_fails_with_ls_hint() {
    let mut f = ok_facts();
    f.state.ok = false;
    let checks = super::checks::decide(&f);
    let c = checks.iter().find(|c| c.id == "state.writable").unwrap();
    assert_eq!(c.status, Status::Fail);
    #[cfg(unix)]
    assert!(c.fix.value.starts_with("ls -ld "), "{}", c.fix.value);
    #[cfg(windows)]
    assert!(c.fix.value.starts_with("icacls "), "{}", c.fix.value);
}

#[test]
fn daemon_down_fails_and_skipped_stays_skip() {
    let mut f = ok_facts();
    f.daemon = Daemon::Down {
        error: "cannot talk to krilld: refused".to_owned(),
    };
    assert_eq!(
        status_of(&super::checks::decide(&f), "daemon.connect"),
        Status::Fail
    );
    f.daemon = Daemon::Skipped {
        reason: "fix the failures above first".to_owned(),
    };
    assert_eq!(
        status_of(&super::checks::decide(&f), "daemon.connect"),
        Status::Skip
    );
}

/// Windows도 실제 holder 기동 경로를 사용하고, 실행 파일 누락은 미지원이 아닌 실패다.
#[test]
fn missing_daemon_helper_reports_start_failure() {
    let dir = std::env::temp_dir().join(format!("kd-{}", std::process::id()));
    let paths = krill_proto::daemon::Paths {
        socket: dir.join("krilld.sock"),
        holder: dir.join("missing-holder"),
        krilld: dir.join("missing-krilld"),
    };
    let result = super::connect_daemon(&paths);
    assert!(
        matches!(&result, Daemon::Down { error } if error.contains("executable not found")),
        "{result:?}"
    );
    assert!(!dir.exists(), "missing helpers must not start a daemon");
}

/// OWE-684: holder 상실은 경고하고(종료 코드 영향 없음) 복구 힌트를 남긴다.
#[test]
fn holder_orphan_warns_with_recovery_hint() {
    let mut f = ok_facts();
    f.holder_orphan = Some(true);
    let checks = super::checks::decide(&f);
    let c = checks.iter().find(|c| c.id == "holder.orphan").unwrap();
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.contains("krilld restarts"), "{}", c.detail);
    assert!(c.detail.contains("krill ls"), "{}", c.detail);
}

#[test]
fn holder_linked_is_ok_and_unknown_skips() {
    assert_eq!(
        status_of(&super::checks::decide(&ok_facts()), "holder.orphan"),
        Status::Ok
    );
    let mut f = ok_facts();
    f.holder_orphan = None;
    assert_eq!(
        status_of(&super::checks::decide(&f), "holder.orphan"),
        Status::Skip
    );
}

#[test]
fn shell_path_failure_is_fail_not_warn() {
    let mut f = ok_facts();
    f.shell_path_ok = false;
    assert_eq!(
        status_of(&super::checks::decide(&f), "shell.path"),
        Status::Fail
    );
}

#[test]
fn history_off_skips_and_warning_warns() {
    let mut f = ok_facts();
    f.history.recording = false;
    assert_eq!(
        status_of(&super::checks::decide(&f), "history.key"),
        Status::Skip
    );
    let mut f = ok_facts();
    f.history.warning = true;
    assert_eq!(
        status_of(&super::checks::decide(&f), "history.key"),
        Status::Warn
    );
}

#[test]
fn missing_git_is_warn_with_copy_fix() {
    let mut f = ok_facts();
    f.git_version = None;
    let checks = super::checks::decide(&f);
    let c = checks.iter().find(|c| c.id == "git").unwrap();
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix.value, "xcode-select --install");
}

#[test]
fn zero_agents_warns_with_install_hint() {
    let mut f = ok_facts();
    f.agents.clear();
    let checks = super::checks::decide(&f);
    let c = checks.iter().find(|c| c.id == "agent.any").unwrap();
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix.value, f.agent_install_hint);
    assert!(
        !checks
            .iter()
            .any(|c| c.id != "agent.any" && c.id.starts_with("agent."))
    );
}

#[test]
fn missing_hook_warns_with_install_command() {
    let mut f = ok_facts();
    f.agents.push(Agent {
        id: "cursor".to_owned(),
        name: "Cursor CLI".to_owned(),
        exe: Some(PathBuf::from("/opt/homebrew/bin/cursor-agent")),
        version: None,
        install: "curl https://cursor.com/install -fsS | bash".to_owned(),
    });
    f.hook_receipts.retain(|r| r != "cursor");
    let checks = super::checks::decide(&f);
    let c = checks.iter().find(|c| c.id == "hook.cursor").unwrap();
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix.value, "krill hook install cursor");
}

#[test]
fn hook_for_uninstalled_agent_is_skipped() {
    let checks = super::checks::decide(&ok_facts());
    let id = "cursor";
    let c = checks
        .iter()
        .find(|c| c.id == format!("hook.{id}"))
        .unwrap();
    assert_eq!(c.status, Status::Skip, "{id}");
    assert!(c.summary.contains("not installed"), "{}", c.summary);
    let c = checks.iter().find(|c| c.id == "hook.codex").unwrap();
    assert_eq!(c.status, Status::Ok);
}

#[test]
fn cli_conflict_and_manual_warn() {
    use krill_proto::doctor::FixKind;
    let mut f = ok_facts();
    f.cli = CliPath::Conflict {
        found: PathBuf::from("/usr/bin/krill"),
    };
    let c = super::checks::decide(&f)
        .into_iter()
        .find(|c| c.id == "cli.path")
        .unwrap();
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix.kind, FixKind::Copy);
    assert_eq!(c.fix.value, "/usr/bin/krill");
    f.cli = CliPath::Offer {
        link: PathBuf::from("/opt/homebrew/bin/krill"),
    };
    let c = super::checks::decide(&f)
        .into_iter()
        .find(|c| c.id == "cli.path")
        .unwrap();
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix.kind, FixKind::Command);
    assert_eq!(c.fix.value, "/opt/homebrew/bin/krill");
    f.cli = CliPath::Manual {
        hint: "ln -s /tmp/bin/krill /tmp/home/.local/bin/krill (then add ~/.local/bin to PATH)"
            .to_owned(),
    };
    let c = super::checks::decide(&f)
        .into_iter()
        .find(|c| c.id == "cli.path")
        .unwrap();
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix.kind, FixKind::Copy);
    assert!(c.fix.value.contains("~/.local/bin"), "{}", c.fix.value);
}
