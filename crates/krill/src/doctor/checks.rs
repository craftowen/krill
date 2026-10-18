//! 점검 판정 순수 함수(OWE-689): 수집한 [`Facts`] → [`Vec<Check>`].
//!
//! 파일·프로세스·소켓을 만지지 않는다. 수집은 부모(`doctor`)가 얇게 하고,
//! 사람용 출력·`--json` 모두 이 `Vec<Check>`에서 나온다.

use std::path::PathBuf;

use krill_proto::doctor::{Check, Fix, FixKind, Level, Status};

/// 데몬 연결 판정.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Daemon {
    /// 붙었다(없으면 띄워 재결합, `--no-start`면 소켓 ping만).
    Up {
        build: String,
        instance: u64,
        proto: u16,
        launched: bool,
    },
    /// 붙지 못했다.
    Down { error: String },
    /// 바이너리·상태 고장 때문에 시도하지 않았다.
    Skipped { reason: String },
}

/// PATH의 `krill` 판정(앱 온보딩 `Cli`와 같은 네 가지).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliPath {
    /// PATH의 `krill`가 이 설치와 같은 파일이다.
    Installed,
    /// PATH에 다른 `krill`가 있다. 덮어쓰지 않는다.
    Conflict { found: PathBuf },
    /// 이 경로에 링크를 만들 수 있다.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "Windows does not offer Unix symlinks")
    )]
    Offer { link: PathBuf },
    /// PATH 안에 쓰기 가능한 후보 디렉터리가 없다.
    Manual { hint: String },
}

/// 감지한 에이전트 하나. `exe`가 `None`이면 미설치다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub version: Option<String>,
    pub install: String,
}

/// `krill-holder`·`krilld` 존재·실행 권한.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helpers {
    pub ok: bool,
    /// 없을 때 이름 목록(`krill-holder`·`krilld` 중).
    pub missing: Vec<String>,
    /// 사람·JSON용 한 줄 설명.
    pub detail: String,
}

/// 상태 디렉터리·`run/` 쓰기 가능, 소유자 일치.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub ok: bool,
    pub dir: PathBuf,
}

/// 세그먼트 키 경고 파일 존재 여부와 기록 on/off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    /// 세그먼트 키 경고 파일 존재.
    pub warning: bool,
    /// `history.toml`에서 기록을 껐으면 판정하지 않는다.
    pub recording: bool,
}

/// 수집한 사실. 전부 평범한 값이라 단위 테스트에서 직접 만든다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub helpers: Helpers,
    /// 이 `krill`의 빌드 식별자(`krill_proto::BUILD`).
    pub cli_build: String,
    /// 붙은 krilld의 빌드. 못 붙었으면 `None`.
    pub daemon_build: Option<String>,
    pub state: State,
    pub daemon: Daemon,
    /// 감시 연결로 본 holder 상태. `None`이면 데몬에 닿지 못했거나 판단을 건너뛰었다.
    /// `Some(true)`면 holder 상실(OWE-684: 세션은 kept master fd로 서빙 중).
    pub holder_orphan: Option<bool>,
    pub history: History,
    /// 로그인·대화형 셸 PATH 해석 성공.
    pub shell_path_ok: bool,
    /// `git --version` 첫 줄.
    pub git_version: Option<String>,
    /// 프로필 전체(설치·미설치). 순서는 호출자가 정한다.
    pub agents: Vec<Agent>,
    /// `agent.any`가 `warn`일 때 보여 줄 대표 설치 명령.
    pub agent_install_hint: String,
    /// 영수증이 있는 hook ID(`codex` 등).
    pub hook_receipts: Vec<String>,
    /// 이 `krill` 실행 파일.
    pub krill_exe: PathBuf,
    pub cli: CliPath,
}

/// hook 점검 대상 ID(OWE-689 고정). Claude는 krilld가 세션마다 주입한다.
pub const HOOK_IDS: [&str; 3] = ["codex", "opencode", "cursor"];

fn fix(kind: FixKind, value: &str) -> Fix {
    Fix {
        kind,
        value: value.to_owned(),
    }
}

fn check(id: &str, level: Level, status: Status, summary: &str, detail: &str, fix: Fix) -> Check {
    Check {
        id: id.to_owned(),
        level,
        status,
        summary: summary.to_owned(),
        detail: detail.to_owned(),
        fix,
    }
}

/// [`Facts`]를 점검 목록으로 바꾼다. 순서는 OWE-689 항목 순서다.
#[must_use]
pub fn decide(facts: &Facts) -> Vec<Check> {
    let mut out = Vec::with_capacity(13 + facts.agents.len());
    out.push(bin_helpers(facts));
    out.push(bin_version(facts));
    out.push(state_writable(facts));
    out.push(daemon_connect(facts));
    out.push(holder_orphan(facts));
    out.push(history_key(facts));
    out.push(shell_path(facts));
    out.push(git(facts));
    out.push(agent_any(facts));
    for agent in &facts.agents {
        if agent.exe.is_some() {
            out.push(agent_info(agent));
        }
    }
    for id in HOOK_IDS {
        out.push(hook(facts, id));
    }
    out.push(cli_path(facts));
    out
}

fn bin_helpers(facts: &Facts) -> Check {
    if facts.helpers.ok {
        check(
            "bin.helpers",
            Level::Required,
            Status::Ok,
            "krill-holder and krilld are present",
            &facts.helpers.detail,
            fix(FixKind::None, ""),
        )
    } else {
        check(
            "bin.helpers",
            Level::Required,
            Status::Fail,
            "krill-holder or krilld is missing or not executable",
            &format!(
                "{} missing or not executable (reinstall Krill)",
                facts.helpers.missing.join(", ")
            ),
            fix(FixKind::Reinstall, ""),
        )
    }
}

/// 빌드 식별자의 버전 앞부분. `--version`이 보고하는 값으로, 개발 중 소스 해시(`+dev.…`)가
/// 달라져도 같은 릴리스면 일치한다. 바이너리를 따로 빌드하는 개발 트리에서도 흔들리지 않는다.
fn version_of(build: &str) -> &str {
    build.split_once('+').map_or(build, |(v, _)| v)
}

fn bin_version(facts: &Facts) -> Check {
    match &facts.daemon_build {
        None => check(
            "bin.version",
            Level::Required,
            Status::Skip,
            "daemon unreachable, versions not compared",
            "fix daemon.connect first",
            fix(FixKind::None, ""),
        ),
        Some(build) if version_of(build) == version_of(&facts.cli_build) => check(
            "bin.version",
            Level::Required,
            Status::Ok,
            "binaries are the same version",
            &format!(
                "version {} (krill {} / krilld {build})",
                version_of(&facts.cli_build),
                facts.cli_build
            ),
            fix(FixKind::None, ""),
        ),
        Some(build) => check(
            "bin.version",
            Level::Required,
            Status::Fail,
            "krill and krilld builds differ",
            &format!(
                "krill {} != krilld {build} (reinstall Krill)",
                facts.cli_build
            ),
            fix(FixKind::Reinstall, ""),
        ),
    }
}

fn state_writable(facts: &Facts) -> Check {
    if facts.state.ok {
        check(
            "state.writable",
            Level::Required,
            Status::Ok,
            "state directory is writable",
            &format!("{}", facts.state.dir.display()),
            fix(FixKind::None, ""),
        )
    } else {
        #[cfg(windows)]
        let (summary, detail, remedy) = (
            "state directory permission check is not supported",
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Windows ACL validation is not implemented yet",
            )
            .to_string(),
            fix(FixKind::None, ""),
        );
        #[cfg(unix)]
        let (summary, detail, remedy) = (
            "state directory is not writable",
            format!(
                "{} is not writable (check owner: ls -ld)",
                facts.state.dir.display()
            ),
            fix(
                FixKind::Command,
                &format!("ls -ld {}", facts.state.dir.display()),
            ),
        );
        check(
            "state.writable",
            Level::Required,
            Status::Fail,
            summary,
            &detail,
            remedy,
        )
    }
}

fn daemon_connect(facts: &Facts) -> Check {
    match &facts.daemon {
        Daemon::Up {
            build,
            instance,
            proto,
            launched,
        } => check(
            "daemon.connect",
            Level::Required,
            Status::Ok,
            "connected to krilld",
            &format!(
                "krilld {build} instance {instance} proto {proto}{}",
                if *launched { " (launched)" } else { "" }
            ),
            fix(FixKind::None, ""),
        ),
        Daemon::Down { error } => check(
            "daemon.connect",
            Level::Required,
            Status::Fail,
            "cannot talk to krilld",
            error,
            fix(FixKind::None, ""),
        ),
        Daemon::Skipped { reason } => check(
            "daemon.connect",
            Level::Required,
            Status::Skip,
            "daemon connection skipped",
            reason,
            fix(FixKind::None, ""),
        ),
    }
}

/// holder 상실(OWE-684). 데몬이 살아 있어 세션은 서빙 중이지만 krilld 재시작
/// 전에 새 holder가 인계받지 못하면 세션이 유실된다. `warn`이라 종료 코드에
/// 영향을 주지 않고, 다음 `krill` 연결이 새 holder를 띄워 치유한다.
fn holder_orphan(facts: &Facts) -> Check {
    match facts.holder_orphan {
        None => check(
            "holder.orphan",
            Level::Recommended,
            Status::Skip,
            "holder state is unknown",
            "daemon is unreachable or checks were skipped",
            fix(FixKind::None, ""),
        ),
        Some(false) => check(
            "holder.orphan",
            Level::Recommended,
            Status::Ok,
            "krill-holder is connected",
            "sessions are owned by the holder",
            fix(FixKind::None, ""),
        ),
        Some(true) => check(
            "holder.orphan",
            Level::Recommended,
            Status::Warn,
            "krill-holder is lost",
            "sessions keep running on kept master file descriptors, but if krilld restarts before a new holder takes over, sessions may be lost; run any krill command (e.g. `krill ls`) to launch a new holder",
            fix(FixKind::Command, "krill ls"),
        ),
    }
}

fn history_key(facts: &Facts) -> Check {
    if !facts.history.recording {
        return check(
            "history.key",
            Level::Required,
            Status::Skip,
            "disk recording is off",
            "recording = false in history.toml",
            fix(FixKind::None, ""),
        );
    }
    if facts.history.warning {
        check(
            "history.key",
            Level::Required,
            Status::Warn,
            "segment key is not accessible",
            "recording unavailable; check history.toml and unlock/restore the segment key in Keychain, then restart krilld",
            fix(FixKind::None, ""),
        )
    } else {
        check(
            "history.key",
            Level::Required,
            Status::Ok,
            "segment key is accessible",
            "no recording warning",
            fix(FixKind::None, ""),
        )
    }
}

fn shell_path(facts: &Facts) -> Check {
    if facts.shell_path_ok {
        check(
            "shell.path",
            Level::Required,
            Status::Ok,
            "login shell PATH resolved",
            "agent detection uses the login shell PATH",
            fix(FixKind::None, ""),
        )
    } else {
        check(
            "shell.path",
            Level::Required,
            Status::Fail,
            "login shell PATH unavailable",
            "login shell did not answer within 5s; agent detection may be wrong",
            fix(FixKind::None, ""),
        )
    }
}

fn git(facts: &Facts) -> Check {
    match &facts.git_version {
        Some(v) => check(
            "git",
            Level::Recommended,
            Status::Ok,
            "git is installed",
            v,
            fix(FixKind::None, ""),
        ),
        None => check(
            "git",
            Level::Recommended,
            Status::Warn,
            "git is not installed",
            "`git --version` failed (install Xcode Command Line Tools)",
            fix(FixKind::Copy, "xcode-select --install"),
        ),
    }
}

fn agent_any(facts: &Facts) -> Check {
    let found = facts.agents.iter().filter(|a| a.exe.is_some()).count();
    if found > 0 {
        check(
            "agent.any",
            Level::Recommended,
            Status::Ok,
            &format!("{found} agent(s) found"),
            &installed_names(facts),
            fix(FixKind::None, ""),
        )
    } else {
        check(
            "agent.any",
            Level::Recommended,
            Status::Warn,
            "no agent detected",
            "no built-in profile agent found on the login shell PATH",
            fix(FixKind::Copy, &facts.agent_install_hint),
        )
    }
}

fn installed_names(facts: &Facts) -> String {
    facts
        .agents
        .iter()
        .filter(|a| a.exe.is_some())
        .map(|a| a.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn agent_info(agent: &Agent) -> Check {
    check(
        &format!("agent.{}", agent.id),
        Level::Recommended,
        Status::Ok,
        &format!("{} detected", agent.name),
        agent.version.as_deref().unwrap_or("version unknown"),
        fix(FixKind::None, ""),
    )
}

fn hook(facts: &Facts, id: &str) -> Check {
    let installed = facts.agents.iter().any(|a| a.id == id && a.exe.is_some());
    if !installed {
        return check(
            &format!("hook.{id}"),
            Level::Recommended,
            Status::Skip,
            &format!("{id} agent is not installed"),
            "hook check needs the agent installed",
            fix(FixKind::None, ""),
        );
    }
    if facts.hook_receipts.iter().any(|r| r == id) {
        check(
            &format!("hook.{id}"),
            Level::Recommended,
            Status::Ok,
            &format!("{id} hook is installed"),
            "receipt recorded",
            fix(FixKind::None, ""),
        )
    } else {
        check(
            &format!("hook.{id}"),
            Level::Recommended,
            Status::Warn,
            &format!("{id} hook is not installed"),
            "receipt not found",
            fix(FixKind::Command, &format!("krill hook install {id}")),
        )
    }
}

fn cli_path(facts: &Facts) -> Check {
    match &facts.cli {
        CliPath::Installed => check(
            "cli.path",
            Level::Recommended,
            Status::Ok,
            "PATH krill is this installation",
            &format!("{}", facts.krill_exe.display()),
            fix(FixKind::None, ""),
        ),
        CliPath::Conflict { found } => check(
            "cli.path",
            Level::Recommended,
            Status::Warn,
            "another krill is on PATH",
            &format!("{} (not overwritten)", found.display()),
            fix(FixKind::Copy, &format!("{}", found.display())),
        ),
        CliPath::Offer { link } => check(
            "cli.path",
            Level::Recommended,
            Status::Warn,
            "krill is not on PATH",
            &format!("{}", link.display()),
            fix(FixKind::Command, &format!("{}", link.display())),
        ),
        CliPath::Manual { hint } => check(
            "cli.path",
            Level::Recommended,
            Status::Warn,
            "krill is not on PATH",
            hint,
            fix(FixKind::Copy, hint),
        ),
    }
}
