//! Claude 공유 항목 정책. 실제 파일 이행은 프로바이더 공통 저장소가 담당한다.
use std::io;
use std::path::Path;

use super::{
    HISTORY, Report,
    history_store::{ForeignLinks, Store},
};
const PROMPTS: &str = "history.jsonl";
pub(super) const BACKUP_NAME: &str = "projects-merged-backup";

fn store<'a>(host: &'a Path, profile: &'a Path) -> Store<'a> {
    Store {
        host,
        profile,
        directories: HISTORY,
        files: &[PROMPTS],
        backup_name: BACKUP_NAME,
        foreign_links: ForeignLinks::Preserve,
        live: true,
    }
}

pub(super) fn needs_merge(host: &Path, profile: &Path) -> bool {
    store(host, profile).needs_merge()
}

pub(super) fn share(host: &Path, profile: &Path, report: &mut Report) -> io::Result<()> {
    store(host, profile).share(&|| true, report)
}

#[cfg(test)]
fn archive_root(host: &Path, profile: &Path) -> std::path::PathBuf {
    super::history_store::archive_root(host, profile, BACKUP_NAME)
}

#[cfg(test)]
#[path = "claude_history_tests.rs"]
mod tests;
