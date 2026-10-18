//! OWE-739(MK-185): 오른쪽 사이드바 `PR · CI` 탭 — PR CI 체크 상태·실패 로그 핵심 꼬리·미해결 리뷰 코멘트 조회 및
//! 선택 항목을 묶어 활성 에이전트 세션에 수정 프롬프트로 즉시 전송(`Fix with AI`)하는 UI.
//!
//! - UI 배치 및 선택 큐 동작 참고:
//!   `.ref/orca/src/renderer/src/components/right-sidebar/ChecksPanel.tsx`,
//!   `.ref/orca/src/renderer/src/components/right-sidebar/use-checks-panel-ai-queue.tsx`,
//!   `.ref/orca/src/renderer/src/components/right-sidebar/use-checks-panel-check-and-review-actions.tsx`
//!   (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use gpui::{AnyElement, Context, FontWeight, Stateful, Task, Window, div, prelude::*, px};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, IconName, Sizable as _};

use super::{Panel, SendComments};
use crate::i18n::catalog::pull_request as text;
use crate::pull_request::{
    CheckItem, CheckState, PrInspection, ReviewComment, Status, build_fix_with_ai_prompt,
};

/// 오른쪽 사이드바 상단 탭.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PanelTab {
    #[default]
    Changes,
    Files,
    Search,
    Checks,
}

impl PanelTab {
    pub const ALL: [Self; 4] = [Self::Changes, Self::Files, Self::Search, Self::Checks];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Changes => crate::i18n::catalog::diff::TITLE.get(),
            Self::Files => crate::i18n::catalog::diff::FILES.get(),
            Self::Search => crate::i18n::catalog::diff::SEARCH.get(),
            Self::Checks => text::CHECKS_TAB.get(),
        }
    }

    #[must_use]
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Changes => crate::i18n::catalog::diff::CHANGES_TOOLTIP.get(),
            Self::Files => crate::i18n::catalog::diff::FILES_TOOLTIP.get(),
            Self::Search => crate::i18n::catalog::diff::SEARCH_TOOLTIP.get(),
            Self::Checks => text::CHECKS_TAB.get(),
        }
    }

    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Changes => "diff-tab-changes",
            Self::Files => "diff-tab-files",
            Self::Search => "diff-tab-search",
            Self::Checks => "diff-tab-checks",
        }
    }

    #[must_use]
    pub const fn active_selector(self) -> &'static str {
        match self {
            Self::Changes => "diff-tab-changes-active",
            Self::Files => "diff-tab-files-active",
            Self::Search => "diff-tab-search-active",
            Self::Checks => "diff-tab-checks-active",
        }
    }

    #[must_use]
    pub const fn tooltip_selector(self) -> &'static str {
        match self {
            Self::Changes => "diff-tab-changes-tooltip",
            Self::Files => "diff-tab-files-tooltip",
            Self::Search => "diff-tab-search-tooltip",
            Self::Checks => "diff-tab-checks-tooltip",
        }
    }

    #[must_use]
    pub const fn icon(self) -> IconName {
        match self {
            Self::Changes => IconName::FileText,
            Self::Files => IconName::Folder,
            Self::Search => IconName::Search,
            Self::Checks => IconName::CircleCheck,
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    pub branch_override: Option<String>,
    pub branch: String,
    pub inspection: Option<PrInspection>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected_checks: BTreeSet<usize>,
    pub expanded_logs: BTreeSet<usize>,
    pub selected_comments: BTreeSet<usize>,
    pub fix_status: Option<crate::i18n::Message>,
    pub generation: u64,
    pub task: Option<Task<()>>,
}

impl State {
    pub fn reset(&mut self) {
        let branch_override = self.branch_override.clone();
        *self = Self {
            branch_override,
            ..Self::default()
        };
    }

    pub fn selected_total(&self) -> usize {
        self.selected_checks.len() + self.selected_comments.len()
    }
}

fn resolve_branch(repo: &Path, branch_override: Option<&str>) -> String {
    if let Some(custom) = branch_override
        && !custom.trim().is_empty()
    {
        return custom.trim().to_owned();
    }
    crate::pull_request::git_output(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty() && s != "HEAD")
        .unwrap_or_else(|| "HEAD".to_owned())
}

impl Panel {
    #[must_use]
    pub fn active_tab(&self) -> PanelTab {
        self.tab
    }

    /// 우측 패널 탭을 전환한다. `Files` 탭으로 처음 들어가면 루트 폴더를 백그라운드에서 지연 로드한다.
    pub fn set_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        if self.tab == tab && (tab != PanelTab::Files || self.explorer.loaded_root) {
            return;
        }
        self.tab = tab;
        if tab == PanelTab::Files {
            self.ensure_explorer_loaded_async(cx);
        }
        if tab == PanelTab::Search
            && !self.search.options.query.is_empty()
            && self.search.result.files.is_empty()
            && self.search.error.is_none()
            && !self.search.searching
        {
            self.spawn_async_search(cx);
        }
        if tab == PanelTab::Checks && self.checks.inspection.is_none() && !self.checks.loading {
            self.refresh_checks(cx);
        }
        cx.notify();
    }

    /// 테스트 또는 외부 주입용 PR 서비스 설정.
    pub fn set_pr_service(&mut self, service: Arc<crate::pull_request::Service>) {
        self.editing.pr_service = service;
    }

    /// 테스트에서 실제 git 저장소 없이 브랜치 이름을 지정할 때 사용한다.
    pub fn set_checks_branch(&mut self, branch: impl Into<String>) {
        let branch = branch.into();
        self.checks.branch_override = Some(branch.clone());
        self.checks.branch = branch;
    }

    pub fn refresh_checks(&mut self, cx: &mut Context<Self>) {
        self.checks.loading = true;
        self.checks.error = None;
        self.checks.generation = self.checks.generation.wrapping_add(1);
        let generation = self.checks.generation;
        let repo = self.repo.clone();
        let branch_override = self.checks.branch_override.clone();
        let service = Arc::clone(&self.editing.pr_service);

        self.checks.task = Some(cx.spawn(async move |this, cx| {
            let (branch, result) = cx
                .background_executor()
                .spawn(async move {
                    let branch = resolve_branch(&repo, branch_override.as_deref());
                    let res = service.inspect(&repo, &branch);
                    (branch, res)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.checks.generation != generation {
                    return;
                }
                this.checks.loading = false;
                this.checks.branch = branch;
                match result {
                    Ok(inspection) => {
                        this.checks.selected_checks.clear();
                        this.checks.selected_comments.clear();
                        this.checks.expanded_logs = inspection
                            .checks
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.state == CheckState::Failed && c.log_tail.is_some())
                            .map(|(idx, _)| idx)
                            .collect();
                        this.checks.inspection = Some(inspection);
                        this.checks.error = None;
                    }
                    Err(err) => {
                        this.checks.error = Some(err.to_owned());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn toggle_check_selection(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(pr) = &self.checks.inspection else {
            return;
        };
        if idx >= pr.checks.len() {
            return;
        }
        if !self.checks.selected_checks.remove(&idx) {
            self.checks.selected_checks.insert(idx);
        }
        self.checks.fix_status = None;
        cx.notify();
    }

    pub(super) fn toggle_check_log(&mut self, idx: usize, cx: &mut Context<Self>) {
        if !self.checks.expanded_logs.remove(&idx) {
            self.checks.expanded_logs.insert(idx);
        }
        cx.notify();
    }

    pub(super) fn toggle_comment_selection(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(pr) = &self.checks.inspection else {
            return;
        };
        if idx >= pr.review_comments.len() {
            return;
        }
        if !self.checks.selected_comments.remove(&idx) {
            self.checks.selected_comments.insert(idx);
        }
        self.checks.fix_status = None;
        cx.notify();
    }

    pub(super) fn select_all_failed_checks(&mut self, cx: &mut Context<Self>) {
        let Some(pr) = &self.checks.inspection else {
            return;
        };
        let failed_indices: Vec<usize> = pr
            .checks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.state == CheckState::Failed)
            .map(|(i, _)| i)
            .collect();
        if failed_indices.is_empty() {
            return;
        }
        let all_selected = failed_indices
            .iter()
            .all(|idx| self.checks.selected_checks.contains(idx));
        if all_selected {
            for idx in failed_indices {
                self.checks.selected_checks.remove(&idx);
            }
        } else {
            for idx in failed_indices {
                self.checks.selected_checks.insert(idx);
            }
        }
        self.checks.fix_status = None;
        cx.notify();
    }

    pub(super) fn select_all_unresolved_comments(&mut self, cx: &mut Context<Self>) {
        let Some(pr) = &self.checks.inspection else {
            return;
        };
        let total = pr.review_comments.len();
        if total == 0 {
            return;
        }
        if self.checks.selected_comments.len() == total {
            self.checks.selected_comments.clear();
        } else {
            self.checks.selected_comments = (0..total).collect();
        }
        self.checks.fix_status = None;
        cx.notify();
    }

    pub(super) fn send_selected_pr_fixes(&mut self, cx: &mut Context<Self>) {
        let Some(pr) = &self.checks.inspection else {
            return;
        };
        let selected_checks: Vec<&CheckItem> = self
            .checks
            .selected_checks
            .iter()
            .filter_map(|&idx| pr.checks.get(idx))
            .collect();
        let selected_comments: Vec<&ReviewComment> = self
            .checks
            .selected_comments
            .iter()
            .filter_map(|&idx| pr.review_comments.get(idx))
            .collect();
        if selected_checks.is_empty() && selected_comments.is_empty() {
            return;
        }
        let prompt = build_fix_with_ai_prompt(&self.repo, pr, &selected_checks, &selected_comments);
        self.checks.fix_status = None;
        cx.emit(SendComments::checks_fix(prompt));
        cx.notify();
    }

    pub(super) fn checks_badge_count(&self) -> Option<usize> {
        let pr = self.checks.inspection.as_ref()?;
        if pr.status == Status::NoPr {
            return None;
        }
        let actionable = pr.count(CheckState::Failed) + pr.review_comments.len();
        Some(actionable)
    }

    pub(super) fn checks_status_color(&self, p: &crate::ui::Palette) -> Option<gpui::Hsla> {
        let pr = self.checks.inspection.as_ref()?;
        if pr.status == Status::NoPr {
            return None;
        }
        if pr.count(CheckState::Failed) > 0 || !pr.review_comments.is_empty() {
            Some(p.destructive)
        } else if pr.count(CheckState::Pending) > 0 {
            Some(p.warning)
        } else if pr.count(CheckState::Passed) > 0 {
            Some(p.success)
        } else {
            None
        }
    }

    fn render_check_row(
        &self,
        idx: usize,
        check: &CheckItem,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let selected = self.checks.selected_checks.contains(&idx);
        let expanded = self.checks.expanded_logs.contains(&idx);
        let (icon_name, status_color) = match check.state {
            CheckState::Passed => (IconName::CircleCheck, p.success),
            CheckState::Failed => (IconName::CircleX, p.destructive),
            CheckState::Pending => (IconName::Loader, p.warning),
            CheckState::Skipped => (IconName::Minus, p.muted_foreground),
        };
        let checkbox_mark = if selected { "☑" } else { "☐" };

        let mut card = div()
            .id(("pr-check-row", idx))
            .debug_selector(move || format!("pr-check-row-{idx}"))
            .px_2()
            .py_1p5()
            .rounded(crate::ui::RADIUS_XS)
            .border_1()
            .border_color(if selected { p.ring } else { p.sidebar_border })
            .bg(if selected { p.sidebar_accent } else { p.card })
            .flex()
            .flex_col()
            .gap_1();

        let top_row = div()
            .flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .child(
                div()
                    .id(("pr-check-select", idx))
                    .debug_selector(move || format!("pr-check-select-{idx}"))
                    .cursor_pointer()
                    .px_1()
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(if selected {
                        p.sidebar_foreground
                    } else {
                        p.muted_foreground
                    })
                    .child(checkbox_mark)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_check_selection(idx, cx);
                    })),
            )
            .child(Icon::new(icon_name).size(px(13.)).text_color(status_color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.sidebar_foreground)
                    .child(check.name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(p.muted)
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(status_color)
                    .child(check.state.label()),
            )
            .when(check.log_tail.is_some(), |d| {
                let label = if expanded {
                    text::LOG_HIDE.get()
                } else {
                    text::LOG_SHOW.get()
                };
                d.child(
                    Button::new(("pr-check-log", idx))
                        .debug_selector(move || format!("pr-check-log-{idx}"))
                        .ghost()
                        .xsmall()
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_check_log(idx, cx);
                        })),
                )
            });

        card = card.child(top_row);

        if let Some(desc) = &check.description
            && !desc.trim().is_empty()
        {
            card = card.child(
                div()
                    .pl_5()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(desc.clone()),
            );
        }

        if expanded && let Some(log_tail) = &check.log_tail {
            card = card.child(
                div()
                    .id(("pr-check-log-tail", idx))
                    .debug_selector(move || format!("pr-check-log-tail-{idx}"))
                    .mt_1()
                    .p_2()
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(p.background)
                    .border_1()
                    .border_color(p.sidebar_border)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_2XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.destructive)
                            .child(text::LOGS_HEADER.get()),
                    )
                    .child(
                        div()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.foreground)
                            .child(log_tail.clone()),
                    ),
            );
        }

        card
    }

    fn render_review_comment_row(
        &self,
        idx: usize,
        comment: &ReviewComment,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let selected = self.checks.selected_comments.contains(&idx);
        let checkbox_mark = if selected { "☑" } else { "☐" };

        let mut card = div()
            .id(("pr-comment-row", idx))
            .debug_selector(move || format!("pr-comment-row-{idx}"))
            .px_2()
            .py_1p5()
            .rounded(crate::ui::RADIUS_XS)
            .border_1()
            .border_color(if selected { p.ring } else { p.sidebar_border })
            .bg(if selected { p.sidebar_accent } else { p.card })
            .flex()
            .flex_col()
            .gap_1();

        let header = div()
            .flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .child(
                div()
                    .id(("pr-comment-select", idx))
                    .debug_selector(move || format!("pr-comment-select-{idx}"))
                    .cursor_pointer()
                    .px_1()
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(if selected {
                        p.sidebar_foreground
                    } else {
                        p.muted_foreground
                    })
                    .child(checkbox_mark)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_comment_selection(idx, cx);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_2XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.sidebar_foreground)
                    .child(comment.location()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(format!("@{}", comment.author)),
            )
            .when(comment.is_bot, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(crate::ui::RADIUS_XS)
                        .bg(p.muted)
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(text::BOT_BADGE.get()),
                )
            })
            .when(comment.is_outdated, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(crate::ui::RADIUS_XS)
                        .bg(p.muted)
                        .text_size(crate::ui::TEXT_2XS)
                        .text_color(p.warning)
                        .child(text::OUTDATED_BADGE.get()),
                )
            });

        card = card.child(header).child(
            div()
                .pl_5()
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.sidebar_foreground)
                .child(comment.body.clone()),
        );

        if !comment.replies.is_empty() {
            let replies = comment.replies.iter().map(|reply| {
                div()
                    .pl_5()
                    .pt_0p5()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(format!("@{}: {}", reply.author, reply.body))
            });
            card = card.children(replies);
        }

        card
    }

    pub(super) fn render_checks_tab(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = crate::ui::palette(cx);

        if self.checks.loading && self.checks.inspection.is_none() {
            return Self::render_empty_state(
                "pr-checks-loading",
                IconName::Loader,
                text::LOADING_CHECKS.get(),
                "",
                cx,
            )
            .into_any_element();
        }

        if let Some(err) = &self.checks.error {
            return Self::render_empty_state("pr-checks-error", IconName::CircleAlert, err, "", cx)
                .into_any_element();
        }

        let Some(pr) = &self.checks.inspection else {
            return Self::render_empty_state(
                "pr-checks-empty",
                IconName::CircleCheck,
                text::NO_PR_TITLE.get(),
                text::NO_PR_DESC.get(),
                cx,
            )
            .into_any_element();
        };

        if pr.status == Status::NoPr {
            return div()
                .id("pr-checks-no-pr")
                .debug_selector(|| "pr-checks-no-pr".into())
                .flex_1()
                .min_h_0()
                .px_4()
                .py_6()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(p.sidebar)
                .child(
                    Icon::new(IconName::CircleCheck)
                        .size(px(22.))
                        .text_color(p.muted_foreground),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.sidebar_foreground)
                        .child(text::NO_PR_TITLE.get()),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NO_PR_DESC.get()),
                )
                .child(
                    Button::new("pr-checks-create")
                        .debug_selector(|| "pr-checks-create".into())
                        .small()
                        .outline()
                        .label(text::CREATE.get())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_pr_dialog(window, cx);
                        })),
                )
                .into_any_element();
        }

        let passed = pr.count(CheckState::Passed);
        let failed = pr.count(CheckState::Failed);
        let pending = pr.count(CheckState::Pending);
        let skipped = pr.count(CheckState::Skipped);
        let badge_text = pr.status.badge().unwrap_or(text::OPEN.get());

        let pr_header =
            div()
                .id("pr-checks-header")
                .debug_selector(|| "pr-checks-header".into())
                .px_2p5()
                .py_2()
                .flex_none()
                .flex()
                .flex_col()
                .gap_1()
                .border_b_1()
                .border_color(p.sidebar_border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .min_w_0()
                        .child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded(crate::ui::RADIUS_XS)
                                .bg(p.sidebar_accent)
                                .font_family(crate::ui::FONT_MONO)
                                .text_size(crate::ui::TEXT_2XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.sidebar_accent_foreground)
                                .child(badge_text),
                        )
                        .when_some(pr.number, |d, num| {
                            d.child(
                                div()
                                    .flex_none()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(format!("#{num}")),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::ui::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.sidebar_foreground)
                                .child(if pr.title.is_empty() {
                                    self.checks.branch.clone()
                                } else {
                                    pr.title.clone()
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .text_size(crate::ui::TEXT_2XS)
                        .child(
                            div()
                                .text_color(p.destructive)
                                .child(text::SUMMARY_FAILED.format(&[("n", &failed.to_string())])),
                        )
                        .child(
                            div().text_color(p.warning).child(
                                text::SUMMARY_PENDING.format(&[("n", &pending.to_string())]),
                            ),
                        )
                        .child(
                            div()
                                .text_color(p.success)
                                .child(text::SUMMARY_PASSED.format(&[("n", &passed.to_string())])),
                        )
                        .when(skipped > 0, |d| {
                            d.child(div().text_color(p.muted_foreground).child(
                                text::SUMMARY_SKIPPED.format(&[("n", &skipped.to_string())]),
                            ))
                        }),
                );

        let check_rows: Vec<AnyElement> = pr
            .checks
            .iter()
            .enumerate()
            .map(|(i, check)| self.render_check_row(i, check, cx).into_any_element())
            .collect();

        let comment_rows: Vec<AnyElement> = pr
            .review_comments
            .iter()
            .enumerate()
            .map(|(i, comment)| {
                self.render_review_comment_row(i, comment, cx)
                    .into_any_element()
            })
            .collect();

        let checks_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.sidebar_foreground)
                                    .child(text::CHECKS_SECTION.get()),
                            )
                            .child(
                                div()
                                    .px_1()
                                    .rounded(crate::ui::RADIUS_XS)
                                    .bg(p.muted)
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(pr.checks.len().to_string()),
                            ),
                    )
                    .when(failed > 0, |d| {
                        d.child(
                            Button::new("pr-select-all-checks")
                                .debug_selector(|| "pr-select-all-checks".into())
                                .ghost()
                                .xsmall()
                                .label(text::SELECT_ALL_FAILED.get())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.select_all_failed_checks(cx);
                                })),
                        )
                    }),
            )
            .when(check_rows.is_empty(), |d| {
                d.child(
                    div()
                        .py_2()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::CHECKS_EMPTY.get()),
                )
            })
            .children(check_rows);

        let comments_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .pt_2()
            .border_t_1()
            .border_color(p.sidebar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.sidebar_foreground)
                                    .child(text::COMMENTS_SECTION.get()),
                            )
                            .child(
                                div()
                                    .px_1()
                                    .rounded(crate::ui::RADIUS_XS)
                                    .bg(p.muted)
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(pr.review_comments.len().to_string()),
                            ),
                    )
                    .when(!pr.review_comments.is_empty(), |d| {
                        d.child(
                            Button::new("pr-select-all-comments")
                                .debug_selector(|| "pr-select-all-comments".into())
                                .ghost()
                                .xsmall()
                                .label(text::SELECT_ALL_COMMENTS.get())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.select_all_unresolved_comments(cx);
                                })),
                        )
                    }),
            )
            .when(comment_rows.is_empty(), |d| {
                d.child(
                    div()
                        .py_2()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::COMMENTS_EMPTY.get()),
                )
            })
            .children(comment_rows);

        let selected_total = self.checks.selected_total();
        let fix_label = if selected_total > 0 {
            format!("{} ({selected_total})", text::FIX_WITH_AI.get())
        } else {
            text::FIX_WITH_AI.get().to_owned()
        };
        let no_agent = self.checks.fix_status == Some(text::FIX_NO_AGENT);
        let is_error = no_agent || self.checks.fix_status == Some(text::FIX_FAILED);

        let footer = div()
            .id("pr-fix-footer")
            .debug_selector(|| "pr-fix-footer".into())
            .flex_none()
            .p_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .border_t_1()
            .border_color(p.sidebar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(if selected_total > 0 {
                        text::SELECTED_COUNT.format(&[("n", &selected_total.to_string())])
                    } else {
                        text::FIX_QUEUE_HINT.get().to_owned()
                    })
                    .when(selected_total > 0, |d| {
                        d.child(
                            Button::new("pr-clear-selection")
                                .debug_selector(|| "pr-clear-selection".into())
                                .ghost()
                                .xsmall()
                                .label(text::CLEAR_SELECTION.get())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.checks.selected_checks.clear();
                                    this.checks.selected_comments.clear();
                                    this.checks.fix_status = None;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                Button::new("pr-fix-with-ai")
                    .debug_selector(|| "pr-fix-with-ai".into())
                    .primary()
                    .icon(Icon::default().data(crate::ui::icons::SPARKLES_SVG))
                    .label(fix_label)
                    .disabled(selected_total == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.send_selected_pr_fixes(cx);
                    })),
            )
            .when_some(self.checks.fix_status, |d, status| {
                d.child(
                    div()
                        .debug_selector(move || {
                            if no_agent {
                                "pr-fix-no-agent".into()
                            } else {
                                "pr-fix-status".into()
                            }
                        })
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(if is_error {
                            p.destructive
                        } else {
                            p.muted_foreground
                        })
                        .child(status.get()),
                )
            });

        div()
            .id("pr-checks-panel")
            .debug_selector(|| "pr-checks-panel".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(pr_header)
            .child(
                div()
                    .id("pr-checks-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(checks_section)
                    .child(comments_section),
            )
            .child(footer)
            .into_any_element()
    }
}
