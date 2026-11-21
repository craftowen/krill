use super::*;

impl Shell {
    /// An open modal suspends terminal input without closing sessions.
    pub(super) fn page_open(&self) -> bool {
        self.modal.is_some()
    }

    pub(super) fn settings(&self) -> Option<&gpui::Entity<krill_app::settings::SettingsView>> {
        match &self.modal {
            Some(Modal::Settings(view)) => Some(view),
            _ => None,
        }
    }

    pub(super) fn replace_modal(&mut self, next: Option<Modal>, cx: &mut Context<Self>) -> bool {
        let previous = std::mem::replace(&mut self.modal, next);
        if let Some(Modal::Settings(settings)) = &previous {
            let settings = settings.clone();
            // Back/Escape can invoke this host while SettingsView itself is
            // borrowed by its handler; stop hidden polling after that borrow ends.
            cx.defer(move |cx| settings.update(cx, |view, cx| view.set_visible(false, cx)));
        } else if let Some(Modal::Artifacts(view)) = &previous {
            let view = view.clone();
            cx.defer(move |cx| view.update(cx, krill_app::artifacts::View::on_hide));
        } else if let Some(Modal::Bots(view)) = &previous {
            let view = view.clone();
            cx.defer(move |cx| view.update(cx, krill_app::bots::View::on_hide));
        }
        previous.is_some()
    }

    pub(super) fn focus_modal_or_workspace(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if let Some(window) = window {
            let focus = self
                .modal
                .as_ref()
                .map_or_else(|| self.focus.clone(), |modal| modal.focus_handle(cx));
            window.focus(&focus, cx);
            self.pending_focus = PendingFocus::None;
        } else if self.modal.is_some() {
            self.pending_focus = PendingFocus::Modal;
        } else {
            self.pending_focus = PendingFocus::Workspace;
        }
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
        if !matches!(self.modal, Some(Modal::Artifacts(_))) {
            let view = if let Some(cached) = self.artifacts_cache.clone() {
                cached.update(cx, krill_app::artifacts::View::reopen);
                cached
            } else {
                #[cfg(not(test))]
                let state = self.state_dir(cx);
                #[cfg(not(test))]
                let created = cx.new(|cx| krill_app::artifacts::View::new(state, cx));
                #[cfg(test)]
                let created = cx.new(|cx| {
                    krill_app::artifacts::View::with_call(
                        Arc::new(|_| Ok(krill_app::artifacts::Page::default())),
                        cx,
                    )
                });
                cx.subscribe(&created, |this, _, _: &krill_app::artifacts::Close, cx| {
                    this.close_settings(None, cx);
                })
                .detach();
                self.artifacts_cache = Some(created.clone());
                created
            };
            self.replace_modal(Some(Modal::Artifacts(view)), cx);
        }
        self.nav_history.push(NavEntry::Artifacts);
        self.sync_focused_session(cx);
        self.focus_modal_or_workspace(window, cx);
        cx.notify();
    }

    pub(super) fn open_automations(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self.role == WindowRole::Floating {
            return;
        }
        if let Some(pane) = self.layout.focused() {
            self.cancel_pending_paste_for(pane, cx);
            self.commit_ime(pane, window.as_deref(), cx);
        }
        self.record_workspace_nav(cx);
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
        if let Some(Modal::Automations(view)) = &self.modal {
            view.update(cx, krill_app::automations::View::reload);
        } else {
            let state = self.state_dir(cx);
            let default_cwd = self
                .sidebar
                .read(cx)
                .selected_workspace()
                .map(|(_, path)| path)
                .or_else(|| {
                    self.layout
                        .focused()
                        .and_then(|p| self.worktree_for_pane(p, cx))
                });
            let view = cx.new(|cx| krill_app::automations::View::new(state, default_cwd, cx));
            cx.subscribe(&view, |this, _, _: &krill_app::automations::Close, cx| {
                this.close_settings(None, cx);
            })
            .detach();
            self.replace_modal(Some(Modal::Automations(view)), cx);
        }
        self.nav_history.push(NavEntry::Automations);
        self.sync_focused_session(cx);
        self.focus_modal_or_workspace(window, cx);
        cx.notify();
    }

    pub(super) fn open_bots(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self.role == WindowRole::Floating {
            return;
        }
        if let Some(pane) = self.layout.focused() {
            self.cancel_pending_paste_for(pane, cx);
            self.commit_ime(pane, window.as_deref(), cx);
        }
        self.record_workspace_nav(cx);
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
        if !matches!(self.modal, Some(Modal::Bots(_))) {
            let view = if let Some(cached) = self.bots_cache.clone() {
                cached.update(cx, krill_app::bots::View::reopen);
                cached
            } else {
                let default_cwd = self
                    .sidebar
                    .read(cx)
                    .selected_workspace()
                    .map(|(_, path)| path)
                    .or_else(|| {
                        self.layout
                            .focused()
                            .and_then(|p| self.worktree_for_pane(p, cx))
                    });
                #[cfg(not(test))]
                let target = self.paths.clone().map(krill_app::hosts::HostTarget::local);
                #[cfg(not(test))]
                let created = cx.new(|cx| krill_app::bots::View::new(target, default_cwd, cx));
                #[cfg(test)]
                let created = cx.new(|cx| {
                    krill_app::bots::View::with_rpc(
                        Arc::new(|req| match req {
                            krill_proto::bots::BotRpcRequest::ListBots => {
                                Ok(krill_proto::bots::BotRpcResponse::Bots(Vec::new()))
                            }
                            krill_proto::bots::BotRpcRequest::ListItems { .. } => {
                                Ok(krill_proto::bots::BotRpcResponse::Items(Vec::new()))
                            }
                            krill_proto::bots::BotRpcRequest::ConnectorStatus { .. } => Ok(
                                krill_proto::bots::BotRpcResponse::ConnectorStatuses(Vec::new()),
                            ),
                            krill_proto::bots::BotRpcRequest::ListAccessRequests { .. } => {
                                Ok(krill_proto::bots::BotRpcResponse::AccessRequests(Vec::new()))
                            }
                            krill_proto::bots::BotRpcRequest::ToolRelayStatus => {
                                Ok(krill_proto::bots::BotRpcResponse::ToolRelayStatus(
                                    krill_proto::bots::BotToolRelayStatus::default(),
                                ))
                            }
                            _ => Ok(krill_proto::bots::BotRpcResponse::Ok),
                        }),
                        default_cwd,
                        cx,
                    )
                });
                cx.subscribe(&created, |this, _, _: &krill_app::bots::Close, cx| {
                    this.close_settings(None, cx);
                })
                .detach();
                cx.subscribe(
                    &created,
                    |this, _, ev: &krill_app::bots::OpenSession, cx| {
                        this.open_session(ev.session_id, cx);
                        if let Some(pane) = this.layout.focused() {
                            this.set_pane_chat_view(pane, true, cx);
                        }
                    },
                )
                .detach();
                cx.subscribe(
                    &created,
                    |this, _, ev: &krill_app::bots::BadgeChanged, cx| {
                        this.apply_sidebar_bots(ev.attention_count, ev.bots.clone(), cx);
                    },
                )
                .detach();
                self.bots_cache = Some(created.clone());
                created
            };
            self.replace_modal(Some(Modal::Bots(view)), cx);
        }
        self.nav_history.push(NavEntry::Bots);
        self.sync_focused_session(cx);
        self.focus_modal_or_workspace(window, cx);
        cx.notify();
    }

    fn apply_sidebar_bots(
        &mut self,
        attention: usize,
        bots: Vec<krill_app::bots::SidebarBot>,
        cx: &mut Context<Self>,
    ) {
        if self.bots_attention_count != attention || self.sidebar_bots != bots {
            self.bots_attention_count = attention;
            self.sidebar_bots = bots;
            cx.notify();
        }
    }

    /// Sidebar Bots section: a bot row opens that bot, `None` (the + button) opens New bot.
    pub(super) fn open_bot(
        &mut self,
        bot_id: Option<krill_proto::bots::BotId>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.open_bots(window, cx);
        if let Some(view) = self.bots_cache.clone() {
            view.update(cx, |v, cx| match bot_id {
                Some(id) => v.select_bot(id, cx),
                None => v.open_create_alone(cx),
            });
        }
    }

    /// In-app bot changes arrive as `BadgeChanged`; this slow loop only catches the rest.
    #[cfg(not(test))]
    pub(super) fn poll_sidebar_bots(cx: &mut Context<Self>) -> Task<()> {
        const INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(INTERVAL).await;
                let polled = this.update(cx, |s, cx| {
                    // Floating windows hide the sidebar, so they have nothing to refresh.
                    if s.role != WindowRole::Floating {
                        s.refresh_bots_badge(cx);
                    }
                });
                if polled.is_err() {
                    return;
                }
            }
        })
    }

    #[cfg(not(test))]
    pub(super) fn refresh_bots_badge(&mut self, cx: &mut Context<Self>) {
        if self.link != Link::Up {
            return;
        }
        let target = self.paths.clone().map(krill_app::hosts::HostTarget::local);
        let rpc = krill_app::bots::daemon_bot_rpc(target);
        self.bots_badge_task = Some(cx.spawn(async move |this, cx| {
            let Some(snapshot) = cx
                .background_executor()
                .spawn(async move { krill_app::bots::fetch_sidebar_snapshot(&rpc) })
                .await
            else {
                return;
            };
            this.update(cx, |this, cx| {
                this.apply_sidebar_bots(snapshot.attention, snapshot.bots, cx);
            })
            .ok();
        }));
    }
}
