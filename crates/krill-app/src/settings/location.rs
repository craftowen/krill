use super::*;

impl SettingsView {
    fn save_worktree_location(&mut self, raw: String, cx: &mut Context<Self>) {
        let Some(dir) = state_dir(cx) else { return };
        if !self.workflow_hooks_ready(cx) {
            return;
        }
        let Some(form) = &self.workflow_form else {
            return;
        };
        let repo = form.repo_path.read(cx).value().trim().to_owned();
        let saving_repo = repo.clone();
        self.file_job(
            "worktree-location-edit",
            move || {
                crate::sidebar::external::save_project_worktree_location(&dir, &repo, &raw)
                    .map_err(|error| error.to_string())
            },
            move |this, result, cx| {
                if let Some(form) = &mut this.workflow_form
                    && form.repo_path.read(cx).value().trim() == saving_repo
                {
                    form.location_status = Some(match result {
                        Ok(()) => (text::HOOKS_SAVED.get().into(), false),
                        Err(error) => (error.into(), true),
                    });
                }
                this.refresh_service_cache(cx);
                cx.defer(notify_external_change);
            },
            cx,
        );
    }

    pub(super) fn render_worktree_location(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = crate::ui::palette(cx);
        let Some(form) = &self.workflow_form else {
            return div().into_any_element();
        };
        let input = form.worktree_location.clone();
        let busy = self.file_jobs.contains_key("worktree-location-edit");
        let ready = self.workflow_hooks_ready(cx) && !busy;
        let mut section = div()
            .id("admin-worktree-location")
            .debug_selector(|| "admin-worktree-location".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(crate::ui::field_label(
                text::WORKTREE_LOCATION_TITLE.get(),
                None,
                p,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("admin-worktree-location-input")
                            .debug_selector(|| "admin-worktree-location-input".into())
                            .flex_1()
                            .min_w_0()
                            .child(gpui_component::input::Input::new(&input).small()),
                    )
                    .child(
                        layout::controls()
                            .child(
                                layout::ghost(
                                    "admin-worktree-location-default",
                                    text::WORKTREE_LOCATION_DEFAULT.get(),
                                )
                                .debug_selector(|| "admin-worktree-location-default".into())
                                .disabled(!ready)
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if let Some(form) = &this.workflow_form {
                                            form.worktree_location.update(cx, |input, cx| {
                                                input.set_value("", window, cx);
                                            });
                                        }
                                        this.save_worktree_location(String::new(), cx);
                                    },
                                )),
                            )
                            .child(
                                layout::primary(
                                    "admin-worktree-location-save",
                                    text::QC_SAVE.get(),
                                )
                                .debug_selector(|| "admin-worktree-location-save".into())
                                .disabled(!ready)
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if let Some(form) = &this.workflow_form {
                                            let value =
                                                form.worktree_location.read(cx).value().to_string();
                                            this.save_worktree_location(value, cx);
                                        }
                                    },
                                )),
                            ),
                    ),
            )
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::WORKTREE_LOCATION_DESC.get()),
            );
        if let Some((message, error)) = &form.location_status {
            section = section.child(
                div()
                    .id("admin-worktree-location-status")
                    .debug_selector(|| "admin-worktree-location-status".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(if *error {
                        p.destructive
                    } else {
                        p.muted_foreground
                    })
                    .child(message.clone()),
            );
        }
        section.into_any_element()
    }
}
