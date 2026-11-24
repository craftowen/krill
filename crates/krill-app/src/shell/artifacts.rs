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
        self.open_bots_on(None, window, cx);
    }

    pub(super) fn open_bots_on(
        &mut self,
        preferred_host: Option<krill_app::hosts::HostId>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
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
        let host = preferred_host.unwrap_or_else(|| {
            if self.sidebar_bots.iter().any(|b| b.host == self.launch_host) {
                self.launch_host.clone()
            } else {
                self.sidebar_bots
                    .first()
                    .map_or_else(|| self.launch_host.clone(), |b| b.host.clone())
            }
        });
        let target = self.target_for_host(&host);
        let mut host_choices: Vec<(
            krill_app::hosts::HostId,
            String,
            Option<krill_app::hosts::HostTarget>,
        )> = self
            .execution_locations(cx)
            .into_iter()
            .map(|(hid, label)| {
                let tgt = self.target_for_host(&hid);
                (hid, label, tgt)
            })
            .collect();
        if !host_choices.iter().any(|(hid, _, _)| hid == &host) {
            host_choices.push((
                host.clone(),
                self.host_display_label(&host, cx).to_string(),
                target.clone(),
            ));
        }
        if let Some(Modal::Bots(view)) = &self.modal {
            view.update(cx, |v, cx| {
                v.set_host_target(host.clone(), target.clone(), host_choices, cx);
            });
        } else {
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
                    let _ = &target;
                    let mut v =
                        krill_app::bots::View::with_rpc(
                            self.bot_rpc_on(&host).unwrap_or_else(|| {
                                Arc::new(|req| match req {
                                    krill_proto::bots::BotRpcRequest::ListBots => {
                                        Ok(krill_proto::bots::BotRpcResponse::Bots(Vec::new()))
                                    }
                                    krill_proto::bots::BotRpcRequest::ListItems { .. } => {
                                        Ok(krill_proto::bots::BotRpcResponse::Items(Vec::new()))
                                    }
                                    krill_proto::bots::BotRpcRequest::ConnectorStatus {
                                        ..
                                    } => Ok(krill_proto::bots::BotRpcResponse::ConnectorStatuses(
                                        Vec::new(),
                                    )),
                                    krill_proto::bots::BotRpcRequest::ListAccessRequests {
                                        ..
                                    } => Ok(krill_proto::bots::BotRpcResponse::AccessRequests(
                                        Vec::new(),
                                    )),
                                    krill_proto::bots::BotRpcRequest::ToolRelayStatus => {
                                        Ok(krill_proto::bots::BotRpcResponse::ToolRelayStatus(
                                            krill_proto::bots::BotToolRelayStatus::default(),
                                        ))
                                    }
                                    _ => Ok(krill_proto::bots::BotRpcResponse::Ok),
                                })
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
            |this, view, ev: &krill_app::bots::RequestDeleteBot, cx| {
                let host = view.read(cx).host().clone();
                if let Some(bot) = this
                    .sidebar_bots
                    .iter()
                    .find(|b| b.host == host && b.id == ev.0)
                    .cloned()
                {
                    this.bot_ui.delete = Some(bot);
                    cx.notify();
                }
            },
        )
        .detach();
        cx.subscribe(view, |this, _, ev: &krill_app::bots::OpenBotHome, cx| {
            this.close_settings(None, cx);
            if ev.host != this.launch_host {
                this.set_launch_host(ev.host.clone(), cx);
            }
            this.refresh_bots_for_host(&ev.host, true, cx);
            this.open_bot_home_on(&ev.host, ev.bot_id.clone(), ev.kickoff.clone(), cx);
        })
        .detach();
        cx.subscribe(view, |this, _, _: &krill_app::bots::RequestImport, cx| {
            this.open_bots_screen(bot_section::BotsScreen::Import, None, cx);
        })
        .detach();
        cx.subscribe(
            view,
            |this, view, ev: &krill_app::bots::OpenBotConnectors, cx| {
                let host = view.read(cx).host().clone();
                this.open_bot_home_on(&host, ev.0.clone(), None, cx);
                this.bot_ui.panel.tab = bot_panel::BotPanelTab::Connectors;
                this.bot_ui.panel.settings = None;
                if this.panel_collapsed {
                    this.toggle_panel(cx);
                }
                cx.notify();
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
            this.open_bot_session_on(&host, &ev.bot_id, ev.session_id, cx);
        })
        .detach();
        cx.subscribe(
            view,
            move |this, view, ev: &krill_app::bots::BadgeChanged, cx| {
                if feed_sidebar {
                    this.apply_sidebar_bots_for_host(
                        &ev.host,
                        ev.attention_count,
                        ev.bots.clone(),
                        cx,
                    );
                } else {
                    let host = view.read(cx).host().clone();
                    this.refresh_bots_for_host(&host, true, cx);
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

    #[cfg(test)]
    pub(super) fn apply_sidebar_bots(
        &mut self,
        attention: usize,
        bots: Vec<krill_app::bots::SidebarBot>,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        self.apply_sidebar_bots_for_host(&host, attention, bots, cx);
    }

    pub(super) fn apply_sidebar_bots_for_host(
        &mut self,
        host: &krill_app::hosts::HostId,
        attention: usize,
        bots: Vec<krill_app::bots::SidebarBot>,
        cx: &mut Context<Self>,
    ) {
        let groups = self.bot_ui.hosts.get(host).map_or_else(
            || {
                self.sidebar_groups
                    .iter()
                    .filter(|g| &g.host == host)
                    .map(|g| g.summary.clone())
                    .collect()
            },
            |h| h.groups.clone(),
        );
        self.apply_sidebar_state_for_host(host, attention, bots, groups, cx);
    }

    #[cfg(test)]
    pub(super) fn apply_sidebar_state(
        &mut self,
        attention: usize,
        bots: Vec<krill_app::bots::SidebarBot>,
        groups: Vec<krill_proto::bots::BotGroupSummary>,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        self.apply_sidebar_state_for_host(&host, attention, bots, groups, cx);
    }

    pub(super) fn apply_sidebar_state_for_host(
        &mut self,
        host: &krill_app::hosts::HostId,
        attention: usize,
        mut bots: Vec<krill_app::bots::SidebarBot>,
        groups: Vec<krill_proto::bots::BotGroupSummary>,
        cx: &mut Context<Self>,
    ) {
        for bot in &mut bots {
            bot.host = host.clone();
        }
        let entry = self.bot_ui.hosts.entry(host.clone()).or_default();
        entry.probed = true;
        entry.snapshot = krill_app::bots::SidebarSnapshot { bots, attention };
        entry.groups = groups;
        let active = self.active_bot_hosts(cx);
        self.rebuild_merged_sidebar_bots(&active, true, cx);
    }

    /// Sidebar Bots section: a bot row opens that bot on `host`, `None` opens New bot.
    pub(super) fn open_bot_on(
        &mut self,
        host: &krill_app::hosts::HostId,
        bot_id: Option<krill_proto::bots::BotId>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.open_bots_on(Some(host.clone()), window, cx);
        if let Some(view) = self.bots_cache.clone()
            && let Some(id) = bot_id
        {
            view.update(cx, |v, cx| v.select_bot(id, cx));
        }
    }

    /// Active hosts eligible for sidebar bots: Local (when `Link::Up`) plus every connected,
    /// non-suppressed remote host, ordered deterministically with Local first.
    pub(super) fn active_bot_hosts(&self, cx: &App) -> Vec<krill_app::hosts::HostId> {
        let mut hosts = Vec::new();
        if self.link == Link::Up {
            hosts.push(krill_app::hosts::HostId::Local);
        }
        let sidebar = self.sidebar.read(cx);
        for remote in sidebar.connected_remote_hosts() {
            let id = krill_app::hosts::HostId::Remote(remote.id.clone());
            if !hosts.contains(&id) && self.can_query_remote_bot_host(&id) {
                hosts.push(id);
            }
        }
        for (spec, status) in &self.host_statuses {
            let id = krill_app::hosts::HostId::Remote(spec.id.clone());
            if *status == krill_app::hosts::HostStatus::Connected
                && !hosts.contains(&id)
                && !sidebar.is_suppressed_remote_host(&id)
                && self.can_query_remote_bot_host(&id)
            {
                hosts.push(id);
            }
        }
        hosts
    }

    fn can_query_remote_bot_host(&self, host: &krill_app::hosts::HostId) -> bool {
        #[cfg(test)]
        if self.bot_rpc_overrides.contains_key(host)
            || (self.bot_rpc_override.is_some() && *host == self.launch_host)
        {
            return true;
        }
        self.host_targets.contains_key(host)
    }

    fn is_remote_host_live_or_connecting(&self, host: &krill_app::hosts::HostId, cx: &App) -> bool {
        let Some(remote_id) = host.remote_id() else {
            return matches!(self.link, Link::Up | Link::Connecting);
        };
        let sidebar = self.sidebar.read(cx);
        if sidebar.is_suppressed_remote_host(host) {
            return false;
        }
        let in_sidebar = sidebar
            .remote_hosts()
            .iter()
            .find(|r| r.id == remote_id)
            .is_some_and(|r| {
                matches!(
                    r.status,
                    krill_app::hosts::HostStatus::Connected
                        | krill_app::hosts::HostStatus::Connecting
                )
            });
        let in_statuses = self
            .host_statuses
            .iter()
            .find(|(s, _)| s.id == remote_id)
            .is_some_and(|(_, st)| {
                matches!(
                    st,
                    krill_app::hosts::HostStatus::Connected
                        | krill_app::hosts::HostStatus::Connecting
                )
            });
        in_sidebar || in_statuses
    }

    fn prune_inactive_bot_hosts(
        &mut self,
        active: &[krill_app::hosts::HostId],
        cx: &mut Context<Self>,
    ) {
        let stale: Vec<krill_app::hosts::HostId> = self
            .bot_ui
            .hosts
            .keys()
            .filter(|host| {
                !active.contains(host)
                    && (host.is_remote() && !self.is_remote_host_live_or_connecting(host, cx))
            })
            .cloned()
            .collect();
        if stale.is_empty() {
            return;
        }
        for host in &stale {
            self.bot_ui.hosts.remove(host);
            self.bot_ui.host_tasks.remove(host);
            krill_app::bots::invalidate_host_connections(host);
            self.sidebar.update(cx, |sb, cx| {
                sb.set_bot_sessions(host.clone(), std::collections::BTreeMap::new(), cx);
            });
        }
        self.bot_ui
            .avatars
            .retain(|(host, _), _| !stale.contains(host));
        self.rebuild_merged_sidebar_bots(active, false, cx);
    }

    pub(super) fn rebuild_merged_sidebar_bots(
        &mut self,
        active: &[krill_app::hosts::HostId],
        force_loaded: bool,
        cx: &mut Context<Self>,
    ) {
        let mut ordered_hosts: Vec<krill_app::hosts::HostId> = Vec::new();
        if self
            .bot_ui
            .hosts
            .contains_key(&krill_app::hosts::HostId::Local)
        {
            ordered_hosts.push(krill_app::hosts::HostId::Local);
        }
        for host in active {
            if !ordered_hosts.contains(host) && self.bot_ui.hosts.contains_key(host) {
                ordered_hosts.push(host.clone());
            }
        }
        for host in self.bot_ui.hosts.keys() {
            if !ordered_hosts.contains(host) {
                ordered_hosts.push(host.clone());
            }
        }

        let mut bots = Vec::new();
        let mut groups = Vec::new();
        let mut attention = 0usize;
        for host in &ordered_hosts {
            if let Some(state) = self.bot_ui.hosts.get(host) {
                attention = attention.saturating_add(state.snapshot.attention);
                bots.extend(state.snapshot.bots.iter().cloned());
                groups.extend(state.groups.iter().cloned().map(|summary| {
                    bot_section::SidebarGroup {
                        host: host.clone(),
                        summary,
                    }
                }));
            }
        }

        let all_active_probed = !active.is_empty()
            && active
                .iter()
                .all(|host| self.bot_ui.hosts.get(host).is_some_and(|h| h.probed));
        let next_loaded = self.bot_ui.loaded
            || force_loaded
            || !bots.is_empty()
            || !groups.is_empty()
            || all_active_probed;
        if next_loaded {
            self.bot_ui.loaded_fallback_task = None;
        }
        if self.bots_attention_count != attention
            || self.sidebar_bots != bots
            || self.sidebar_groups != groups
            || self.bot_ui.loaded != next_loaded
        {
            self.bots_attention_count = attention;
            self.sidebar_bots = bots;
            self.sidebar_groups = groups;
            self.bot_ui.loaded = next_loaded;
            self.bot_ui.panel.stale = true;
            // A restored chat may only now turn out to be a bot's: root its Files there.
            self.sync_diff_workspace(cx);
            cx.notify();
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
                        s.refresh_all_bot_hosts(false, cx);
                    }
                });
                if polled.is_err() {
                    return;
                }
            }
        })
    }

    pub(super) fn refresh_bots_badge(&mut self, cx: &mut Context<Self>) {
        self.refresh_all_bot_hosts(true, cx);
    }

    pub(super) fn refresh_all_bot_hosts(&mut self, force: bool, cx: &mut Context<Self>) {
        let active = self.active_bot_hosts(cx);
        self.prune_inactive_bot_hosts(&active, cx);
        if active.is_empty() {
            return;
        }
        #[cfg(not(test))]
        if !self.bot_ui.loaded && self.bot_ui.loaded_fallback_task.is_none() {
            self.bot_ui.loaded_fallback_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(2500))
                    .await;
                this.update(cx, |this, cx| {
                    this.bot_ui.loaded_fallback_task = None;
                    if !this.bot_ui.loaded {
                        this.bot_ui.loaded = true;
                        cx.notify();
                    }
                })
                .ok();
            }));
        }
        for host in active {
            self.refresh_bots_for_host(&host, force, cx);
        }
    }

    pub(super) fn refresh_bots_for_host(
        &mut self,
        host: &krill_app::hosts::HostId,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        const UNSUPPORTED_RECHECK: std::time::Duration = std::time::Duration::from_secs(300);

        if host.is_local() {
            if self.link != Link::Up {
                return;
            }
        } else if !self.can_query_remote_bot_host(host) {
            return;
        }

        #[cfg(test)]
        let test_rpc = self.bot_rpc_on(host);
        #[cfg(test)]
        let Some(test_rpc) = test_rpc else {
            return;
        };

        let target_opt = self.target_for_host(host).or_else(|| {
            #[cfg(not(test))]
            if host.is_local() {
                return krill_app::hosts::HostTarget::local_dev().ok();
            }
            None
        });
        #[cfg(not(test))]
        let Some(target) = target_opt.clone() else {
            // Never fall back to local_dev() for a remote host without a target.
            return;
        };
        let transport_epoch = target_opt
            .as_ref()
            .map_or(0, krill_app::hosts::HostTarget::transport_epoch);
        let now = std::time::Instant::now();

        let state = self.bot_ui.hosts.entry(host.clone()).or_default();
        let endpoint_changed =
            state.target != target_opt || state.transport_epoch != transport_epoch;
        if endpoint_changed {
            state.target.clone_from(&target_opt);
            state.transport_epoch = transport_epoch;
            state.unsupported = None;
            state.consecutive_failures = 0;
            state.next_retry_at = None;
            state.retry_task = None;
            state.in_flight = false;
            state.dirty = false;
            krill_app::bots::invalidate_host_connections(host);
        }

        if !force {
            if let Some((_, checked_at)) = &state.unsupported
                && now.saturating_duration_since(*checked_at) < UNSUPPORTED_RECHECK
            {
                return;
            }
            if let Some(until) = state.next_retry_at
                && now < until
            {
                return;
            }
        } else if let Some((_, checked_at)) = &state.unsupported
            && !endpoint_changed
            && now.saturating_duration_since(*checked_at) < UNSUPPORTED_RECHECK
        {
            // Even forced sidebar refreshes skip hosts already verified to lack FEAT_BOTS until
            // their connection/epoch changes or the recheck window elapses.
            return;
        }

        if state.in_flight {
            state.dirty = true;
            return;
        }

        state.in_flight = true;
        state.dirty = false;
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let host_owned = host.clone();
        let known = self.known_bot_avatars_for_host(host);

        let task = cx.spawn(async move |this, cx| {
            let fetch_host = host_owned.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    {
                        let _ = target_opt;
                        krill_app::bots::fetch_sidebar_state_detailed_with_rpc(
                            &fetch_host,
                            &test_rpc,
                            &known,
                        )
                    }
                    #[cfg(not(test))]
                    {
                        krill_app::bots::fetch_sidebar_state_for_target(
                            &fetch_host,
                            &target,
                            &known,
                        )
                    }
                })
                .await;

            this.update(cx, |this, cx| {
                let mut should_retry_again = false;
                let mut fetch_orch = false;
                let mut retry_delay = None;
                {
                    let Some(state) = this.bot_ui.hosts.get_mut(&host_owned) else {
                        return;
                    };
                    if state.generation != generation {
                        return;
                    }
                    state.in_flight = false;
                    let dirty = std::mem::take(&mut state.dirty);
                    match outcome {
                        Ok(sidebar_state) => {
                            if state.consecutive_failures > 0 {
                                tracing::info!(
                                    host = %host_owned,
                                    failures = state.consecutive_failures,
                                    "bot list refresh recovered"
                                );
                            }
                            state.probed = true;
                            state.unsupported = None;
                            state.consecutive_failures = 0;
                            state.next_retry_at = None;
                            state.last_error = None;
                            state.retry_task = None;
                            let has_bots = !sidebar_state.snapshot.bots.is_empty();
                            state.snapshot = sidebar_state.snapshot;
                            state.groups = sidebar_state.groups;
                            let sessions = sidebar_state.sessions;
                            let avatars = sidebar_state.avatars;
                            fetch_orch = host_owned.is_remote() && has_bots;
                            should_retry_again = dirty;
                            this.sidebar.update(cx, |sb, cx| {
                                sb.set_bot_sessions(host_owned.clone(), sessions, cx);
                            });
                            if !avatars.is_empty() {
                                this.store_bot_avatars_for_host(&host_owned, avatars);
                            }
                        }
                        Err(krill_app::bots::SidebarFetchError::Unsupported(version)) => {
                            state.probed = true;
                            state.unsupported = Some((version, std::time::Instant::now()));
                            state.consecutive_failures = 0;
                            state.next_retry_at = None;
                            state.last_error = None;
                            state.retry_task = None;
                            state.snapshot = krill_app::bots::SidebarSnapshot::default();
                            state.groups.clear();
                            this.sidebar.update(cx, |sb, cx| {
                                sb.set_bot_sessions(
                                    host_owned.clone(),
                                    std::collections::BTreeMap::new(),
                                    cx,
                                );
                            });
                        }
                        Err(krill_app::bots::SidebarFetchError::Transient(err)) => {
                            state.probed = true;
                            state.consecutive_failures =
                                state.consecutive_failures.saturating_add(1);
                            let delay = bot_retry_backoff(state.consecutive_failures);
                            state.next_retry_at = Some(std::time::Instant::now() + delay);
                            if state.consecutive_failures == 1 {
                                tracing::warn!(
                                    host = %host_owned,
                                    error = %err,
                                    retry_secs = delay.as_secs(),
                                    "bot list refresh failed; preserving cached bots"
                                );
                            }
                            state.last_error = Some(err);
                            if state.consecutive_failures <= 3 {
                                retry_delay = Some(delay);
                            }
                        }
                    }
                }
                let active = this.active_bot_hosts(cx);
                this.rebuild_merged_sidebar_bots(&active, false, cx);
                if fetch_orch && !this.remote_orch_snapshots.contains_key(&host_owned) {
                    this.fetch_remote_orchestration(host_owned.clone(), cx);
                }
                #[cfg(not(test))]
                if let Some(delay) = retry_delay {
                    let retry_host = host_owned.clone();
                    let retry_task = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        this.update(cx, |this, cx| {
                            this.refresh_bots_for_host(&retry_host, false, cx);
                        })
                        .ok();
                    });
                    if let Some(state) = this.bot_ui.hosts.get_mut(&host_owned) {
                        state.retry_task = Some(retry_task);
                    }
                }
                #[cfg(test)]
                let _ = retry_delay;
                if should_retry_again {
                    this.refresh_bots_for_host(&host_owned, true, cx);
                }
            })
            .ok();
        });
        self.bot_ui.host_tasks.insert(host.clone(), task);
    }
}

fn bot_retry_backoff(consecutive_failures: u32) -> std::time::Duration {
    let secs = match consecutive_failures {
        0 | 1 => 2,
        2 => 4,
        3 => 8,
        4 => 15,
        5 => 30,
        _ => 60,
    };
    std::time::Duration::from_secs(secs)
}
