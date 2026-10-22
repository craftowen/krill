//! MK-093: 워크트리 코멘트(`krill set --comment`) 조회와 외부 `.claude/worktrees` 감지·숨김 분류·표시.
//! 선행 MK-046(`sidebar.rs`)에 대한 의존과 UI 표시 헬퍼는 이 모듈로 격리한다.

use krill_proto::os::fs::DirBuilderExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use gpui::{Context, Div, Hsla, Stateful, div, prelude::*, px};
use serde::Deserialize;

use super::Sidebar;

pub const COMMENTS_FILE: &str = krill_proto::orchestration::COMMENTS_FILE;

const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_COMMENTS: usize = 1_024;
const MAX_COMMENT_CHARS: usize = 256;
const MAX_CLAUDE_WORKTREES: usize = 64;

type Store = krill_proto::orchestration::WorktreeStore;

/// 워크트리 경로의 체크포인트 배지(`REVIEW #128 OWE-626 links 2`)를 구성한다(MK-116, MK-118).
///
/// 미확인 여부·참조 수 같은 도형 정보는 문자열에 유니코드 글리프(`●`·`🔗`)로 넣지 않고
/// [`CheckpointParts`]와 [`checkpoint_badge_element`]에서 `ui::status::unread_dot()`·
/// Lucide 아이콘(`IconName::Link`)+`ui::badge`로 그린다(`docs/STYLEGUIDE.md` §6.4).
pub fn checkpoint_badge_for(sidebar_path: Option<&Path>, worktree: &Path) -> Option<String> {
    checkpoint_badge_for_card(sidebar_path, worktree, false)
}

/// 체크포인트 배지의 구조화된 내용(문자열 뱃지와 그래픽 뱃지가 공유).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointParts {
    pub unread: bool,
    pub status: Option<String>,
    pub pr: Option<u64>,
    pub issue: Option<String>,
    pub refs_len: usize,
}

impl CheckpointParts {
    /// 카드·행에 그리는 라벨 텍스트. 미확인 점과 참조 수는 아이콘으로 그리므로
    /// 문자열에는 상태·PR·이슈·참조 수(`links {n}`)만 남긴다. 미확인은
    /// 행 상태 묶음의 `UNREAD`와 미확인 점이 이미 알리므로 여기서 덧붙이지 않는다.
    #[must_use]
    pub fn label(&self, omit_issue: bool) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(st) = &self.status {
            parts.push(st.clone());
        }
        if let Some(n) = self.pr {
            parts.push(format!("#{n}"));
        }
        if !omit_issue && let Some(iss) = &self.issue {
            parts.push(iss.clone());
        }
        if self.refs_len > 0 {
            parts.push(format!("links {}", self.refs_len));
        }
        if parts.is_empty() {
            return None;
        }
        Some(parts.join(" "))
    }
}

/// 카드 렌더링용 체크포인트 배지. 별도 `issue_badge`가 이미 표시될 때(`omit_issue == true`)는
/// 이슈 키 중복 출력을 생략해 배지가 길어져 잘리는 현상을 막는다(OWE-846).
pub fn checkpoint_badge_for_card(
    sidebar_path: Option<&Path>,
    worktree: &Path,
    omit_issue: bool,
) -> Option<String> {
    checkpoint_badge_parts(sidebar_path, worktree).and_then(|p| p.label(omit_issue))
}

/// 체크포인트 배지의 구조화된 내용을 읽는다. 표시할 정보가 하나도 없으면 `None`.
pub fn checkpoint_badge_parts(
    sidebar_path: Option<&Path>,
    worktree: &Path,
) -> Option<CheckpointParts> {
    let dir = sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())?;
    let store = krill_proto::orchestration::WorktreeStore::load_cached(dir);
    checkpoint_parts_from_store(&store, worktree)
}

pub(super) fn checkpoint_parts_from_store(
    store: &Store,
    worktree: &Path,
) -> Option<CheckpointParts> {
    let candidates = path_aliases(worktree);
    for key in candidates {
        let unread = store.unread.get(&key).copied().unwrap_or(false);
        let status = store
            .statuses
            .get(&key)
            .and_then(|s| krill_proto::orchestration::WorkspaceStatus::parse(s).ok())
            // 진행 상태는 그룹화·보드·저장소에 유지하고 카드의 WIP 장식만 생략한다.
            .filter(|s| *s != krill_proto::orchestration::WorkspaceStatus::InProgress)
            .map(krill_proto::orchestration::WorkspaceStatus::badge_label)
            .map(str::to_owned);
        let pr = store.prs.get(&key).copied();
        let issue = store.issues.get(&key).map(String::as_str);
        let refs_len = store.references.get(&key).map_or(0, Vec::len);
        if !unread && status.is_none() && pr.is_none() && issue.is_none() && refs_len == 0 {
            continue;
        }
        return Some(CheckpointParts {
            unread,
            status,
            pr,
            issue: issue.map(str::to_owned),
            refs_len,
        });
    }
    None
}

/// 체크포인트 배지 그래픽 요소: [미확인 점][라벨 텍스트][링크 아이콘 + 참조 수].
/// 워크트리 카드(`p.worktree_sidebar`) 위에서는 카드보다 밝은 `p.worktree_sidebar_accent`
/// 배경과 `p.muted_foreground` 전경을 쓴다(다크 모드 `#262626` 역전 방지, STYLEGUIDE §3.1).
pub fn checkpoint_badge_element(parts: &CheckpointParts, p: &crate::ui::Palette) -> Div {
    use gpui_component::{Icon, Sizable as _};
    let mut row = div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .rounded(crate::ui::RADIUS_XS)
        .bg(p.worktree_sidebar_accent)
        .font_family(crate::ui::FONT_MONO)
        .text_size(crate::ui::TEXT_2XS)
        .text_color(p.muted_foreground);
    if parts.unread {
        row = row.child(crate::ui::status::unread_dot());
    }
    row = row.children(parts.status.iter().map(|st| div().child(st.clone())));
    row = row.children(parts.pr.map(|n| div().child(format!("#{n}"))));
    row = row.children(parts.issue.iter().map(|iss| div().child(iss.clone())));
    if parts.refs_len > 0 {
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .child(
                    Icon::new(gpui_kit_assets::IconName::Link)
                        .with_size(crate::ui::ICON_XS)
                        .text_color(p.muted_foreground),
                )
                .child(parts.refs_len.to_string()),
        );
    }
    row
}

/// 세션 오케스트레이션 배지의 구조화된 내용(워커 이름 또는 지휘자).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchBadge {
    Worker(String),
    Coordinator,
}

/// 세션이 오케스트레이션 워커 또는 지휘자로 등록되어 있으면 배지를 돌려준다(MK-114, MK-118).
pub fn session_orch_parts(sidebar_path: Option<&Path>, session_id: u64) -> Option<OrchBadge> {
    let dir = sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())?;
    let orch = krill_proto::orchestration::OrchestrationStore::load_cached(dir);
    orch_parts_from_store(&orch, session_id)
}

pub(super) fn orch_parts_from_store(
    orch: &krill_proto::orchestration::OrchestrationStore,
    session_id: u64,
) -> Option<OrchBadge> {
    if let Some(disp) = orch
        .dispatches
        .iter()
        .rev()
        .find(|d| d.session_id == Some(session_id))
    {
        return Some(OrchBadge::Worker(disp.worker_name.clone()));
    }
    if orch
        .runs
        .iter()
        .any(|r| r.coordinator_session_id == Some(session_id))
    {
        return Some(OrchBadge::Coordinator);
    }
    None
}

/// 세션이 오케스트레이션 워커 또는 지휘자로 등록되어 있으면 배지 라벨을 돌려준다(MK-114, MK-118).
///
/// 행 그래픽은 [`session_orch_element`]가 Lucide 워크플로 아이콘(`WORKFLOW_SVG`) +
/// `ui::badge`로 그리며, 이 함수는 스크린 리더·테스트용 텍스트만 돌려준다.
pub fn session_orch_badge(sidebar_path: Option<&Path>, session_id: u64) -> Option<String> {
    match session_orch_parts(sidebar_path, session_id)? {
        OrchBadge::Worker(name) => Some(name),
        // AS-26: 사용자 노출 배지는 카탈로그 키로(한국어 UI에서 영어 노출 금지).
        OrchBadge::Coordinator => Some(crate::i18n::catalog::sidebar::COORD_BADGE.get().to_owned()),
    }
}

/// 세션 오케스트레이션 배지 그래픽 요소: [워크플로 아이콘][워커 이름 또는 지휘자].
/// `ui::badge` 톤(`BadgeTone::Muted`)과 같은 배경·테두리·전경을 써서
/// 사이드바 표면 위에서도 명도 대비를 유지한다(STYLEGUIDE §3.1·§6.2).
pub fn session_orch_element(badge: &OrchBadge, p: &crate::ui::Palette) -> Div {
    let (bg, border, fg) = crate::ui::BadgeTone::Muted.colors(p);
    let label = match badge {
        OrchBadge::Worker(name) => name.clone(),
        OrchBadge::Coordinator => crate::i18n::catalog::sidebar::COORD_BADGE.get().to_owned(),
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(crate::ui::TEXT_2XS)
        .text_color(fg)
        .child(crate::ui::icons::svg_icon(
            crate::ui::icons::WORKFLOW_SVG,
            crate::ui::ICON_XS,
            fg,
        ))
        .child(label)
}

pub(super) fn path_aliases(worktree: &Path) -> Vec<String> {
    let mut out = vec![worktree.to_string_lossy().into_owned()];
    if let Ok(rest) = worktree.strip_prefix("/private") {
        out.push(Path::new("/").join(rest).to_string_lossy().into_owned());
    } else if let Ok(rest) = worktree.strip_prefix("/") {
        out.push(
            Path::new("/private")
                .join(rest)
                .to_string_lossy()
                .into_owned(),
        );
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 상한 도달 시 가장 오래된 기록(시각 없음은 0, 동점은 경로 사전순)의 키를 고른다.
fn oldest_key(store: &Store) -> Option<String> {
    store
        .comments
        .keys()
        .min_by(|a, b| {
            let ta = store.updated_at.get(*a).copied().unwrap_or(0);
            let tb = store.updated_at.get(*b).copied().unwrap_or(0);
            ta.cmp(&tb).then_with(|| a.cmp(b))
        })
        .cloned()
}

/// 사이드바 설정 파일이나 데몬 소켓 경로에서 상태 디렉터리를 찾는다.
pub fn state_dir_from(sidebar_path: Option<&Path>, socket_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(dir.to_path_buf());
    }
    if let Some(dir) = socket_path
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(dir.to_path_buf());
    }
    krill_proto::daemon::state_dir()
}

/// 사이드바 설정 파일(`sidebar.bin`)이 있는 디렉터리에서 워크트리 코멘트를 읽는다.
pub fn load_comments_from_sidebar_path(sidebar_path: Option<&Path>) -> BTreeMap<PathBuf, String> {
    sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .map(load_comments)
        .unwrap_or_default()
}

/// 상태 디렉터리의 `worktree-comments.toml`에서 워크트리별 코멘트를 읽는다.
pub fn load_comments(state_dir: &Path) -> BTreeMap<PathBuf, String> {
    let mut bytes = Vec::new();
    let Ok(file) = fs::File::open(state_dir.join(COMMENTS_FILE)) else {
        return BTreeMap::new();
    };
    if file
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES)
    {
        return BTreeMap::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return BTreeMap::new();
    };
    let store: Store = toml::from_str(text).unwrap_or_default();
    store
        .comments
        .into_iter()
        .filter_map(|(k, v)| {
            let clean = sanitize(&v);
            (!clean.is_empty()).then(|| (PathBuf::from(k), clean))
        })
        .take(MAX_COMMENTS)
        .collect()
}

/// 테스트·내부 호출에서 워크트리 코멘트를 상태 디렉터리에 기록한다.
/// 시각 기록(`updated_at`)을 함께 읽어 상한 도달 시 가장 오래된 항목을 지운다.
pub fn save_comment(state_dir: &Path, worktree: &Path, comment: &str) -> io::Result<()> {
    let key = krill_proto::orchestration::worktree_key(worktree);
    let clean = sanitize(comment);
    Store::mutate(state_dir, |store| {
        // 손상된 코멘트 값을 정리한다(표시 경로와 같은 sanitize 기준).
        store.comments.retain(|_, v| !sanitize(v).is_empty());
        if clean.is_empty() {
            store.comments.remove(&key);
            store.updated_at.remove(&key);
        } else {
            if store.comments.len() >= MAX_COMMENTS
                && !store.comments.contains_key(&key)
                && let Some(oldest) = oldest_key(store)
            {
                store.comments.remove(&oldest);
                store.updated_at.remove(&oldest);
            }
            store.comments.insert(key.clone(), clean);
            store.updated_at.insert(key, now_secs());
        }
        Ok(())
    })
}

/// 워크트리 경로로 등록된 코멘트를 찾는다.
/// 부모 저장소(`/repo`)와 중첩 워크트리(`/repo/.claude/worktrees/...`) 간에 코멘트가 섞이지 않도록
/// 조상·자손 접두사 매칭은 하지 않고, 정확히 같은 경로(macOS `/private` 심링크 별칭 포함)만 매칭한다.
pub fn comment_for<'a>(
    comments: &'a BTreeMap<PathBuf, String>,
    worktree: &Path,
) -> Option<&'a str> {
    if let Some(exact) = comments.get(worktree).filter(|s| !s.is_empty()) {
        return Some(exact.as_str());
    }
    if let Ok(rest) = worktree.strip_prefix("/private") {
        let alias = Path::new("/").join(rest);
        if let Some(found) = comments.get(&alias).filter(|s| !s.is_empty()) {
            return Some(found.as_str());
        }
    } else if let Ok(rest) = worktree.strip_prefix("/") {
        let alias = Path::new("/private").join(rest);
        if let Some(found) = comments.get(&alias).filter(|s| !s.is_empty()) {
            return Some(found.as_str());
        }
    }
    None
}

fn sanitize(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .collect();
    mapped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_COMMENT_CHARS)
        .collect()
}

/// 경로 구성 요소에 `.claude/worktrees`가 들어 있는지 판정한다.
pub fn is_claude_worktree(path: &Path) -> bool {
    let mut comps = path.components().peekable();
    while let Some(c) = comps.next() {
        if c.as_os_str() == ".claude"
            && comps
                .peek()
                .is_some_and(|next| next.as_os_str() == "worktrees")
        {
            return true;
        }
    }
    false
}

/// 정렬된 워크트리 목록을 일반 워크트리와 `.claude/worktrees` 외부 워크트리로 가른다.
/// 주 저장소 루트(`repo`) 자신은 숨기지 않는다.
pub fn partition_trees<'a>(
    repo: &Path,
    trees: Vec<&'a PathBuf>,
) -> (Vec<&'a PathBuf>, Vec<&'a PathBuf>) {
    trees
        .into_iter()
        .partition(|tree| tree.as_path() == repo || !is_claude_worktree(tree))
}

/// 같은 저장소 그룹 안에서 주 워크트리(`repo` 키) 밑으로 묶이지 않은 연결 워크트리를
/// Orca처럼 본 워크트리 자식으로 묶는다(MK-176). 명시적 부모가 있는 항목은 건드리지 않으며,
/// `can_assign_parent`을 통과한 것만 돌려준다. git linked 워크트리·Orca 이관 워크트리가
/// 하단에서 평탄하게 풀리는 대신 `N 하위` 토글로 접히도록 렌더 계보의 입력이 된다.
/// 호출자는 처음 묶인 부모를 접힘 상태로 시작시켜 Orca 기본 보기와 맞춘다.
#[must_use]
pub fn infer_repo_main_lineage(
    trees: &std::collections::BTreeMap<PathBuf, Vec<PathBuf>>,
    existing: &std::collections::BTreeMap<PathBuf, PathBuf>,
) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut probe = existing.clone();
    for (repo, list) in trees {
        if list.len() < 3 || !list.contains(repo) {
            continue;
        }
        for child in list {
            if child == repo || probe.contains_key(child) {
                continue;
            }
            if !seen.insert(child.clone()) {
                continue;
            }
            if super::lineage::can_assign_parent(&probe, child, repo) {
                probe.insert(child.clone(), repo.clone());
                out.push((child.clone(), repo.clone()));
            }
        }
    }
    out
}

/// `<repo>/.claude/worktrees` 아래에 있는 디렉터리도 스냅샷에 보탠다(이미 있으면 중복 추가하지 않음).
/// AS-09: 디렉터리 자체의 mtime이 그대로면 재스캔하지 않는다. `apply`가 0.5초마다 UI 스레드에서
/// 부르고 백그라운드 조회가 같은 함수를 이미 실행하므로, 매번 `read_dir`+`canonicalize`(최대 64개)를
/// 반복하면 네트워크·외장 볼륨에서 UI가 멈춘다. 새 워크트리는 부모 mtime을 바꾸므로 발견이 늦어지지 않는다.
pub fn include_claude_worktrees(trees: &mut BTreeMap<PathBuf, Vec<PathBuf>>) {
    for (repo, list) in trees.iter_mut() {
        for path in discover_claude_worktrees(repo) {
            if !list.contains(&path) {
                list.push(path);
            }
        }
    }
}

pub const PROJECTS_FILE: &str = "projects.toml";
const MAX_PROJECTS_BYTES: u64 = 512 * 1024;

/// OWE-852: `프로젝트 추가`로 연 폴더를 `<state_dir>/projects.toml`에 영속 등록한다.
/// 이미 등록(동일 경로, 미보관)돼 있으면 `Ok(false)`로 아무것도 쓰지 않는다.
/// 보관(`archived`)된 동일 경로는 보관 해제하고 `Ok(true)`를 돌려준다.
/// 파싱 실패·상한 초과 등에서는 폴더 열기를 막지 않도록 `Ok(false)`로 조용히 넘어간다.
pub fn register_project(state_dir: &Path, path: &Path) -> io::Result<bool> {
    // linked 워크트리(⌘N으로 만든 탭 등)는 독립 프로젝트가 아니라 주 워크트리(저장소)로 등록한다.
    let main = main_worktree(path);
    let path = main.as_deref().unwrap_or(path);
    let normalized = normalize_project_path(path);
    if normalized.is_empty() {
        return Ok(false);
    }
    let file_path = state_dir.join(PROJECTS_FILE);
    let mut raw = String::new();
    if let Ok(file) = fs::File::open(&file_path) {
        let mut bytes = Vec::new();
        if file
            .take(MAX_PROJECTS_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
            || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_PROJECTS_BYTES)
        {
            return Ok(false);
        }
        if !bytes.is_empty() {
            match std::str::from_utf8(&bytes) {
                Ok(text) => raw.push_str(text),
                Err(_) => return Ok(false),
            }
        }
    }
    let mut value: toml::Value = if raw.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        match toml::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        }
    };
    let Some(table) = value.as_table_mut() else {
        return Ok(false);
    };
    let projects = table
        .entry("projects".to_owned())
        .or_insert(toml::Value::Array(Vec::new()));
    let Some(list) = projects.as_array_mut() else {
        return Ok(false);
    };
    for entry in list.iter_mut() {
        let Some(item) = entry.as_table_mut() else {
            continue;
        };
        let entry_path = item
            .get("path")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !same_project_path(&entry_path, &normalized) {
            continue;
        }
        let archived = item
            .get("archived")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        if archived {
            item.insert("archived".to_owned(), toml::Value::Boolean(false));
            write_projects_file(&file_path, &value)?;
            return Ok(true);
        }
        return Ok(false);
    }
    let basename = Path::new(&normalized)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("project")
        .to_owned();
    let kind = if Path::new(&normalized).join(".git").exists() {
        "git"
    } else {
        "folder"
    };
    let mut used: Vec<String> = list
        .iter()
        .filter_map(|e| {
            e.as_table()?
                .get("id")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let mut id = slugify_project_id(&basename);
    if used.iter().any(|u| u == &id) {
        let mut n: u32 = 2;
        loop {
            let candidate = format!("{id}-{n}");
            if !used.iter().any(|u| u == &candidate) {
                id = candidate;
                break;
            }
            n = n.saturating_add(1);
            if n > 9999 {
                return Ok(false);
            }
        }
    }
    used.push(id.clone());
    let mut item = toml::map::Map::new();
    item.insert("id".to_owned(), toml::Value::String(id));
    item.insert("path".to_owned(), toml::Value::String(normalized.clone()));
    item.insert("display_name".to_owned(), toml::Value::String(basename));
    item.insert("kind".to_owned(), toml::Value::String(kind.to_owned()));
    item.insert("pinned".to_owned(), toml::Value::Boolean(false));
    item.insert("archived".to_owned(), toml::Value::Boolean(false));
    list.push(toml::Value::Table(item));
    write_projects_file(&file_path, &value)?;
    Ok(true)
}

/// linked git 워크트리면 주 워크트리 경로. `.git` 파일 → `gitdir` → `commondir`를 따라간다.
/// 주 워크트리(`.git` 디렉터리)·서브모듈(`commondir` 없음)·git 아님은 `None`.
fn main_worktree(path: &Path) -> Option<PathBuf> {
    let link = fs::read_to_string(path.join(".git")).ok()?;
    let gitdir = path.join(link.strip_prefix("gitdir:")?.trim());
    let common = fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = fs::canonicalize(gitdir.join(common.trim())).ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    common.parent().map(Path::to_path_buf)
}

fn normalize_project_path(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        return String::new();
    }
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical.to_string_lossy().trim().to_owned()
}

fn strip_private_prefix(s: &str) -> &str {
    s.strip_prefix("/private").unwrap_or(s)
}

fn same_project_path(a: &str, b: &str) -> bool {
    let a = a.trim();
    let b = b.trim();
    if a == b {
        return true;
    }
    strip_private_prefix(a) == strip_private_prefix(b)
}

fn slugify_project_id(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 64 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        "project".to_owned()
    } else {
        trimmed
    }
}

fn write_projects_file(path: &Path, value: &toml::Value) -> io::Result<()> {
    let body = toml::to_string(value).map_err(io::Error::other)?;
    if u64::try_from(body.len()).map_or(true, |n| n > MAX_PROJECTS_BYTES) {
        return Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            "projects.toml exceeds size limit",
        ));
    }
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "projects path has no parent",
        ));
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    builder.create(parent)?;
    let tmp = parent.join(format!(".{}.tmp.{}", PROJECTS_FILE, std::process::id()));
    let result = (|| -> io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        fs::remove_file(&tmp).ok();
    }
    result
}

#[derive(Debug, Default, Deserialize)]
struct ImportedProjectsFile {
    #[serde(default)]
    projects: Vec<ImportedProject>,
    #[serde(default)]
    worktrees: BTreeMap<String, ImportedWorktreeMeta>,
    #[serde(default)]
    lineage: BTreeMap<String, ImportedWorktreeLineage>,
}

/// `projects.toml`에 저장된 프로젝트 아이콘 명세(Orca `RepoIcon` 패리티).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct ImportedRepoIcon {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Default, Deserialize)]
struct ImportedProject {
    #[serde(default)]
    id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    badge_color: Option<String>,
    #[serde(default)]
    repo_icon: Option<ImportedRepoIcon>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    host_id: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Default, Deserialize)]
struct ImportedWorktreeMeta {
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    workspace_status: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    issue: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ImportedWorktreeLineage {
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    worktree_path: String,
    #[serde(default)]
    branch: Option<String>,
}

/// `<state_dir>/projects.toml`에 등록된 이관 프로젝트 경로 집합(OWE-900 자동 접힘 판정용).
/// 보관·원격 프로젝트를 제외한 로컬 프로젝트의 다듬은 경로만 담는다.
pub(super) fn imported_repo_paths(state_dir: &Path) -> BTreeSet<String> {
    let Some(file) = load_imported_projects(state_dir) else {
        return BTreeSet::new();
    };
    file.projects
        .iter()
        .filter(|p| !p.archived && p.host_id.is_none())
        .map(|p| p.path.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn load_imported_projects(state_dir: &Path) -> Option<ImportedProjectsFile> {
    let mut bytes = Vec::new();
    let file = fs::File::open(state_dir.join(PROJECTS_FILE)).ok()?;
    if file
        .take(MAX_PROJECTS_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_PROJECTS_BYTES)
    {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    toml::from_str(text).ok()
}

fn resolve_repo_for_worktree(
    projects: &[ImportedProject],
    repo_id: Option<&str>,
    wt_path: &Path,
) -> PathBuf {
    let wt_norm = normalize_imported_path(wt_path);
    if let Some(id) = repo_id
        && let Some(proj) = projects
            .iter()
            .find(|p| !p.archived && p.host_id.is_none() && p.id == id)
    {
        return normalize_imported_path(Path::new(proj.path.trim()));
    }
    if let Some(proj) = projects.iter().find(|p| {
        !p.archived
            && p.host_id.is_none()
            && !p.path.trim().is_empty()
            && ({
                let repo_norm = normalize_imported_path(Path::new(p.path.trim()));
                wt_norm == repo_norm || wt_norm.starts_with(&repo_norm)
            })
    }) {
        return normalize_imported_path(Path::new(proj.path.trim()));
    }
    wt_norm
}

/// 이관 경로 정규화(OWE-803). 존재하면 `fs::canonicalize`로 풀고,
/// 없으면 macOS 기본 별칭(`/tmp`·`/var`·`/etc` → `/private/...`)만 어휘적으로 맞춘다.
/// 과거 이관분(`[[projects]]` 원본 경로 vs `[worktrees]` 정규 경로)이 섞인 파일도
/// 사이드바에서 하나의 저장소로 합쳐지도록 읽는 쪽에서도 정규화한다.
/// 별칭 후보가 아니면 시스템 콜 없이 그대로 둔다(스냅샷 폴링 부하 방지).
fn normalize_imported_path(path: &Path) -> PathBuf {
    if !["/tmp", "/var", "/etc", "/private"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return path.to_path_buf();
    }
    if let Ok(canon) = fs::canonicalize(path) {
        return canon;
    }
    for base in ["tmp", "var", "etc"] {
        let prefix = Path::new("/").join(base);
        if let Ok(rest) = path.strip_prefix(&prefix) {
            let aliased = Path::new("/private").join(base).join(rest);
            if let Ok(canon) = fs::canonicalize(&aliased) {
                return canon;
            }
            return aliased;
        }
    }
    path.to_path_buf()
}

/// `<state_dir>/projects.toml`(Orca 이관 프로젝트·워크트리·계보)을 사이드바 스냅샷 트리와 브랜치 메타에 병합한다.
pub fn include_imported_projects(
    state_dir: &Path,
    trees: &mut BTreeMap<PathBuf, Vec<PathBuf>>,
    meta: &mut super::Meta,
) {
    let Some(file) = load_imported_projects(state_dir) else {
        return;
    };
    for proj in &file.projects {
        if proj.archived || proj.host_id.is_some() || proj.path.trim().is_empty() {
            continue;
        }
        let repo = normalize_imported_path(Path::new(proj.path.trim()));
        // v0.1.8이 linked 워크트리를 프로젝트로 등록했다. 주 저장소 그룹 안에 넣어 중복 그룹을 막는다.
        let key =
            main_worktree(&repo).map_or_else(|| repo.clone(), |m| normalize_imported_path(&m));
        let list = trees.entry(key).or_default();
        if !list.contains(&repo) {
            list.push(repo);
        }
    }
    for (wt_key, wt) in &file.worktrees {
        if wt.archived || wt_key.trim().is_empty() {
            continue;
        }
        let wt_path = normalize_imported_path(Path::new(wt_key.trim()));
        let repo = resolve_repo_for_worktree(&file.projects, wt.repo_id.as_deref(), &wt_path);
        let list = trees.entry(repo).or_default();
        if !list.contains(&wt_path) {
            list.push(wt_path);
        }
    }
    for (lin_key, lin) in &file.lineage {
        let wt_str = if lin.worktree_path.trim().is_empty() {
            lin_key.trim()
        } else {
            lin.worktree_path.trim()
        };
        if wt_str.is_empty() {
            continue;
        }
        let wt_path = normalize_imported_path(Path::new(wt_str));
        let repo = resolve_repo_for_worktree(&file.projects, lin.repo_id.as_deref(), &wt_path);
        let list = trees.entry(repo).or_default();
        if !list.contains(&wt_path) {
            list.push(wt_path.clone());
        }
        if let Some(ref branch) = lin.branch
            && !branch.trim().is_empty()
        {
            let entry = meta.entry(wt_path).or_default();
            if entry.branch.is_none() {
                entry.branch = Some(branch.trim().to_owned());
            }
        }
    }
}

/// `<state_dir>/projects.toml`에서 지정한 저장소·폴더 프로젝트와 소속 워크트리·계보 항목을 제거한다(OWE-845).
pub fn remove_imported_project(state_dir: &Path, project_path: &Path) {
    let path = state_dir.join(PROJECTS_FILE);
    let mut bytes = Vec::new();
    let Ok(file) = fs::File::open(&path) else {
        return;
    };
    if file
        .take(MAX_PROJECTS_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_PROJECTS_BYTES)
    {
        return;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return;
    };
    let Ok(mut doc) = toml::from_str::<toml::Value>(text) else {
        return;
    };
    let Some(root) = doc.as_table_mut() else {
        return;
    };
    let aliases = path_aliases(project_path);
    let matches_project = |candidate: &str| {
        let trimmed = candidate.trim();
        !trimmed.is_empty()
            && aliases
                .iter()
                .any(|a| trimmed == a || Path::new(trimmed).starts_with(a))
    };
    let mut removed_ids = Vec::new();
    let mut changed = false;
    if let Some(toml::Value::Array(projects)) = root.get_mut("projects") {
        let before = projects.len();
        projects.retain(|item| {
            let Some(tbl) = item.as_table() else {
                return true;
            };
            let p = tbl.get("path").and_then(toml::Value::as_str).unwrap_or("");
            if aliases.iter().any(|a| p.trim() == a) {
                if let Some(id) = tbl.get("id").and_then(toml::Value::as_str)
                    && !id.trim().is_empty()
                {
                    removed_ids.push(id.trim().to_owned());
                }
                false
            } else {
                true
            }
        });
        changed |= projects.len() != before;
    }
    if let Some(toml::Value::Table(worktrees)) = root.get_mut("worktrees") {
        let before = worktrees.len();
        worktrees.retain(|wt_key, val| {
            let repo_id = val
                .as_table()
                .and_then(|t| t.get("repo_id"))
                .and_then(toml::Value::as_str)
                .map(str::trim);
            let by_id = repo_id.is_some_and(|id| removed_ids.iter().any(|r| r == id));
            !(by_id || matches_project(wt_key))
        });
        changed |= worktrees.len() != before;
    }
    if let Some(toml::Value::Table(lineage)) = root.get_mut("lineage") {
        let before = lineage.len();
        lineage.retain(|lin_key, val| {
            let tbl = val.as_table();
            let repo_id = tbl
                .and_then(|t| t.get("repo_id"))
                .and_then(toml::Value::as_str)
                .map(str::trim);
            let wt_path = tbl
                .and_then(|t| t.get("worktree_path"))
                .and_then(toml::Value::as_str)
                .unwrap_or(lin_key);
            let by_id = repo_id.is_some_and(|id| removed_ids.iter().any(|r| r == id));
            !(by_id || matches_project(lin_key) || matches_project(wt_path))
        });
        changed |= lineage.len() != before;
    }
    if !changed {
        return;
    }
    let Ok(serialized) = toml::to_string_pretty(&doc) else {
        return;
    };
    let tmp = path.with_extension("toml.tmp");
    if fs::write(&tmp, serialized.as_bytes()).is_err() {
        return;
    }
    fs::rename(&tmp, &path).ok();
}

/// `<state_dir>/projects.toml`의 표시 이름·고정 여부·코멘트·이슈·상태를
/// 사이드바 인메모리 상태에 반영한다. 계보(부모 워크트리)는 이관기가 `WorktreeStore`(단일 원천)에 기록한다.
///
/// `pinned`와 `display_name`은 `prefs`에 해당 경로 엔트리가 아직 없을 때(`Entry::Vacant`)만
/// 초기값으로 주입해, 이미 사용자가 수정한 `sidebar.bin` 설정(핀 해제·이름 초기화)을 덮어쓰지 않는다(`OWE-767`).
pub(super) fn apply_imported_project_prefs(
    state_dir: &Path,
    prefs: &mut super::Preferences,
    comments: &mut BTreeMap<PathBuf, String>,
    issues: &mut BTreeMap<PathBuf, String>,
    statuses: &mut BTreeMap<PathBuf, krill_proto::orchestration::WorkspaceStatus>,
) {
    let Some(file) = load_imported_projects(state_dir) else {
        return;
    };
    for proj in &file.projects {
        if proj.archived || proj.host_id.is_some() || proj.path.trim().is_empty() {
            continue;
        }
        let repo = normalize_imported_path(Path::new(proj.path.trim()));
        let dn = proj.display_name.trim();
        if (!dn.is_empty() || proj.pinned)
            && let std::collections::btree_map::Entry::Vacant(vacant) = prefs.entry(repo)
        {
            vacant.insert(super::Preference {
                pinned: proj.pinned,
                name: dn.to_owned(),
            });
        }
    }
    for (wt_key, wt) in &file.worktrees {
        if wt.archived || wt_key.trim().is_empty() {
            continue;
        }
        let wt_path = normalize_imported_path(Path::new(wt_key.trim()));
        let dn = wt
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if (dn.is_some() || wt.pinned)
            && let std::collections::btree_map::Entry::Vacant(vacant) = prefs.entry(wt_path.clone())
        {
            vacant.insert(super::Preference {
                pinned: wt.pinned,
                name: dn.unwrap_or_default().to_owned(),
            });
        }
        if let Some(ref c) = wt.comment {
            let clean = sanitize(c);
            if !clean.is_empty() {
                comments.entry(wt_path.clone()).or_insert(clean);
            }
        }
        if let Some(ref iss) = wt.issue
            && !iss.trim().is_empty()
        {
            issues
                .entry(wt_path.clone())
                .or_insert_with(|| iss.trim().to_owned());
        }
        if let Some(ref st_str) = wt.workspace_status
            && let Ok(st) = krill_proto::orchestration::WorkspaceStatus::parse(st_str)
        {
            statuses.entry(wt_path).or_insert(st);
        }
    }
}

/// 사이드바 프로젝트 헤더에 그릴 이름·배지 색·아이콘 정보.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectVisualMeta {
    pub display_name: Option<String>,
    pub badge_color: Option<String>,
    pub icon: Option<crate::ui::icons::ProjectIcon>,
}

const REPO_ICON_DIR: &str = "repo-icons";
const MAX_ICON_SOURCE_BYTES: u64 = 64 * 1024;
const MAX_ICON_IMAGE_BYTES: usize = 256 * 1024;

/// Orca `REPO_ICON_FILE_STEMS`(`.ref/orca/src/main/repo-icon-file-detection.ts`)와 같은 탐색 우선순위.
const REPO_ICON_FILE_STEMS: &[&str] = &[
    "favicon",
    "public/favicon",
    "app/favicon",
    "app/icon",
    "src/favicon",
    "src/app/icon",
    "assets/favicon",
    "assets/icon",
    "static/favicon",
    "logo",
    "public/logo",
    "public/icon",
    "src-tauri/icons/icon",
    "app-icon",
    "icon",
    "packaging/macos/favicon",
    "packaging/windows/favicon",
    "packaging/favicon",
];

const REPO_ICON_RASTER_EXTS: &[&str] = &["png", "webp", "ico"];

/// Orca `REPO_ICON_SOURCE_FILE_CANDIDATES`(`.ref/orca/src/main/repo-icon-file-detection.ts`).
const REPO_ICON_SOURCE_FILES: &[&str] = &[
    "index.html",
    "public/index.html",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
    "app/root.tsx",
    "src/root.tsx",
    "src/index.html",
];

const RESERVED_GITHUB_PATHS: &[&str] = &[
    "orgs",
    "users",
    "settings",
    "marketplace",
    "explore",
    "topics",
    "collections",
    "events",
    "sponsors",
    "notifications",
    "login",
    "join",
    "about",
    "pricing",
    "security",
    "features",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum CachedIconOutcome {
    LocalFile(PathBuf),
    RemoteUrl(String),
    None,
}

type IconProbeKey = (PathBuf, bool);
type IconProbeEntry = (Option<SystemTime>, CachedIconOutcome);

static DETECTED_ICON_CACHE: LazyLock<Mutex<BTreeMap<IconProbeKey, IconProbeEntry>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

#[cfg(not(test))]
static IN_FLIGHT_ICON_URLS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

/// PNG·WebP·ICO 헤더인지 확인한다. 실제 이미지 디코딩은 gpui가 담당한다.
#[must_use]
pub fn has_valid_icon_bytes(bytes: &[u8]) -> bool {
    let is_png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    let is_webp = bytes.len() >= 12
        && bytes.starts_with(b"RIFF")
        && bytes.get(8..).is_some_and(|tail| tail.starts_with(b"WEBP"));
    let is_ico = bytes.len() >= 6
        && bytes.starts_with(&[0, 0, 1, 0])
        && bytes
            .get(4)
            .zip(bytes.get(5))
            .is_some_and(|(&lo, &hi)| u16::from_le_bytes([lo, hi]) > 0);
    is_png || is_webp || is_ico
}

fn has_valid_icon_magic(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() < 8 || meta.len() > (MAX_ICON_IMAGE_BYTES as u64) * 8 {
        return false;
    }
    let mut header = [0u8; 12];
    let Ok(n) = file.read(&mut header) else {
        return false;
    };
    header.get(..n).is_some_and(has_valid_icon_bytes)
}

fn short_sha256_hex(input: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(16);
    for &b in digest.as_slice().iter().take(8) {
        use std::fmt::Write as _;
        write!(&mut out, "{b:02x}").ok();
    }
    out
}

/// 원격 아이콘 URL이 로컬 디스크에 캐시될 경로(`<cache_dir>/url-<sha256_prefix>.png`).
#[must_use]
pub fn cached_icon_path_for_url(cache_dir: &Path, url: &str) -> PathBuf {
    cache_dir.join(format!("url-{}.png", short_sha256_hex(url.trim())))
}

fn write_cached_icon_bytes(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if !has_valid_icon_bytes(bytes) || bytes.len() > MAX_ICON_IMAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid icon image bytes",
        ));
    }
    let Some(parent) = target.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "icon target has no parent",
        ));
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    builder.create(parent)?;
    let tmp = target.with_extension(format!("tmp.{}", std::process::id()));
    let res = (|| -> io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, target)?;
        Ok(())
    })();
    if res.is_err() {
        fs::remove_file(&tmp).ok();
    }
    res
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
            if out.len() > MAX_ICON_IMAGE_BYTES {
                return None;
            }
        }
    }
    Some(out)
}

/// `data:image/(png|webp);base64,...` 데이터 URL을 `<cache_dir>`에 디코드·저장하고 경로를 돌려준다.
pub fn cache_data_url_icon(data_url: &str, cache_dir: &Path) -> Option<PathBuf> {
    let trimmed = data_url.trim();
    let (ext, b64) = if let Some(rest) = trimmed.strip_prefix("data:image/png;base64,") {
        ("png", rest)
    } else {
        let rest = trimmed.strip_prefix("data:image/webp;base64,")?;
        ("webp", rest)
    };
    let target = cache_dir.join(format!("data-{}.{ext}", short_sha256_hex(trimmed)));
    if has_valid_icon_magic(&target) {
        return Some(target);
    }
    let bytes = decode_base64(b64)?;
    write_cached_icon_bytes(&target, &bytes).ok()?;
    Some(target)
}

fn resolve_or_fetch_remote_icon(
    url: &str,
    cache_dir: &Path,
) -> Option<crate::ui::icons::ProjectIcon> {
    let target = cached_icon_path_for_url(cache_dir, url);
    if has_valid_icon_magic(&target) {
        return Some(crate::ui::icons::ProjectIcon::Image(target));
    }
    #[cfg(not(test))]
    {
        let clean_url = url.trim().to_owned();
        let should_spawn = IN_FLIGHT_ICON_URLS
            .lock()
            .is_ok_and(|mut set| set.insert(clean_url.clone()));
        if should_spawn {
            std::thread::spawn(move || {
                let out = std::process::Command::new("/usr/bin/curl")
                    .args([
                        "-fsSL",
                        "--max-time",
                        "5",
                        "--max-filesize",
                        "262144",
                        &clean_url,
                    ])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .output();
                if let Ok(out) = out
                    && out.status.success()
                {
                    write_cached_icon_bytes(&target, &out.stdout).ok();
                }
                if let Ok(mut set) = IN_FLIGHT_ICON_URLS.lock() {
                    set.remove(clean_url.as_str());
                }
            });
        }
    }
    None
}

fn extract_attr_value(tag: &str, attr_name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(pos) = lower.get(search_from..)?.find(attr_name) {
        let idx = search_from + pos;
        let before_ok = idx == 0
            || lower.as_bytes().get(idx - 1).is_some_and(|b| {
                b.is_ascii_whitespace() || *b == b'<' || *b == b'"' || *b == b'\''
            });
        let after_idx = idx + attr_name.len();
        let rest = tag.get(after_idx..)?.trim_start();
        if before_ok && let Some(after_eq) = rest.strip_prefix('=') {
            let after_eq = after_eq.trim_start();
            if let Some(quote) = after_eq.chars().next().filter(|&c| c == '"' || c == '\'') {
                let inner = after_eq.get(quote.len_utf8()..)?;
                if let Some(end) = inner.find(quote) {
                    return inner.get(..end).map(|s| s.trim().to_owned());
                }
            }
        }
        search_from = after_idx;
    }
    None
}

fn extract_icon_link_hrefs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = source.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower.get(from..).and_then(|s| s.find("<link")) {
        let start = from + pos;
        let Some(end_rel) = lower.get(start..).and_then(|s| s.find('>')) else {
            break;
        };
        let end = start + end_rel + 1;
        if let Some(tag) = source.get(start..end)
            && let Some(rel) = extract_attr_value(tag, "rel")
            && rel
                .split_ascii_whitespace()
                .any(|tok| tok.eq_ignore_ascii_case("icon") || tok.eq_ignore_ascii_case("shortcut"))
            && let Some(href) = extract_attr_value(tag, "href")
            && !href.is_empty()
        {
            out.push(href);
        }
        from = end;
    }
    out
}

fn resolve_local_icon_href(
    repo_path: &Path,
    source_file: &Path,
    raw_href: &str,
) -> Option<PathBuf> {
    let href = raw_href.trim().split('#').next()?.split('?').next()?.trim();
    if href.is_empty()
        || href.starts_with("http://")
        || href.starts_with("https://")
        || href.starts_with("data:")
        || href.starts_with("//")
    {
        return None;
    }
    let canon_repo = fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    let mut candidates = Vec::new();
    if let Some(stripped) = href.strip_prefix('/') {
        candidates.push(repo_path.join("public").join(stripped));
        candidates.push(repo_path.join(stripped));
    } else {
        if let Some(parent) = source_file.parent() {
            candidates.push(parent.join(href));
        }
        candidates.push(repo_path.join("public").join(href));
        candidates.push(repo_path.join(href));
    }
    for cand in candidates {
        let ext_ok = cand.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            REPO_ICON_RASTER_EXTS
                .iter()
                .any(|ext| e.eq_ignore_ascii_case(ext))
        });
        if !ext_ok {
            continue;
        }
        let Ok(canon_cand) = fs::canonicalize(&cand) else {
            continue;
        };
        if canon_cand.starts_with(&canon_repo) && has_valid_icon_magic(&canon_cand) {
            return Some(canon_cand);
        }
    }
    None
}

/// 프로젝트 폴더에서 로컬 파비콘·로고 이미지 파일(PNG·WebP·ICO)을 찾는다.
#[must_use]
pub fn detect_local_file_icon(repo_path: &Path) -> Option<PathBuf> {
    for stem in REPO_ICON_FILE_STEMS {
        for ext in REPO_ICON_RASTER_EXTS {
            let candidate = repo_path.join(format!("{stem}.{ext}"));
            if has_valid_icon_magic(&candidate) {
                return Some(candidate);
            }
        }
    }
    for rel in REPO_ICON_SOURCE_FILES {
        let source_path = repo_path.join(rel);
        let Ok(file) = fs::File::open(&source_path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_ICON_SOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_ICON_SOURCE_BYTES)
        {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for href in extract_icon_link_hrefs(text) {
            if let Some(found) = resolve_local_icon_href(repo_path, &source_path, &href) {
                return Some(found);
            }
        }
    }
    None
}

/// 웹사이트 주소에서 Google S2 파비콘 URL을 만든다(Orca `faviconUrlFromWebsite` + `detectLocalPackageHomepageIcon` 패리티).
#[must_use]
pub fn favicon_url_from_website(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let authority = without_scheme.split(['/', '?', '#']).next()?.trim();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host_port.split(':').next()?.trim().to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host.is_empty()
        || !host.contains('.')
        || matches!(
            host,
            "localhost"
                | "127.0.0.1"
                | "0.0.0.0"
                | "::1"
                | "github.com"
                | "gitlab.com"
                | "bitbucket.org"
        )
    {
        return None;
    }
    Some(format!(
        "https://www.google.com/s2/favicons?domain={host}&sz=64"
    ))
}

/// `<repo_path>/package.json`의 `homepage` 필드에서 파비콘 URL을 추출한다.
#[must_use]
pub fn detect_package_homepage_favicon_url(repo_path: &Path) -> Option<String> {
    let pkg_path = repo_path.join("package.json");
    let file = fs::File::open(pkg_path).ok()?;
    let mut bytes = Vec::new();
    if file
        .take(MAX_ICON_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_ICON_SOURCE_BYTES)
    {
        return None;
    }
    let val: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let homepage = val.get("homepage")?.as_str()?;
    favicon_url_from_website(homepage)
}

fn read_icon_cargo_manifest(path: &Path) -> Option<toml::Value> {
    let mut source = String::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_ICON_SOURCE_BYTES + 1)
        .read_to_string(&mut source)
        .ok()?;
    if source.len() as u64 > MAX_ICON_SOURCE_BYTES {
        return None;
    }
    toml::from_str(&source).ok()
}

/// Rust 패키지·워크스페이스의 homepage에서 파비콘 URL을 찾는다.
#[must_use]
pub fn detect_cargo_homepage_favicon_url(repo_path: &Path) -> Option<String> {
    let manifest = read_icon_cargo_manifest(&repo_path.join("Cargo.toml"))?;
    let package = manifest.get("package");
    let homepage = package.and_then(|p| p.get("homepage"));
    if let Some(url) = homepage.and_then(toml::Value::as_str) {
        return favicon_url_from_website(url);
    }
    let workspace_homepage = |manifest: &toml::Value| {
        favicon_url_from_website(
            manifest
                .get("workspace")?
                .get("package")?
                .get("homepage")?
                .as_str()?,
        )
    };
    // 가상 워크스페이스 루트와 같은 루트의 상속 설정을 먼저 처리한다.
    if package.is_none()
        || homepage
            .and_then(|h| h.get("workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true)
    {
        if manifest.get("workspace").is_some() {
            return workspace_homepage(&manifest);
        }
        if let Some(workspace) = package
            .and_then(|p| p.get("workspace"))
            .and_then(toml::Value::as_str)
        {
            return workspace_homepage(&read_icon_cargo_manifest(
                &repo_path.join(workspace).join("Cargo.toml"),
            )?);
        }
        for parent in repo_path.ancestors().skip(1) {
            if let Some(manifest) = read_icon_cargo_manifest(&parent.join("Cargo.toml"))
                && manifest.get("workspace").is_some()
            {
                return workspace_homepage(&manifest);
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "project_icon_tests.rs"]
mod project_icon_tests;

fn is_valid_github_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.is_empty() || bytes.len() > 39 {
        return false;
    }
    let first_last_ok = bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric);
    first_last_ok
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && !RESERVED_GITHUB_PATHS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(slug))
}

/// GitHub 원격 URL이나 소유자 문자열에서 계정/조직 슬러그를 추출한다(Orca `githubAvatarSlug` 패리티).
#[must_use]
pub fn github_avatar_slug(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('@').trim();
    if trimmed.is_empty() {
        return None;
    }
    // 1. SCP 형식: `git@github.com:owner/repo.git`
    if let Some((host_part, path_part)) = trimmed.split_once(':')
        && !path_part.starts_with("//")
        && host_part
            .rsplit_once('@')
            .map_or(host_part, |(_, h)| h)
            .eq_ignore_ascii_case("github.com")
    {
        let owner = path_part
            .trim_start_matches('/')
            .split('/')
            .next()?
            .trim_end_matches(".git");
        return is_valid_github_slug(owner).then(|| owner.to_ascii_lowercase());
    }
    // 2. 스킴 있는 URL(`https://github.com/owner/repo.git`, `ssh://git@github.com/owner/repo`) 또는 `github.com/owner/repo`
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .or_else(|| trimmed.strip_prefix("git://"))
        .unwrap_or(trimmed);
    if let Some((authority, rest)) = without_scheme.split_once('/') {
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, h)| h)
            .split(':')
            .next()
            .unwrap_or("")
            .trim();
        let host = host.strip_prefix("www.").unwrap_or(host);
        if host.eq_ignore_ascii_case("github.com") {
            let owner = rest
                .trim_start_matches('/')
                .split(['/', '?', '#'])
                .next()?
                .trim_end_matches(".git");
            return is_valid_github_slug(owner).then(|| owner.to_ascii_lowercase());
        }
    }
    None
}

fn parse_git_config_remotes(config_text: &str) -> (Option<String>, Option<String>) {
    let mut current_remote: Option<&str> = None;
    let mut origin: Option<String> = None;
    let mut upstream: Option<String> = None;
    for raw_line in config_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let section = section.trim();
            current_remote = section
                .strip_prefix("remote")
                .map(str::trim)
                .and_then(|rest| rest.strip_prefix('"'))
                .and_then(|rest| rest.strip_suffix('"'));
            continue;
        }
        if let Some(remote_name) = current_remote
            && let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case("url")
        {
            let url = v.trim().trim_matches('"').trim();
            if !url.is_empty() {
                if remote_name == "origin" && origin.is_none() {
                    origin = Some(url.to_owned());
                } else if remote_name == "upstream" && upstream.is_none() {
                    upstream = Some(url.to_owned());
                }
            }
        }
    }
    (origin, upstream)
}

/// Git 저장소의 `.git/config`(`upstream` → `origin` 순)에서 GitHub 소유자 아바타 URL을 추출한다(Orca `detectGitHubAvatarIcon` 패리티).
#[must_use]
pub fn detect_github_avatar_url(repo_path: &Path) -> Option<String> {
    let direct_cfg = repo_path.join(".git/config");
    let config_path = if direct_cfg.is_file() {
        direct_cfg
    } else {
        let main_repo = main_worktree(repo_path)?;
        let main_cfg = main_repo.join(".git/config");
        if main_cfg.is_file() {
            main_cfg
        } else {
            return None;
        }
    };
    let file = fs::File::open(config_path).ok()?;
    let mut bytes = Vec::new();
    if file
        .take(MAX_ICON_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_ICON_SOURCE_BYTES)
    {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let (origin, upstream) = parse_git_config_remotes(text);
    for remote_url in [upstream.as_deref(), origin.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(slug) = github_avatar_slug(remote_url) {
            return Some(format!("https://github.com/{slug}.png?size=64"));
        }
    }
    None
}

fn probe_project_icon_source(repo_path: &Path, is_git: bool) -> CachedIconOutcome {
    let mtime = fs::metadata(repo_path).ok().and_then(|m| m.modified().ok());
    let key = (repo_path.to_path_buf(), is_git);
    if let Ok(cache) = DETECTED_ICON_CACHE.lock()
        && let Some((cached_mtime, outcome)) = cache.get(&key)
        && *cached_mtime == mtime
    {
        return outcome.clone();
    }
    let outcome = if let Some(local) = detect_local_file_icon(repo_path) {
        CachedIconOutcome::LocalFile(local)
    } else if let Some(favicon_url) = detect_package_homepage_favicon_url(repo_path) {
        CachedIconOutcome::RemoteUrl(favicon_url)
    } else if let Some(favicon_url) = detect_cargo_homepage_favicon_url(repo_path) {
        CachedIconOutcome::RemoteUrl(favicon_url)
    } else if is_git && let Some(avatar_url) = detect_github_avatar_url(repo_path) {
        CachedIconOutcome::RemoteUrl(avatar_url)
    } else {
        CachedIconOutcome::None
    };
    if let Ok(mut cache) = DETECTED_ICON_CACHE.lock() {
        cache.insert(key, (mtime, outcome.clone()));
    }
    outcome
}

fn resolve_explicit_repo_icon(
    spec: &ImportedRepoIcon,
    repo_path: &Path,
    cache_dir: Option<&Path>,
) -> Option<crate::ui::icons::ProjectIcon> {
    let val = spec.value.trim();
    if val.is_empty() {
        return None;
    }
    match spec.kind.trim() {
        "lucide" => Some(crate::ui::icons::ProjectIcon::Lucide(val.to_owned())),
        "emoji" => Some(crate::ui::icons::ProjectIcon::Emoji(val.to_owned())),
        "image" => {
            if val.starts_with("data:image/") {
                cache_data_url_icon(val, cache_dir?).map(crate::ui::icons::ProjectIcon::Image)
            } else if val.starts_with("https://") || val.starts_with("http://") {
                resolve_or_fetch_remote_icon(val, cache_dir?)
            } else {
                let raw_path = val.strip_prefix("file://").unwrap_or(val);
                let p = Path::new(raw_path);
                let candidate = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    repo_path.join(p)
                };
                has_valid_icon_magic(&candidate)
                    .then_some(crate::ui::icons::ProjectIcon::Image(candidate))
            }
        }
        _ => None,
    }
}

/// 명시된 아이콘 또는 자동 감지(파일 → package.json/Cargo.toml 홈페이지 → GitHub 아바타).
#[must_use]
pub fn resolve_project_icon(
    repo_path: &Path,
    is_git: bool,
    cache_dir: Option<&Path>,
    explicit: Option<&ImportedRepoIcon>,
) -> Option<crate::ui::icons::ProjectIcon> {
    if let Some(spec) = explicit
        && let Some(resolved) = resolve_explicit_repo_icon(spec, repo_path, cache_dir)
    {
        return Some(resolved);
    }
    match probe_project_icon_source(repo_path, is_git) {
        CachedIconOutcome::LocalFile(path) => Some(crate::ui::icons::ProjectIcon::Image(path)),
        CachedIconOutcome::RemoteUrl(url) => resolve_or_fetch_remote_icon(&url, cache_dir?),
        CachedIconOutcome::None => None,
    }
}

#[derive(Clone)]
struct ImportedVisualEntry {
    display_name: Option<String>,
    badge_color: Option<String>,
    repo_icon: Option<ImportedRepoIcon>,
    is_git: bool,
}

fn default_icon_cache_dir() -> Option<PathBuf> {
    #[cfg(not(test))]
    {
        krill_proto::daemon::state_dir().map(|d| d.join(REPO_ICON_DIR))
    }
    #[cfg(test)]
    {
        None
    }
}

/// 사이드바에 표시할 프로젝트별(`repo_paths` + `folder_paths`) 표시 이름·배지 색·아이콘 맵을 계산한다.
pub(super) fn resolve_project_visuals(
    state_dir: Option<&Path>,
    repo_paths: &[PathBuf],
    folder_paths: &[PathBuf],
) -> BTreeMap<PathBuf, ProjectVisualMeta> {
    let cache_dir = state_dir
        .map(|d| d.join(REPO_ICON_DIR))
        .or_else(default_icon_cache_dir);
    let mut imported_map: BTreeMap<PathBuf, ImportedVisualEntry> = BTreeMap::new();
    if let Some(dir) = state_dir
        && let Some(file) = load_imported_projects(dir)
    {
        for proj in file.projects {
            if proj.archived || proj.host_id.is_some() || proj.path.trim().is_empty() {
                continue;
            }
            let norm = normalize_imported_path(Path::new(proj.path.trim()));
            let key =
                main_worktree(&norm).map_or_else(|| norm.clone(), |m| normalize_imported_path(&m));
            let display_name = Some(proj.display_name.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let badge_color = proj
                .badge_color
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let is_git = proj.kind.as_deref() != Some("folder");
            let entry = ImportedVisualEntry {
                display_name,
                badge_color,
                repo_icon: proj.repo_icon,
                is_git,
            };
            if norm != key {
                imported_map.insert(norm, entry.clone());
            }
            imported_map.insert(key, entry);
        }
    }

    let mut out = BTreeMap::new();
    for (path, default_is_git) in repo_paths
        .iter()
        .map(|p| (p, true))
        .chain(folder_paths.iter().map(|p| (p, false)))
    {
        let norm_path = normalize_imported_path(path);
        let imported = imported_map
            .get(path)
            .or_else(|| imported_map.get(&norm_path));
        let display_name = imported.and_then(|e| e.display_name.clone());
        let badge_color = imported.and_then(|e| e.badge_color.clone());
        let explicit_icon = imported.and_then(|e| e.repo_icon.as_ref());
        let is_git = imported.map_or(default_is_git, |e| e.is_git) || path.join(".git").exists();
        let icon = resolve_project_icon(path, is_git, cache_dir.as_deref(), explicit_icon);
        if display_name.is_some() || badge_color.is_some() || icon.is_some() {
            out.insert(
                path.clone(),
                ProjectVisualMeta {
                    display_name,
                    badge_color,
                    icon,
                },
            );
        }
    }
    out
}

/// 디렉터리 mtime별 발견 목록 캐시(스냅샷 폴링마다 디렉터리 IO를 반복하지 않기 위해).
type WorktreeDiscovery = (Option<SystemTime>, Vec<PathBuf>);
static WORKTREE_CACHE: LazyLock<Mutex<BTreeMap<PathBuf, WorktreeDiscovery>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn discover_claude_worktrees(repo: &Path) -> Vec<PathBuf> {
    let dir = repo.join(".claude/worktrees");
    let mtime = fs::metadata(&dir).ok().and_then(|m| m.modified().ok());
    if let Ok(cache) = WORKTREE_CACHE.lock()
        && let Some((cached_mtime, cached)) = cache.get(repo)
        && *cached_mtime == mtime
    {
        return cached.clone();
    }
    let mut discovered: Vec<PathBuf> = fs::read_dir(&dir).map_or(Vec::new(), |entries| {
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .take(MAX_CLAUDE_WORKTREES)
            .map(|e| {
                let p = e.path();
                fs::canonicalize(&p).unwrap_or(p)
            })
            .collect()
    });
    discovered.sort();
    if let Ok(mut cache) = WORKTREE_CACHE.lock() {
        cache.insert(repo.to_path_buf(), (mtime, discovered.clone()));
    }
    discovered
}

/// 외부 에이전트 한 줄: 사이드바 표시 상태 + 프로세스 id(AS-08 계보 판정용).
/// pid가 없으면 `stop` 뒤 잔류형이라 계보를 확인할 수 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalHit {
    pub state: krill_proto::control::AgentState,
    pub pid: Option<u32>,
}

/// AS-07: Finder·Dock 실행(launchd 기본 PATH)에서도 외부 `claude`를 찾는다.
/// `KRILL_CLAUDE_BIN` 지정 > 사용자 PATH(`krill_proto::agent::which`) > 설치 위치
/// (`~/.local/bin` 실측, Homebrew) 순이다. 없으면 `"claude"`를 그대로 돌려줘
/// 기존처럼 조용히 fail-open한다.
pub fn resolve_claude_bin(explicit: Option<&str>, path_env: &OsStr, home: &Path) -> PathBuf {
    if let Some(bin) = explicit.filter(|s| !s.is_empty()) {
        return PathBuf::from(bin);
    }
    if let Some(found) = krill_proto::agent::which("claude", path_env) {
        return found;
    }
    for dir in [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        let candidate = dir.join("claude");
        if is_executable(&candidate) {
            return candidate;
        }
    }
    PathBuf::from("claude")
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && crate::platform::has_exec_bits(&m))
}

/// `ps -eo pid,ppid` 출력 파서(pid → ppid). 헤더·깨진 행은 건너뛴다.
pub fn parse_ps_parents(text: &str) -> BTreeMap<u32, u32> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: u32 = parts.next()?.parse().ok()?;
            let ppid: u32 = parts.next()?.parse().ok()?;
            (pid != 0).then_some((pid, ppid))
        })
        .collect()
}

/// 현재 프로세스 표(pid → ppid). 실패하면 `None`(호출자는 기존 cwd 동작으로 fail-open).
type ParentMapCache = (Option<BTreeMap<u32, u32>>, Option<std::time::Instant>);

pub fn parent_map() -> Option<BTreeMap<u32, u32>> {
    static CACHE: LazyLock<Mutex<ParentMapCache>> = LazyLock::new(|| Mutex::new((None, None)));
    const TTL: std::time::Duration = std::time::Duration::from_secs(2);
    if let Ok(cache) = CACHE.lock()
        && let (Some(map), Some(at)) = (&cache.0, &cache.1)
        && at.elapsed() < TTL
    {
        return Some(map.clone());
    }
    let out = std::process::Command::new("ps")
        .args(["-eo", "pid,ppid"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let map = std::str::from_utf8(&out.stdout).ok().map(parse_ps_parents);
    if let Ok(mut cache) = CACHE.lock()
        && let Some(ref m) = map
    {
        cache.0 = Some(m.clone());
        cache.1 = Some(std::time::Instant::now());
    }
    map
}

/// AS-08: 외부 에이전트 pid가 세션 루트 pid 자신이거나 후손이면 참.
/// 외부에서(다른 터미널·웹) 띄운 에이전트를 cwd만 같다고 무관한 셸에 덮어씌우지 않는다.
/// pid가 없으면(잔류형) 계보를 확인할 수 없어 참으로 둔다(기존 cwd 동작).
/// 프로세스 표가 없으면(`ps` 실패) 조용히 fail-open한다.
pub fn linked_to_session(
    ext_pid: Option<u32>,
    session_pid: u32,
    parents: Option<&BTreeMap<u32, u32>>,
) -> bool {
    let Some(pid) = ext_pid else {
        return true;
    };
    if pid == session_pid {
        return true;
    }
    let Some(parents) = parents else {
        return true;
    };
    let mut current = pid;
    for _ in 0..64 {
        let Some(&parent) = parents.get(&current) else {
            return false;
        };
        if parent == session_pid {
            return true;
        }
        if parent == current || parent == 0 {
            return false;
        }
        current = parent;
    }
    false
}

/// 스크린 리더용 워크트리 행 레이블을 조합한다.
pub fn worktree_a11y_label(title: &str, name: &str, comment: Option<&str>) -> String {
    match comment {
        Some(c) => format!("{title} {name}, {c}"),
        None => format!("{title} {name}"),
    }
}

/// 워크트리 행의 코멘트 배지 요소를 만든다(UI-2 사이드바 재설계 대비 단일 호출 지점).
pub fn render_comment(comment: Option<&str>, color: Hsla) -> Option<Div> {
    comment.map(|c| {
        div()
            .w_full()
            .min_w_0()
            .truncate()
            .text_size(crate::ui::TEXT_XS)
            .text_color(color)
            .child(c.to_owned())
    })
}

/// `.claude/worktrees 숨김 (N)` 접기/펼치기 카드 행을 렌더링한다.
pub fn render_hidden_row(
    base: Stateful<Div>,
    repo: &Path,
    count: usize,
    expanded: bool,
    cx: &mut Context<Sidebar>,
) -> Stateful<Div> {
    use gpui_component::{Icon, IconName, Sizable as _};
    let p = crate::ui::palette(cx);
    let repo = repo.to_path_buf();
    let a11y_label = format!(
        "{} ({count})",
        crate::i18n::catalog::general::HIDDEN_WORKTREES.get()
    );
    let display_text =
        match (expanded, count) {
            (true, 1) => crate::i18n::catalog::sidebar::SHOWING_DISCOVERED_ONE
                .get()
                .to_owned(),
            (true, _) => crate::i18n::catalog::sidebar::SHOWING_DISCOVERED
                .format(&[("n", &count.to_string())]),
            (false, 1) => crate::i18n::catalog::sidebar::HIDING_DISCOVERED_ONE
                .get()
                .to_owned(),
            (false, _) => crate::i18n::catalog::sidebar::HIDING_DISCOVERED
                .format(&[("n", &count.to_string())]),
        };
    base.aria_label(a11y_label)
        .aria_expanded(expanded)
        .h(px(26.))
        .px_2p5()
        .mx_1p5()
        .cursor_pointer()
        .rounded(crate::ui::RADIUS_SM)
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(crate::ui::TEXT_XS)
        .text_color(p.muted_foreground.opacity(0.75))
        .hover(|d| {
            d.bg(p.sidebar_accent.opacity(0.35))
                .text_color(p.sidebar_foreground)
        })
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .with_size(crate::ui::ICON_XS)
            .text_color(p.muted_foreground.opacity(0.65)),
        )
        .child(div().flex_1().min_w_0().truncate().child(display_text))
        .on_click(cx.listener(move |s, _, _, cx| s.toggle_external(&repo, cx)))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use krill_proto::orchestration::{OrchestrationStore, WorktreeStore};

    /// AS-07: Finder·Dock 실행처럼 PATH에 `~/.local/bin`이 없어도 외부 `claude`를 찾는다.
    #[test]
    fn resolve_claude_bin_prefers_override_then_path_then_known_spots() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = std::env::temp_dir().join(format!("mk-claude-bin-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let local_bin = home.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let local_claude = local_bin.join("claude");
        std::fs::write(&local_claude, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&local_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let empty_path = std::ffi::OsStr::new("/nonexistent");

        // 명시 지정이 최우선.
        assert_eq!(
            resolve_claude_bin(Some("/custom/claude"), empty_path, &home),
            PathBuf::from("/custom/claude")
        );
        // PATH에 없어도 `~/.local/bin`을 찾는다(launchd 기본 PATH 대응).
        assert_eq!(resolve_claude_bin(None, empty_path, &home), local_claude);
        // PATH에 있으면 그걸 쓴다.
        let path_with = std::env::join_paths([local_bin.as_os_str(), empty_path]).unwrap();
        assert_eq!(
            resolve_claude_bin(None, &path_with, Path::new("/nonexistent-home")),
            local_claude
        );
        // 아무 데도 없으면 기존처럼 `"claude"`로 조용히 fail-open.
        assert_eq!(
            resolve_claude_bin(None, empty_path, Path::new("/nonexistent-home")),
            PathBuf::from("claude")
        );
        std::fs::remove_dir_all(&home).ok();
    }

    /// AS-08: pid 계보 판정. 외부에서 띄운 에이전트는 무관한 세션에 묶지 않는다.
    #[test]
    fn linked_to_session_checks_process_kinship() {
        let parents: BTreeMap<u32, u32> = BTreeMap::from([(10, 5), (5, 1), (20, 1), (1, 0)]);
        // pid가 없으면(잔류형) 기존 cwd 동작 유지.
        assert!(linked_to_session(None, 999, Some(&parents)));
        // 세션 루트 자신이거나 후손이면 연결.
        assert!(linked_to_session(Some(5), 5, Some(&parents)));
        assert!(linked_to_session(Some(10), 5, Some(&parents)));
        // 같은 조상(1)을 공유해도 세션(5)의 자손이 아니면 끊는다.
        assert!(!linked_to_session(Some(20), 5, Some(&parents)));
        assert!(!linked_to_session(Some(999), 5, Some(&parents)));
        // 프로세스 표가 없으면 조용히 fail-open.
        assert!(linked_to_session(Some(20), 5, None));
    }

    /// AS-08: `ps` 출력 파서(헤더·깨진 행 무시).
    #[test]
    fn parse_ps_parents_skips_header_and_broken_rows() {
        let parsed = parse_ps_parents("  PID  PPID\n    1     0\n  424     1\nbroken\n  777\n");
        assert_eq!(parsed, BTreeMap::from([(1, 0), (424, 1)]));
    }

    /// AS-09: mtime이 그대로면 재스캔하지 않고, 새 디렉터리는 mtime이 바뀌어 발견된다.
    #[test]
    fn claude_worktrees_cached_by_mtime() -> io::Result<()> {
        let repo = std::env::temp_dir().join(format!("mk-claude-wt-{}", std::process::id()));
        std::fs::remove_dir_all(&repo).ok();
        let dir = repo.join(".claude/worktrees");
        std::fs::create_dir_all(&dir)?;
        let mut trees: BTreeMap<PathBuf, Vec<PathBuf>> =
            BTreeMap::from([(repo.clone(), Vec::new())]);
        include_claude_worktrees(&mut trees);
        assert!(trees[&repo].is_empty());
        // 새 워크트리를 만들면 다음 폴링에 보인다.
        std::fs::create_dir_all(dir.join("wt-1"))?;
        include_claude_worktrees(&mut trees);
        assert_eq!(trees[&repo].len(), 1, "{trees:?}");
        // mtime이 그대로면 캐시로 같은 결과가 나온다.
        include_claude_worktrees(&mut trees);
        assert_eq!(trees[&repo].len(), 1, "{trees:?}");
        std::fs::remove_dir_all(&repo).ok();
        Ok(())
    }

    #[test]
    fn checkpoint_badge_reads_persisted_fields_and_private_path_aliases() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "mk-sidebar-checkpoint-badge-{}",
            std::process::id()
        ));
        let sidebar = dir.join("sidebar.bin");
        let full = "/tmp/krill-review";
        let store = WorktreeStore {
            comments: BTreeMap::from([("/repo/comment-only".to_owned(), "Review me".to_owned())]),
            statuses: BTreeMap::from([
                (full.to_owned(), "in-review".to_owned()),
                ("/repo/todo".to_owned(), "todo".to_owned()),
                ("/repo/wip".to_owned(), "in-progress".to_owned()),
                ("/repo/done".to_owned(), "completed".to_owned()),
                ("/repo/invalid".to_owned(), "unknown".to_owned()),
                ("/repo/linked".to_owned(), "unknown".to_owned()),
            ]),
            unread: BTreeMap::from([
                (full.to_owned(), true),
                ("/repo/unread".to_owned(), true),
                ("/repo/read".to_owned(), false),
            ]),
            prs: BTreeMap::from([(full.to_owned(), 128)]),
            issues: BTreeMap::from([
                (full.to_owned(), "OWE-626".to_owned()),
                ("/repo/linked".to_owned(), "OWE-627".to_owned()),
            ]),
            references: BTreeMap::from([(
                full.to_owned(),
                vec![
                    "https://example.com/design".to_owned(),
                    "https://example.com/review".to_owned(),
                ],
            )]),
            ..WorktreeStore::default()
        };
        store.save(&dir)?;
        assert!(dir.join(COMMENTS_FILE).is_file());

        assert_eq!(
            checkpoint_badge_for(Some(&sidebar), Path::new(full)).as_deref(),
            Some("REVIEW #128 OWE-626 links 2")
        );
        assert_eq!(
            checkpoint_badge_for(Some(&sidebar), Path::new("/private/tmp/krill-review")).as_deref(),
            Some("REVIEW #128 OWE-626 links 2")
        );
        for (path, badge) in [
            ("/repo/todo", "TODO"),
            ("/repo/done", "DONE"),
            ("/repo/linked", "OWE-627"),
        ] {
            assert_eq!(
                checkpoint_badge_for(Some(&sidebar), Path::new(path)).as_deref(),
                Some(badge)
            );
        }
        for path in [
            "/repo/missing",
            "/repo/read",
            "/repo/comment-only",
            "/repo/invalid",
            "/repo/unread",
            "/repo/wip",
            "/tmp/krill-review/child",
        ] {
            assert_eq!(checkpoint_badge_for(Some(&sidebar), Path::new(path)), None);
        }
        assert_eq!(checkpoint_badge_for(None, Path::new(full)), None);
        assert_eq!(
            WorktreeStore::load_cached(&dir).statuses.get("/repo/wip"),
            Some(&"in-progress".to_owned()),
            "hiding the WIP decoration must preserve the saved workspace status"
        );
        assert_eq!(
            checkpoint_badge_for(Some(Path::new("sidebar.bin")), Path::new(full)),
            None
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn session_orch_badge_reads_latest_worker_and_coordinator_from_disk() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "mk-sidebar-orchestration-badge-{}",
            std::process::id()
        ));
        let sidebar = dir.join("sidebar.bin");
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Badge run", Some(1))?;
        store.create_run("Coordinator only", Some(2))?;
        store.start_worker(
            &run.id,
            None,
            "older-worker",
            "claude",
            Some(10),
            "/tmp",
            "First dispatch",
        )?;
        store.start_worker(
            &run.id,
            None,
            "latest-worker",
            "codex",
            Some(10),
            "/tmp",
            "Latest dispatch",
        )?;
        store.start_worker(
            &run.id,
            None,
            "coordinator-worker",
            "claude",
            Some(1),
            "/tmp",
            "Worker badge takes precedence",
        )?;
        store.save(&dir)?;
        assert!(dir.join(krill_proto::orchestration::ORCH_FILE).is_file());

        assert_eq!(
            session_orch_badge(Some(&sidebar), 10).as_deref(),
            Some("latest-worker")
        );
        assert_eq!(
            session_orch_badge(Some(&sidebar), 1).as_deref(),
            Some("coordinator-worker")
        );
        assert_eq!(
            session_orch_badge(Some(&sidebar), 2).as_deref(),
            Some(crate::i18n::catalog::sidebar::COORD_BADGE.get()),
            "지휘자 배지도 카탈로그 키로 보인다(AS-26)"
        );
        assert_eq!(session_orch_badge(Some(&sidebar), 999), None);
        assert_eq!(session_orch_badge(None, 10), None);
        assert_eq!(session_orch_badge(Some(Path::new("sidebar.bin")), 10), None);
        assert_eq!(
            session_orch_badge(Some(&dir.join("missing/sidebar.bin")), 10),
            None
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}

#[cfg(test)]
#[test]
fn as12_sidebar_comment_save_preserves_future_fields() {
    let dir = std::env::temp_dir().join(format!("as12-sidebar-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(COMMENTS_FILE);
    fs::write(
        &path,
        "# keep\nfuture = { mode = 'v2' }\n[comments]\n'/work' = 'old'\n",
    )
    .unwrap();
    save_comment(&dir, Path::new("/work"), "edited").unwrap();
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("# keep"));
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert_eq!(value["future"]["mode"].as_str(), Some("v2"));
    assert_eq!(value["comments"]["/work"].as_str(), Some("edited"));
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(test)]
#[test]
fn owe767_imported_prefs_do_not_clobber_unpin_or_cleared_name() {
    let dir = std::env::temp_dir().join(format!("owe767-sidebar-prefs-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(PROJECTS_FILE),
        r#"
[[projects]]
id = "repo-1"
path = "/Users/test/repo"
display_name = "Imported Repo"
kind = "git"
pinned = true

[worktrees."/Users/test/repo-wt-1"]
repo_id = "repo-1"
display_name = "Imported Worktree"
pinned = true

[lineage."/Users/test/repo-wt-1"]
repo_id = "repo-1"
worktree_path = "/Users/test/repo-wt-1"
branch = "feat/owe-767"
parent_worktree_path = "/Users/test/repo"
"#,
    )
    .unwrap();

    let repo = PathBuf::from("/Users/test/repo");
    let wt = PathBuf::from("/Users/test/repo-wt-1");
    let mut prefs = super::Preferences::default();
    let mut comments = BTreeMap::new();
    let mut issues = BTreeMap::new();
    let mut statuses = BTreeMap::new();

    // First sync seeds vacant prefs
    apply_imported_project_prefs(&dir, &mut prefs, &mut comments, &mut issues, &mut statuses);
    assert_eq!(
        prefs.get(&repo),
        Some(&super::Preference {
            pinned: true,
            name: "Imported Repo".to_owned(),
        })
    );
    assert_eq!(
        prefs.get(&wt),
        Some(&super::Preference {
            pinned: true,
            name: "Imported Worktree".to_owned(),
        })
    );

    // User unpins and clears custom names (as Sidebar::toggle_pin and Sidebar::finish_rename do),
    prefs.get_mut(&repo).unwrap().pinned = false;
    prefs.get_mut(&repo).unwrap().name.clear();
    prefs.get_mut(&wt).unwrap().pinned = false;
    prefs.get_mut(&wt).unwrap().name.clear();

    // Subsequent polling sync must NOT overwrite the occupied prefs
    apply_imported_project_prefs(&dir, &mut prefs, &mut comments, &mut issues, &mut statuses);
    assert_eq!(
        prefs.get(&repo),
        Some(&super::Preference {
            pinned: false,
            name: String::new(),
        })
    );
    assert_eq!(
        prefs.get(&wt),
        Some(&super::Preference {
            pinned: false,
            name: String::new(),
        })
    );

    fs::remove_dir_all(dir).unwrap();
}

/// OWE-803: 과거 이관분(`[[projects]]` 원본 경로 vs `[worktrees]` 정규 경로)이 섞여도
/// 사이드바 트리는 하나의 저장소로 합쳐진다.
#[cfg(unix)]
#[test]
fn owe803_symlink_alias_imports_merge_into_single_repo() {
    let root = std::env::temp_dir().join(format!("owe803-sidebar-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let real = root.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let canon = fs::canonicalize(&real).unwrap();
    fs::write(
        root.join(PROJECTS_FILE),
        format!(
            "[[projects]]\nid = \"r1\"\npath = \"{}\"\ndisplay_name = \"Link Repo\"\nkind = \"git\"\n\n\
             [worktrees.\"{}\"]\nrepo_id = \"r1\"\ndisplay_name = \"Link Repo\"\n",
            link.display(),
            canon.display(),
        ),
    )
    .unwrap();

    let mut trees: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut meta: super::Meta = BTreeMap::new();
    include_imported_projects(&root, &mut trees, &mut meta);
    assert_eq!(trees.len(), 1, "{trees:?}");
    assert_eq!(trees.keys().next(), Some(&canon));
    assert_eq!(trees.get(&canon), Some(&vec![canon.clone()]));

    let mut prefs = super::Preferences::default();
    let mut comments = BTreeMap::new();
    let mut issues = BTreeMap::new();
    let mut statuses = BTreeMap::new();
    apply_imported_project_prefs(&root, &mut prefs, &mut comments, &mut issues, &mut statuses);
    assert!(prefs.contains_key(&canon));

    fs::remove_dir_all(root).unwrap();
}

/// 설 실기(v0.1.8): ⌘N으로 만든 linked 워크트리가 독립 프로젝트로 등록돼 "저장소 wt1" 그룹이 하나 더 생겼다.
/// 등록은 주 워크트리로, 이미 잘못 등록된 항목은 주 저장소 그룹 안으로 들어가야 한다.
#[cfg(test)]
#[test]
fn linked_worktree_registers_and_groups_under_main_repo() {
    let base = std::env::temp_dir().join(format!("mk-linked-wt-{}", std::process::id()));
    fs::remove_dir_all(&base).ok();
    let (state, demo, wt) = (base.join("state"), base.join("demo"), base.join("wt1"));
    let gitdir = demo.join(".git/worktrees/wt1");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&gitdir).unwrap();
    fs::create_dir_all(&wt).unwrap();
    fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    let canon = |p: &Path| fs::canonicalize(p).unwrap();

    assert_eq!(main_worktree(&wt), Some(canon(&demo)));
    assert_eq!(main_worktree(&demo), None);
    assert!(register_project(&state, &wt).unwrap());
    assert!(
        !register_project(&state, &demo).unwrap(),
        "same repo as the worktree"
    );
    let body = fs::read_to_string(state.join(PROJECTS_FILE)).unwrap();
    let value: toml::Value = toml::from_str(&body).unwrap();
    let projects = value["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        projects[0]["path"].as_str().unwrap(),
        canon(&demo).to_string_lossy()
    );

    // v0.1.8이 남긴 잘못된 항목(linked 워크트리 경로)도 주 저장소 그룹으로 합친다.
    fs::write(
        state.join(PROJECTS_FILE),
        format!("[[projects]]\npath = \"{}\"\n", canon(&wt).display()),
    )
    .unwrap();
    let mut trees = BTreeMap::new();
    let mut meta = super::Meta::default();
    include_imported_projects(&state, &mut trees, &mut meta);
    assert_eq!(trees.keys().collect::<Vec<_>>(), vec![&canon(&demo)]);
    assert_eq!(trees[&canon(&demo)], vec![canon(&wt)]);
    fs::remove_dir_all(&base).ok();
}

#[cfg(test)]
#[test]
fn owe852_register_project_persists_and_dedupes() {
    let dir = std::env::temp_dir().join(format!("owe852-register-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let repo = std::env::temp_dir().join(format!("owe852-repo-{}", std::process::id()));
    fs::remove_dir_all(&repo).ok();
    fs::create_dir_all(repo.join(".git")).unwrap();

    assert!(register_project(&dir, &repo).unwrap());
    assert!(
        !register_project(&dir, &repo).unwrap(),
        "duplicate registers nothing"
    );
    let body = fs::read_to_string(dir.join(PROJECTS_FILE)).unwrap();
    let value: toml::Value = toml::from_str(&body).unwrap();
    let projects = value["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    let expected = fs::canonicalize(&repo)
        .unwrap_or_else(|_| repo.clone())
        .to_string_lossy()
        .trim()
        .to_owned();
    assert_eq!(projects[0]["path"].as_str().unwrap(), expected);
    assert_eq!(projects[0]["kind"].as_str(), Some("git"));

    let other = repo.join("sibling");
    fs::create_dir_all(&other).unwrap();
    assert!(register_project(&dir, &other).unwrap());
    let body = fs::read_to_string(dir.join(PROJECTS_FILE)).unwrap();
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert_eq!(value["projects"].as_array().unwrap().len(), 2);

    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&repo).ok();
}

#[cfg(test)]
#[test]
fn orca_project_icon_detection_and_visual_meta_resolution() {
    use crate::ui::icons::ProjectIcon;

    let base = std::env::temp_dir().join(format!("mk-proj-icons-{}", std::process::id()));
    fs::remove_dir_all(&base).ok();
    let state_dir = base.join("state");
    let repo_local_icon = base.join("aasoft-client");
    let repo_github = base.join("krill");
    let folder_drive = base.join("gdrive");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(repo_local_icon.join("public")).unwrap();
    fs::create_dir_all(repo_github.join(".git")).unwrap();
    fs::create_dir_all(&folder_drive).unwrap();

    // 1. Local file icon (`public/logo.png` with valid PNG header)
    let png_bytes: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let logo_path = repo_local_icon.join("public/logo.png");
    fs::write(&logo_path, png_bytes).unwrap();
    assert_eq!(
        detect_local_file_icon(&repo_local_icon),
        Some(logo_path.clone())
    );

    // 2. GitHub avatar URL from `.git/config` (`upstream` preferred over `origin`)
    fs::write(
        repo_github.join(".git/config"),
        r#"[remote "origin"]
	url = git@github.com:craftowen/krill.git
"#,
    )
    .unwrap();
    let gh_url = detect_github_avatar_url(&repo_github).unwrap();
    assert_eq!(gh_url, "https://github.com/craftowen.png?size=64");

    // Pre-seed cached GitHub avatar on disk so `resolve_project_visuals` resolves immediately without network
    let cached_gh = cached_icon_path_for_url(&state_dir.join(REPO_ICON_DIR), &gh_url);
    fs::create_dir_all(cached_gh.parent().unwrap()).unwrap();
    fs::write(&cached_gh, png_bytes).unwrap();

    // 3. `package.json` homepage favicon URL (skips github.com, accepts custom domain)
    fs::write(
        repo_local_icon.join("package.json"),
        r#"{"homepage": "https://aasoft.kr/docs"}"#,
    )
    .unwrap();
    assert_eq!(
        detect_package_homepage_favicon_url(&repo_local_icon).as_deref(),
        Some("https://www.google.com/s2/favicons?domain=aasoft.kr&sz=64")
    );

    // 4. `projects.toml` with badge_color, display_name, and explicit repo_icon
    fs::write(
        state_dir.join(PROJECTS_FILE),
        format!(
            r##"[[projects]]
id = "p-local"
path = "{}"
display_name = "AAsoft Client"
badge_color = "#3b82f6"
kind = "git"

[[projects]]
id = "p-gh"
path = "{}"
display_name = "krill"
kind = "git"

[[projects]]
id = "p-folder"
path = "{}"
display_name = "구글드라이브"
badge_color = "#ef4444"
kind = "folder"
"##,
            repo_local_icon.display(),
            repo_github.display(),
            folder_drive.display(),
        ),
    )
    .unwrap();

    let visuals = resolve_project_visuals(
        Some(&state_dir),
        &[repo_local_icon.clone(), repo_github.clone()],
        std::slice::from_ref(&folder_drive),
    );

    let v_local = visuals.get(&repo_local_icon).unwrap();
    assert_eq!(v_local.display_name.as_deref(), Some("AAsoft Client"));
    assert_eq!(v_local.badge_color.as_deref(), Some("#3b82f6"));
    assert_eq!(v_local.icon, Some(ProjectIcon::Image(logo_path)));

    let v_gh = visuals.get(&repo_github).unwrap();
    assert_eq!(v_gh.display_name.as_deref(), Some("krill"));
    assert_eq!(v_gh.icon, Some(ProjectIcon::Image(cached_gh)));

    let v_folder = visuals.get(&folder_drive).unwrap();
    assert_eq!(v_folder.display_name.as_deref(), Some("구글드라이브"));
    assert_eq!(v_folder.badge_color.as_deref(), Some("#ef4444"));
    assert_eq!(v_folder.icon, None);

    fs::remove_dir_all(&base).ok();
}

/// Orca처럼 같은 저장소의 연결 워크트리를 본 워크트리 자식으로 묶는다.
/// 2개 미만은 평탄 유지, 명시적 부모는 유지, 사이클은 만들지 않는다.
#[test]
fn infer_repo_main_lineage_groups_linked_worktrees() {
    let repo = PathBuf::from("/repo");
    let trees: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::from([(
        repo.clone(),
        vec![
            repo.clone(),
            PathBuf::from("/repo/wt-1"),
            PathBuf::from("/repo/wt-2"),
        ],
    )]);
    let inferred = infer_repo_main_lineage(&trees, &BTreeMap::new());
    assert_eq!(
        inferred,
        vec![
            (PathBuf::from("/repo/wt-1"), repo.clone()),
            (PathBuf::from("/repo/wt-2"), repo.clone()),
        ]
    );
    // 2개 미만(본체+1)은 Orca처럼 평탄하게 둔다.
    let small: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::from([(
        repo.clone(),
        vec![repo.clone(), PathBuf::from("/repo/wt-1")],
    )]);
    assert!(infer_repo_main_lineage(&small, &BTreeMap::new()).is_empty());
    // 명시적 부모가 있으면 건드리지 않는다.
    let explicit = BTreeMap::from([(PathBuf::from("/repo/wt-1"), PathBuf::from("/other"))]);
    let inferred = infer_repo_main_lineage(&trees, &explicit);
    assert_eq!(inferred, vec![(PathBuf::from("/repo/wt-2"), repo.clone())]);
}
