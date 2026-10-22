//! `krill doctor`(MK-025, MK-067): 경로를 보이고 고장을 진단한 뒤 krilld에 붙는다(없거나 holder를 잃었으면 띄워 재결합).
//! 탐지하는 고장: 데몬 바이너리 누락, 상태 디렉터리 쓰기 불가, git 없음, 데몬 연결 실패. 확인하지 않은 크래시
//! 로그는 경고로 보인다. 하나라도 FAIL이면 1로 끝난다.
//!
//! `krill doctor --bundle`: 리댁션한 진단 번들(`krill_proto::diag`)을 쓰고 경로를 출력한다. 데몬에 붙지 않는다.
//!
//! `krill doctor --json [--no-start]`(MK-128): [`checks::decide`]의 `Vec<Check>`를 stdout 한 개 JSON 문서로
//! 출력한다. `--no-start`면 데몬을 띄우지 않고 소켓 ping만 한다. 사람용 출력도 같은 `Vec<Check>`를 그린다.

mod checks;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
#[cfg(unix)]
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
#[cfg(unix)]
use std::sync::mpsc;
use std::time::Duration;

use krill_proto::agent::{self, Agents};
use krill_proto::daemon::{self, HolderStatus, Paths};
use krill_proto::diag;
use krill_proto::doctor::{Check, Report, SCHEMA, Status};
use krill_proto::history;
use krill_proto::ipc::ClientRole;
use krill_proto::telemetry::{self, Consent, Reason};
#[cfg(unix)]
use rustix::fs::{Access, access};

use checks::{Agent as AgentFact, CliPath, Daemon, Facts, HOOK_IDS, Helpers, History, State};

/// 로그인 셸이 PATH를 알려 주기까지 기다리는 최대 시간(앱 온보딩과 같은 5초).
#[cfg(unix)]
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);
/// rc 파일이 stdout에 무엇을 찍어도 PATH 줄을 골라내기 위한 표시.
#[cfg(unix)]
const PATH_MARK: &str = "__MK_PATH__=";
/// 데몬을 띄우지 않는 ping의 Hello 상한.
const PING_TIMEOUT: Duration = Duration::from_secs(5);

/// 사람용 출력에 덧붙이는 정보(점검은 아니어서 JSON에 안 나간다).
struct Extra {
    crashes: usize,
    crash_dir: PathBuf,
    records: Vec<String>,
    record_file: PathBuf,
    state: Option<PathBuf>,
}

struct Collected {
    paths: Option<Paths>,
    paths_error: Option<String>,
    facts: Facts,
    extra: Extra,
}

pub fn run() -> ExitCode {
    let collected = collect(false);
    let checks = checks::decide(&collected.facts);
    render(&collected, &checks);
    exit_code(&checks)
}

pub fn run_json(no_start: bool) -> ExitCode {
    let collected = collect(no_start);
    let checks = checks::decide(&collected.facts);
    let report = Report {
        schema: SCHEMA,
        checks: checks.clone(),
    };
    match serde_json::to_string_pretty(&report) {
        Ok(text) => {
            println!("{text}");
            exit_code(&report.checks)
        }
        Err(e) => {
            eprintln!("doctor: {e}");
            ExitCode::FAILURE
        }
    }
}

fn exit_code(checks: &[Check]) -> ExitCode {
    if checks.iter().any(|c| c.status == Status::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn find<'a>(checks: &'a [Check], id: &str) -> Option<&'a Check> {
    checks.iter().find(|c| c.id == id)
}

fn render(collected: &Collected, checks: &[Check]) {
    render_head(collected);
    render_legacy(collected, checks);
    render_checks(checks);
}

/// 소켓·바이너리 경로 머리말.
fn render_head(collected: &Collected) {
    if let Some(paths) = &collected.paths {
        println!("socket     {}", paths.socket.display());
        println!("krill-holder  {}", paths.holder.display());
        println!("krilld     {}", paths.krilld.display());
    } else {
        let error = collected.paths_error.as_deref().unwrap_or("unknown");
        println!("socket     unknown ({error})");
        println!("krill-holder  unknown ({error})");
        println!("krilld     unknown ({error})");
    }
    #[cfg(unix)]
    println!("launch     dev build (posix_spawn + setsid; TCC attribution follows the launcher)");
    #[cfg(windows)]
    println!("launch     detached holder (Windows bootstrap + Job breakaway)");
}

/// 현행 줄 형식을 그대로 그리는 항목(helpers·state·crash·record·telemetry·git·daemon·history).
fn render_legacy(collected: &Collected, checks: &[Check]) {
    if !collected.facts.helpers.ok {
        for name in &collected.facts.helpers.missing {
            println!("{name:<10} FAIL: missing or not executable (reinstall Krill)");
        }
    }
    if let Some(c) = find(checks, "bin.version") {
        match c.status {
            Status::Ok => println!("version    ok: {}", c.detail),
            Status::Fail => println!("version    FAIL: {}", c.detail),
            Status::Skip => println!("version    SKIP: {}", c.detail),
            Status::Warn => println!("version    WARN: {}", c.detail),
        }
    }
    if let Some(c) = find(checks, "state.writable")
        && c.status == Status::Fail
    {
        println!("state      FAIL: {}", c.detail);
    }
    if collected.extra.crashes > 0 {
        println!(
            "crash      WARN: {} unreported crash log(s) in {}; run `krill doctor --bundle`",
            collected.extra.crashes,
            collected.extra.crash_dir.display()
        );
    }
    if !collected.extra.records.is_empty() {
        println!(
            "record     WARN: {} recording warning(s) in {}; disk history may be incomplete",
            collected.extra.records.len(),
            collected.extra.record_file.display()
        );
    }
    if let Some(state) = &collected.extra.state {
        // 정보 표시만 하며 종료 코드에 영향을 주지 않는다.
        match telemetry::consent(Some(state), &telemetry::Env::live()) {
            Consent::Enabled => println!("telemetry  on (opt-in)"),
            Consent::Disabled(Reason::NoConsentFile) => {
                println!("telemetry  off (default; nothing is sent)");
            }
            Consent::Disabled(Reason::EnvVeto(var)) => {
                println!("telemetry  off ({var} is set; nothing is sent)");
            }
        }
    }
    if let Some(c) = find(checks, "git") {
        match c.status {
            Status::Ok => println!("git        ok: {}", c.detail),
            _ => println!("git        WARN: {}", c.detail),
        }
    }
    if let Some(c) = find(checks, "daemon.connect") {
        match c.status {
            Status::Ok => println!("daemon     ok: {}", c.detail),
            Status::Fail => println!("daemon     FAIL: {}", c.detail),
            Status::Skip => println!("daemon     SKIP: {}", c.detail),
            Status::Warn => println!("daemon     WARN: {}", c.detail),
        }
    }
    // OWE-684: holder 상실은 데몬이 살아 있어도 경고한다. 정상이면 조용하다.
    if let Some(c) = find(checks, "holder.orphan") {
        match c.status {
            Status::Warn => println!("holder     WARN: {}", c.detail),
            Status::Fail => println!("holder     FAIL: {}", c.detail),
            Status::Ok | Status::Skip => {}
        }
    }
    if let Some(c) = find(checks, "history.key")
        && c.status == Status::Warn
    {
        println!("history    WARN: {}", c.detail);
    }
}

/// 새로 생긴 점검 항목(version·shell·agents·hooks·cli).
fn render_checks(checks: &[Check]) {
    if let Some(c) = find(checks, "shell.path") {
        match c.status {
            Status::Ok => println!("shell      ok: {}", c.summary),
            _ => println!("shell      FAIL: {}", c.detail),
        }
    }
    if let Some(c) = find(checks, "agent.any") {
        match c.status {
            Status::Ok => println!("agents     ok: {} ({})", c.summary, c.detail),
            _ => println!("agents     WARN: {} (install: {})", c.summary, c.fix.value),
        }
    }
    for c in checks {
        if let Some(id) = c.id.strip_prefix("agent.")
            && id != "any"
        {
            println!("{id:<10} ok: {} {}", c.summary, c.detail);
        }
    }
    for id in HOOK_IDS {
        if let Some(c) = find(checks, &format!("hook.{id}")) {
            match c.status {
                Status::Ok => println!("hook.{id:<6} ok: {}", c.summary),
                Status::Skip => println!("hook.{id:<6} SKIP: {}", c.summary),
                _ => println!("hook.{id:<6} WARN: {} ({})", c.summary, c.fix.value),
            }
        }
    }
    if let Some(c) = find(checks, "cli.path") {
        match c.status {
            Status::Ok => println!("cli        ok: {}", c.summary),
            _ => println!("cli        WARN: {} ({})", c.summary, c.detail),
        }
    }
}

/// 수집(얇게). 판정은 [`checks::decide`]가 한다.
fn collect(no_start: bool) -> Collected {
    let paths_error: Option<String>;
    let paths = match Paths::dev() {
        Ok(p) => {
            paths_error = None;
            Some(p)
        }
        Err(e) => {
            paths_error = Some(e.to_string());
            None
        }
    };
    let state_dir = daemon::state_dir();
    let (helpers_ok, helpers_missing, helpers_detail) = check_helpers(paths.as_ref());
    let state_ok = state_dir.as_deref().is_some_and(state_writable);
    // holder 감시 상태는 데몬을 띄우지 않는 감시 핸드셰이크로만 본다(OWE-684).
    // `connect_daemon`이 고아 상태에서 새 holder를 띄워 치유하므로 그보다 먼저 관측한다.
    let holder_orphan = match &paths {
        Some(p) if helpers_ok && state_ok => probe_holder(p).map(|s| s == HolderStatus::Orphan),
        _ => None,
    };
    let daemon = match &paths {
        None => Daemon::Down {
            error: paths_error
                .clone()
                .unwrap_or_else(|| "paths unknown".to_owned()),
        },
        Some(p) if !helpers_ok || !state_ok => Daemon::Skipped {
            reason: "fix the failures above first".to_owned(),
        },
        Some(p) if no_start => ping_daemon(p),
        Some(p) => connect_daemon(p),
    };
    let daemon_build = match &daemon {
        Daemon::Up { build, .. } => Some(build.clone()),
        _ => None,
    };
    let login = login_path();
    let shell_path_ok = login.is_some();
    let path = login.unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let git = git_version();
    let state_path = state_dir.clone().unwrap_or_default();
    let agents = collect_agents(&path);
    let hook_receipts = HOOK_IDS
        .into_iter()
        .filter(|id| {
            state_dir.as_deref().is_some_and(|state| {
                krill_proto::agent::hooks::Agent::parse(id)
                    .is_some_and(|agent| krill_proto::agent::hooks::receipt(state, agent).exists())
            })
        })
        .map(str::to_owned)
        .collect();
    let krill_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("krill"));
    let home: Option<PathBuf> = crate::hook_install::home_dir().map(PathBuf::from);
    let facts = Facts {
        helpers: Helpers {
            ok: helpers_ok,
            missing: helpers_missing,
            detail: helpers_detail,
        },
        cli_build: krill_proto::BUILD.to_owned(),
        daemon_build,
        state: State {
            ok: state_ok,
            dir: state_dir.clone().unwrap_or_else(|| PathBuf::from("$HOME")),
        },
        daemon,
        holder_orphan,
        history: History {
            warning: state_dir
                .as_deref()
                .is_some_and(|state| state.join(history::RECORDING_WARNING_FILE).exists()),
            recording: !state_dir.as_deref().is_some_and(history_recording_off),
        },
        shell_path_ok,
        git_version: git,
        agents,
        agent_install_hint: builtin_install_hint(),
        hook_receipts,
        krill_exe: krill_exe.clone(),
        cli: cli_path(&krill_exe, &path, home.as_deref()),
    };
    let extra = Extra {
        crashes: state_dir.as_deref().map_or(0, |s| diag::pending(s).len()),
        crash_dir: diag::crash_dir(&state_path),
        records: state_dir
            .as_deref()
            .map_or_else(Vec::new, diag::record_warnings),
        record_file: state_path.join(diag::RECORD_WARNINGS_FILE),
        state: state_dir,
    };
    Collected {
        paths,
        paths_error,
        facts,
        extra,
    }
}

/// `krill-holder`·`krilld` 존재·실행 권한(현행 doctor와 같은 `access` 검사).
fn check_helpers(paths: Option<&Paths>) -> (bool, Vec<String>, String) {
    let Some(paths) = paths else {
        return (
            false,
            vec!["krill-holder".to_owned(), "krilld".to_owned()],
            "paths unknown".to_owned(),
        );
    };
    let mut missing = Vec::new();
    for (name, bin) in [("krill-holder", &paths.holder), ("krilld", &paths.krilld)] {
        if !executable(bin) {
            missing.push(name.to_owned());
        }
    }
    if missing.is_empty() {
        (
            true,
            missing,
            format!(
                "{} + {} executable",
                paths.holder.display(),
                paths.krilld.display()
            ),
        )
    } else {
        (false, missing.clone(), missing.join(", "))
    }
}

/// 상태 디렉터리·`run/` 쓰기 가능, 소유자=현재 사용자. 없으면 데몬이 만든다.
#[cfg(unix)]
fn state_writable(state: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let me = rustix::process::geteuid().as_raw();
    for dir in [state.to_path_buf(), state.join("run")] {
        if !dir.exists() {
            continue;
        }
        if access(&dir, Access::WRITE_OK | Access::EXEC_OK).is_err() {
            return false;
        }
        if std::fs::metadata(&dir).is_ok_and(|m| m.uid() != me) {
            return false;
        }
    }
    true
}

/// holder 감시 상태를 묻는다(데몬을 띄우지 않는다). 소켓이 없거나
/// 감시 미지원 구 데몬이면 `None`.
fn probe_holder(paths: &Paths) -> Option<HolderStatus> {
    let hello = daemon::watch_hello(ClientRole::Cli, "krill doctor");
    let c = daemon::handshake(&paths.socket, &hello, PING_TIMEOUT).ok()?;
    c.holder_status
}

/// krilld에 붙는다. 없으면 holder를 띄워 재결합까지 기다린다(현행 doctor).
fn connect_daemon(paths: &Paths) -> Daemon {
    let hello = daemon::client_hello(ClientRole::Cli, "krill doctor");
    match daemon::connect(paths, &hello) {
        Ok(c) => Daemon::Up {
            build: c.server.build,
            instance: c.server.instance_id,
            proto: c.negotiated.proto,
            launched: c.launched,
        },
        Err(e) => Daemon::Down {
            error: e.to_string(),
        },
    }
}

/// 소켓 ping만(데몬을 띄우지 않는다).
fn ping_daemon(paths: &Paths) -> Daemon {
    let hello = daemon::client_hello(ClientRole::Cli, "krill doctor");
    match daemon::handshake(&paths.socket, &hello, PING_TIMEOUT) {
        Ok(c) => Daemon::Up {
            build: c.server.build,
            instance: c.server.instance_id,
            proto: c.negotiated.proto,
            launched: false,
        },
        Err(e) => Daemon::Down {
            error: e.to_string(),
        },
    }
}

/// `history.toml`에서 기록을 껐는지. 없거나 못 읽으면 켠 것으로 본다.
#[derive(serde::Deserialize)]
struct HistoryToml {
    recording: Option<bool>,
}

fn history_recording_off(state: &Path) -> bool {
    std::fs::read_to_string(state.join("history.toml"))
        .ok()
        .and_then(|text| toml::from_str::<HistoryToml>(&text).ok())
        .and_then(|parsed| parsed.recording)
        == Some(false)
}

/// `shell -l -i -c`로 로그인·대화형 셸의 PATH를 얻는다. 실패하거나 [`SHELL_TIMEOUT`]을 넘기면 `None`.
/// 앱 온보딩(`login_path`)과 같은 방식. 읽기는 별도 스레드+시간 제한이라 rc가 파이프를
/// 쥐고 버텨도 호출자는 돌아온다.
#[cfg(unix)]
fn login_path() -> Option<OsString> {
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| OsString::from("/bin/zsh"));
    let script = format!("printf '\\n{PATH_MARK}%s\\n' \"$PATH\"");
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).ok();
        tx.send(text).ok();
    });
    let out = rx.recv_timeout(SHELL_TIMEOUT).ok()?;
    child.kill().ok();
    child.wait().ok();
    out.lines()
        .rev()
        .find_map(|l| l.strip_prefix(PATH_MARK))
        .filter(|p| !p.is_empty())
        .map(OsString::from)
}

/// 내장 프로필·PATH에서 에이전트를 찾는다(앱 온보딩 `survey`와 같은 범위).
fn collect_agents(path: &OsString) -> Vec<AgentFact> {
    let file = daemon::state_dir().map(|d| d.join(agent::FILE_NAME));
    let user = file
        .as_deref()
        .and_then(|f| std::fs::read_to_string(f).ok());
    let agents = Agents::load(user.as_deref())
        .or_else(|_| Agents::load(None))
        .unwrap_or_default();
    let mut rows: Vec<_> = agents.profiles.into_iter().collect();
    rows.sort_by(|(a, _), (b, _)| {
        agent::profile_rank(a)
            .cmp(&agent::profile_rank(b))
            .then_with(|| a.cmp(b))
    });
    rows.into_iter()
        .map(|(id, p)| {
            let exe = p.program().and_then(|prog| agent::which(prog, path));
            AgentFact {
                version: exe.as_deref().and_then(agent::version),
                exe,
                id,
                name: p.name,
                install: p.install,
            }
        })
        .collect()
}

/// `agent.any`가 `warn`일 때 보여 줄 대표 설치 명령(내장 Claude 프로필).
fn builtin_install_hint() -> String {
    Agents::load(None)
        .ok()
        .and_then(|agents| agents.profiles.get("claude").map(|p| p.install.clone()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "curl -fsSL https://claude.ai/install.sh | bash".to_owned())
}

#[cfg(unix)]
fn is_broken_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok() && std::fs::metadata(path).is_err()
}

/// PATH의 `krill`가 이 설치와 같은 파일인지(앱 온보딩 `Cli`와 같은 네 가지).
#[cfg(unix)]
fn cli_path(exe: &Path, path: &OsString, home: Option<&Path>) -> CliPath {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join("krill");
        if candidate.is_file() && access(&candidate, Access::EXEC_OK).is_ok() {
            let same = std::fs::canonicalize(&candidate)
                .ok()
                .zip(std::fs::canonicalize(exe).ok())
                .is_some_and(|(found, mine)| found == mine);
            return if same {
                CliPath::Installed
            } else {
                CliPath::Conflict { found: candidate }
            };
        }
        if is_broken_symlink(&candidate) {
            return CliPath::Conflict { found: candidate };
        }
    }
    let mut preferred = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = home {
        preferred.push(home.join(".local/bin"));
        preferred.push(home.join("bin"));
    }
    if let Some(dir) = std::env::split_paths(path)
        .find(|d| preferred.contains(d) && access(d, Access::WRITE_OK | Access::EXEC_OK).is_ok())
    {
        return CliPath::Offer {
            link: dir.join("krill"),
        };
    }
    CliPath::Manual {
        hint: if let Some(home) = home {
            format!(
                "ln -s {} {} (then add ~/.local/bin to PATH)",
                krill_proto::agent::hooks::shell_quote(&exe.to_string_lossy()),
                home.join(".local/bin/krill").display()
            )
        } else {
            format!(
                "sudo ln -s {} /usr/local/bin/krill",
                krill_proto::agent::hooks::shell_quote(&exe.to_string_lossy())
            )
        },
    }
}

/// `git --version`의 첫 줄. PATH에 없거나(새 macOS의 `/usr/bin/git`은 CLT 없이는 실패한다) 실패하면 `None`.
fn git_version() -> Option<String> {
    let out = Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    out.status
        .success()
        .then(|| text.lines().next().map(str::to_owned))
        .flatten()
}

pub fn bundle() -> ExitCode {
    let Some(state) = daemon::state_dir() else {
        println!("bundle     FAIL: HOME is not set");
        return ExitCode::FAILURE;
    };
    match diag::write_bundle(&state) {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("bundle     FAIL: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    access(path, Access::EXEC_OK).is_ok()
}

#[cfg(windows)]
fn executable(path: &Path) -> bool {
    path.parent().is_some_and(|dir| {
        path.to_str()
            .and_then(|program| agent::which(program, dir.as_os_str()))
            .is_some()
    })
}

#[cfg(windows)]
fn login_path() -> Option<OsString> {
    std::env::var_os("PATH")
}

/// 상태 디렉터리·`run/`에 실제로 써 본다. Unix 소유자(uid) 검사는 Windows ACL에 대응하는 것이 없어 건너뛴다
/// ([`checks`]가 이유를 보인다). 없는 디렉터리는 데몬이 만든다.
#[cfg(windows)]
fn state_writable(state: &Path) -> bool {
    [state.to_path_buf(), state.join("run")]
        .iter()
        .filter(|dir| dir.exists())
        .all(|dir| {
            let probe = dir.join(format!(".krill-doctor-{}", std::process::id()));
            let written = std::fs::write(&probe, b"").is_ok();
            std::fs::remove_file(&probe).ok();
            written
        })
}

#[cfg(windows)]
fn cli_path(exe: &Path, path: &OsString, _home: Option<&Path>) -> CliPath {
    if let Some(found) = agent::which("krill", path) {
        let same = std::fs::canonicalize(&found)
            .ok()
            .zip(std::fs::canonicalize(exe).ok())
            .is_some_and(|(found, mine)| found == mine);
        if same {
            CliPath::Installed
        } else {
            CliPath::Conflict { found }
        }
    } else {
        CliPath::Manual {
            hint: format!("add {} to PATH", exe.parent().unwrap_or(exe).display()),
        }
    }
}
