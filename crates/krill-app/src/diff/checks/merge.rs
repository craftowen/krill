//! 짧은 병합 확인과 확인 시점 대상 고정. 원격 작업은 확인 버튼에서만 시작한다.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AnyElement, Context, FocusHandle, Window, div, prelude::*};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Sizable as _};

use super::super::Panel;
use crate::i18n::catalog::pull_request as text;
use crate::pull_request::{PrInspection, PrMergeMethod};

pub(super) struct Confirmation {
    repo: PathBuf,
    branch: String,
    branch_override: Option<String>,
    generation: u64,
    inspection: PrInspection,
    method: PrMergeMethod,
    focus: FocusHandle,
}

impl Panel {
    pub fn merge_pr(&mut self, method: PrMergeMethod, window: &mut Window, cx: &mut Context<Self>) {
        // 원격 호스트의 PR은 로컬 gh로 병합할 수 없다(같은 경로의 로컬 PR을 병합하면 안 된다, APP-F01).
        if self.checks.loading
            || self.checks.merging
            || self.checks.pending_merge.is_some()
            || self.remote_host()
        {
            return;
        }
        let Some(inspection) = self
            .checks
            .inspection
            .as_ref()
            .filter(|pr| pr.mergeable_snapshot())
        else {
            self.checks.action_error = Some(text::MERGE_TARGET_MISSING.get().to_owned());
            cx.notify();
            return;
        };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.checks.pending_merge = Some(Confirmation {
            repo: self.repo.clone(),
            branch: self.checks.branch.clone(),
            branch_override: self.checks.branch_override.clone(),
            generation: self.checks.generation,
            inspection: inspection.clone(),
            method,
            focus,
        });
        self.checks.action_error = None;
        cx.notify();
    }

    fn cancel_merge(&mut self, cx: &mut Context<Self>) {
        self.checks.pending_merge = None;
        cx.notify();
    }

    fn confirm_merge(&mut self, cx: &mut Context<Self>) {
        // 먼저 소비해 같은 프레임의 연속 클릭도 중복 병합을 만들지 않는다.
        let Some(confirmation) = self.checks.pending_merge.take() else {
            return;
        };
        if self.checks.loading
            || self.checks.merging
            || self.remote_host()
            || self.repo != confirmation.repo
            || self.checks.branch != confirmation.branch
            || self.checks.branch_override != confirmation.branch_override
            || self.checks.generation != confirmation.generation
            || self.checks.inspection.as_ref() != Some(&confirmation.inspection)
        {
            self.checks.action_error = Some(text::MERGE_TARGET_CHANGED.get().to_owned());
            cx.notify();
            return;
        }
        self.checks.loading = true;
        self.checks.merging = true;
        self.checks.fix_status = None;
        self.checks.action_error = None;
        let service = Arc::clone(&self.editing.pr_service);
        let repo = confirmation.repo.clone();
        let generation = confirmation.generation;
        self.checks.merge_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    service.merge_confirmed(
                        &confirmation.repo,
                        &confirmation.branch,
                        &confirmation.inspection,
                        confirmation.method,
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                // The retained task owns the busy state even after a repository switch.
                this.checks.loading = false;
                this.checks.merging = false;
                if this.repo != repo || this.checks.generation != generation {
                    if this.visible && this.tab == super::PanelTab::Checks {
                        this.refresh_checks(cx);
                    }
                    cx.notify();
                    return;
                }
                match result {
                    Ok(()) => {
                        this.checks.fix_status = Some(text::ACTION_SUCCESS);
                        this.refresh_checks(cx);
                    }
                    Err(reason) => {
                        this.checks.action_error =
                            Some(text::ACTION_FAILED.format(&[("reason", &reason)]));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn render_merge_confirmation(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let confirmation = self.checks.pending_merge.as_ref()?;
        let pr = &confirmation.inspection;
        let metadata = pr.metadata.as_ref()?;
        let p = crate::ui::palette(cx);
        let method = match confirmation.method {
            PrMergeMethod::Squash => text::MERGE_SQUASH,
            PrMergeMethod::Rebase => text::MERGE_REBASE,
            PrMergeMethod::Merge => text::MERGE_COMMIT,
        };
        let detail = |selector: &'static str, value: String| {
            let debug_value = value.clone();
            div()
                .debug_selector(move || format!("{selector}:{debug_value}"))
                .text_size(crate::ui::TEXT_SM)
                .child(value)
        };
        let card = div()
            .id("pr-merge-confirmation")
            .debug_selector(|| "pr-merge-confirmation".into())
            .occlude()
            .track_focus(&confirmation.focus)
            .w(crate::ui::DIALOG_WIDTH)
            .max_w(window.viewport_size().width - crate::ui::SPACE_4 * 2.)
            .p_6()
            .rounded(crate::ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(crate::ui::FONT_UI)
            .text_size(crate::ui::TEXT_MD)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_3()
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.cancel_merge(cx),
                    "enter"
                        if this
                            .checks
                            .pending_merge
                            .as_ref()
                            .is_some_and(|confirmation| confirmation.focus.is_focused(window)) =>
                    {
                        this.confirm_merge(cx);
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.cancel_merge(cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::MERGE_CONFIRM_TITLE.get()),
                    )
                    .child(
                        Button::new("pr-merge-close")
                            .debug_selector(|| "pr-merge-close".into())
                            .ghost()
                            .small()
                            .label(text::MERGE_CLOSE.get())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_merge(cx))),
                    ),
            )
            .child(detail(
                "pr-merge-repository",
                text::MERGE_REPOSITORY.format(&[("repo", &pr.url)]),
            ))
            .child(detail(
                "pr-merge-worktree",
                text::MERGE_WORKTREE.format(&[("path", &confirmation.repo.display().to_string())]),
            ))
            .child(detail(
                "pr-merge-branch",
                text::MERGE_BRANCH.format(&[("branch", &confirmation.branch)]),
            ))
            .child(detail(
                "pr-merge-number",
                format!("PR #{} · {}", pr.number.unwrap_or_default(), pr.title),
            ))
            .child(detail(
                "pr-merge-refs",
                text::MERGE_REFS.format(&[
                    ("base", &metadata.base_ref_name),
                    ("head", &metadata.head_ref_name),
                ]),
            ))
            .child(detail(
                "pr-merge-method",
                text::MERGE_METHOD.format(&[("method", method.get())]),
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("pr-merge-cancel")
                            .debug_selector(|| "pr-merge-cancel".into())
                            .ghost()
                            .small()
                            .label(text::MERGE_CANCEL.get())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_merge(cx))),
                    )
                    .child(
                        Button::new("pr-merge-confirm")
                            .debug_selector(|| "pr-merge-confirm".into())
                            .primary()
                            .small()
                            .label(text::MERGE_CONFIRM.get())
                            .disabled(self.checks.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_merge(cx))),
                    ),
            );
        Some(crate::ui::modal_backdrop(window.viewport_size(), card).into_any_element())
    }
}

#[cfg(test)]
mod tests;
