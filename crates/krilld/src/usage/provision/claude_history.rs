//! Claude 공유 항목 정책. 실제 파일 이행은 프로바이더 공통 저장소가 담당한다.
use std::io;
use std::path::Path;

use super::{LINKED, Report, history_store::Store};
const HISTORY: &str = "history.jsonl";

fn store<'a>(host: &'a Path, profile: &'a Path) -> Store<'a> {
    Store {
        host,
        profile,
        directories: LINKED,
        files: &[HISTORY],
        backup_name: "projects-merged-backup",
    }
}

pub(super) fn quiescent(profile: &Path) -> bool {
    super::history_probe::quiescent(profile, "CLAUDE_CONFIG_DIR")
}

pub(super) fn needs_merge(host: &Path, profile: &Path) -> bool {
    store(host, profile).needs_merge()
}

pub(super) fn share(
    host: &Path,
    profile: &Path,
    idle: &dyn Fn() -> bool,
    report: &mut Report,
) -> io::Result<()> {
    store(host, profile).share(idle, report)
}

#[cfg(test)]
fn archive_root(host: &Path, profile: &Path) -> std::path::PathBuf {
    super::history_store::archive_root(host, profile, "projects-merged-backup")
}

#[cfg(test)]
#[path = "claude_history_tests.rs"]
mod tests;
