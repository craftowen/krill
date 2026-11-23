use super::*;

impl SettingsView {
    pub(super) fn save_worktree_location(&mut self, raw: String, cx: &mut Context<Self>) {
        let Some(dir) = state_dir(cx) else { return };
        if !self.workflow_hooks_ready(cx) {
            return;
        }
        let Some(form) = &mut self.workflow_form else {
            return;
        };
        form.location_status = None;
        let repo = form.repo_path.read(cx).value().trim().to_owned();
        let saving_repo = repo.clone();
        let saved = raw.trim().to_owned();
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
                    match result {
                        // 저장한 값이 새 기준이다. 결과 문구는 저장 버튼 옆에 나오되 훅 실패를 덮지 않는다.
                        Ok(()) => {
                            form.loaded_location = saved;
                            form.location_status = None;
                            if !form.status.as_ref().is_some_and(|(_, error)| *error) {
                                form.status = Some((text::HOOKS_SAVED.get().into(), false));
                            }
                        }
                        Err(error) => form.location_status = Some((error.into(), true)),
                    }
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
            .gap_1()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(crate::ui::field_label(
                        text::WORKTREE_LOCATION_TITLE.get(),
                        None,
                        p,
                    ))
                    .child(
                        layout::ghost(
                            "admin-worktree-location-default",
                            text::WORKTREE_LOCATION_DEFAULT.get(),
                        )
                        .debug_selector(|| "admin-worktree-location-default".into())
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(form) = &this.workflow_form {
                                form.worktree_location.update(cx, |input, cx| {
                                    input.set_value("", window, cx);
                                });
                            }
                            this.save_worktree_location(String::new(), cx);
                        })),
                    ),
            )
            .child(
                div()
                    .id("admin-worktree-location-input")
                    .debug_selector(|| "admin-worktree-location-input".into())
                    .w_full()
                    .min_w_0()
                    .child(gpui_component::input::Input::new(&input).small()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn save_worktree_location_clears_prior_error_on_default_reset(cx: &mut gpui::TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("krill-location-clear-err-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo_str = repo.to_string_lossy().into_owned();
        let d = dir.clone();
        cx.update(|cx| {
            gpui_component::init(cx);
            init_at(Some(d), cx);
        });
        cx.run_until_parked();

        let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.services
                    .aliases
                    .insert(repo_str.clone(), repo_str.clone());
                let mk = |val: &str, window: &mut Window, cx: &mut Context<SettingsView>| {
                    let s = val.to_owned();
                    cx.new(|cx| gpui_component::input::InputState::new(window, cx).default_value(s))
                };
                v.workflow_form = Some(WorkflowRepoForm {
                    repo_path: mk(&repo_str, window, cx),
                    symlink_path: mk("", window, cx),
                    setup_script: mk("", window, cx),
                    archive_script: mk("", window, cx),
                    pending_repo_path: None,
                    loaded_repo: Some(repo_str.clone()),
                    loaded_setup: String::new(),
                    loaded_archive: String::new(),
                    worktree_location: mk("", window, cx),
                    loaded_location: "/bad".into(),
                    location_status: Some(("invalid location".into(), true)),
                    symlink_status: None,
                    refresh_hooks: false,
                    status: None,
                    _subs: Vec::new(),
                });
                v.save_worktree_location(String::new(), cx);
                assert!(
                    v.workflow_form.as_ref().unwrap().location_status.is_none(),
                    "prior location_status error must be cleared when starting save"
                );
            });
        });
        vcx.run_until_parked();
        vcx.read_entity(&view, |v, _| {
            let form = v.workflow_form.as_ref().unwrap();
            assert!(
                form.location_status.is_none(),
                "location_status error must remain cleared after Ok(())"
            );
            assert!(form.loaded_location.is_empty());
        });
        std::fs::remove_dir_all(&dir).ok();
    }
}
