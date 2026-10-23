use super::*;

impl Shell {
    /// In-app document libraries and settings suspend terminal input without closing sessions.
    pub(super) fn page_open(&self) -> bool {
        self.settings.is_some() || self.artifacts.is_some()
    }

    pub(super) fn open_artifacts(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self.role == WindowRole::Floating {
            return;
        }
        if let Some(pane) = self.layout.focused() {
            self.cancel_pending_paste_for(pane, cx);
            self.commit_ime(pane, window.as_deref(), cx);
        }
        self.record_workspace_nav(cx);
        self.settings = None;
        self.jump = None;
        self.continue_session = None;
        self.dashboard = None;
        self.sync_sidebar_visibility(cx);
        self.files.close_quick_open();
        if self.search.take().is_some()
            && let Some(live) = self.focused_live()
        {
            live.pane.clear_search();
        }
        if self.artifacts.is_none() {
            #[cfg(not(test))]
            let state = self.state_dir(cx);
            #[cfg(not(test))]
            let view = cx.new(|cx| krill_app::artifacts::View::new(state, cx));
            #[cfg(test)]
            let view = cx.new(|cx| {
                krill_app::artifacts::View::with_call(
                    Arc::new(|_| Ok(krill_app::artifacts::Page::default())),
                    cx,
                )
            });
            cx.subscribe(&view, |this, _, _: &krill_app::artifacts::Close, cx| {
                this.close_settings(None, cx);
            })
            .detach();
            self.artifacts = Some(view);
        }
        self.nav_history.push(NavEntry::Artifacts);
        self.sync_focused_session(cx);
        if let Some(window) = window {
            if let Some(view) = &self.artifacts {
                window.focus(&view.read(cx).focus_handle(), cx);
            }
            self.pending_focus = PendingFocus::None;
        } else {
            self.pending_focus = PendingFocus::Artifacts;
        }
        cx.notify();
    }
}
