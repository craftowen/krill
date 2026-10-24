//! Codex는 세션 `-c` hook trust를 `CODEX_HOME`(`config.toml`의 `hooks.state`)마다 따로 둔다.
//! 완료된 receipt가 사용자 동의다. 실행이 실제로 쓰는 계정 홈의 Codex에게 receipt 명령의 해시를 받아
//! 버전 검사 RPC로 기록한다. 해시를 복사하거나 trust를 우회하지 않는다.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::usage::ProviderKind;

type Stamp = Option<(SystemTime, u64)>;

/// 홈별 마지막 확인. 성공은 (명령, 확인 직후 `config.toml` 도장)이 같을 동안 유지한다.
/// 실패는 [`RETRY`] 동안만, 도장이 그대로일 때만 기억한다. 한 번의 실패가 굳지 않는다.
// ponytail: Codex 업그레이드로 해시가 바뀌면 config가 바뀌거나 krilld가 재시작될 때까지 다시 묻지 않는다. 필요하면 codex 실행 파일 도장을 키에 더한다.
enum Checked {
    Checking,
    Trusted(String, Stamp),
    Failed(Instant, Stamp),
}

/// 없거나 멈춘 Codex가 실행마다 기다리게 하지 않을 만큼만 쉰다.
const RETRY: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(30)
};

static CHECKED: Mutex<BTreeMap<PathBuf, Checked>> = Mutex::new(BTreeMap::new());

/// Codex가 시작 시 trust를 읽으므로 갱신 완료 전에 실행하지 않는다. 실패하면 원래 세션을 보존한다.
pub(super) fn ensure_trust(
    program: Option<&str>,
    receipt_config: Option<&str>,
    command: &str,
    env: &[(Vec<u8>, Vec<u8>)],
) -> io::Result<()> {
    // 테스트는 실제 codex를 띄우지 않는다. `ensure_trust_in`을 가짜 codex로 직접 검사한다.
    if cfg!(test)
        || std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0")
    {
        return Ok(());
    }
    let Some(home) = launch_home(env) else {
        return Ok(());
    };
    let host = crate::usage::accounts::host_default_dir(
        &crate::usage::accounts::default_home(),
        ProviderKind::Codex,
    );
    let program = program.map(PathBuf::from);
    let receipt_config = receipt_config.map(PathBuf::from);
    let command = command.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("codex-hook-trust".into())
        .spawn(move || {
            let search = search_path(program.as_deref());
            let result = ensure_trust_in(
                &home,
                &host,
                receipt_config.as_deref(),
                &command,
                Some(&search),
            );
            tx.send(result).ok();
        })?;
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => result.map(|_| ()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(io::Error::from_raw_os_error(libc::EBUSY))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("codex hook trust worker ended"))
        }
    }
}

/// 실행될 Codex의 `CODEX_HOME`. PREFLIGHT·Windows 런처처럼 계정이 Codex를 맡으면 그 프로필 폴더
/// (비면 호스트 기본)이고, 아니면 프로필 env 값이다. `None`은 호스트 기본이라 설치기가 맡는다.
pub(super) fn launch_home(env: &[(Vec<u8>, Vec<u8>)]) -> Option<PathBuf> {
    let value = |key: &str| {
        env.iter()
            .rev()
            .find(|(k, _)| k.as_slice() == key.as_bytes())
            .map(|(_, v)| v.as_slice())
    };
    let dir = if value("KRILL_ACCOUNT_PROVIDER") == Some(ProviderKind::Codex.as_str().as_bytes()) {
        value("KRILL_ACCOUNT_DIR")
    } else {
        value(ProviderKind::Codex.env_var())
    };
    dir.filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(OsStr::from_bytes(d)))
}

/// receipt 홈·호스트 기본이 아닌 `home`에서 receipt `command`의 trust를 그 홈의 Codex에게 받는다. 썼으면 `true`.
/// 홈 경로는 정규화해 링크 거부 검사를 통과시키지만 `config.toml` 자체가 링크면 `hook_install`이 거부한다.
pub(super) fn ensure_trust_in(
    home: &Path,
    host: &Path,
    receipt_config: Option<&Path>,
    command: &str,
    search: Option<&OsStr>,
) -> io::Result<bool> {
    let home = home.canonicalize()?;
    let is_home = |dir: Option<&Path>| {
        dir.and_then(|d| d.canonicalize().ok())
            .is_some_and(|d| d == home)
    };
    if is_home(Some(host)) || is_home(receipt_config.and_then(Path::parent)) {
        return Ok(false);
    }
    let config = home.join("config.toml");
    let current = stamp(&config);
    {
        let mut checked = crate::session::lock(&CHECKED);
        match checked.get(&home) {
            Some(Checked::Trusted(command_at, stamp_at))
                if command_at == command && *stamp_at == current =>
            {
                return Ok(false);
            }
            Some(Checked::Failed(at, failed)) if *failed == current && at.elapsed() < RETRY => {
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
            Some(Checked::Checking) => return Err(io::Error::from_raw_os_error(libc::EBUSY)),
            _ => {}
        }
        checked.insert(home.clone(), Checked::Checking);
    }
    let result = krill_proto::hook_install::refresh_codex_trust(&home, command, search);
    let checked = match &result {
        Ok(_) => Checked::Trusted(command.to_owned(), stamp(&config)),
        Err(_) => Checked::Failed(Instant::now(), stamp(&config)),
    };
    crate::session::lock(&CHECKED).insert(home, checked);
    result
}

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// 로그인 셸 `PATH`. 프로필이 절대 경로 codex면 그 폴더를 앞에 둬 실제로 뜰 바이너리에게 묻는다.
fn search_path(program: Option<&Path>) -> OsString {
    let login = OsStr::from_bytes(crate::shell::login_path());
    let first = program
        .filter(|p| p.is_absolute())
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    std::env::join_paths(first.into_iter().chain(std::env::split_paths(login)))
        .unwrap_or_else(|_| login.to_owned())
}

type NoDaemonKey = (PathBuf, Stamp);
static NO_DAEMON_CACHE: Mutex<BTreeMap<NoDaemonKey, (Instant, bool)>> = Mutex::new(BTreeMap::new());
const NO_DAEMON_TTL: Duration = Duration::from_hours(1);

/// Codex 0.156+는 기본으로 `CODEX_HOME`마다 공용 백그라운드 `app-server --managed-daemon`을 띄워
/// 모든 탭의 훅을 첫 탭 환경으로 실행하고 rollout 파일도 데몬 프로세스가 열어 둔다.
/// 바이너리가 `--no-daemon`을 지원하면 이를 붙여 탭별 프로세스·훅·대화 귀속을 격리한다.
pub(super) fn no_daemon_supported(program: Option<&str>) -> bool {
    if cfg!(test)
        || std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0")
    {
        return false;
    }
    let search = search_path(program.map(Path::new));
    let bin = program.unwrap_or("codex");
    let Some(exe) = krill_proto::agent::which(bin, &search) else {
        return false;
    };
    no_daemon_supported_cached(&exe)
}

fn no_daemon_supported_cached(exe: &Path) -> bool {
    let key = (exe.to_path_buf(), stamp(exe));
    if let Some(supported) = crate::session::lock(&NO_DAEMON_CACHE)
        .get(&key)
        .filter(|(at, _)| at.elapsed() < NO_DAEMON_TTL)
        .map(|(_, supported)| *supported)
    {
        return supported;
    }
    let supported = no_daemon_supported_with(exe);
    let mut cache = crate::session::lock(&NO_DAEMON_CACHE);
    if cache.len() >= 64 {
        cache.pop_first();
    }
    cache.insert(key, (Instant::now(), supported));
    supported
}

pub(super) fn no_daemon_supported_with(exe: &Path) -> bool {
    let Ok(mut child) = Command::new(exe)
        .arg("--help")
        .env("PATH", OsStr::from_bytes(crate::shell::login_path()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let Some(pipe) = child.stdout.take() else {
        child.kill().ok();
        child.wait().ok();
        return false;
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut raw = Vec::new();
        pipe.take(256 * 1024).read_to_end(&mut raw).ok();
        tx.send(raw).ok();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                child.kill().ok();
                child.wait().ok();
                break false;
            }
        }
    };
    if !ok {
        return false;
    }
    let Ok(raw) = rx.recv_timeout(Duration::from_secs(1)) else {
        return false;
    };
    String::from_utf8_lossy(&raw).contains("--no-daemon")
}
