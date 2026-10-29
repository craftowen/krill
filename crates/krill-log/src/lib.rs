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

/// Explicit override isolates tests (test runs fall back to a temp directory, see [`is_test_run`]);
/// production uses `LocalAppData` on Windows and the Console.app-visible directory
/// on macOS (XDG state on Linux).
pub fn directory() -> Option<PathBuf> {
    directory_in(
        |key| std::env::var_os(key),
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
    home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = get("KRILL_LOG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if is_test_run(&get) {
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

/// 테스트·예제·검증이 띄운 프로세스와 그 자식(holder·krilld·앱·CLI)인가. 셋 다 자식에게 그대로 물려진다.
/// - `NEXTEST`: nextest가 늘 1로 둔다.
/// - `CARGO_MANIFEST_DIR`: `cargo test`·`cargo run --example`이 런타임에 둔다(설치본은 cargo 밖에서 뜬다).
/// - 임시 폴더의 `KRILL_STATE_DIR`: 테스트 바이너리를 직접 돌리는 릴리스 검증·통합 테스트는 이것만 준다.
///   원격 호스트·사용자가 고른 상태 폴더는 임시 폴더 밖이라 그대로 운영 로그에 쓴다.
fn is_test_run(get: &impl Fn(&str) -> Option<OsString>) -> bool {
    get("NEXTEST").is_some()
        || get("CARGO_MANIFEST_DIR").is_some()
        || get("KRILL_STATE_DIR").is_some_and(|dir| is_temp(Path::new(&dir)))
}

fn is_temp(path: &Path) -> bool {
    path.starts_with(std::env::temp_dir())
        || [
            "/tmp",
            "/private/tmp",
            "/var/folders",
            "/private/var/folders",
        ]
        .iter()
        .any(|root| path.starts_with(root))
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
        assert_eq!(
            directory_in(override_dir, || None),
            Some(PathBuf::from("/KRILL_LOG_DIR"))
        );
        assert_eq!(directory_in(|_| None, || None), None);
    }

    #[test]
    fn nextest_runs_never_use_the_production_directory() {
        let nextest = |k: &str| (k == "NEXTEST").then(|| OsString::from("1"));
        let home = PathBuf::from("/h");
        assert_eq!(
            directory_in(nextest, || Some(home.clone())),
            Some(std::env::temp_dir().join("krill-test-logs"))
        );
        let both = |k: &str| match k {
            "NEXTEST" => Some(OsString::from("1")),
            "KRILL_LOG_DIR" => Some(OsString::from("/x")),
            _ => None,
        };
        assert_eq!(directory_in(both, || None), Some(PathBuf::from("/x")));
        // 실제 테스트 프로세스(nextest든 `cargo test`든)가 운영 경로를 고르지 않는다.
        if std::env::var_os("KRILL_LOG_DIR").is_none() {
            assert_eq!(
                directory(),
                Some(std::env::temp_dir().join("krill-test-logs"))
            );
        }
    }

    /// 2026-10-28 사건: `cargo test -p krilld --test upgrade`(nextest 아님)가 띄운 holder·krilld가 운영 로그에 썼다.
    #[test]
    fn cargo_runs_and_temp_state_dirs_never_use_the_production_directory() {
        let temp = std::env::temp_dir().join("krill-test-logs");
        let home = || Some(PathBuf::from("/h"));
        let cargo = |k: &str| (k == "CARGO_MANIFEST_DIR").then(|| OsString::from("/src/krilld"));
        assert_eq!(directory_in(cargo, home), Some(temp.clone()));
        let mk062 = std::env::temp_dir().join("mk062-1");
        for state in [
            mk062.as_path(),
            Path::new("/tmp/krill-qa"),
            Path::new("/private/var/folders/x/T/s"),
        ] {
            let get = |k: &str| (k == "KRILL_STATE_DIR").then(|| state.as_os_str().to_owned());
            assert_eq!(directory_in(get, home), Some(temp.clone()), "{state:?}");
        }
        // 사용자가 고른 상태 폴더(원격 호스트 포함)는 운영 로그를 그대로 쓴다.
        let user =
            |k: &str| (k == "KRILL_STATE_DIR").then(|| OsString::from("/Users/u/krill-state"));
        assert_ne!(directory_in(user, home), Some(temp));
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
            directory_in(|_| None, || Some(PathBuf::from("/h"))),
            Some(PathBuf::from(want))
        );
    }

    #[cfg(windows)]
    #[test]
    fn directory_prefers_local_app_data_then_home() {
        let local = |k: &str| (k == "LOCALAPPDATA").then(|| OsString::from(r"C:\local"));
        assert_eq!(
            directory_in(local, || Some(PathBuf::from(r"C:\home"))),
            Some(PathBuf::from(r"C:\local\Krill\Logs"))
        );
        assert_eq!(
            directory_in(|_| None, || Some(PathBuf::from(r"C:\home"))),
            Some(PathBuf::from(r"C:\home\Krill\Logs"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn directory_is_library_logs_krill() {
        let home = PathBuf::from("/Users/u");
        assert_eq!(
            directory_in(|_| None, || Some(home.clone())),
            Some(home.join("Library/Logs/Krill"))
        );
    }
}
