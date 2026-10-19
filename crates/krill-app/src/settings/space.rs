//! 워크스페이스 디스크 용량 분석(`Space`) 및 머지된 워크트리 일괄 클린업(`Cleanup`) 모달(MK-175, OWE-729).
//!
//! - `Settings > Resources` 탭 안에서 각 워크트리별 전체 디스크 사용량과 주요 빌드 아티팩트
//!   (`target`, `node_modules`, `.next` 등) 용량을 백그라운드 비차단 스캔으로 집계·정렬·시각화한다.
//! - `Cleanup` 모달에서 PR 상태(`Merged` / `Closed`), 마지막 활동 경과 일수(`7일` / `14일` / `30일` 이상),
//!   `dirty`(미커밋 변경) 제외 안전 필터로 정리 대상을 고르고 일괄 삭제 및 병합된 로컬 브랜치를 정리한다.
//!
//! 스캔·삭제 엔진은 앱에 두지 않는다(OWE-744). 스테이징된 `krill` CLI를 하위 프로세스로 실행해
//! `--json` 표준출력(`krill_proto::worktree_space`에 고정된 스키마)을 파싱·표시만 한다.
//! - 용량: `krill worktree space [--repo <r>]... --json` → `SpaceReport`
//! - 전체 후보: `krill worktree cleanup [--repo <r>]... --pr any --idle-days 0 --include-dirty --dry-run --json`
//!   → `CleanupReport.candidates`(필터는 앱이 `filter_cleanup_candidates`로 적용)
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
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::{Disableable as _, Sizable as _};
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

impl CliRunner {
    pub fn new(f: impl Fn(&[OsString]) -> io::Result<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[OsString]) -> io::Result<T> {
        let out = (self.0)(args)?;
        serde_json::from_slice(&out).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// 스테이징된 `krill` 바이너리를 하위 프로세스로 실행한다.
    fn process(bin: PathBuf, state_dir: Option<PathBuf>) -> Self {
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
            let output = cmd.output()?;
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

/// 용량 보고서와 필터 전 전체 클린업 후보를 CLI에서 가져온다.
fn fetch_space(
    run: &CliRunner,
    repos: Option<&[PathBuf]>,
) -> io::Result<(SpaceReport, Vec<WorktreeCleanupCandidate>)> {
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
    let candidates = run.json::<CleanupReport>(&all)?.candidates;
    Ok((report, candidates))
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

/// `SettingsView`가 소유하는 디스크 용량 분석(`Space`) 및 워크트리 클린업(`Cleanup`) 상태.
#[derive(Debug, Default)]
pub struct Panel {
    /// 지정 저장소 오버라이드(`--repo`로 전달). `None`이면 CLI가 상태 디렉터리·데몬에서 자동 탐색한다.
    pub repos_override: Option<Vec<PathBuf>>,
    /// 테스트용 CLI 실행기 오버라이드. `None`이면 스테이징된 `krill` 바이너리를 실행한다.
    pub runner_override: Option<CliRunner>,
    /// 마지막 백그라운드 디스크 용량 스캔 결과.
    pub report: Option<SpaceReport>,
    /// 백그라운드 용량 스캔 진행 중 여부.
    pub scanning: bool,
    /// 워크트리 일괄 클린업 모달 열림 여부.
    pub cleanup_open: bool,
    /// 발견된 전체 링크된 워크트리 클린업 후보 목록.
    pub candidates: Vec<WorktreeCleanupCandidate>,
    /// 현재 클린업 필터 조건(PR 상태, 경과 일수, dirty 제외 안전 필터).
    pub filter: CleanupFilter,
    /// 클린업 모달에서 체크된 워크트리 경로 집합.
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

    /// 현재 필터에 일치하는 후보들로 기본 선택 집합을 동기화한다.
    pub fn sync_selection_to_filter(&mut self) {
        self.selected = self
            .filtered_candidates()
            .into_iter()
            .map(|c| c.path)
            .collect();
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
        self.space.repos_override = Some(repos);
        self.space.runner_override = runner;
        self.scan_workspace_space(cx);
    }

    /// 오버라이드 또는 스테이징된 `krill` CLI 실행기. 바이너리가 없으면 오류 메시지를 남기고 `None`.
    fn space_runner(&mut self, cx: &mut Context<Self>) -> Option<CliRunner> {
        if let Some(run) = self.space.runner_override.clone() {
            return Some(run);
        }
        if let Some(bin) = crate::orca_import::resolve_krill_bin(cx) {
            return Some(CliRunner::process(bin, state_dir(cx)));
        }
        self.space.error = Some(
            text::SPACE_ERROR
                .format(&[(
                    "error",
                    crate::i18n::catalog::orca_import::ERR_CLI_MISSING.get(),
                )])
                .into(),
        );
        cx.notify();
        None
    }

    /// 백그라운드 실행기에서 비차단으로 워크트리 디스크 용량과 클린업 후보를 스캔한다.
    pub fn scan_workspace_space(&mut self, cx: &mut Context<Self>) {
        if self.space.scanning {
            return;
        }
        self.space.error = None;
        let Some(run) = self.space_runner(cx) else {
            return;
        };
        self.space.scanning = true;
        let repos = self.space.repos_override.clone();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { fetch_space(&run, repos.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                this.space.scanning = false;
                this.apply_space_result(res);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_space_result(
        &mut self,
        res: io::Result<(SpaceReport, Vec<WorktreeCleanupCandidate>)>,
    ) {
        match res {
            Ok((report, candidates)) => {
                self.space.report = Some(report);
                self.space.candidates = candidates;
                self.space.sync_selection_to_filter();
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

    /// 워크트리 일괄 클린업 모달을 연다. 아직 스캔 결과가 없으면 백그라운드 스캔을 함께 시작한다.
    pub fn open_cleanup_modal(&mut self, cx: &mut Context<Self>) {
        self.space.cleanup_open = true;
        self.space.last_summary = None;
        self.space.sync_selection_to_filter();
        if self.space.report.is_none() && !self.space.scanning {
            self.scan_workspace_space(cx);
        }
        cx.notify();
    }

    /// 워크트리 일괄 클린업 모달을 닫는다.
    pub fn close_cleanup_modal(&mut self, cx: &mut Context<Self>) {
        if self.space.cleanup_open {
            self.space.cleanup_open = false;
            cx.notify();
        }
    }

    /// 클린업 모달의 PR 상태 필터를 변경한다.
    pub fn set_cleanup_pr_filter(&mut self, pr_filter: CleanupPrFilter, cx: &mut Context<Self>) {
        self.space.filter.pr_filter = pr_filter;
        self.space.sync_selection_to_filter();
        cx.notify();
    }

    /// 클린업 모달의 마지막 활동 경과 일수 필터를 변경한다.
    pub fn set_cleanup_idle_days(&mut self, days: u64, cx: &mut Context<Self>) {
        self.space.filter.min_idle_days = days;
        self.space.sync_selection_to_filter();
        cx.notify();
    }

    /// 클린업 모달의 미커밋(`dirty`) 워크트리 제외 안전 필터를 변경한다.
    pub fn set_cleanup_exclude_dirty(&mut self, exclude_dirty: bool, cx: &mut Context<Self>) {
        self.space.filter.exclude_dirty = exclude_dirty;
        self.space.sync_selection_to_filter();
        cx.notify();
    }

    /// 개별 후보 워크트리의 선택 여부를 토글한다.
    pub fn toggle_cleanup_candidate(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.space.selected.remove(path) {
            self.space.selected.insert(path.to_path_buf());
        }
        cx.notify();
    }

    /// 현재 필터에 일치하는 모든 후보를 전체 선택하거나 선택 해제한다.
    pub fn toggle_select_all_cleanup(&mut self, cx: &mut Context<Self>) {
        let matched = self.space.filtered_candidates();
        let all_selected = !matched.is_empty()
            && matched
                .iter()
                .all(|c| self.space.selected.contains(&c.path));
        if all_selected {
            self.space.selected.clear();
        } else {
            self.space.selected = matched.into_iter().map(|c| c.path).collect();
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
            .filtered_candidates()
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
        let filter = self.space.filter.clone();
        let repos = self.space.repos_override.clone();
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
                match summary {
                    Ok(summary) => this.space.last_summary = Some(summary),
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

    /// `Settings > Resources` 탭에 들어가는 디스크 용량 분석(`Space`) 및 클린업 진입 섹션.
    pub(super) fn render_space_section(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let scan_label = if self.space.scanning {
            text::SPACE_SCANNING.get()
        } else {
            text::SPACE_SCAN.get()
        };
        let scan_btn = layout::secondary("admin-space-scan", scan_label)
            .icon(IconName::RefreshCw)
            .disabled(self.space.scanning)
            .debug_selector(|| "admin-space-scan".into())
            .on_click(cx.listener(|this, _, _, cx| {
                this.scan_workspace_space(cx);
            }));
        let cleanup_btn = layout::primary(
            "admin-worktree-cleanup-open",
            text::CLEANUP_OPEN_BUTTON.get(),
        )
        .debug_selector(|| "admin-worktree-cleanup-open".into())
        .on_click(cx.listener(|this, _, _, cx| {
            this.open_cleanup_modal(cx);
        }));

        let header_row = layout::row(p)
            .id("admin-space-header-row")
            .debug_selector(|| "admin-space-header-row".into())
            .flex_wrap()
            .child(
                layout::label(
                    p,
                    text::SPACE_CARD_TITLE.get(),
                    Some(text::SPACE_CARD_DESC.get()),
                )
                .min_w(px(200.)),
            )
            .child(
                layout::controls()
                    .flex_wrap()
                    .child(scan_btn)
                    .child(cleanup_btn),
            );

        let mut section = div()
            .id("admin-space-section")
            .debug_selector(|| "admin-space-section".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(layout::section_title(
                p,
                "admin-section-space",
                text::SPACE_TITLE.get(),
            ))
            .child(header_row);
        if let Some(err) = &self.space.error {
            section = section.child(
                crate::ui::error_banner(p, err.clone())
                    .id("admin-space-error")
                    .debug_selector(|| "admin-space-error".into()),
            );
        }

        let Some(report) = &self.space.report else {
            return section.child(
                layout::card(p)
                    .id("admin-space-empty")
                    .debug_selector(|| "admin-space-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(if self.space.scanning {
                        text::SPACE_SCANNING.get()
                    } else {
                        text::SPACE_EMPTY.get()
                    }),
            );
        };

        let summary_text = text::SPACE_TOTAL_SUMMARY.format(&[
            ("total", &format_bytes(report.total_bytes)),
            ("count", &report.worktrees.len().to_string()),
            ("artifacts", &format_bytes(report.total_artifact_bytes)),
        ]);

        let mut top_artifacts = div().flex().items_center().gap_1p5().flex_wrap();
        if report.artifacts.is_empty() {
            top_artifacts = top_artifacts.child(
                div()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(text::SPACE_ARTIFACTS_NONE.get()),
            );
        } else {
            for art in &report.artifacts {
                let chip_id = format!("admin-space-artifact-{}", art.name);
                let label = format!("{} · {}", art.name, format_bytes(art.bytes));
                top_artifacts = top_artifacts.child(
                    crate::ui::badge(p, BadgeTone::Accent, label)
                        .id(SharedString::from(chip_id.clone()))
                        .debug_selector(move || chip_id.clone()),
                );
            }
        }

        let summary_card = layout::card(p)
            .id("admin-space-summary")
            .debug_selector(|| "admin-space-summary".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(summary_text),
            )
            .child(top_artifacts);

        section = section.child(summary_card);

        if report.worktrees.is_empty() {
            return section.child(
                layout::card(p)
                    .id("admin-space-empty")
                    .debug_selector(|| "admin-space-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SPACE_EMPTY.get()),
            );
        }

        let max_bytes = report.worktrees.first().map_or(1, |w| w.total_bytes.max(1));
        let mut wt_list = div()
            .id("admin-space-worktree-list")
            .debug_selector(|| "admin-space-worktree-list".into())
            .flex()
            .flex_col()
            .gap_2();

        for wt in &report.worktrees {
            wt_list = wt_list.child(Self::render_space_worktree_row(p, wt, max_bytes));
        }

        section.child(wt_list)
    }

    fn render_space_worktree_row(
        p: &crate::ui::Palette,
        wt: &WorktreeSpaceEntry,
        max_bytes: u64,
    ) -> gpui::Stateful<gpui::Div> {
        let row_id = format!("admin-space-wt-{}", wt.name);
        let pct_ratio = wt
            .total_bytes
            .saturating_mul(100)
            .checked_div(max_bytes)
            .unwrap_or(0)
            .clamp(4, 100);
        let bar_ratio = u8::try_from(pct_ratio).map_or(1.0_f32, f32::from) / 100.0_f32;

        let mut badges = div().flex().items_center().gap_1p5().flex_wrap();
        if wt.is_main {
            badges = badges.child(crate::ui::badge(
                p,
                BadgeTone::Neutral,
                text::SPACE_MAIN_BADGE.get(),
            ));
        }
        if let Some(branch) = &wt.branch {
            badges = badges.child(crate::ui::badge(p, BadgeTone::Muted, branch.clone()));
        }

        let mut art_row = div().flex().items_center().gap_1p5().flex_wrap();
        if wt.artifacts.is_empty() {
            art_row = art_row.child(
                div()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(text::SPACE_ARTIFACTS_NONE.get()),
            );
        } else {
            for art in &wt.artifacts {
                let art_id = format!("admin-space-wt-art-{}-{}", wt.name, art.name);
                art_row = art_row.child(
                    crate::ui::badge(
                        p,
                        BadgeTone::Warning,
                        format!("{} {}", art.name, format_bytes(art.bytes)),
                    )
                    .id(SharedString::from(art_id.clone()))
                    .debug_selector(move || art_id.clone()),
                );
            }
        }

        layout::card(p)
            .id(SharedString::from(row_id.clone()))
            .debug_selector(move || row_id.clone())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(wt.name.clone()),
                            )
                            .child(badges),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format_bytes(wt.total_bytes)),
                    ),
            )
            .child(
                div()
                    .w_full()
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
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .flex_wrap()
                    .child(art_row)
                    .child(
                        div()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(wt.path.display().to_string()),
                    ),
            )
    }

    /// 워크트리 일괄 클린업(`Cleanup`) 모달 오버레이를 그린다.
    pub(super) fn render_cleanup_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        if !self.space.cleanup_open {
            return None;
        }
        let p = crate::ui::palette(cx);
        let matched = self.space.filtered_candidates();
        let selected_candidates: Vec<&WorktreeCleanupCandidate> = matched
            .iter()
            .filter(|c| self.space.selected.contains(&c.path))
            .collect();
        let selected_bytes: u64 = selected_candidates
            .iter()
            .fold(0_u64, |acc, c| acc.saturating_add(c.disk_bytes));

        let pr_filters: &[(CleanupPrFilter, &'static str, &'static str)] = &[
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
        let pr_seg = layout::segmented_control(
            p,
            pr_filters.iter().map(|&(mode, id_str, label)| {
                let active = self.space.filter.pr_filter == mode;
                layout::segment(id_str, label, active)
                    .debug_selector(move || id_str.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_cleanup_pr_filter(mode, cx);
                    }))
            }),
        );
        let pr_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .flex_wrap()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(text::CLEANUP_FILTER_PR.get()),
            )
            .child(pr_seg);

        let idle_presets: &[(IdleDaysPreset, &'static str, &'static str)] = &[
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
        let idle_seg = layout::segmented_control(
            p,
            idle_presets.iter().map(|&(preset, id_str, label)| {
                let days = preset.days();
                let active = self.space.filter.min_idle_days == days;
                layout::segment(id_str, label, active)
                    .debug_selector(move || id_str.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_cleanup_idle_days(days, cx);
                    }))
            }),
        );
        let idle_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .flex_wrap()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(text::CLEANUP_FILTER_IDLE.get()),
            )
            .child(idle_seg);

        let exclude_dirty = self.space.filter.exclude_dirty;
        let view = cx.entity();
        let safety_switch = layout::switch(
            p,
            "admin-cleanup-exclude-dirty",
            exclude_dirty,
            self.space.cleaning,
            move |next, _, cx| {
                view.update(cx, |this, cx| {
                    this.set_cleanup_exclude_dirty(next, cx);
                });
            },
        );
        let safety_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .flex_wrap()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.muted_foreground)
                            .child(text::CLEANUP_FILTER_SAFETY.get()),
                    )
                    .child(safety_switch)
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::CLEANUP_EXCLUDE_DIRTY.get()),
                    ),
            )
            .child(
                div()
                    .id("admin-cleanup-summary")
                    .debug_selector(|| "admin-cleanup-summary".into())
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::CLEANUP_CANDIDATES_SUMMARY.format(&[
                        ("matched", &matched.len().to_string()),
                        ("selected", &selected_candidates.len().to_string()),
                        ("size", &format_bytes(selected_bytes)),
                    ])),
            );

        let mut list = div()
            .id("admin-cleanup-candidate-list")
            .debug_selector(|| "admin-cleanup-candidate-list".into())
            .max_h(px(280.))
            .overflow_y_scroll()
            .flex()
            .flex_col();

        if matched.is_empty() {
            list = list.child(
                div()
                    .id("admin-cleanup-empty")
                    .debug_selector(|| "admin-cleanup-empty".into())
                    .w_full()
                    .py_4()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::CLEANUP_EMPTY.get()),
            );
        } else {
            for (idx, cand) in matched.iter().enumerate() {
                let is_selected = self.space.selected.contains(&cand.path);
                let row_id = format!("admin-cleanup-row-{}", cand.name);
                let sel_id = format!("admin-cleanup-select-{}", cand.name);
                let path_cl = cand.path.clone();
                let pr_badge = match cand.pr_state {
                    CleanupPrState::Merged => crate::ui::badge(
                        p,
                        BadgeTone::Success,
                        crate::i18n::catalog::pull_request::MERGED.get(),
                    ),
                    CleanupPrState::Closed => crate::ui::badge(
                        p,
                        BadgeTone::Warning,
                        crate::i18n::catalog::pull_request::CLOSED.get(),
                    ),
                    CleanupPrState::Open => crate::ui::badge(
                        p,
                        BadgeTone::Neutral,
                        crate::i18n::catalog::pull_request::OPEN.get(),
                    ),
                    CleanupPrState::Draft => crate::ui::badge(
                        p,
                        BadgeTone::Muted,
                        crate::i18n::catalog::pull_request::DRAFT.get(),
                    ),
                    CleanupPrState::None => crate::ui::badge(p, BadgeTone::Muted, "—"),
                };
                let state_badge = if cand.dirty {
                    crate::ui::badge(
                        p,
                        BadgeTone::Destructive,
                        text::CLEANUP_BADGE_DIRTY.format(&[("n", &cand.dirty_count.to_string())]),
                    )
                } else {
                    crate::ui::badge(p, BadgeTone::Muted, text::CLEANUP_BADGE_CLEAN.get())
                };
                let idle_badge = crate::ui::badge(
                    p,
                    BadgeTone::Neutral,
                    text::CLEANUP_BADGE_IDLE.format(&[("days", &cand.idle_days.to_string())]),
                );
                let check_id = gpui::ElementId::Name(sel_id.clone().into());
                let check_box = div()
                    .flex_none()
                    .debug_selector(move || sel_id.clone())
                    .child(
                        Checkbox::new(check_id)
                            .checked(is_selected)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_cleanup_candidate(&path_cl, cx);
                            })),
                    );

                list = list.child(
                    div()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .w_full()
                        .min_w_0()
                        .py_2()
                        .when(idx > 0, |d| d.border_t_1().border_color(p.border))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(check_box)
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .child(
                                                    div()
                                                        .text_size(crate::ui::TEXT_SM)
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .truncate()
                                                        .child(cand.name.clone()),
                                                )
                                                .children(cand.branch.as_ref().map(|b| {
                                                    crate::ui::badge(p, BadgeTone::Muted, b.clone())
                                                })),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .flex_wrap()
                                                .child(pr_badge)
                                                .child(idle_badge)
                                                .child(state_badge),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(crate::ui::FONT_MONO)
                                .text_size(crate::ui::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format_bytes(cand.disk_bytes)),
                        ),
                );
            }
        }

        let all_selected = !matched.is_empty() && selected_candidates.len() == matched.len();
        let select_all_label = if all_selected {
            text::CLEANUP_DESELECT_ALL.get()
        } else {
            text::CLEANUP_SELECT_ALL.get()
        };
        let execute_label = if self.space.cleaning {
            text::CLEANUP_EXECUTING.get().to_owned()
        } else {
            text::CLEANUP_EXECUTE.format(&[("count", &selected_candidates.len().to_string())])
        };

        let card = div()
            .id("admin-cleanup-modal")
            .debug_selector(|| "admin-cleanup-modal".into())
            .occlude()
            .role(gpui::Role::Dialog)
            .w(crate::ui::SETTINGS_MODAL_WIDTH)
            .max_w(window.viewport_size().width - crate::ui::SPACE_4 * 2.)
            .p_6()
            .rounded(crate::ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_cleanup_modal(cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text::CLEANUP_MODAL_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::CLEANUP_MODAL_DESC.get()),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .pb_3()
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(pr_row)
                    .child(idle_row)
                    .child(safety_row),
            )
            .child(list)
            .children(self.space.last_summary.as_ref().map(|sum| {
                div()
                    .id("admin-cleanup-status")
                    .debug_selector(|| "admin-cleanup-status".into())
                    .p_2()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.card)
                    .border_1()
                    .border_color(p.border)
                    .text_size(crate::ui::TEXT_XS)
                    .child(text::CLEANUP_RESULT_SUMMARY.format(&[
                        ("removed", &sum.removed_count.to_string()),
                        ("size", &format_bytes(sum.freed_bytes)),
                        ("deleted", &sum.branches_deleted.to_string()),
                        ("preserved", &sum.branches_preserved.to_string()),
                    ]))
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        layout::secondary("admin-cleanup-select-all", select_all_label)
                            .disabled(matched.is_empty() || self.space.cleaning)
                            .debug_selector(|| "admin-cleanup-select-all".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_select_all_cleanup(cx);
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                layout::ghost("admin-cleanup-cancel", text::CLEANUP_CLOSE.get())
                                    .debug_selector(|| "admin-cleanup-cancel".into())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_cleanup_modal(cx);
                                    })),
                            )
                            .child(
                                Button::new("admin-cleanup-execute")
                                    .small()
                                    .danger()
                                    .label(execute_label)
                                    .disabled(selected_candidates.is_empty() || self.space.cleaning)
                                    .debug_selector(|| "admin-cleanup-execute".into())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.execute_selected_cleanup(cx);
                                    })),
                            ),
                    ),
            );

        Some(crate::ui::modal_backdrop(window.viewport_size(), card))
    }
}
