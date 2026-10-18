//! 번들 id 이관(MK-170): `dev.minikrill.app` → `dev.krill.app`.
//!
//! - 새 상태 디렉터리가 없고 옛 디렉터리가 실제 디렉터리면 [`super::state_dir`]는 옛 경로를 준다. 그래서 옛 holder가
//!   살아 있는 동안 새 앱·CLI는 옛 소켓에 그대로 붙고 새 holder를 띄우지 않는다(AGENTS 규칙 1).
//! - [`run`]는 앱·CLI 시작 때 돈다. 옛 소켓에 아무도 없을 때만(연결이 없음·거부, krilld 소켓 락이 비어 있음,
//!   기동 락을 바로 잡음) 항목마다 `symlink(새 이름, 옛.krill-migrate)` → `rename(옛, 새)` →
//!   `rename(옛.krill-migrate, 옛)`으로 옮긴다. 단계마다 syscall 하나라 어디서 kill -9 돼도 다음 실행이 이어서
//!   끝내거나(2단계 뒤) 옛 경로로 남는다(1단계 뒤, 남은 링크는 지운다).
//! - 상태 디렉터리가 옮겨진 뒤에만 Caches·Logs·Preferences·Saved Application State를 같은 방식으로 옮기고,
//!   옛 launchd 라벨(holder·link)을 해제한다. link `LaunchAgent`는 새 라벨 plist로 다시 설치한다.
//! - 에이전트 hook은 `krill` 실행 파일 경로만 박고 상태 디렉터리 경로는 박지 않는다. receipt(`hooks/*.json`)는
//!   상태 디렉터리와 함께 옮겨지므로 따로 고칠 것이 없다.
//!
//! 여러 프로세스가 동시에 이관하지 않도록 옮겨지지 않는 자리(`Application Support/.dev.krill.app.migrate.lock`)의
//! flock 아래에서만 옮긴다.
// ponytail: 두 rename 사이(µs)에 이관을 거치지 않는 기동자(구버전 앱 등)가 옛 경로에 holder를 띄우면 옛 자리에 실제
// 디렉터리가 다시 생겨 마지막 rename이 실패할 수 있다(세션은 살지만 새 경로 클라이언트가 못 본다). 막으려면
// `renamex_np(RENAME_SWAP)`(FFI, krill-proto 밖)로 한 번에 바꾼다.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::{BUNDLE_ID, LEGACY_BUNDLE_ID, LEGACY_HOLDER_LABEL, is_absent, launch_lock};
use crate::os::net::UnixStream;

/// 1단계(옛 자리 옆 링크) 이름 꼬리.
const LINK_SUFFIX: &str = ".krill-migrate";
/// 다른 이관자를 기다리는 최대 시간. 넘으면 이번 실행은 옮기지 않는다.
const LOCK_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(10);
/// [`run`]가 launchctl을 부르지 않게 하는 환경변수(테스트 격리: launchd 도메인은 `HOME`과 무관하다).
pub const SKIP_LAUNCHCTL_ENV: &str = "KRILL_MIGRATE_SKIP_LAUNCHCTL";

/// `HOME` 기준 (옛, 새) 경로. 첫 항목(상태 디렉터리)이 이관 여부를 정한다.
fn items(home: &Path) -> [(PathBuf, PathBuf); 5] {
    let lib = home.join("Library");
    let pair =
        |dir: &str, old: String, new: String| (lib.join(dir).join(old), lib.join(dir).join(new));
    [
        pair(
            "Application Support",
            LEGACY_BUNDLE_ID.to_owned(),
            BUNDLE_ID.to_owned(),
        ),
        pair("Caches", LEGACY_BUNDLE_ID.to_owned(), BUNDLE_ID.to_owned()),
        pair("Logs", "Minikrill".to_owned(), "Krill".to_owned()),
        pair(
            "Preferences",
            format!("{LEGACY_BUNDLE_ID}.plist"),
            format!("{BUNDLE_ID}.plist"),
        ),
        pair(
            "Saved Application State",
            format!("{LEGACY_BUNDLE_ID}.savedState"),
            format!("{BUNDLE_ID}.savedState"),
        ),
    ]
}

/// `HOME` 기준 상태 디렉터리. 이관 전이면 옛 경로, 아니면 새 경로.
#[cfg(not(any(windows, target_os = "linux")))]
pub fn state_dir_in(home: &Path) -> PathBuf {
    let [(old, new), ..] = items(home);
    if pending(&old, &new) { old } else { new }
}

/// Windows: `local_app_data`(`%LOCALAPPDATA%`) 아래 `Krill`. 개명 전 Windows 판이 없어 이관할 옛 경로도 없다.
#[cfg(windows)]
pub fn state_dir_in(local_app_data: &Path) -> PathBuf {
    local_app_data.join("Krill")
}

/// Linux(원격 호스트): `~/.local/share/<번들 id>`. 개명 전 Linux 판이 없어 이관할 옛 경로도 없다.
#[cfg(target_os = "linux")]
pub fn state_dir_in(home: &Path) -> PathBuf {
    home.join(LINUX_STATE_PARENT).join(BUNDLE_ID)
}

/// Linux 상태 디렉터리의 부모(`HOME` 기준). 원격 소켓 탐색(`krill-hosts`)도 쓴다.
pub const LINUX_STATE_PARENT: &str = ".local/share";

/// 새 경로가 없고 옛 경로가 실제(링크 아님) 파일·디렉터리다.
fn pending(old: &Path, new: &Path) -> bool {
    new.symlink_metadata().is_err() && old.symlink_metadata().is_ok_and(|m| !m.is_symlink())
}

fn is_link(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_symlink())
}

fn link_of(old: &Path) -> PathBuf {
    let mut name = OsString::from(old);
    name.push(LINK_SUFFIX);
    PathBuf::from(name)
}

/// [`run`] 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Windows, 상태 디렉터리 재정의(`KRILL_STATE_DIR`)나 `HOME` 없음으로 건너뜀.
    Skipped,
    /// 옛 holder·krilld가 살아 있거나 다른 기동자가 띄우는 중이라 옛 경로를 그대로 쓴다.
    Legacy,
    /// 이번 실행에서 무언가 옮겼다(중단된 이관 마무리 포함).
    Migrated,
    /// 할 일이 없다.
    Current,
}

/// launchctl을 부를지(테스트는 [`Launchctl::Skip`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launchctl {
    Run,
    Skip,
}

/// 한 항목 이관의 단계. 테스트가 각 단계 직후 강제 종료를 흉내 낸다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `옛.krill-migrate` 링크를 만들었다.
    Linked,
    /// 옛 → 새 rename을 마쳤다(커밋).
    Moved,
}

/// 앱·CLI 시작 때 한 번 부른다. 오류는 세션에 영향이 없다(옮기지 못한 항목은 옛 자리에 남고 다음 실행이 이어 한다).
///
/// # Errors
/// 잠금·rename·symlink·LaunchAgent plist 쓰기 실패.
pub fn run() -> io::Result<Outcome> {
    // 개명 이관은 macOS 번들 경로(`~/Library`·launchd)만 다룬다.
    if cfg!(windows) || super::state_dir_override().is_some() {
        return Ok(Outcome::Skipped);
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(Outcome::Skipped);
    };
    let launchctl = if std::env::var_os(SKIP_LAUNCHCTL_ENV).is_some() {
        Launchctl::Skip
    } else {
        Launchctl::Run
    };
    migrate_in(Path::new(&home), launchctl, &mut |_| Ok(()))
}

/// [`run`]의 본체. `step`이 오류를 내면 그 자리에서 멈춘다(정리 코드 없음 = kill -9와 같다).
///
/// # Errors
/// [`run`]와 같고, `step`의 오류를 그대로 돌려준다.
pub fn migrate_in(
    home: &Path,
    launchctl: Launchctl,
    step: &mut dyn FnMut(Step) -> io::Result<()>,
) -> io::Result<Outcome> {
    let items = items(home);
    let agents = home.join("Library/LaunchAgents");
    let legacy_link_plist = agents.join(format!("{LEGACY_BUNDLE_ID}.link.plist"));
    let work = |(old, new): &(PathBuf, PathBuf)| pending(old, new) || is_link(&link_of(old));
    if !items.iter().any(work) && legacy_link_plist.symlink_metadata().is_err() {
        return Ok(Outcome::Current);
    }
    let support = home.join("Library/Application Support");
    fs::create_dir_all(&support)?;
    let Some(_migrating) = wait_lock(&support.join(format!(".{BUNDLE_ID}.migrate.lock")))? else {
        return Ok(Outcome::Legacy);
    };
    let [(old, new), aux @ ..] = &items;
    // 옮기는 동안 기동 락·krilld 소켓 락을 쥔다.
    let _idle = if pending(old, new) {
        let Some(locks) = idle(&old.join("run/krilld.sock"))? else {
            return Ok(Outcome::Legacy);
        };
        if launchctl == Launchctl::Run {
            bootout(home, LEGACY_HOLDER_LABEL);
        }
        Some(locks)
    } else {
        None
    };
    let mut moved = move_item(old, new, step)?;
    for (old, new) in aux {
        moved |= move_item(old, new, step)?;
    }
    moved |= migrate_link_agent(home, &legacy_link_plist, launchctl)?;
    Ok(if moved {
        Outcome::Migrated
    } else {
        Outcome::Current
    })
}

/// 옛 소켓에 holder·krilld가 없으면 그동안 쥘 락들(기동 락, krilld 소켓 락). 있으면 `None`.
fn idle(socket: &Path) -> io::Result<Option<(File, Option<File>)>> {
    // 기동 락을 쥔 동안에는 이관을 거치는 기동자가 옛 경로에 holder를 띄우지 못한다.
    let Ok(launch) = launch_lock(socket, Duration::ZERO) else {
        return Ok(None);
    };
    // 붙거나(살아 있음) 판단할 수 없는 오류면 옮기지 않는다.
    if !UnixStream::connect(socket).is_err_and(|e| is_absent(&e)) {
        return Ok(None);
    }
    let mut lock = OsString::from(socket);
    lock.push(".lock");
    let krilld = match OpenOptions::new().read(true).write(true).open(&lock) {
        Ok(f) => match f.try_lock() {
            Ok(()) => Some(f),
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    Ok(Some((launch, krilld)))
}

/// 한 항목을 옮기거나 중단된 이관을 마무리한다. 무언가 바꿨으면 `true`.
fn move_item(
    old: &Path,
    new: &Path,
    step: &mut dyn FnMut(Step) -> io::Result<()>,
) -> io::Result<bool> {
    let link = link_of(old);
    if pending(old, new) {
        remove_link(&link)?; // 1단계 뒤에 끊긴 흔적
        let target = new.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        symlink(target, &link)?;
        step(Step::Linked)?;
        fs::rename(old, new)?;
        step(Step::Moved)?;
        fs::rename(&link, old)?;
        return Ok(true);
    }
    if is_link(&link) && old.symlink_metadata().is_err() && new.symlink_metadata().is_ok() {
        fs::rename(&link, old)?; // 2단계 뒤에 끊겼다
        return Ok(true);
    }
    remove_link(&link)?;
    Ok(false)
}

/// 링크일 때만 지운다(같은 이름의 실제 파일은 사용자 데이터일 수 있다).
fn remove_link(path: &Path) -> io::Result<()> {
    if is_link(path) {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// 옛 link `LaunchAgent` plist가 있으면 새 라벨로 다시 쓰고 옛 잡을 해제한 뒤 새 잡을 올린다.
/// 순서(새 plist 쓰기 → 옛 해제 → 새 등록 → 옛 plist 삭제)라 중간에 끊겨도 다음 실행이 처음부터 다시 한다.
fn migrate_link_agent(home: &Path, legacy: &Path, launchctl: Launchctl) -> io::Result<bool> {
    let text = match fs::read_to_string(legacy) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let dir = legacy.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let plist = dir.join(format!("{BUNDLE_ID}.link.plist"));
    // 라벨(`<id>.link`)·AssociatedBundleIdentifiers·옛 상태 디렉터리 안 실행 파일 경로를 함께 바꾼다.
    let tmp = dir.join(format!(".{BUNDLE_ID}.link.plist.tmp"));
    fs::write(&tmp, text.replace(LEGACY_BUNDLE_ID, BUNDLE_ID))?;
    fs::rename(&tmp, &plist)?;
    if launchctl == Launchctl::Run {
        bootout(home, &format!("{LEGACY_BUNDLE_ID}.link"));
        bootout(home, &format!("{BUNDLE_ID}.link"));
        if let Some(domain) = domain(home) {
            Command::new("/bin/launchctl")
                .args(["bootstrap", &domain])
                .arg(&plist)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
        }
    }
    fs::remove_file(legacy)?;
    Ok(true)
}

/// `gui/<uid>`. uid는 `HOME`의 소유자다(krill-proto는 libc를 쓰지 않는다).
#[cfg(target_os = "macos")]
fn domain(home: &Path) -> Option<String> {
    fs::metadata(home).ok().map(|m| format!("gui/{}", m.uid()))
}

/// launchd는 macOS에만 있다.
#[cfg(not(target_os = "macos"))]
fn domain(_home: &Path) -> Option<String> {
    None
}

/// [`run`]이 Windows에서 이관을 건너뛰므로 부르지 않는다.
#[cfg(windows)]
fn symlink(_target: impl AsRef<Path>, _link: impl AsRef<Path>) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// 등록돼 있지 않으면 launchctl이 실패하고 끝난다. 결과는 보지 않는다.
fn bootout(home: &Path, label: &str) {
    if let Some(domain) = domain(home) {
        Command::new("/bin/launchctl")
            .args(["bootout", &format!("{domain}/{label}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
    }
}

/// `path`의 flock을 [`LOCK_TIMEOUT`]까지 기다린다. 넘으면 `None`.
fn wait_lock(path: &Path) -> io::Result<Option<File>> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let deadline = Instant::now()
        .checked_add(LOCK_TIMEOUT)
        .ok_or(io::ErrorKind::TimedOut)?;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(Some(f)),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => thread::sleep(POLL),
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
}

// 개명 이관은 macOS 번들 경로(`~/Library`·launchd)만 다룬다. Linux 상태 경로는 이관 대상이 없다.
#[cfg(test)]
mod tests;
