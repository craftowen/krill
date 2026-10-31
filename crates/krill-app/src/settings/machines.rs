use super::*;
use crate::hosts::{HostId, HostStatus};
use crate::machines::{AddMethod, Machine, MachinePage, MachineRoute, RouteKind};

mod ssh;

impl SettingsView {
    pub fn sync_machine_remote_statuses(
        &mut self,
        statuses: std::collections::BTreeMap<HostId, HostStatus>,
        cx: &mut Context<Self>,
    ) {
        if self.machine_remote_statuses != statuses {
            self.machine_remote_statuses = statuses;
            cx.notify();
        }
    }

    pub fn sync_machine_local_status(&mut self, status: HostStatus, cx: &mut Context<Self>) {
        if self.machine_local_status != status {
            self.machine_local_status = status;
            cx.notify();
        }
    }

    pub fn sync_machine_observations(
        &mut self,
        identities: std::collections::BTreeMap<HostId, krill_proto::ipc::MachineIdentity>,
        cx: &mut Context<Self>,
    ) {
        if self.machine_identities != identities {
            self.machine_identities = identities;
            cx.notify();
        }
    }

    pub fn show_machine_add(&mut self, method: AddMethod, cx: &mut Context<Self>) {
        self.set_tab(AdminTab::Machines, cx);
        if method == AddMethod::Ssh && self.editing_ssh_host_id().is_some() {
            self.ssh_form = None;
        }
        if self.machine_page != MachinePage::Add(method) {
            self.machine_add_focus = Some(method);
        }
        self.machine_page = MachinePage::Add(method);
        cx.notify();
    }

    fn ensure_machine_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.remote_krill.is_some() {
            return;
        }
        let Some(dir) = state_dir(cx) else {
            return;
        };
        let backend = std::sync::Arc::new(crate::link_ui::real::RealLinkBackend::new(
            dir,
            crate::hosts::link::executable(),
        ));
        let view = cx.new(|cx| crate::link_ui::PairingView::new(backend, window, cx));
        view.update(cx, |v, cx| v.set_visible(self.visible, cx));
        let server = view.read(cx).server.clone();
        self.machine_pairing_observe = Some(cx.observe(&server, |_, _, cx| cx.notify()));
        self.remote_krill = Some(view);
    }

    pub(super) fn machine_catalog(&self, cx: &App) -> Vec<Machine> {
        let identity = |host: &HostId| self.machine_identities.get(host).copied();
        let mut routes = vec![MachineRoute {
            host: HostId::Local,
            label: text::MACHINES_THIS_COMPUTER.get().into(),
            kind: RouteKind::Local,
            status: self.machine_local_status,
            identity: identity(&HostId::Local),
        }];
        for host in &self.ssh_hosts {
            let id = HostId::Remote(host.id.clone());
            routes.push(MachineRoute {
                identity: identity(&id),
                host: id,
                label: if host.label.trim().is_empty() {
                    host.destination.clone()
                } else {
                    host.label.clone()
                },
                kind: RouteKind::Ssh,
                status: self
                    .machine_remote_statuses
                    .get(&HostId::Remote(host.id.clone()))
                    .copied()
                    .unwrap_or(HostStatus::Disconnected),
            });
        }
        if let Some(view) = &self.remote_krill {
            for host in view.read(cx).server.read(cx).remote_hosts() {
                let id = HostId::Remote(format!("iroh:{}", host.id));
                routes.push(MachineRoute {
                    identity: identity(&id),
                    host: id,
                    label: host.name.clone(),
                    kind: RouteKind::Invite,
                    status: host.status,
                });
            }
        }
        crate::machines::project(routes)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Variant A overview combines local hub card and inline remote machine list with direct actions"
    )]
    fn render_machine_overview(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let catalog = self.machine_catalog(cx);
        let local_machine = catalog.iter().find(|m| m.is_local());
        let remote_machines: Vec<&Machine> = catalog.iter().filter(|m| !m.is_local()).collect();

        let mut local_hub = div().flex().flex_col().gap_2().child(layout::section_title(
            p,
            "admin-machines-local-section",
            text::MACHINES_LOCAL_SECTION.get(),
        ));
        if let Some(machine) = local_machine {
            let (color, state) = machine_status(machine.status(), p);
            let badge_tone = match machine.status() {
                HostStatus::Connected => crate::ui::BadgeTone::Success,
                HostStatus::Connecting => crate::ui::BadgeTone::Warning,
                HostStatus::Disconnected => crate::ui::BadgeTone::Muted,
            };
            local_hub = local_hub.child(
                layout::row(p)
                    .id("admin-machine-local")
                    .debug_selector(|| "admin-machine-local".into())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.machine_page = MachinePage::ThisComputer;
                            this.disarm(cx);
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .aria_label(machine.label().to_owned())
                    .cursor_pointer()
                    .hover(|d| d.bg(p.accent))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.machine_page = MachinePage::ThisComputer;
                        this.disarm(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .text_size(crate::ui::TEXT_SM)
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(machine.label().to_owned()),
                                    )
                                    .child(crate::ui::badge(p, badge_tone, state).child(
                                        div().size(crate::ui::STATE_DOT).rounded_full().bg(color),
                                    )),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::MACHINES_LOCAL_HUB_DESC.get()),
                            ),
                    )
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary(
                                    "admin-local-open-mobile",
                                    text::MACHINES_OPEN_MOBILE_QR.get(),
                                )
                                .debug_selector(|| "admin-local-open-mobile".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.set_tab(AdminTab::Mobile, cx);
                                    },
                                )),
                            )
                            .child(
                                layout::secondary(
                                    "admin-local-open-invite",
                                    text::MACHINES_MANAGE_INVITE.get(),
                                )
                                .debug_selector(|| "admin-local-open-invite".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.machine_page = MachinePage::ThisComputer;
                                        this.disarm(cx);
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            );
        }

        let mut list = div()
            .id("admin-machines-list")
            .debug_selector(|| "admin-machines-list".into())
            .flex()
            .flex_col()
            .gap_2();

        if remote_machines.is_empty() {
            list = list.child(
                layout::card(p)
                    .id("admin-machines-empty")
                    .debug_selector(|| "admin-machines-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::MACHINES_EMPTY_REMOTE.get()),
            );
        }

        for machine in remote_machines {
            let key = machine
                .routes
                .first()
                .map_or("local", |r| r.host.as_key())
                .to_owned();
            let selector = format!("admin-machine-{key}");
            let detail_btn_id = format!("admin-machine-detail-{key}");
            let page = MachinePage::Details(
                machine
                    .routes
                    .first()
                    .map_or(HostId::Local, |r| r.host.clone()),
            );
            let keyboard_page = page.clone();
            let detail_page = page.clone();
            let (color, state) = machine_status(machine.status(), p);

            let ssh_host = machine
                .routes
                .iter()
                .filter(|r| r.kind == RouteKind::Ssh)
                .find_map(|r| {
                    self.ssh_hosts
                        .iter()
                        .find(|h| Some(h.id.as_str()) == r.host.remote_id())
                        .cloned()
                });

            let address_line = if let Some(host) = &ssh_host {
                host.format_summary()
            } else {
                text::MACHINES_INVITE_ROUTE_DESC.get().to_owned()
            };

            let mut title_row = div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .flex_wrap()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(machine.label().to_owned()),
                );

            for route in machine.routes.iter().filter(|r| r.kind != RouteKind::Local) {
                let tone = match route.kind {
                    RouteKind::Ssh => crate::ui::BadgeTone::Neutral,
                    RouteKind::Invite => crate::ui::BadgeTone::Accent,
                    RouteKind::Local => crate::ui::BadgeTone::Muted,
                };
                title_row = title_row.child(crate::ui::badge(p, tone, route_label(route.kind)));
            }

            title_row = title_row.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(crate::ui::STATE_DOT).rounded_full().bg(color))
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(color)
                            .child(state),
                    ),
            );

            let mut row_controls = layout::controls();
            if let Some(host) = ssh_host {
                let ssh_status = self.ssh_host_status(&host.id);
                let conn_id = format!("admin-machine-connect-{}", host.id);
                let disc_id = format!("admin-machine-disconnect-{}", host.id);
                let test_id = format!("admin-machine-test-{}", host.id);
                let host_conn = host.clone();
                let host_disc = host.clone();
                let host_test = host.clone();
                let connect_or_disconnect = if ssh_status == SshHostCardStatus::Connected {
                    layout::secondary(
                        gpui::ElementId::Name(disc_id.clone().into()),
                        text::SSH_DISCONNECT.get(),
                    )
                    .debug_selector(move || disc_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.disconnect_ssh_host(&host_disc, cx);
                    }))
                } else {
                    layout::secondary(
                        gpui::ElementId::Name(conn_id.clone().into()),
                        text::SSH_CONNECT.get(),
                    )
                    .debug_selector(move || conn_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.connect_ssh_host(host_conn.clone(), cx);
                    }))
                };
                let testing = self.is_ssh_test_pending(&host.id);
                let test_btn = crate::ui::button_with_reserved_label(
                    gpui::ElementId::Name(test_id.clone().into()),
                    text::SSH_TEST.get(),
                    text::SSH_TESTING.get(),
                    testing,
                )
                .outline()
                .small()
                .disabled(testing)
                .debug_selector(move || test_id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.test_ssh_host(host_test.clone(), cx);
                }));
                row_controls = row_controls.child(connect_or_disconnect).child(test_btn);
            }

            row_controls = row_controls.child(
                layout::ghost(
                    gpui::ElementId::Name(detail_btn_id.clone().into()),
                    text::MACHINES_DETAIL_BUTTON.get(),
                )
                .debug_selector(move || detail_btn_id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.machine_page = detail_page.clone();
                    this.disarm(cx);
                    cx.notify();
                })),
            );

            list = list.child(
                layout::row(p)
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.machine_page = keyboard_page.clone();
                            this.disarm(cx);
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .aria_label(machine.label().to_owned())
                    .cursor_pointer()
                    .hover(|d| d.bg(p.accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.machine_page = page.clone();
                        this.disarm(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(title_row)
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(address_line),
                            ),
                    )
                    .child(row_controls),
            );
        }

        div().flex().flex_col().gap_4().child(local_hub).child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .pt_2()
                        .pb_1()
                        .border_b_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .id("admin-machines-remote-section")
                                .debug_selector(|| "admin-machines-remote-section".into())
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(p.muted_foreground)
                                .child(text::MACHINES_REMOTE_SECTION.get()),
                        )
                        .child(
                            layout::controls()
                                .child(
                                    layout::secondary(
                                        "admin-machines-add-ssh",
                                        text::MACHINES_ADD_SSH.get(),
                                    )
                                    .debug_selector(|| "admin-machines-add-ssh".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.show_machine_add(AddMethod::Ssh, cx);
                                        },
                                    )),
                                )
                                .child(
                                    layout::primary("admin-machines-add", text::MACHINES_ADD.get())
                                        .debug_selector(|| "admin-machines-add".into())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_machine_add(AddMethod::Invite, cx);
                                        })),
                                ),
                        ),
                )
                .child(list)
                .child(self.render_ssh_status(cx)),
        )
    }

    fn render_machine_details(
        &mut self,
        machine: &Machine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let mut content = div().flex().flex_col().gap_3().child(layout::label(
            p,
            machine.label().to_owned(),
            text::MACHINES_METHODS.get(),
        ));
        let paired: Vec<String> = machine
            .routes
            .iter()
            .filter(|r| r.kind == RouteKind::Invite)
            .filter_map(|r| r.host.remote_id()?.strip_prefix("iroh:").map(str::to_owned))
            .collect();
        if machine.is_local() || !paired.is_empty() {
            if let Some(view) = &self.remote_krill {
                view.update(cx, |v, cx| {
                    v.show(
                        crate::link_ui::PairingPage::Details {
                            local: machine.is_local(),
                            routes: paired,
                        },
                        cx,
                    );
                });
                content = content.child(view.clone());
            } else {
                content = content.child(crate::ui::error_banner(
                    p,
                    crate::i18n::link_ui::ERR_UNAVAILABLE.get(),
                ));
            }
        }
        for route in &machine.routes {
            if route.kind != RouteKind::Ssh {
                continue;
            }
            if let Some(host) = self
                .ssh_hosts
                .iter()
                .find(|h| Some(h.id.as_str()) == route.host.remote_id())
                .cloned()
            {
                content = content.child(self.render_ssh_host_card(&host, cx));
                if self.editing_ssh_host_id() == Some(host.id.as_str()) {
                    self.ensure_ssh_form(window, cx);
                    content = content.child(self.render_ssh_form(cx));
                }
            }
        }
        content.child(self.render_ssh_status(cx))
    }

    pub(super) fn render_machines_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        self.ensure_machine_pairing(window, cx);
        let page = self.machine_page.clone();
        if let Some(method) = self.machine_add_focus.take()
            && page == MachinePage::Add(method)
        {
            if method == AddMethod::Ssh {
                self.ensure_ssh_form(window, cx);
            }
            cx.defer_in(window, move |this, window, cx| {
                if this.machine_page != MachinePage::Add(method) {
                    return;
                }
                match method {
                    AddMethod::Ssh => {
                        if let Some(form) = &this.ssh_form {
                            form.label.update(cx, |input, cx| input.focus(window, cx));
                        }
                    }
                    AddMethod::Invite => {
                        if let Some(view) = &this.remote_krill {
                            let client = view.read(cx).client.clone();
                            client.update(cx, |client, cx| client.focus_link(window, cx));
                        }
                    }
                }
            });
        }
        let p = crate::ui::palette(cx);
        if page == MachinePage::Overview {
            return self.render_machine_overview(cx);
        }
        let mut content = div().flex().flex_col().gap_3().child(
            layout::ghost("admin-machines-back", text::MACHINES_BACK.get())
                .debug_selector(|| "admin-machines-back".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.machine_page = MachinePage::Overview;
                    this.disarm(cx);
                    cx.notify();
                })),
        );
        match page {
            MachinePage::Overview => content,
            MachinePage::Add(method) => {
                content = content
                    .child(layout::label(
                        p,
                        text::MACHINES_ADD.get(),
                        text::MACHINES_ADD_DESC.get(),
                    ))
                    .child(crate::ui::segmented_control(
                        p,
                        [
                            crate::ui::segment(
                                p,
                                "admin-machines-invite",
                                text::MACHINES_INVITE.get(),
                                method == AddMethod::Invite,
                            )
                            .debug_selector(|| "admin-machines-invite".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_machine_add(AddMethod::Invite, cx);
                            })),
                            crate::ui::segment(
                                p,
                                "admin-machines-ssh",
                                text::MACHINES_SSH.get(),
                                method == AddMethod::Ssh,
                            )
                            .debug_selector(|| "admin-machines-ssh".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_machine_add(AddMethod::Ssh, cx);
                            })),
                        ],
                    ));
                match method {
                    AddMethod::Ssh => content.child(self.render_ssh_add(window, cx)),
                    AddMethod::Invite => {
                        if let Some(view) = &self.remote_krill {
                            view.update(cx, |v, cx| v.show(crate::link_ui::PairingPage::Add, cx));
                            content.child(view.clone())
                        } else {
                            content.child(crate::ui::error_banner(
                                p,
                                crate::i18n::link_ui::ERR_UNAVAILABLE.get(),
                            ))
                        }
                    }
                }
            }
            MachinePage::Details(host) => {
                if let Some(machine) = self
                    .machine_catalog(cx)
                    .into_iter()
                    .find(|m| m.routes.iter().any(|r| r.host == host))
                {
                    content.child(self.render_machine_details(&machine, window, cx))
                } else {
                    self.machine_page = MachinePage::Overview;
                    self.render_machine_overview(cx)
                }
            }
            MachinePage::ThisComputer => {
                if let Some(machine) = self.machine_catalog(cx).into_iter().find(Machine::is_local)
                {
                    content.child(self.render_machine_details(&machine, window, cx))
                } else {
                    content
                }
            }
        }
    }
}

fn route_label(kind: RouteKind) -> &'static str {
    match kind {
        RouteKind::Local => text::MACHINES_THIS_COMPUTER.get(),
        RouteKind::Ssh => text::MACHINES_SSH.get(),
        RouteKind::Invite => text::MACHINES_INVITE.get(),
    }
}

fn machine_status(status: HostStatus, p: &crate::ui::Palette) -> (gpui::Hsla, &'static str) {
    use crate::i18n::catalog::sidebar as labels;
    match status {
        HostStatus::Connected => (p.success, labels::HOST_CONNECTED.get()),
        HostStatus::Connecting => (p.warning, labels::HOST_CONNECTING.get()),
        HostStatus::Disconnected => (p.muted_foreground, labels::HOST_DISCONNECTED.get()),
    }
}
