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
            let host = self.launch_host.clone();
            let target = self.target_for_host(&host);
            let mut host_choices = vec![(
                krill_app::hosts::HostId::Local,
                krill_app::i18n::catalog::bots::HOST_LOCAL.get().to_owned(),
                self.local_target(),
            )];
            for (hid, tgt) in &self.host_targets {
                if !hid.is_local() {
                    let label = self.remote_host_name(hid.remote_id().unwrap_or_default(), cx);
                    host_choices.push((hid.clone(), label, Some(tgt.clone())));
                }
            }
            let view = if let Some(cached) = self.bots_cache.clone() {
                cached.update(cx, |v, cx| {
                    v.set_host_target(host.clone(), target.clone(), host_choices, cx);
                    v.reopen(cx);
                });
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
                let created = cx.new(|cx| {
                    let mut v = krill_app::bots::View::new_on_host(
                        host.clone(),
                        target.clone(),
                        default_cwd,
                        cx,
                    );
                    v.set_host_target(host.clone(), target.clone(), host_choices, cx);
                    v
                });
                #[cfg(test)]
                let created = cx.new(|cx| {
                    let _ = (&host, &target);
                    let mut v = krill_app::bots::View::with_rpc(
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
                    );
                    v.set_host_target(host.clone(), target.clone(), host_choices, cx);
                    v
                });
                Self::subscribe_bots_view(&created, true, cx);
                cx.subscribe(&created, |this, _, _: &krill_app::bots::Close, cx| {
                    this.close_settings(None, cx);
                })
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

    /// Events every Bots view sends the Shell (the modal and a bot page's right sidebar).
    /// A view that doesn't `feed_sidebar` keeps an older bot list, so its changes only prompt a
    /// fresh read from krilld instead of replacing the sidebar's bots.
    pub(super) fn subscribe_bots_view(
        view: &gpui::Entity<krill_app::bots::View>,
        feed_sidebar: bool,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(
            view,
            |this, _, ev: &krill_app::bots::OpenSettingsPage, cx| {
                this.open_bot_settings_page(ev.0, cx);
            },
        )
        .detach();
        cx.subscribe(
            view,
            |this, _, ev: &krill_app::bots::RequestDeleteBot, cx| {
                if let Some(bot) = this.sidebar_bots.iter().find(|b| b.id == ev.0).cloned() {
                    this.bot_ui.delete = Some(bot);
                    cx.notify();
                }
            },
        )
        .detach();
        cx.subscribe(view, |this, _, _: &krill_app::bots::RequestNewBot, cx| {
            this.request_new_bot(None, None, cx);
        })
        .detach();
        cx.subscribe(
            view,
            |this, _, _: &krill_app::bots::OpenRemoteSettings, cx| {
                this.open_settings(krill_app::settings::AdminTab::Machines, None, cx);
            },
        )
        .detach();
        cx.subscribe(view, |this, view, ev: &krill_app::bots::OpenSession, cx| {
            let host = view.read(cx).host().clone();
            if let Some(meta) = view.read(cx).session_meta_for(&ev.bot_id) {
                this.sidebar.update(cx, |sb, cx| {
                    sb.register_bot_session(host.clone(), ev.session_id, meta, cx);
                });
            }
            let deferred = this.layout_loading;
            this.open_session_on(host, ev.session_id, cx);
            if deferred {
                this.restore_session_chat = true;
            } else if let Some(pane) = this.layout.focused() {
                this.set_pane_chat_view(pane, true, cx);
            }
        })
        .detach();
        cx.subscribe(
            view,
            move |this, _, ev: &krill_app::bots::BadgeChanged, cx| {
                if feed_sidebar {
                    this.apply_sidebar_bots(ev.attention_count, ev.bots.clone(), cx);
                } else {
                    this.refresh_bots_badge(cx);
                }
            },
        )
        .detach();
        if feed_sidebar {
            cx.subscribe(
                view,
                |this, _, ev: &krill_app::bots::SessionsChanged, cx| {
                    this.sidebar.update(cx, |sb, cx| {
                        sb.set_bot_sessions(ev.host.clone(), ev.sessions.clone(), cx);
                    });
                },
            )
            .detach();
        }
    }

    pub(super) fn apply_sidebar_bots(
        &mut self,
        attention: usize,
        bots: Vec<krill_app::bots::SidebarBot>,
        cx: &mut Context<Self>,
    ) {
        if self.bots_attention_count != attention
            || self.sidebar_bots != bots
            || !self.bot_ui.loaded
        {
            self.bots_attention_count = attention;
            self.sidebar_bots = bots;
            self.bot_ui.loaded = true;
            self.bot_ui.panel.stale = true;
            // A restored chat may only now turn out to be a bot's: root its Files there.
            self.sync_diff_workspace(cx);
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
        if let Some(view) = self.bots_cache.clone()
            && let Some(id) = bot_id
        {
            view.update(cx, |v, cx| v.select_bot(id, cx));
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

    pub(super) fn refresh_bots_badge(&mut self, cx: &mut Context<Self>) {
        if self.link != Link::Up {
            return;
        }
        let host = self.launch_host.clone();
        #[cfg(test)]
        let Some(rpc) = self.bot_rpc_override.clone() else {
            return;
        };
        #[cfg(not(test))]
        let rpc = {
            let target = self.target_for_host(&host);
            krill_app::bots::daemon_bot_rpc(target)
        };
        let known = self.known_bot_avatars();
        self.bots_badge_task = Some(cx.spawn(async move |this, cx| {
            let Some(state) = cx
                .background_executor()
                .spawn(async move { krill_app::bots::fetch_sidebar_state(&rpc, &known) })
                .await
            else {
                return;
            };
            this.update(cx, |this, cx| {
                this.sidebar.update(cx, |sb, cx| {
                    sb.set_bot_sessions(host, state.sessions, cx);
                });
                this.apply_sidebar_bots(state.snapshot.attention, state.snapshot.bots, cx);
                if !state.avatars.is_empty() {
                    this.store_bot_avatars(state.avatars);
                    cx.notify();
                }
            })
            .ok();
        }));
    }
}
