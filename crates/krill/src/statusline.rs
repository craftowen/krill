//! `krill statusline`(OWE-701): Claude statusLine이 부른다. 무슨 일이 있어도 0으로 끝난다.
//!
//! - 실행 모드(`krill statusline [--chain <명령>]`: 데몬 `--settings`·사용자 `settings.json`의
//!   statusLine): stdin JSON의 `rate_limits`를 `$CLAUDE_CONFIG_DIR/usage_cache.json`에 기록하고,
//!   사용자 명령이 있으면 실행해 그 stdout을 그대로 내보낸다(체인 보존). 기록·체인 실패는
//!   stderr에만 남기고 표시를 깨지 않는다.
//! - 설치 모드(`krill statusline install|uninstall [--dry-run] [--yes]`): 사용자
//!   `settings.json`의 statusLine 슬롯을 동의·백업·원문 보존 경로(`hook_install` receipt)로만
//!   건드린다. 자동 설치는 없다. 미지원 Claude 버전(파싱된 버전이 1.0.64 미만)에는 주입하지 않는다.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::daemon;
use krill_proto::hook_install::{Change, Receipt, store};
use krill_proto::statusline::{self, DEPTH_ENV, OwnCommand, SlotState, classify_command};

const USAGE: &str = "usage: krill statusline [--chain <command>] | krill statusline install|uninstall [--dry-run] [--yes]";
const CONSENT: &str = "Apply the changes shown above? [y/N] ";
const RECEIPT_AGENT: &str = "claude-statusline";
const RECEIPT_FILE: &str = "hooks/statusline-claude.json";
/// stdin 전체 읽기 제한. statusline 페이로드는 수 KB라 넉넉하다.
const STDIN_MAX: u64 = 1024 * 1024;
/// stdin EOF 대기. 넘으면 가진 만큼으로 진행한다(틱 pile-up 방지).
const STDIN_WAIT: Duration = Duration::from_secs(5);
/// 틱 전체(stdin 대기 + 체인 실행) 제한. 넘으면 체인을 죽이고 빈 출력으로 둔다.
const CHAIN_WAIT: Duration = Duration::from_secs(10);
/// 체인 셸 종료 확인 간격.
const CHAIN_POLL: Duration = Duration::from_millis(10);
/// 체인 셸이 끝난 뒤 이미 파이프에 있는 출력을 거두는 유예.
const CHAIN_EXIT_GRACE: Duration = Duration::from_millis(50);
/// `claude --version` 프로브 제한.
const PROBE_WAIT: Duration = Duration::from_secs(10);

pub fn run(args: &[OsString]) -> ExitCode {
    // `--chain <command>` 값 자리는 사용자 명령이므로 첫 두 토큰만 본다.
    let head: Vec<_> = args.iter().take(2).collect();
    if head
        .iter()
        .any(|a| matches!(a.to_str(), Some("-h" | "--help" | "help")))
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match args.first().and_then(|s| s.to_str()) {
        Some("install" | "uninstall") => manage(args),
        _ => run_mode(args),
    }
}

/// 실행 모드. 어떤 실패도 statusline 표시를 깨지 않게 항상 0으로 끝난다.
fn run_mode(args: &[OsString]) -> ExitCode {
    let deadline = std::time::Instant::now() + CHAIN_WAIT;
    let parsed = parse_run_args(args);
    let payload = read_stdin(STDIN_WAIT, STDIN_MAX);
    let dir = config_dir();
    if let Some(state) = daemon::state_dir()
        && let Some(profile) = dir.as_deref()
        && let Ok(data) = serde_json::from_slice(&payload)
    {
        statusline::context::record(&state, profile, &data).ok();
    }
    if let Some(dir) = &dir {
        record(dir, &payload, now_epoch());
    }
    let output = match resolve_chain(
        parsed.chain.as_deref(),
        dir.as_ref(),
        std::env::var_os(DEPTH_ENV).is_some(),
        &exe_string(),
    ) {
        Some(command) => run_chain_within(
            &command,
            &payload,
            deadline.saturating_duration_since(std::time::Instant::now()),
        ),
        None => Vec::new(),
    };
    io::stdout().write_all(&output).ok();
    ExitCode::SUCCESS
}

struct RunArgs {
    chain: Option<OsString>,
}

/// 실행 인자. 모르는 플래그는 무시한다(틱 경로에서 미래 플래그에 깨지지 않게).
fn parse_run_args(args: &[OsString]) -> RunArgs {
    let mut chain = None;
    let mut rest: &[OsString] = args;
    while let Some((first, tail)) = rest.split_first() {
        rest = tail;
        if first == "--chain" {
            if let Some((value, left)) = tail.split_first() {
                chain = Some(value.clone());
                rest = left;
            }
        } else if let Some(value) = first.to_str().and_then(|s| s.strip_prefix("--chain=")) {
            chain = Some(OsString::from(value));
        }
    }
    RunArgs { chain }
}

/// stdin을 EOF까지 읽는다. 제한을 넘으면 앞에서 자르고, 마감이 지나면 가진 만큼만 쓴다.
fn read_stdin(wait: Duration, max: u64) -> Vec<u8> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("statusline-stdin".into())
        .spawn(move || {
            let mut buffer = Vec::new();
            let outcome = io::stdin()
                .take(max)
                .read_to_end(&mut buffer)
                .map(|_| buffer)
                .unwrap_or_default();
            sender.send(outcome).ok();
        })
        .ok();
    receiver.recv_timeout(wait).unwrap_or_default()
}

/// 기록 대상 디렉터리. `CLAUDE_CONFIG_DIR`이 없으면 `~/.claude`(Windows `%USERPROFILE%\.claude`)로 둔다.
fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)
        && !dir.as_os_str().is_empty()
    {
        return Some(dir);
    }
    crate::hook_install::home_dir().map(|home| PathBuf::from(home).join(".claude"))
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `rate_limits`가 있으면 캐시 파일을 쓴다. 실패는 stderr에만 남긴다.
fn record(dir: &Path, payload: &[u8], now: i64) {
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return;
    };
    let Some(rate) = statusline::extract_rate_limits(&payload) else {
        return;
    };
    let path = statusline::cache_path(dir);
    let existing = std::fs::read_to_string(&path).ok();
    if !statusline::should_record(existing.as_deref(), rate, now) {
        return;
    }
    let text = statusline::render_cache(existing.as_deref(), rate, now);
    if let Err(e) = statusline::write_cache_file(dir, &text) {
        eprintln!("krill statusline: cache write skipped: {e}");
    }
}

/// 현재 실행 파일 경로 문자열. 못 읽으면 관례 이름으로 둔다.
fn exe_string() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .unwrap_or_else(|| "krill".to_owned())
}

/// 체인 명령을 정한다. 명시 `--chain`이 우선, 없으면 `settings.json` 슬롯을 본다.
/// 깊이 가드가 있으면(우리 체인 안에서 다시 불림) 체인하지 않는다.
fn resolve_chain(
    explicit: Option<&OsStr>,
    config_dir: Option<&PathBuf>,
    depth_set: bool,
    exe: &str,
) -> Option<OsString> {
    if depth_set {
        return None;
    }
    if let Some(command) = explicit.filter(|c| !c.is_empty()) {
        return Some(command.to_owned());
    }
    let dir = config_dir.as_ref()?;
    let text = std::fs::read_to_string(dir.join("settings.json")).ok()?;
    let slot = statusline::read_slot(Some(&text)).ok()??;
    let command = slot.get("command")?.as_str()?;
    match classify_command(command, exe) {
        OwnCommand::Bare => None,
        OwnCommand::Chained(original) => Some(OsString::from(original)),
        OwnCommand::Foreign => Some(OsString::from(command)),
    }
}

/// 체인 명령을 해석할 POSIX 셸. 설치한 statusLine 명령 자체가 POSIX 인용이라 Claude Code가 그 명령을
/// 돌린 셸과 같은 종류여야 한다: Windows는 Git Bash(`CLAUDE_CODE_GIT_BASH_PATH`, 없으면 PATH의 `git` 옆
/// `bin\bash.exe`, 그것도 없으면 PATH의 `sh`).
#[cfg(unix)]
fn chain_shell() -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(windows)]
fn chain_shell() -> PathBuf {
    std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let git = krill_proto::agent::which("git", &std::env::var_os("PATH")?)?;
            Some(git.parent()?.parent()?.join("bin").join("bash.exe")).filter(|b| b.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("sh"))
}

/// 사용자 명령을 [`chain_shell`]`-c`로 실행하고 stdout을 그대로 돌려준다.
/// 실패·타임아웃은 빈 출력이다(표시 우선).
fn run_chain_within(command: &OsStr, input: &[u8], wait: Duration) -> Vec<u8> {
    let deadline = std::time::Instant::now() + wait;
    let mut shell = std::process::Command::new(chain_shell());
    shell
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .env(DEPTH_ENV, "1");
    // 마감에 셸만 죽이면 셸이 기다리던 자식이 고아로 남는다. 자기 그룹으로 띄워 그룹째 정리한다.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut shell, 0);
    let mut child = match shell.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("krill statusline: chain spawn skipped: {e}");
            return Vec::new();
        }
    };
    let mut stdin = child.stdin.take();
    let input = input.to_vec();
    // 기다리지 않는다: 셸이 끝난 뒤에도 자식이 stdin을 쥐고 있으면 쓰기가 끝나지 않는다.
    std::thread::Builder::new()
        .name("statusline-chain-stdin".into())
        .spawn(move || {
            if let Some(mut stdin) = stdin.take() {
                stdin.write_all(&input).ok();
            }
        })
        .ok();
    // stdout을 별도 스레드에서 읽어야 출력을 닫지 않는 명령도 마감 시각에 kill에 닿는다.
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx
                            .send(chunk.get(..n).unwrap_or_default().to_vec())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
    }
    if let Some(output) = collect_until_exit(&mut child, &rx, deadline) {
        return output;
    }
    // 거두기 전에 보낸다: 거두지 않은 리더가 그룹 ID를 붙잡아 재사용되지 않는다.
    #[cfg(unix)]
    if let Some(pid) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).ok();
    }
    child.kill().ok();
    child.wait().ok();
    Vec::new()
}

/// 셸이 끝날 때까지 stdout 조각을 모은다. 셸이 끝나면 EOF를 기다리지 않는다: 백그라운드 자식이 stdout을
/// 쥐고 있어도 그때까지 받은 출력으로 끝낸다. 마감까지 셸이 안 끝나면 `None`.
fn collect_until_exit(
    child: &mut std::process::Child,
    chunks: &std::sync::mpsc::Receiver<Vec<u8>>,
    deadline: std::time::Instant,
) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let exited = child.try_wait().ok()?.is_some();
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return exited.then_some(output);
        }
        // ponytail: 셸이 끝난 뒤 파이프에 남은 꼬리는 읽기 스레드가 CHAIN_EXIT_GRACE 안에 넘긴다고 본다.
        // EOF가 오면 바로 끝나므로 이 유예는 백그라운드 자식이 stdout을 쥔 경우에만 쓰인다.
        let wait = if exited { CHAIN_EXIT_GRACE } else { CHAIN_POLL };
        match chunks.recv_timeout(wait.min(left)) {
            Ok(chunk) => output.extend(chunk),
            Err(_) if exited => return Some(output),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(CHAIN_POLL.min(left));
            }
        }
    }
}

fn usage_error(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn manage(args: &[OsString]) -> ExitCode {
    let action = args.first().and_then(|s| s.to_str()).unwrap_or_default();
    let flags = args.get(1..).unwrap_or_default();
    if flags
        .iter()
        .any(|s| s.to_str().is_none_or(|s| s != "--yes" && s != "--dry-run"))
    {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let yes = flags.iter().any(|s| s == "--yes");
    let dry_run = flags.iter().any(|s| s == "--dry-run");
    match execute(action, yes, dry_run, &mut io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("krill statusline {action}: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("krill statusline {action}: {e}");
            ExitCode::FAILURE
        }
    }
}

struct InstallPaths {
    settings: PathBuf,
    receipt: PathBuf,
    lock_dir: PathBuf,
}

fn execute(action: &str, yes: bool, dry_run: bool, out: &mut impl Write) -> io::Result<()> {
    let home = crate::hook_install::home_dir().ok_or_else(|| io::Error::other("HOME is unset"))?;
    let home = crate::hook_install::canonicalize(Path::new(&home))?;
    // 환경 루트는 심볼릭링크를 풀어 둔다(`/tmp` 등, `store::safe_path` 기준).
    let resolve_dir = |path: PathBuf| crate::hook_install::canonicalize(&path).unwrap_or(path);
    let config_dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => resolve_dir(PathBuf::from(dir)),
        _ => home.join(".claude"),
    };
    let state =
        daemon::state_dir().ok_or_else(|| io::Error::other("state directory unavailable"))?;
    let state = resolve_dir(state);
    let exe = std::env::current_exe()?;
    let exe = exe
        .to_str()
        .ok_or_else(|| io::Error::other("krill executable path is not UTF-8"))?;
    let paths = InstallPaths {
        settings: krill_proto::hook_install::claude_settings_path(&config_dir, &home),
        receipt: state.join(RECEIPT_FILE),
        lock_dir: state.join("hooks"),
    };
    match action {
        "install" => install(&paths, exe, yes, dry_run, out),
        "uninstall" => uninstall(&paths, exe, yes, dry_run, out),
        _ => Err(usage_error(USAGE)),
    }
}

/// `claude --version` 첫 줄. 바이너리가 없거나 못 읽으면 `None`이다.
fn probe_claude_version() -> Option<String> {
    let mut child = std::process::Command::new("claude")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("statusline-probe".into())
        .spawn(move || {
            let mut text = String::new();
            stdout.take(4096).read_to_string(&mut text).ok();
            sender.send(text).ok();
        })
        .ok()?;
    let text = receiver.recv_timeout(PROBE_WAIT).unwrap_or_default();
    child.kill().ok();
    child.wait().ok();
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn check_version(probe: &dyn Fn() -> Option<String>) -> io::Result<()> {
    let version = probe();
    if statusline::supports_status_line(version.as_deref()) {
        if version.is_none() {
            eprintln!("krill statusline: Claude 버전을 확인하지 못했습니다; 설치를 계속합니다");
        }
        return Ok(());
    }
    Err(io::Error::other(format!(
        "이 Claude 버전에서는 statusLine을 지원하지 않습니다: {}",
        version.unwrap_or_default()
    )))
}

fn install(
    paths: &InstallPaths,
    exe: &str,
    yes: bool,
    dry_run: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    install_with(paths, exe, &probe_claude_version, yes, dry_run, out)
}

fn install_with(
    paths: &InstallPaths,
    exe: &str,
    probe: &dyn Fn() -> Option<String>,
    yes: bool,
    dry_run: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    check_version(probe)?;
    let previous = store::read(&paths.receipt)?;
    if let Some(text) = &previous {
        let receipt: Receipt = serde_json::from_str(text)
            .map_err(|e| io::Error::other(format!("installation receipt is broken: {e}")))?;
        if !receipt.complete {
            return Err(io::Error::other(
                "interrupted statusline installation; uninstall to recover first",
            ));
        }
        writeln!(
            out,
            "statusline already recorded; uninstall first to change or recover the installation"
        )
        .ok();
        return Ok(());
    }
    let before = store::read(&paths.settings)?;
    let (after, state) = statusline::apply_install(before.as_deref(), exe)?;
    if state == SlotState::Managed {
        writeln!(
            out,
            "Krill statusLine이 있으나 receipt가 없습니다; 수동으로 제거한 뒤 다시 실행하십시오"
        )
        .ok();
        return Ok(());
    }
    write_diff(out, &paths.settings, before.as_deref(), Some(&after))?;
    if state == SlotState::User {
        writeln!(
            out,
            "기존 statusLine을 체인으로 보존합니다 (대체 정책: 체인)"
        )
        .ok();
    }
    if dry_run {
        writeln!(out, "preview only; no files written").ok();
        return Ok(());
    }
    if !yes && !consent()? {
        writeln!(out, "cancelled; no files written").ok();
        return Ok(());
    }
    apply_receipt(
        &paths.receipt,
        &paths.lock_dir,
        previous.as_deref(),
        Receipt {
            agent: RECEIPT_AGENT.to_owned(),
            complete: false,
            changes: vec![Change {
                path: paths.settings.clone(),
                before: before.clone(),
                after: after.clone(),
            }],
            codex: None,
        },
        &paths.settings,
        before.as_deref(),
        Some(&after),
    )?;
    writeln!(
        out,
        "statusline install complete; pre-write backups use .krill-bak-* suffixes"
    )
    .ok();
    Ok(())
}

fn uninstall(
    paths: &InstallPaths,
    exe: &str,
    yes: bool,
    dry_run: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    uninstall_with(paths, exe, yes, dry_run, out)
}

fn uninstall_with(
    paths: &InstallPaths,
    exe: &str,
    yes: bool,
    dry_run: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    let previous = store::read(&paths.receipt)?;
    let Some(text) = &previous else {
        writeln!(out, "no Krill statusline recorded").ok();
        return Ok(());
    };
    let receipt: Receipt = serde_json::from_str(text)
        .map_err(|e| io::Error::other(format!("installation receipt is broken: {e}")))?;
    if receipt.agent != RECEIPT_AGENT || receipt.changes.len() != 1 {
        return Err(io::Error::other("receipt agent mismatch"));
    }
    let change = receipt
        .changes
        .first()
        .ok_or_else(|| io::Error::other("receipt is empty"))?;
    let current = store::read(&paths.settings)?;
    let restored = statusline::apply_uninstall(
        current.as_deref(),
        change.before.as_deref(),
        &change.after,
        exe,
    )?;
    write_diff(
        out,
        &paths.settings,
        current.as_deref(),
        restored.as_deref(),
    )?;
    if dry_run {
        writeln!(out, "preview only; no files written").ok();
        return Ok(());
    }
    if restored.as_deref() != current.as_deref() && !yes && !consent()? {
        writeln!(out, "cancelled; no files written").ok();
        return Ok(());
    }
    let _lock = store::lock(&paths.lock_dir)?;
    if store::read(&paths.receipt)? != previous {
        return Err(io::Error::other(
            "statusline receipt changed; rerun to review",
        ));
    }
    if store::read(&paths.settings)? != current {
        return Err(io::Error::other("configuration changed; rerun to review"));
    }
    store::replace(&paths.settings, current.as_deref(), restored.as_deref())?;
    store::replace(&paths.receipt, previous.as_deref(), None)?;
    writeln!(out, "statusline uninstall complete").ok();
    Ok(())
}

fn apply_receipt(
    receipt_path: &Path,
    lock_dir: &Path,
    previous: Option<&str>,
    mut receipt: Receipt,
    settings: &Path,
    before: Option<&str>,
    after: Option<&str>,
) -> io::Result<()> {
    let _lock = store::lock(lock_dir)?;
    if store::read(receipt_path)?.as_deref() != previous {
        return Err(io::Error::other(
            "statusline receipt changed; rerun to review",
        ));
    }
    if store::read(settings)?.as_deref() != before {
        return Err(io::Error::other("configuration changed; rerun to review"));
    }
    let text = serde_json::to_string_pretty(&receipt).map(|s| s + "\n")?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > store::LIMIT {
        return Err(io::Error::other(
            "installation receipt exceeds 4 MiB; configuration was not changed",
        ));
    }
    store::replace(receipt_path, previous, Some(&text))?;
    store::replace(settings, before, after)?;
    receipt.complete = true;
    let completed = serde_json::to_string_pretty(&receipt).map(|s| s + "\n")?;
    store::replace(receipt_path, Some(&text), Some(&completed))?;
    Ok(())
}

fn write_diff(
    out: &mut impl Write,
    path: &Path,
    before: Option<&str>,
    after: Option<&str>,
) -> io::Result<()> {
    if before == after {
        return Ok(());
    }
    writeln!(out, "--- {}\n+++ {}", path.display(), path.display())?;
    for line in before.unwrap_or_default().lines() {
        writeln!(out, "-{line}")?;
    }
    for line in after.unwrap_or_default().lines() {
        writeln!(out, "+{line}")?;
    }
    Ok(())
}

fn consent() -> io::Result<bool> {
    print!("{CONSENT}");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests;
