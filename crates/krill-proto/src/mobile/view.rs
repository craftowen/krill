//! 모바일에서 바꾼 작업 공간 옵션(Orca "Synced across your devices").
//! `sidebar.bin`은 앱만 쓴다. krilld는 바뀐 항목만 이 파일에 쌓고, 앱이 읽어 사이드바에 반영한 뒤 지운다.
//! 앱이 꺼져 있어도 krilld는 `sidebar.bin` 위에 이 파일을 덧씌워 읽으므로 폰에는 바로 보인다.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

pub const FILE: &str = "sidebar-mobile.json";
const MAX_BYTES: u64 = 256 * 1024;

/// 바뀐 항목만 담는다. `group_by`·`sort_by`는 데스크톱 `GroupBy`·`SortBy` 변형 인덱스다
/// (0 프로젝트·1 상태·2 PR·3 없음 / 0 스마트·1 최근·2 이름·3 생성순).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewChange {
    pub group_by: Option<u32>,
    pub sort_by: Option<u32>,
    pub hide_sleeping: Option<bool>,
    pub always_show_default_branch: Option<bool>,
    pub hide_automation: Option<bool>,
    pub hide_cli: Option<bool>,
    /// 보이는 프로젝트(비면 전체).
    pub repos: Option<BTreeSet<PathBuf>>,
    /// 프로젝트 접힘(true 접힘).
    pub collapsed: BTreeMap<PathBuf, bool>,
}

impl ViewChange {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 더 나중 변경을 덮어쓴다.
    pub fn merge(&mut self, newer: Self) {
        let Self {
            group_by,
            sort_by,
            hide_sleeping,
            always_show_default_branch,
            hide_automation,
            hide_cli,
            repos,
            collapsed,
        } = newer;
        self.group_by = group_by.or(self.group_by);
        self.sort_by = sort_by.or(self.sort_by);
        self.hide_sleeping = hide_sleeping.or(self.hide_sleeping);
        self.always_show_default_branch =
            always_show_default_branch.or(self.always_show_default_branch);
        self.hide_automation = hide_automation.or(self.hide_automation);
        self.hide_cli = hide_cli.or(self.hide_cli);
        if repos.is_some() {
            self.repos = repos;
        }
        self.collapsed.extend(collapsed);
    }

    /// 대기 중인 변경과 원본 바이트. 없거나 깨졌으면 `None`(깨진 파일은 다음 쓰기가 덮는다).
    #[must_use]
    pub fn load(dir: &Path) -> Option<(Vec<u8>, Self)> {
        let path = dir.join(FILE);
        if fs::metadata(&path).ok()?.len() > MAX_BYTES {
            return None;
        }
        let bytes = fs::read(&path).ok()?;
        let change = serde_json::from_slice(&bytes).ok()?;
        Some((bytes, change))
    }

    /// 대기 중인 변경에 합쳐 원자적으로 저장한다(0600).
    pub fn append(dir: &Path, newer: Self) -> io::Result<()> {
        let mut pending = Self::load(dir).map(|(_, c)| c).unwrap_or_default();
        pending.merge(newer);
        let bytes = serde_json::to_vec(&pending).map_err(io::Error::other)?;
        let target = dir.join(FILE);
        let tmp = dir.join(format!(".{FILE}.tmp.{}", std::process::id()));
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&tmp)?;
        if let Err(e) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            fs::remove_file(&tmp).ok();
            return Err(e);
        }
        drop(file);
        fs::rename(&tmp, &target).inspect_err(|_| {
            fs::remove_file(&tmp).ok();
        })
    }

    /// 앱이 반영한 뒤 부른다. 그 사이 폰이 더 쓴 변경은 지우지 않는다.
    pub fn consume(dir: &Path, applied: &[u8]) -> io::Result<()> {
        let path = dir.join(FILE);
        match fs::read(&path) {
            Ok(now) if now == applied => fs::remove_file(path),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_accumulate_until_the_app_consumes_them() {
        let dir = std::env::temp_dir().join(format!("krill-view-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        ViewChange::append(
            &dir,
            ViewChange {
                hide_sleeping: Some(true),
                collapsed: BTreeMap::from([(PathBuf::from("/a"), true)]),
                ..ViewChange::default()
            },
        )
        .unwrap();
        ViewChange::append(
            &dir,
            ViewChange {
                sort_by: Some(2),
                repos: Some(BTreeSet::from([PathBuf::from("/a")])),
                collapsed: BTreeMap::from([(PathBuf::from("/b"), false)]),
                ..ViewChange::default()
            },
        )
        .unwrap();
        let (bytes, pending) = ViewChange::load(&dir).unwrap();
        assert_eq!(pending.hide_sleeping, Some(true));
        assert_eq!(pending.sort_by, Some(2));
        assert_eq!(pending.repos.unwrap().len(), 1);
        assert_eq!(pending.collapsed.len(), 2);
        // 앱이 읽은 뒤 폰이 또 바꾸면 그 변경은 남는다.
        ViewChange::append(
            &dir,
            ViewChange {
                group_by: Some(3),
                ..ViewChange::default()
            },
        )
        .unwrap();
        ViewChange::consume(&dir, &bytes).unwrap();
        let (latest, pending) = ViewChange::load(&dir).unwrap();
        assert_eq!(pending.group_by, Some(3));
        ViewChange::consume(&dir, &latest).unwrap();
        assert!(ViewChange::load(&dir).is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
