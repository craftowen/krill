//! 워크스페이스 디스크 용량 분석(`Space`) 및 워크트리 일괄 정리(`Cleanup`) 페이지(MK-175, OWE-729).
//!
//! - 설정의 디스크 용량 페이지는 워크트리별 용량(`Space`)과 정리 후보(`Cleanup`)를 경로로 합친 한 표다.
//!   행마다 지울 수 있는지와 그 이유를 쓰고, 경로와 빌드 아티팩트는 행을 눌러 펼친다.
//! - PR 상태(`Merged` / `Closed`), 마지막 활동 경과 일수(`7일` / `14일` / `30일` 이상),
//!   `dirty`(미커밋 변경) 제외 안전 필터로 고를 수 있는 행을 정한다. 선택은 비운 채 시작하고,
//!   삭제는 본문 아래 띠의 버튼 하나에서 2단계 확인으로만 한다.
//!
//! 스캔·삭제 엔진은 앱에 두지 않는다(OWE-744). 스테이징된 `krill` CLI를 하위 프로세스로 실행해
//! `--json` 표준출력(`krill_proto::worktree_space`에 고정된 스키마)을 파싱·표시만 한다.
//! - 용량: `krill worktree space [--repo <r>]... --json` → `SpaceReport`
//! - 전체 후보: `krill worktree cleanup [--repo <r>]... --pr any --idle-days 0 --include-dirty --dry-run --json`
//!   → `CleanupReport.candidates`(필터는 앱이 `filter_cleanup_candidates`로 적용),
//!   `summary.items`(실행 중인 세션 때문에 건너뛸 워크트리)
//! - 실행: `krill worktree cleanup [--repo <r>]... --pr <p> --idle-days <n> --exclude-dirty|--include-dirty
//!   --worktree <path>... --yes --json` → `CleanupReport.summary`

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::checkbox::Checkbox;
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::worktree_space::{
    CleanupFilter, CleanupPrFilter, CleanupPrState, CleanupReport, CleanupSummary, IdleDaysPreset,
    SpaceReport, WorktreeCleanupCandidate, WorktreeSpaceEntry, filter_cleanup_candidates,
    format_bytes,
};

use super::{SettingsView, layout, state_dir};
use crate::i18n::catalog::settings as text;
use crate::ui::BadgeTone;

/// `krill` CLI 실행기: `krill` 뒤 인자를 받아 표준출력 바이트를 돌려준다. 테스트는 가짜 JSON을 주입한다.
#[derive(Clone)]
pub struct CliRunner(Arc<RunFn>);

type RunFn = dyn Fn(&[OsString]) -> io::Result<Vec<u8>> + Send + Sync;

/// 함께 설치된 `krill` CLI: 번들·설치 경로의 형제 실행 파일, 테스트 실행 파일이면 한 단계 위
/// (`target/debug/deps` → `target/debug`), 그다음 `CARGO_TARGET_DIR/{debug,release}`.
#[must_use]
pub fn krill_bin() -> Option<std::path::PathBuf> {
    if let Ok(sib) = krill_proto::daemon::sibling_exe("krill") {
        if sib.is_file() {
            return Some(sib);
        }
        if let Some(parent_bin) = sib
            .parent()
            .and_then(std::path::Path::parent)
            .map(|p| p.join("krill"))
            .filter(|p| p.is_file())
        {
            return Some(parent_bin);
        }
    }
    let target_dir = std::path::PathBuf::from(std::env::var_os("CARGO_TARGET_DIR")?);
    ["debug", "release"]
        .into_iter()
        .map(|profile| target_dir.join(profile).join("krill"))
        .find(|candidate| candidate.is_file())
}

impl CliRunner {
    pub fn new(f: impl Fn(&[OsString]) -> io::Result<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[OsString]) -> io::Result<T> {
        let out = self.raw(args)?;
        serde_json::from_slice(&out).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// 표준출력 바이트 그대로(비0 종료라도 stdout이 있으면 그것). JSON 오류 봉투를 직접 읽을 때 쓴다.
    pub(crate) fn raw(&self, args: &[OsString]) -> io::Result<Vec<u8>> {
        (self.0)(args)
    }

    /// 스테이징된 `krill` 바이너리를 하위 프로세스로 실행한다.
    pub(crate) fn process(bin: PathBuf, state_dir: Option<PathBuf>) -> Self {
        Self::new(move |args| {
            let mut cmd = crate::platform::background_command(&bin);
            cmd.args(args)
                // CLI는 `--repo`가 없으면 cwd 저장소를 먼저 고르므로, 저장소가 아닌 `/`에서 실행해 전체 탐색을 강제한다.
                .current_dir("/")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(dir) = &state_dir {
                cmd.env("KRILL_STATE_DIR", dir);
            }
            let output = super::child::output(
                &mut cmd,
                std::time::Duration::from_secs(120),
                4 * 1024 * 1024,
            )?;
            // `cleanup`은 일부 삭제 실패 시 비0으로 끝나도 JSON 요약을 출력한다.
            if !output.stdout.is_empty() {
                return Ok(output.stdout);
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(io::Error::other(if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }))
        })
    }
}

impl fmt::Debug for CliRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CliRunner")
    }
}

fn repo_args(repos: Option<&[PathBuf]>) -> Vec<OsString> {
    repos
        .unwrap_or_default()
        .iter()
        .flat_map(|r| [OsString::from("--repo"), r.clone().into_os_string()])
        .collect()
}

/// 용량 보고서와 필터 전 전체 클린업 미리보기를 CLI에서 가져온다.
fn fetch_space(
    run: &CliRunner,
    repos: Option<&[PathBuf]>,
) -> io::Result<(SpaceReport, CleanupReport)> {
    let repo = repo_args(repos);
    let mut space: Vec<OsString> = vec!["worktree".into(), "space".into()];
    space.extend(repo.iter().cloned());
    space.push("--json".into());
    let report: SpaceReport = run.json(&space)?;

    let mut all: Vec<OsString> = vec!["worktree".into(), "cleanup".into()];
    all.extend(repo);
    all.extend(
        [
            "--pr",
            "any",
            "--idle-days",
            "0",
            "--include-dirty",
            "--dry-run",
            "--json",
        ]
        .map(Into::into),
    );
    Ok((report, run.json::<CleanupReport>(&all)?))
}

/// 선택된 워크트리만 CLI로 삭제한다(CLI가 같은 필터로 후보를 다시 검사한다).
fn run_cleanup(
    run: &CliRunner,
    repos: Option<&[PathBuf]>,
    filter: &CleanupFilter,
    selected: &[PathBuf],
) -> io::Result<CleanupSummary> {
    let mut args: Vec<OsString> = vec!["worktree".into(), "cleanup".into()];
    args.extend(repo_args(repos));
    args.extend(["--pr", filter.pr_filter.as_str(), "--idle-days"].map(Into::into));
    args.push(filter.min_idle_days.to_string().into());
    args.push(
        if filter.exclude_dirty {
            "--exclude-dirty"
        } else {
            "--include-dirty"
        }
        .into(),
    );
    for path in selected {
        args.push("--worktree".into());
        args.push(path.clone().into_os_string());
    }
    args.extend(["--yes", "--json"].map(Into::into));
    Ok(run.json::<CleanupReport>(&args)?.summary)
}

/// 표 한 행. 스캔과 정리 후보 어느 한쪽에만 있을 수 있다.
struct DiskRow<'a> {
    name: &'a str,
    path: &'a Path,
    branch: Option<&'a str>,
    bytes: u64,
    is_main: bool,
    space: Option<&'a WorktreeSpaceEntry>,
    candidate: Option<&'a WorktreeCleanupCandidate>,
}

/// 행을 지울 수 있는지와 지울 수 없는 이유.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Main,
    Locked,
    Busy,
    Dirty,
    OutOfFilter,
    Removable,
}

impl RowKind {
    /// 조건을 바꿔도 지울 수 없게 보호 중인 행인지. 안전 필터는 끌 수 있지만 켜져 있는 동안은 보호다.
    fn is_protected(self) -> bool {
        matches!(self, Self::Main | Self::Locked | Self::Busy | Self::Dirty)
    }

    fn icon(self) -> IconName {
        match self {
            Self::OutOfFilter => IconName::Clock,
            Self::Dirty => IconName::TriangleAlert,
            Self::Main | Self::Locked | Self::Busy | Self::Removable => IconName::Lock,
        }
    }

    fn color(self, p: &crate::ui::Palette) -> gpui::Hsla {
        match self {
            Self::Main | Self::Locked | Self::Busy => p.preserve,
            Self::Dirty => p.warning,
            Self::OutOfFilter | Self::Removable => p.muted_foreground,
        }
    }

    fn reason(self, row: &DiskRow<'_>) -> Option<String> {
        Some(match self {
            Self::Main => text::CLEANUP_REASON_MAIN.get().to_owned(),
            Self::Locked => text::CLEANUP_REASON_LOCKED.get().to_owned(),
            Self::Busy => text::CLEANUP_REASON_SESSION.get().to_owned(),
            Self::Dirty => {
                let n = row.candidate.map_or(0, |c| c.dirty_count);
                text::CLEANUP_REASON_DIRTY.format(&[("n", &n.to_string())])
            }
            Self::OutOfFilter => text::CLEANUP_REASON_FILTER.get().to_owned(),
            Self::Removable => return None,
        })
    }
}

/// `SettingsView`가 소유하는 디스크 용량 분석(`Space`) 및 워크트리 클린업(`Cleanup`) 상태.
#[expect(
    clippy::struct_excessive_bools,
    reason = "서로 독립인 화면 상태(스캔, 정리 진행, 접기 두 곳)"
)]
#[derive(Debug, Default)]
pub struct Panel {
    generation: u64,
    /// 지정 저장소 오버라이드(`--repo`로 전달). `None`이면 CLI가 상태 디렉터리·데몬에서 자동 탐색한다.
    pub repos_override: Option<Vec<PathBuf>>,
    /// 테스트용 CLI 실행기 오버라이드. `None`이면 스테이징된 `krill` 바이너리를 실행한다.
    pub runner_override: Option<CliRunner>,
    /// 마지막 백그라운드 디스크 용량 스캔 결과.
    pub report: Option<SpaceReport>,
    /// 백그라운드 용량 스캔 진행 중 여부.
    pub scanning: bool,
    /// 스캔이 1초를 넘겨 진행 문구를 보일 때(§7.3).
    pub scan_slow: bool,
    scan_grace: Option<gpui::Task<()>>,
    /// 펼친 표 행의 경로.
    pub expanded: BTreeSet<PathBuf>,
    /// 좁은 창에서 조건을 펼쳤는지.
    pub filters_open: bool,
    /// 보호 규칙을 펼쳤는지.
    pub rules_open: bool,
    /// 발견된 전체 링크된 워크트리 클린업 후보 목록.
    pub candidates: Vec<WorktreeCleanupCandidate>,
    /// 실행 중인 세션이 안에 있어 CLI가 건너뛸 워크트리. 고를 수 없게 표시한다.
    pub session_busy: BTreeSet<PathBuf>,
    /// 현재 클린업 필터 조건(PR 상태, 경과 일수, dirty 제외 안전 필터).
    pub filter: CleanupFilter,
    /// 표에서 체크한 워크트리 경로 집합.
    pub selected: BTreeSet<PathBuf>,
    /// 일괄 삭제 백그라운드 작업 진행 중 여부.
    pub cleaning: bool,
    /// 직전 일괄 클린업 실행 요약.
    pub last_summary: Option<CleanupSummary>,
    /// 오류 메시지.
    pub error: Option<SharedString>,
}

impl Panel {
    /// 현재 필터 조건과 일치하는 후보 목록을 돌려준다.
    #[must_use]
    pub fn filtered_candidates(&self) -> Vec<WorktreeCleanupCandidate> {
        filter_cleanup_candidates(&self.candidates, &self.filter)
    }

    /// 필터에 일치하면서 세션 때문에 건너뛰지 않는, 실제로 지울 수 있는 후보.
    #[must_use]
    pub fn selectable_candidates(&self) -> Vec<WorktreeCleanupCandidate> {
        let mut matched = self.filtered_candidates();
        matched.retain(|c| !self.session_busy.contains(&c.path));
        matched
    }

    /// 나머지 조건은 맞지만 안전 필터 때문에 빠진 후보. 빠진 이유를 표시한다.
    #[must_use]
    pub fn safety_excluded_candidates(&self) -> Vec<WorktreeCleanupCandidate> {
        let relaxed = CleanupFilter {
            exclude_dirty: false,
            ..self.filter.clone()
        };
        self.candidates
            .iter()
            .filter(|c| c.matches_filter(&relaxed) && !c.matches_filter(&self.filter))
            .cloned()
            .collect()
    }

    /// 용량 보고서와 정리 후보를 경로로 합친 표의 행. 큰 것부터.
    fn rows(&self) -> Vec<DiskRow<'_>> {
        let by_path = |path: &Path| self.candidates.iter().find(|c| c.path == path);
        let spaces = self.report.iter().flat_map(|report| &report.worktrees);
        let mut rows: Vec<DiskRow<'_>> = spaces
            .map(|space| DiskRow {
                name: &space.name,
                path: &space.path,
                branch: space.branch.as_deref(),
                bytes: space.total_bytes,
                is_main: space.is_main,
                space: Some(space),
                candidate: by_path(&space.path),
            })
            .collect();
        let known: BTreeSet<&Path> = rows.iter().map(|row| row.path).collect();
        let only_candidates = self
            .candidates
            .iter()
            .filter(|c| !known.contains(c.path.as_path()));
        rows.extend(only_candidates.map(|c| DiskRow {
            name: &c.name,
            path: &c.path,
            branch: c.branch.as_deref(),
            bytes: c.disk_bytes,
            is_main: c.is_main,
            space: None,
            candidate: Some(c),
        }));
        rows.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(b.path)));
        rows
    }

    /// [`Self::selectable_candidates`]와 같은 판단을 행 하나에 대해 이유까지 나눈다.
    fn row_kind(&self, row: &DiskRow<'_>) -> RowKind {
        if row.is_main {
            return RowKind::Main;
        }
        // 미리보기는 기본 워크트리와 잠긴 워크트리를 뺀 후보만 돌려준다.
        let Some(candidate) = row.candidate.filter(|c| !c.locked) else {
            return RowKind::Locked;
        };
        if self.session_busy.contains(row.path) {
            RowKind::Busy
        } else if self.filter.exclude_dirty && candidate.dirty {
            RowKind::Dirty
        } else if candidate.matches_filter(&self.filter) {
            RowKind::Removable
        } else {
            RowKind::OutOfFilter
        }
    }

    /// 지울 수 없게 된 경로만 선택에서 뺀다. 새로 맞는 후보를 자동으로 고르지 않는다.
    fn prune_selection(&mut self) {
        let selectable: BTreeSet<PathBuf> = self
            .selectable_candidates()
            .into_iter()
            .map(|c| c.path)
            .collect();
        self.selected.retain(|path| selectable.contains(path));
    }
}

impl SettingsView {
    /// 테스트 또는 외부 호출에서 스캔 대상 저장소와 CLI 실행기를 지정하고 백그라운드 스캔을 시작한다.
    pub fn set_space_repos(
        &mut self,
        repos: Vec<PathBuf>,
        runner: Option<CliRunner>,
        cx: &mut Context<Self>,
    ) {
        self.space.generation = self.space.generation.wrapping_add(1);
        self.space.scanning = false;
        self.space.repos_override = Some(repos);
        self.space.runner_override = runner;
        self.scan_workspace_space(cx);
    }

    /// 오버라이드 또는 스테이징된 `krill` CLI 실행기. 바이너리가 없으면 오류 메시지를 남기고 `None`.
    fn space_runner(&mut self, cx: &mut Context<Self>) -> Option<CliRunner> {
        if let Some(run) = self.space.runner_override.clone() {
            return Some(run);
        }
        if let Some(bin) = krill_bin() {
            return Some(CliRunner::process(bin, state_dir(cx)));
        }
        self.space.error = Some(
            text::SPACE_ERROR
                .format(&[(
                    "error",
                    crate::i18n::catalog::general::ERR_CLI_MISSING.get(),
                )])
                .into(),
        );
        cx.notify();
        None
    }

    /// 백그라운드 실행기에서 비차단으로 워크트리 디스크 용량과 클린업 후보를 스캔한다.
    pub fn scan_workspace_space(&mut self, cx: &mut Context<Self>) {
        if self.space.scanning || self.space.cleaning {
            return;
        }
        self.space.error = None;
        let Some(run) = self.space_runner(cx) else {
            return;
        };
        self.space.scanning = true;
        self.space.scan_slow = false;
        self.space.scan_grace = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(layout::SLOW_FEEDBACK).await;
            this.update(cx, |this, cx| {
                this.space.scan_slow = true;
                cx.notify();
            })
            .ok();
        }));
        let repos = self.space.repos_override.clone();
        let generation = self.space.generation;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { fetch_space(&run, repos.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                if this.space.generation != generation {
                    return;
                }
                this.space.scanning = false;
                this.space.scan_slow = false;
                this.space.scan_grace = None;
                this.apply_space_result(res);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_space_result(&mut self, res: io::Result<(SpaceReport, CleanupReport)>) {
        match res {
            Ok((report, preview)) => {
                self.space.report = Some(report);
                self.space.candidates = preview.candidates;
                self.space.session_busy =
                    preview.summary.items.into_iter().map(|i| i.path).collect();
                self.space.prune_selection();
            }
            Err(e) => {
                self.space.error = Some(
                    text::SPACE_ERROR
                        .format(&[("error", &e.to_string())])
                        .into(),
                );
            }
        }
    }

    /// 디스크 페이지에 들어올 때. 선택과 지난 결과는 비운 채 시작하고, 정리 중이면 그대로 둔다.
    pub(super) fn enter_disk_page(&mut self, cx: &mut Context<Self>) {
        if !self.space.cleaning {
            self.space.selected.clear();
            self.space.last_summary = None;
        }
        if self.space.report.is_none() {
            self.scan_workspace_space(cx);
        }
    }

    /// PR 상태 필터를 변경한다.
    pub fn set_cleanup_pr_filter(&mut self, pr_filter: CleanupPrFilter, cx: &mut Context<Self>) {
        if self.space.cleaning {
            return;
        }
        self.space.filter.pr_filter = pr_filter;
        self.space.prune_selection();
        cx.notify();
    }

    /// 마지막 활동 경과 일수 필터를 변경한다.
    pub fn set_cleanup_idle_days(&mut self, days: u64, cx: &mut Context<Self>) {
        if self.space.cleaning {
            return;
        }
        self.space.filter.min_idle_days = days;
        self.space.prune_selection();
        cx.notify();
    }

    /// 미커밋(`dirty`) 워크트리 제외 안전 필터를 변경한다.
    pub fn set_cleanup_exclude_dirty(&mut self, exclude_dirty: bool, cx: &mut Context<Self>) {
        if self.space.cleaning {
            return;
        }
        self.space.filter.exclude_dirty = exclude_dirty;
        self.space.prune_selection();
        cx.notify();
    }

    /// 개별 후보 워크트리의 선택 여부를 토글한다.
    pub fn toggle_cleanup_candidate(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.space.cleaning {
            return;
        }
        if !self.space.selected.remove(path) {
            self.space.selected.insert(path.to_path_buf());
        }
        cx.notify();
    }

    /// 지울 수 있는 모든 후보를 전체 선택하거나 선택 해제한다.
    pub fn toggle_select_all_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.space.cleaning {
            return;
        }
        let selectable = self.space.selectable_candidates();
        let all_selected = !selectable.is_empty()
            && selectable
                .iter()
                .all(|c| self.space.selected.contains(&c.path));
        if all_selected {
            self.space.selected.clear();
        } else {
            self.space.selected = selectable.into_iter().map(|c| c.path).collect();
        }
        cx.notify();
    }

    /// 선택된 워크트리들을 백그라운드에서 일괄 삭제하고 용량·후보 목록을 갱신한다.
    pub fn execute_selected_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.space.cleaning {
            return;
        }
        let selected: Vec<PathBuf> = self
            .space
            .selectable_candidates()
            .into_iter()
            .map(|c| c.path)
            .filter(|p| self.space.selected.contains(p))
            .collect();
        if selected.is_empty() {
            return;
        }
        self.space.error = None;
        let Some(run) = self.space_runner(cx) else {
            return;
        };
        self.space.cleaning = true;
        self.space.last_summary = None;
        let filter = self.space.filter.clone();
        let repos = self.space.repos_override.clone();
        let generation = self.space.generation;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let (summary, rescan) = cx
                .background_executor()
                .spawn(async move {
                    let summary = run_cleanup(&run, repos.as_deref(), &filter, &selected);
                    (summary, fetch_space(&run, repos.as_deref()))
                })
                .await;
            this.update(cx, |this, cx| {
                this.space.cleaning = false;
                if this.space.generation != generation {
                    return;
                }
                match summary {
                    Ok(summary) => {
                        this.space.last_summary = Some(summary);
                        // 다시 고를 때까지 비워 둔다. 실패한 행도 저절로 다시 고르지 않는다.
                        this.space.selected.clear();
                    }
                    Err(e) => {
                        this.space.error = Some(
                            text::SPACE_ERROR
                                .format(&[("error", &e.to_string())])
                                .into(),
                        );
                    }
                }
                if this.space.error.is_none() {
                    this.apply_space_result(rescan);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 디스크 페이지 본문: 요약, 조건, 한 표, 보호 규칙, 결과. 삭제는 [`Self::render_disk_footer`] 한 곳에서만 한다.
    pub(super) fn render_space_section(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let small = layout::small_body(window);
        let scan_btn = crate::ui::button_with_reserved_label(
            "admin-space-scan",
            text::SPACE_SCAN.get(),
            text::SPACE_SCANNING.get(),
            self.space.scan_slow,
        )
        .small()
        .outline()
        .icon(IconName::RefreshCw)
        .disabled(self.space.scanning || self.space.cleaning)
        .debug_selector(|| "admin-space-scan".into())
        .on_click(cx.listener(|this, _, _, cx| {
            this.scan_workspace_space(cx);
        }));

        let summary = self.space.report.as_ref().map(|report| {
            text::SPACE_TOTAL_SUMMARY.format(&[
                ("total", &format_bytes(report.total_bytes)),
                ("count", &report.worktrees.len().to_string()),
                ("artifacts", &format_bytes(report.total_artifact_bytes)),
            ])
        });
        let header_row = div()
            .id("admin-space-header-row")
            .debug_selector(|| "admin-space-header-row".into())
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .id("admin-space-summary")
                    .debug_selector(|| "admin-space-summary".into())
                    .flex_1()
                    .min_w_0()
                    .flex_basis(gpui::relative(0.5))
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .children(summary),
            )
            .child(scan_btn);

        let mut section = div()
            .id("admin-space-section")
            .debug_selector(|| "admin-space-section".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(header_row);
        if let Some(err) = &self.space.error {
            section = section.child(
                crate::ui::error_banner(p, err.clone())
                    .id("admin-space-error")
                    .debug_selector(|| "admin-space-error".into()),
            );
        }

        let empty_card = || {
            layout::card(p)
                .id("admin-space-empty")
                .debug_selector(|| "admin-space-empty".into())
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(text::SPACE_EMPTY.get())
        };
        let Some(report) = &self.space.report else {
            // 첫 스캔은 문구로 덮지 않고 최종 모양의 빈 행을 보인다(§7.0).
            return if self.space.scanning {
                section.child(Self::render_space_blanks(p))
            } else {
                section.child(empty_card())
            };
        };

        section = section.child(Self::render_space_chips(p, report));
        let rows = self.space.rows();
        if rows.is_empty() {
            return section.child(empty_card());
        }

        section = section.child(self.render_cleanup_filters(p, small, cx));
        if !self.space.filter.exclude_dirty {
            section = section.child(
                crate::ui::error_banner(p, text::CLEANUP_SAFETY_OFF_WARNING.get())
                    .id("admin-cleanup-safety-off")
                    .debug_selector(|| "admin-cleanup-safety-off".into()),
            );
        }
        if self.space.filtered_candidates().is_empty() {
            section = section.child(
                div()
                    .id("admin-cleanup-empty")
                    .debug_selector(|| "admin-cleanup-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::CLEANUP_EMPTY.get()),
            );
        }

        let max_bytes = rows.first().map_or(1, |row| row.bytes.max(1));
        let list = crate::ui::group(
            p,
            rows.iter()
                .map(|row| self.render_disk_row(p, row, max_bytes, small, cx)),
        )
        .id("admin-space-worktree-list")
        .debug_selector(|| "admin-space-worktree-list".into());

        let protected = rows
            .iter()
            .filter(|row| self.space.row_kind(row).is_protected())
            .count();
        section
            .child(list)
            .children(
                (!self.space.safety_excluded_candidates().is_empty()).then(|| {
                    div()
                        .debug_selector(|| "admin-cleanup-dirty-hint".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::CLEANUP_DIRTY_HINT.get())
                }),
            )
            .child(self.render_cleanup_rules(p, protected, cx))
            .children(
                self.space
                    .last_summary
                    .as_ref()
                    .map(|summary| Self::render_cleanup_result(p, summary)),
            )
    }

    fn render_space_blanks(p: &crate::ui::Palette) -> gpui::Stateful<gpui::Div> {
        crate::ui::group(
            p,
            (0..5).map(|_| {
                div()
                    .w_full()
                    .px_2p5()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(layout::blank(p, gpui::relative(0.38)))
                    .child(layout::blank(p, gpui::relative(0.7)))
            }),
        )
        .id("admin-space-blank")
        .debug_selector(|| "admin-space-blank".into())
    }

    /// 빌드 아티팩트 칩과 마지막 스캔 시각.
    fn render_space_chips(p: &crate::ui::Palette, report: &SpaceReport) -> gpui::Div {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let age = crate::jump::format_age_secs(report.scanned_at, now);
        let mut chips = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1();
        if report.artifacts.is_empty() {
            chips = chips.child(
                div()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(text::SPACE_ARTIFACTS_NONE.get()),
            );
        }
        for art in &report.artifacts {
            let chip_id = format!("admin-space-artifact-{}", art.name);
            chips = chips.child(
                crate::ui::badge(
                    p,
                    BadgeTone::Muted,
                    format!("{} {}", art.name, format_bytes(art.bytes)),
                )
                .id(SharedString::from(chip_id.clone()))
                .debug_selector(move || chip_id.clone()),
            );
        }
        chips.when(!age.is_empty(), |chips| {
            chips.child(
                div()
                    .debug_selector(|| "admin-space-scanned-at".into())
                    .ml_auto()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SPACE_SCANNED_AT.format(&[("age", &age)])),
            )
        })
    }

    /// PR 상태, 경과 일수, 안전 필터. 좁은 창에서는 현재 조건 한 줄로 접힌다.
    fn render_cleanup_filters(
        &self,
        p: &crate::ui::Palette,
        small: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let cleaning = self.space.cleaning;
        let filter = &self.space.filter;
        let pr_filters: [(CleanupPrFilter, &'static str, &'static str); 4] = [
            (
                CleanupPrFilter::MergedOrClosed,
                "admin-cleanup-pr-both",
                text::CLEANUP_PR_BOTH.get(),
            ),
            (
                CleanupPrFilter::Merged,
                "admin-cleanup-pr-merged",
                text::CLEANUP_PR_MERGED.get(),
            ),
            (
                CleanupPrFilter::Closed,
                "admin-cleanup-pr-closed",
                text::CLEANUP_PR_CLOSED.get(),
            ),
            (
                CleanupPrFilter::Any,
                "admin-cleanup-pr-any",
                text::CLEANUP_PR_ANY.get(),
            ),
        ];
        let idle_presets: [(IdleDaysPreset, &'static str, &'static str); 4] = [
            (
                IdleDaysPreset::Any,
                "admin-cleanup-days-0",
                text::CLEANUP_IDLE_ALL.get(),
            ),
            (
                IdleDaysPreset::Days7,
                "admin-cleanup-days-7",
                text::CLEANUP_IDLE_7D.get(),
            ),
            (
                IdleDaysPreset::Days14,
                "admin-cleanup-days-14",
                text::CLEANUP_IDLE_14D.get(),
            ),
            (
                IdleDaysPreset::Days30,
                "admin-cleanup-days-30",
                text::CLEANUP_IDLE_30D.get(),
            ),
        ];
        let open = !small || self.space.filters_open;

        let mut card = layout::card(p)
            .id("admin-cleanup-filters")
            .debug_selector(|| "admin-cleanup-filters".into())
            .flex()
            .flex_col()
            .gap_1p5();
        if small {
            let pr_label = pr_filters
                .iter()
                .find(|(mode, ..)| *mode == filter.pr_filter)
                .map_or("", |(.., label)| label);
            let idle_label = idle_presets
                .iter()
                .find(|(preset, ..)| preset.days() == filter.min_idle_days)
                .map_or("", |(.., label)| label);
            let safety = if filter.exclude_dirty {
                text::CLEANUP_COND_DIRTY_EXCLUDED.get()
            } else {
                text::CLEANUP_COND_DIRTY_INCLUDED.get()
            };
            card = card.child(
                div()
                    .id("admin-cleanup-filters-toggle")
                    .debug_selector(|| "admin-cleanup-filters-toggle".into())
                    .role(gpui::Role::Button)
                    .aria_expanded(open)
                    .tab_index(0)
                    .focus_visible(|style| style.bg(p.accent))
                    .cursor_pointer()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.space.filters_open = !this.space.filters_open;
                        cx.notify();
                    }))
                    .child(disclosure_icon(open))
                    .child(div().min_w_0().child(text::CLEANUP_COND_SUMMARY.format(&[
                        ("pr", pr_label),
                        ("idle", idle_label),
                        ("safety", safety),
                    ]))),
            );
        }
        if !open {
            return card;
        }

        let pr_seg = layout::segmented_control(
            p,
            pr_filters.iter().map(|&(mode, id_str, label)| {
                layout::segment(p, id_str, label, filter.pr_filter == mode)
                    .disabled(cleaning)
                    .debug_selector(move || id_str.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_cleanup_pr_filter(mode, cx);
                    }))
            }),
        );
        let idle_seg = layout::segmented_control(
            p,
            idle_presets.iter().map(|&(preset, id_str, label)| {
                let days = preset.days();
                layout::segment(p, id_str, label, filter.min_idle_days == days)
                    .disabled(cleaning)
                    .debug_selector(move || id_str.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_cleanup_idle_days(days, cx);
                    }))
            }),
        );
        let view = cx.entity();
        let safety_switch = layout::switch(
            p,
            "admin-cleanup-exclude-dirty",
            filter.exclude_dirty,
            cleaning,
            move |next, _, cx| {
                view.update(cx, |this, cx| {
                    this.set_cleanup_exclude_dirty(next, cx);
                });
            },
        )
        .aria_label(text::CLEANUP_EXCLUDE_DIRTY.get());
        let safety = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(safety_switch)
            .child(
                div()
                    .min_w_0()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::CLEANUP_EXCLUDE_DIRTY.get()),
            );
        card.child(filter_row(p, text::CLEANUP_FILTER_PR.get(), pr_seg))
            .child(filter_row(p, text::CLEANUP_FILTER_IDLE.get(), idle_seg))
            .child(filter_row(p, text::CLEANUP_FILTER_SAFETY.get(), safety))
    }

    /// 표 한 행. 지울 수 없는 행은 체크칸 자리에 표시를 두고 이유를 그 자리에 쓴다.
    fn render_disk_row(
        &self,
        p: &crate::ui::Palette,
        row: &DiskRow<'_>,
        max_bytes: u64,
        small: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let kind = self.space.row_kind(row);
        let removable = kind == RowKind::Removable;
        let name = row.name;
        let selected = self.space.selected.contains(row.path);
        let open = self.space.expanded.contains(row.path);
        let pct = row
            .bytes
            .saturating_mul(100)
            .checked_div(max_bytes)
            .unwrap_or(0)
            .clamp(4, 100);
        let bar_ratio = u8::try_from(pct).map_or(1.0_f32, f32::from) / 100.0_f32;

        let select_id = format!("admin-cleanup-select-{name}");
        let check_id = gpui::ElementId::Name(select_id.clone().into());
        let select_path = row.path.to_path_buf();
        let slot = div()
            .debug_selector(move || select_id.clone())
            .flex_none()
            .w(gpui::rems(1.))
            .flex()
            .items_center()
            .justify_center()
            .map(|slot| {
                if removable {
                    slot.child(
                        Checkbox::new(check_id)
                            .checked(selected)
                            .disabled(self.space.cleaning)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_cleanup_candidate(&select_path, cx);
                            })),
                    )
                } else {
                    slot.child(Icon::new(kind.icon()).xsmall().text_color(kind.color(p)))
                }
            });

        let toggle_id = format!("admin-space-toggle-{name}");
        let name_id = format!("admin-space-name-{name}");
        let toggle_path = row.path.to_path_buf();
        let hit = div()
            .id(SharedString::from(toggle_id.clone()))
            .debug_selector(move || toggle_id.clone())
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(name.to_owned()))
            .aria_expanded(open)
            .tab_index(0)
            .focus_visible(|style| style.bg(p.accent))
            .cursor_pointer()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.space.expanded.remove(&toggle_path) {
                    this.space.expanded.insert(toggle_path.clone());
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .flex_basis(gpui::relative(0.5))
                    .min_w(px(100.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .debug_selector(move || name_id.clone())
                            .min_w_0()
                            .truncate()
                            .text_size(crate::ui::TEXT_SM)
                            .when(removable, |d| d.font_weight(FontWeight::MEDIUM))
                            .when(!removable, |d| d.text_color(p.muted_foreground))
                            .child(name.to_owned()),
                    )
                    .children(row.branch.map(|branch| {
                        div()
                            .flex_1()
                            .flex_basis(px(0.))
                            .min_w_0()
                            .truncate()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(branch.to_owned())
                    })),
            )
            .child(
                div()
                    .ml_auto()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(if small { 44. } else { 110. }))
                            .h(px(6.))
                            .rounded(crate::ui::RADIUS_XS)
                            .bg(p.muted)
                            .overflow_hidden()
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(bar_ratio))
                                    .rounded(crate::ui::RADIUS_XS)
                                    .bg(p.ring),
                            ),
                    )
                    .child(
                        div()
                            .min_w(gpui::rems(3.5))
                            .text_right()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format_bytes(row.bytes)),
                    ),
            );

        let line_id = format!("admin-cleanup-row-{name}");
        let reason_id = format!("admin-cleanup-reason-{name}");
        let indent = gpui::rems(1.5);
        div()
            .id(SharedString::from(format!("admin-space-wt-{name}")))
            .debug_selector({
                let name = name.to_owned();
                move || format!("admin-space-wt-{name}")
            })
            .w_full()
            .min_w_0()
            .px_2p5()
            .py_1p5()
            .flex()
            .flex_col()
            .gap_1()
            .when(selected, |d| d.bg(p.destructive.opacity(0.07)))
            .child(
                div()
                    .debug_selector(move || line_id.clone())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(slot)
                    .child(hit),
            )
            .child(
                div()
                    .pl(indent)
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .when(removable, |d| {
                        d.children(self.render_row_badges(p, row, selected))
                    })
                    .when_some(kind.reason(row), |d, reason| {
                        d.child(
                            div()
                                .debug_selector(move || reason_id.clone())
                                .min_w_0()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(kind.color(p))
                                .child(reason),
                        )
                    }),
            )
            .when(open, |d| {
                d.child(
                    div()
                        .pl(indent)
                        .pb_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1()
                                .when(row.is_main, |d| {
                                    d.child(crate::ui::badge(
                                        p,
                                        BadgeTone::Neutral,
                                        text::SPACE_MAIN_BADGE.get(),
                                    ))
                                })
                                .children(row.branch.map(|branch| {
                                    crate::ui::badge(p, BadgeTone::Muted, branch.to_owned())
                                        .min_w_0()
                                        .max_w_full()
                                        .overflow_hidden()
                                }))
                                .when(!removable, |d| {
                                    d.children(self.render_row_badges(p, row, selected))
                                })
                                .children(Self::render_row_artifacts(p, row)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .font_family(crate::ui::FONT_MONO)
                                .text_size(crate::ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .child(row.path.display().to_string()),
                        ),
                )
            })
    }

    fn render_row_badges(
        &self,
        p: &crate::ui::Palette,
        row: &DiskRow<'_>,
        selected: bool,
    ) -> Vec<gpui::Div> {
        let Some(cand) = row.candidate else {
            return Vec::new();
        };
        let mut badges = Vec::new();
        if self.space.cleaning && selected {
            badges.push(crate::ui::badge(
                p,
                BadgeTone::Warning,
                text::CLEANUP_EXECUTING.get(),
            ));
        }
        let pr_state = |state: &str| text::CLEANUP_BADGE_PR.format(&[("state", state)]);
        badges.push(match cand.pr_state {
            CleanupPrState::Merged => crate::ui::badge(
                p,
                BadgeTone::Success,
                pr_state(crate::i18n::catalog::pull_request::MERGED.get()),
            ),
            CleanupPrState::Closed => crate::ui::badge(
                p,
                BadgeTone::Warning,
                pr_state(crate::i18n::catalog::pull_request::CLOSED.get()),
            ),
            CleanupPrState::Open => crate::ui::badge(
                p,
                BadgeTone::Neutral,
                pr_state(crate::i18n::catalog::pull_request::OPEN.get()),
            ),
            CleanupPrState::Draft => crate::ui::badge(
                p,
                BadgeTone::Muted,
                pr_state(crate::i18n::catalog::pull_request::DRAFT.get()),
            ),
            CleanupPrState::None => {
                crate::ui::badge(p, BadgeTone::Muted, text::CLEANUP_BADGE_NO_PR.get())
            }
        });
        badges.push(crate::ui::badge(
            p,
            BadgeTone::Neutral,
            text::CLEANUP_BADGE_IDLE.format(&[("days", &cand.idle_days.to_string())]),
        ));
        badges.push(if cand.dirty {
            crate::ui::badge(
                p,
                BadgeTone::Destructive,
                text::CLEANUP_BADGE_DIRTY.format(&[("n", &cand.dirty_count.to_string())]),
            )
        } else {
            crate::ui::badge(p, BadgeTone::Muted, text::CLEANUP_BADGE_CLEAN.get())
        });
        badges
    }

    fn render_row_artifacts(p: &crate::ui::Palette, row: &DiskRow<'_>) -> Vec<gpui::AnyElement> {
        let artifacts = row.space.map_or(&[][..], |space| &space.artifacts);
        if artifacts.is_empty() {
            return vec![
                div()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(text::SPACE_ARTIFACTS_NONE.get())
                    .into_any_element(),
            ];
        }
        artifacts
            .iter()
            .map(|art| {
                let art_id = format!("admin-space-wt-art-{}-{}", row.name, art.name);
                crate::ui::badge(
                    p,
                    BadgeTone::Warning,
                    format!("{} {}", art.name, format_bytes(art.bytes)),
                )
                .id(SharedString::from(art_id.clone()))
                .debug_selector(move || art_id.clone())
                .into_any_element()
            })
            .collect()
    }

    /// 접힌 보호 규칙. 모달에 있던 설명과 CLI가 지키는 규칙을 그대로 옮긴다.
    fn render_cleanup_rules(
        &self,
        p: &crate::ui::Palette,
        protected: usize,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        const RULES: [crate::i18n::Message; 7] = [
            text::CLEANUP_RULE_MAIN,
            text::CLEANUP_RULE_LOCKED,
            text::CLEANUP_RULE_SESSION,
            text::CLEANUP_RULE_SESSION_UNKNOWN,
            text::CLEANUP_RULE_DIRTY,
            text::CLEANUP_RULE_BRANCH,
            text::CLEANUP_RULE_NESTED,
        ];
        let open = self.space.rules_open;
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id("admin-cleanup-rules-toggle")
                    .debug_selector(|| "admin-cleanup-rules-toggle".into())
                    .role(gpui::Role::Button)
                    .aria_expanded(open)
                    .tab_index(0)
                    .focus_visible(|style| style.bg(p.accent))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.space.rules_open = !this.space.rules_open;
                        cx.notify();
                    }))
                    .child(disclosure_icon(open))
                    .child(text::CLEANUP_RULES_TOGGLE.format(&[
                        ("rules", &RULES.len().to_string()),
                        ("protected", &protected.to_string()),
                    ])),
            )
            .when(open, |d| {
                d.child(
                    div()
                        .debug_selector(|| "admin-cleanup-rules".into())
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::CLEANUP_MODAL_DESC.get())
                        .children(RULES.iter().map(|rule| div().child(rule.get()))),
                )
            })
    }

    /// 실행 결과. 실패 수와 CLI가 준 사유와 경고를 항목별로 쓴다.
    fn render_cleanup_result(
        p: &crate::ui::Palette,
        sum: &CleanupSummary,
    ) -> gpui::Stateful<gpui::Div> {
        let item_name = |path: &Path| {
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned()
        };
        div()
            .id("admin-cleanup-status")
            .debug_selector(|| "admin-cleanup-status".into())
            .p_2()
            .rounded(crate::ui::RADIUS_SM)
            .bg(p.card)
            .border_1()
            .border_color(p.border)
            .text_size(crate::ui::TEXT_XS)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .when(sum.failed_count > 0, |d| d.text_color(p.destructive))
                    .child(text::CLEANUP_RESULT_SUMMARY.format(&[
                        ("removed", &sum.removed_count.to_string()),
                        ("size", &format_bytes(sum.freed_bytes)),
                        ("failed", &sum.failed_count.to_string()),
                        ("deleted", &sum.branches_deleted.to_string()),
                        ("preserved", &sum.branches_preserved.to_string()),
                    ])),
            )
            .children(sum.items.iter().filter_map(|item| {
                let error = item.error.as_ref()?;
                let selector = format!("admin-cleanup-failed-{}", item_name(&item.path));
                Some(
                    div()
                        .debug_selector(move || selector.clone())
                        .text_color(p.destructive)
                        .child(format!("{}: {error}", item_name(&item.path))),
                )
            }))
            .children(sum.items.iter().flat_map(|item| {
                item.warnings.iter().map(|warning| {
                    div()
                        .text_color(p.muted_foreground)
                        .child(format!("{}: {warning}", item_name(&item.path)))
                })
            }))
    }

    /// 본문 아래에 고정되는 띠: 지워질 내용 미리보기와 유일한 삭제 버튼. 지울 행이 없으면 그리지 않는다.
    pub(super) fn render_disk_footer(
        &self,
        window: &Window,
        inset_x: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if self.space.report.is_none() || self.space.rows().is_empty() {
            return None;
        }
        let p = crate::ui::palette(cx);
        let small = layout::small_body(window);
        let cleaning = self.space.cleaning;
        let selectable = self.space.selectable_candidates();
        let chosen: Vec<&WorktreeCleanupCandidate> = selectable
            .iter()
            .filter(|c| self.space.selected.contains(&c.path))
            .collect();
        let bytes = chosen
            .iter()
            .fold(0_u64, |acc, c| acc.saturating_add(c.disk_bytes));
        let count = |keep: fn(&WorktreeCleanupCandidate) -> bool| {
            chosen.iter().filter(|c| keep(c)).count().to_string()
        };
        let preview_desc = if chosen.is_empty() {
            (!small).then(|| text::CLEANUP_PREVIEW_IDLE.get().to_owned())
        } else {
            let dirty = chosen.iter().filter(|c| c.dirty).count();
            let dirty = if dirty > 0 {
                text::CLEANUP_PREVIEW_DIRTY.format(&[("n", &dirty.to_string())])
            } else {
                String::new()
            };
            Some(text::CLEANUP_PREVIEW_DESC.format(&[
                (
                    "deleted",
                    &count(|c| c.branch.is_some() && c.merged_into_main),
                ),
                (
                    "preserved",
                    &count(|c| c.branch.is_some() && !c.merged_into_main),
                ),
                ("dirty", &dirty),
            ]))
        };
        let all_selected = !selectable.is_empty() && chosen.len() == selectable.len();
        let select_all_label = if all_selected {
            text::CLEANUP_DESELECT_ALL.get()
        } else {
            text::CLEANUP_SELECT_ALL.get()
        };
        let execute_label = text::CLEANUP_EXECUTE.format(&[("count", &chosen.len().to_string())]);
        let exclude_dirty = self.space.filter.exclude_dirty;

        Some(
            div()
                .id("admin-cleanup-footer")
                .debug_selector(|| "admin-cleanup-footer".into())
                .flex_none()
                .w_full()
                .min_w_0()
                .px(inset_x)
                .py_2()
                .bg(p.background)
                .border_t_1()
                .border_color(p.border)
                .child(
                    div()
                        .max_w(crate::ui::SETTINGS_CONTENT_MAX_WIDTH)
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .when(!exclude_dirty, |d| {
                            d.child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.destructive)
                                    .child(text::CLEANUP_SAFETY_OFF_SHORT.get()),
                            )
                        })
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .flex_basis(gpui::rems(8.))
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .id("admin-cleanup-summary")
                                                .debug_selector(|| "admin-cleanup-summary".into())
                                                .text_size(crate::ui::TEXT_XS)
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(text::CLEANUP_PREVIEW_TITLE.format(&[
                                                    ("count", &chosen.len().to_string()),
                                                    ("size", &format_bytes(bytes)),
                                                ])),
                                        )
                                        .children(preview_desc.map(|desc| {
                                            div()
                                                .text_size(crate::ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(desc)
                                        })),
                                )
                                .child(
                                    layout::controls()
                                        .child(
                                            layout::ghost(
                                                "admin-cleanup-select-all",
                                                select_all_label,
                                            )
                                            .disabled(selectable.is_empty() || cleaning)
                                            .debug_selector(|| "admin-cleanup-select-all".into())
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.toggle_select_all_cleanup(cx);
                                                }),
                                            ),
                                        )
                                        .child(
                                            self.confirm_button_with(
                                                "admin-cleanup-execute",
                                                execute_label,
                                                cleaning.then(|| text::CLEANUP_EXECUTING.get()),
                                                cx,
                                                |this, _, cx| this.execute_selected_cleanup(cx),
                                            )
                                            .disabled(chosen.is_empty() || cleaning),
                                        ),
                                ),
                        ),
                ),
        )
    }
}

fn filter_row(p: &crate::ui::Palette, label: &'static str, control: impl IntoElement) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_x_2()
        .gap_y_1()
        .child(
            div()
                .text_size(crate::ui::TEXT_XS)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.muted_foreground)
                .child(label),
        )
        .child(control)
}

fn disclosure_icon(open: bool) -> Icon {
    Icon::new(if open {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .xsmall()
}
