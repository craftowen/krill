#![forbid(unsafe_code)]
//! Bounded process logs (MK-069). Four fixed families share a decimal 20 MB budget.
//! No terminal bytes, command lines, paths or error Display strings belong at call sites.
//! Logging is fail-open: callers never depend on initialization succeeding.

mod subscriber;
mod writer;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

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

/// Explicit override isolates tests; production uses the Console.app-visible directory.
pub fn directory() -> Option<PathBuf> {
    directory_in(|key| std::env::var_os(key))
}

/// [`directory`]의 본체. 환경변수를 직접 만지지 않아 병렬 테스트에 안전하다.
fn directory_in(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = get("KRILL_LOG_DIR") {
        return Some(PathBuf::from(dir));
    }
    // Linux 원격 호스트: `~/.local/state/krill/logs`(XDG state). macOS 경로를 만들지 않는다.
    if cfg!(target_os = "linux") {
        return get("HOME").map(|h| PathBuf::from(h).join(".local/state/krill/logs"));
    }
    get("HOME").map(|h| {
        let logs = PathBuf::from(h).join("Library/Logs");
        let (new, old) = (logs.join("Krill"), logs.join("Minikrill"));
        // MK-170: 이관 전(새 디렉터리 없음 + 옛 디렉터리가 실제 디렉터리)에는 옛 이름에 계속 쓴다.
        if new.symlink_metadata().is_err() && old.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            old
        } else {
            new
        }
    })
}

/// Install INFO/WARN/ERROR events from Krill targets. No spans are retained.
/// File locks are nonblocking; contention and IO errors drop logs, never fail a session.
/// Files are opened per event so other processes cannot write into a renamed/unlinked archive.
/// This deliberately uses no thread in the fd-owning holder.
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
    fn directory_prefers_override_then_home_default() {
        let override_dir = |k: &str| Some(OsString::from(format!("/{k}")));
        assert_eq!(
            directory_in(override_dir),
            Some(PathBuf::from("/KRILL_LOG_DIR"))
        );
        let home = |k: &str| (k == "HOME").then(|| OsString::from("/h"));
        let want = if cfg!(target_os = "linux") {
            "/h/.local/state/krill/logs"
        } else {
            "/h/Library/Logs/Krill"
        };
        assert_eq!(directory_in(home), Some(PathBuf::from(want)));
        assert_eq!(directory_in(|_| None), None);
    }

    /// MK-170: 이관 전에는 옛 `Logs/Minikrill`, 옮겨진 뒤(옛 자리는 링크)에는 `Logs/Krill`.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: ~/Library/Logs 이관")]
    fn directory_follows_bundle_migration() {
        let home = std::env::temp_dir().join(format!("mk170-log-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let logs = home.join("Library/Logs");
        std::fs::create_dir_all(logs.join("Minikrill")).unwrap();
        let get = |k: &str| (k == "HOME").then(|| home.clone().into_os_string());
        assert_eq!(directory_in(get), Some(logs.join("Minikrill")));
        std::fs::rename(logs.join("Minikrill"), logs.join("Krill")).unwrap();
        std::os::unix::fs::symlink("Krill", logs.join("Minikrill")).unwrap();
        assert_eq!(directory_in(get), Some(logs.join("Krill")));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
