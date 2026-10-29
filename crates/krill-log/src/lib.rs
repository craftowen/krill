#![forbid(unsafe_code)]
//! Bounded process logs (MK-069). Four fixed families share a decimal 20 MB budget.
//! No terminal bytes, command lines, paths or error Display strings belong at call sites.
//! Logging is fail-open: callers never depend on initialization succeeding.

mod forward;
mod subscriber;
mod writer;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

pub use writer::{FILE_BYTES, RollingLog};

/// Total payload bytes across all four families, including their active files.
pub const TOTAL_BYTES: u64 = 20_000_000;
/// Active file plus four archives per component.
pub const FILES: usize = 5;
/// Each event is bounded before formatting can allocate an arbitrarily large string.
pub const RECORD_BYTES: usize = 8_192;

#[derive(Debug, Clone, Copy)]
pub enum Component {
    App,
    Daemon,
    Holder,
    Cli,
}

impl Component {
    pub const ALL: [Self; 4] = [Self::App, Self::Daemon, Self::Holder, Self::Cli];

    pub const fn name(self) -> &'static str {
        match self {
            Self::App => "krill-app",
            Self::Daemon => "krilld",
            Self::Holder => "krill-holder",
            Self::Cli => "krill",
        }
    }
}

/// Explicit override isolates tests (test processes fall back to a temp directory, see
/// [`is_test_process`]); production uses `LocalAppData` on Windows and the Console.app-visible
/// directory on macOS (XDG state on Linux).
pub fn directory() -> Option<PathBuf> {
    directory_in(
        |key| std::env::var_os(key),
        is_test_process(),
        || {
            #[cfg(windows)]
            {
                std::env::home_dir()
            }
            #[cfg(not(windows))]
            {
                std::env::var_os("HOME").map(PathBuf::from)
            }
        },
    )
}

/// [`directory`]의 본체. 환경변수를 직접 만지지 않아 병렬 테스트에 안전하다.
fn directory_in(
    get: impl Fn(&str) -> Option<OsString>,
    test_process: bool,
    home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = get("KRILL_LOG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if test_process {
        return Some(std::env::temp_dir().join("krill-test-logs"));
    }
    #[cfg(windows)]
    let base = get("LOCALAPPDATA").map(PathBuf::from).or_else(home);
    #[cfg(not(windows))]
    let base = home();
    #[cfg(windows)]
    {
        base.map(|path| path.join("Krill").join("Logs"))
    }
    // Linux 원격 호스트: `~/.local/state/krill/logs`(XDG state). macOS 경로를 만들지 않는다.
    #[cfg(target_os = "linux")]
    {
        base.map(|h| h.join(".local/state/krill/logs"))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    base.map(|h| h.join("Library/Logs/Krill"))
}

/// 테스트 하네스가 띄운 프로세스(와 그 자식)인가. 테스트에만 있는 표식만 본다 — 일반 `cargo run`·설치본·
/// 사용자가 고른 `KRILL_STATE_DIR`은 해당하지 않는다.
/// - `NEXTEST`: nextest가 늘 1로 둔다.
/// - `CARGO_BIN_EXE_<이름>`: `cargo test`가 통합 테스트 프로세스에 런타임으로도 둔다(`cargo run`에는 없다).
/// - 실행 파일이 `target/<프로필>/deps/<이름>-<해시>`: libtest 바이너리 자신(직접 실행 포함).
///
/// 앞의 둘은 환경이라 holder → krilld 재기동·exec 핸드오프까지 그대로 이어진다. 테스트 바이너리를 직접 실행하는
/// 게이트(`packaging/release_check.py`)의 자식은 테스트가 `KRILL_LOG_DIR`을 넘긴다.
/// `krill_proto::error_reporting`에 같은 판정이 있다(krill-proto는 이 크레이트에 의존하지 않는다).
#[must_use]
pub fn is_test_process() -> bool {
    is_test_process_in(
        std::env::vars_os().map(|(key, _)| key),
        std::env::current_exe().ok().as_deref(),
    )
}

fn is_test_process_in(keys: impl IntoIterator<Item = OsString>, exe: Option<&Path>) -> bool {
    keys.into_iter().any(|key| {
        key == "NEXTEST"
            || key
                .to_str()
                .is_some_and(|key| key.starts_with("CARGO_BIN_EXE_"))
    }) || exe
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "deps")
}

/// Install INFO/WARN/ERROR events from Krill targets. No spans are retained.
/// File locks are nonblocking; contention and IO errors drop logs, never fail a session.
/// Files are opened per event so other processes cannot write into a renamed/unlinked archive.
/// This deliberately uses no thread in the fd-owning holder.
pub use forward::forward_log_warnings;

pub fn init(component: Component) -> io::Result<()> {
    let dir =
        directory().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no log directory"))?;
    let writer = RollingLog::open(dir, component)?;
    tracing::subscriber::set_global_default(subscriber::Logger::new(writer))
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_prefers_override() {
        let override_dir = |k: &str| Some(OsString::from(format!("/{k}")));
        for test in [false, true] {
            assert_eq!(
                directory_in(override_dir, test, || None),
                Some(PathBuf::from("/KRILL_LOG_DIR"))
            );
        }
        assert_eq!(directory_in(|_| None, false, || None), None);
    }

    #[test]
    fn test_processes_never_use_the_production_directory() {
        let temp = std::env::temp_dir().join("krill-test-logs");
        assert_eq!(
            directory_in(|_| None, true, || Some(PathBuf::from("/h"))),
            Some(temp.clone())
        );
        // 실제 테스트 프로세스(nextest든 `cargo test`든)가 운영 경로를 고르지 않는다.
        if std::env::var_os("KRILL_LOG_DIR").is_none() {
            assert_eq!(directory(), Some(temp));
        }
    }

    /// 2026-10-28 사건: `cargo test -p krilld --test upgrade`(nextest 아님)가 띄운 holder·krilld가 운영 로그에 썼다.
    /// 일반 `cargo run`(`CARGO_MANIFEST_DIR`만 있음)과 임시 폴더의 사용자 상태 폴더는 테스트가 아니다.
    #[test]
    fn only_test_harness_markers_mark_a_test_process() {
        let keys = |names: &[&str]| names.iter().map(OsString::from).collect::<Vec<_>>();
        let dev = Path::new("/w/target/debug/krill-holder");
        assert!(is_test_process_in(keys(&["NEXTEST"]), Some(dev)));
        assert!(is_test_process_in(
            keys(&["CARGO_MANIFEST_DIR", "CARGO_BIN_EXE_krill-holder"]),
            Some(dev)
        ));
        let libtest = Path::new("/w/target/release/deps/survive-0123abcd");
        assert!(is_test_process_in(keys(&[]), Some(libtest)));
        assert!(!is_test_process_in(
            keys(&[
                "CARGO",
                "CARGO_MANIFEST_DIR",
                "CARGO_PKG_NAME",
                "KRILL_STATE_DIR"
            ]),
            Some(dev)
        ));
        let installed = Path::new("/Applications/Krill.app/Contents/Helpers/krilld");
        assert!(!is_test_process_in(keys(&["HOME"]), Some(installed)));
        assert!(!is_test_process_in(keys(&[]), None));
    }

    #[cfg(not(windows))]
    #[test]
    fn directory_uses_home_default() {
        let want = if cfg!(target_os = "linux") {
            "/h/.local/state/krill/logs"
        } else {
            "/h/Library/Logs/Krill"
        };
        assert_eq!(
            directory_in(|_| None, false, || Some(PathBuf::from("/h"))),
            Some(PathBuf::from(want))
        );
    }

    #[cfg(windows)]
    #[test]
    fn directory_prefers_local_app_data_then_home() {
        let local = |k: &str| (k == "LOCALAPPDATA").then(|| OsString::from(r"C:\local"));
        assert_eq!(
            directory_in(local, false, || Some(PathBuf::from(r"C:\home"))),
            Some(PathBuf::from(r"C:\local\Krill\Logs"))
        );
        assert_eq!(
            directory_in(|_| None, false, || Some(PathBuf::from(r"C:\home"))),
            Some(PathBuf::from(r"C:\home\Krill\Logs"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn directory_is_library_logs_krill() {
        let home = PathBuf::from("/Users/u");
        assert_eq!(
            directory_in(|_| None, false, || Some(home.clone())),
            Some(home.join("Library/Logs/Krill"))
        );
    }
}
