//! Bounded reload recovery and a read-only comparison of the draft and disk.
use super::*;
use gpui::Entity;
use gpui_component::input::{Position, Rope};

pub(super) struct ReloadRecovery {
    text: Rope,
    cursor: Position,
    scroll: gpui::Point<Pixels>,
    revision: u64,
    state_id: gpui::EntityId,
}

impl ReloadRecovery {
    pub(super) fn advance(&mut self, revision: u64) {
        self.revision = revision;
    }

    pub(super) fn capture(state: &Entity<EditorState>, revision: u64, cx: &App) -> Self {
        let editor = state.read(cx);
        Self {
            text: editor.text().clone(),
            cursor: editor.cursor_position(),
            scroll: editor.scroll_offset(),
            revision,
            state_id: state.entity_id(),
        }
    }
}

pub(super) struct FileComparison {
    pub(super) draft: Entity<EditorState>,
    pub(super) disk: Entity<EditorState>,
    stamp: editor::Stamp,
}

pub(super) struct ComparedVersion {
    stamp: editor::Stamp,
    text: Rope,
}

/// Refuse a changed/unstable disk version, including changes since the comparison.
/// This uses the same metadata fingerprint as saving and the watcher.
pub(super) fn read_current_file(
    path: &Path,
    preview: bool,
    expected: Option<ComparedVersion>,
) -> PreparedFile {
    let mut loaded = read_file(path, preview);
    if let Ok((_, stamp)) = &loaded.opened {
        let stable = editor::stamp(path).is_ok_and(|now| now == Some(*stamp));
        let changed = expected.is_some_and(|expected| {
            expected.stamp != *stamp
                || match &loaded.opened {
                    Ok((Opened::Text(_, text), _)) => !expected.text.eq(text.as_str()),
                    _ => true,
                }
        });
        if !stable || changed {
            loaded.opened = Err(std::io::Error::other(etext::COMPARE_STALE.get()));
            loaded.baseline = None;
            loaded.preview = None;
        }
    }
    loaded
}

impl Shell {
    pub(super) fn compare_file(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.files.tabs.get(&pane) else {
            return;
        };
        if tab.saving || self.files.loading.contains_key(&pane) {
            return;
        }
        let (Some(doc), Body::Text { state, .. }) = (&tab.doc, &tab.body) else {
            return;
        };
        let before = doc.clone();
        let path = doc.path().to_owned();
        let revision = tab.revision;
        let state_id = state.entity_id();
        let draft = state.read(cx).text().clone();
        self.files.next_request = self.files.next_request.wrapping_add(1);
        let generation = self.files.next_request;
        self.files.loading.insert(pane, (generation, path.clone()));
        #[cfg(test)]
        let gate = self.files.next_io.take();
        cx.spawn_in(window, async move |this, cx| {
            let read_path = path.clone();
            let (loaded, draft) = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(gate) = gate {
                        gate.await.ok();
                    }
                    (
                        read_current_file(&read_path, false, None),
                        draft.to_string(),
                    )
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.files.loading.get(&pane) != Some(&(generation, path.clone())) {
                    return;
                }
                this.files.loading.remove(&pane);
                let Some(tab) = this.files.tabs.get_mut(&pane) else {
                    return;
                };
                let Body::Text { state, .. } = &tab.body else {
                    return;
                };
                if tab.revision != revision
                    || tab.saving
                    || state.entity_id() != state_id
                    || tab.doc.as_ref() != Some(&before)
                    || this.layout.file(pane) != Some(path.as_path())
                {
                    return;
                }
                match loaded.opened {
                    Ok((Opened::Text(_, disk), stamp)) => {
                        let draft = cx.new(|cx| {
                            EditorState::new(window, cx)
                                .line_number(true)
                                .default_value(draft)
                        });
                        let disk = cx.new(|cx| {
                            EditorState::new(window, cx)
                                .line_number(true)
                                .default_value(disk)
                        });
                        tab.comparison = Some(FileComparison {
                            draft: draft.clone(),
                            disk,
                            stamp,
                        });
                        tab.error = None;
                        draft.focus_handle(cx).focus(window, cx);
                    }
                    Ok((other, _)) => tab.error = Some(not_text(&other)),
                    Err(e) => {
                        tab.error = Some(etext::OPEN_FAILED.format(&[("error", &e.to_string())]));
                    }
                }
                this.files.cache_count = usize::MAX;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn cancel_file_comparison(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if let Some(tab) = self.files.tabs.get_mut(&pane) {
            tab.comparison = None;
            self.files.loading.remove(&pane);
            self.files.refocus = true;
            self.files.cache_count = usize::MAX;
            cx.notify();
        }
    }

    pub(super) fn apply_file_comparison(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(expected) = self
            .files
            .tabs
            .get(&pane)
            .and_then(|tab| tab.comparison.as_ref())
            .map(|comparison| ComparedVersion {
                stamp: comparison.stamp,
                text: comparison.disk.read(cx).text().clone(),
            })
        else {
            return;
        };
        self.reload_file_checked(pane, Some(expected), window, cx);
    }

    pub(super) fn restore_reload_draft(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.files.tabs.get_mut(&pane) else {
            return;
        };
        if tab.saving || self.files.loading.contains_key(&pane) {
            return;
        }
        let Some(recovery) = tab.recovery.take() else {
            return;
        };
        let Body::Text { state, shown, .. } = &mut tab.body else {
            return;
        };
        if tab.revision != recovery.revision || state.entity_id() != recovery.state_id {
            return;
        }
        state.update(cx, |state, cx| {
            state.set_value(recovery.text.to_string(), window, cx);
            state.set_cursor_position(recovery.cursor, window, cx);
            state.set_scroll_offset(recovery.scroll, cx);
        });
        *shown = None;
        tab.comparison = None;
        tab.error = None;
        // Keep the newly read disk baseline. The restored draft must be compared
        // against that baseline, not its old saved content (which could look clean).
        self.file_edited(pane, cx);
        self.files.refocus = true;
        self.files.cache_count = usize::MAX;
        cx.notify();
    }

    pub(super) fn reload_recovery_banner(
        &self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let tab = self.files.tabs.get(&pane)?;
        tab.recovery.as_ref()?;
        let p = krill_app::ui::palette(cx);
        Some(
            div()
                .debug_selector(|| "file-reload-recovery".into())
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1()
                .bg(p.sidebar)
                .text_color(p.muted_foreground)
                .text_size(krill_app::ui::TEXT_XS)
                .child(etext::RELOAD_RECOVERY.get())
                .child(
                    Button::new(("file-restore-draft", pane.get()))
                        .debug_selector(|| "file-restore-draft".into())
                        .label(etext::RESTORE_DRAFT.get())
                        .outline()
                        .xsmall()
                        .disabled(tab.saving || self.files.loading.contains_key(&pane))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.restore_reload_draft(pane, window, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_file_comparison(
        &self,
        pane: PaneId,
        comparison: &FileComparison,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = krill_app::ui::palette(cx);
        let version = |label: &'static str, selector: &'static str, state: &Entity<EditorState>| {
            div()
                .debug_selector(move || selector.into())
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(krill_app::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(label),
                )
                .child(
                    div().flex_1().min_h_0().child(
                        Editor::new(state)
                            .readonly(true)
                            .aria_label(label)
                            .bordered(false)
                            .h(gpui::relative(1.)),
                    ),
                )
        };
        div()
            .id(("file-comparison", pane.get()))
            .debug_selector(|| "file-comparison".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(p.editor_surface)
            .capture_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    this.cancel_file_comparison(pane, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .flex_1()
                            .text_size(krill_app::ui::TEXT_XS)
                            .child(etext::COMPARE_READONLY.get()),
                    )
                    .child(
                        Button::new(("file-compare-cancel", pane.get()))
                            .debug_selector(|| "file-compare-cancel".into())
                            .label(etext::CANCEL.get())
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.cancel_file_comparison(pane, cx);
                            })),
                    )
                    .child(
                        Button::new(("file-compare-apply", pane.get()))
                            .debug_selector(|| "file-compare-apply".into())
                            .label(etext::RELOAD.get())
                            .primary()
                            .xsmall()
                            .disabled(self.files.loading.contains_key(&pane))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.apply_file_comparison(pane, window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(version(
                        etext::COMPARE_DRAFT.get(),
                        "file-compare-draft",
                        &comparison.draft,
                    ))
                    .child(version(
                        etext::COMPARE_DISK.get(),
                        "file-compare-disk",
                        &comparison.disk,
                    )),
            )
            .into_any_element()
    }
}
