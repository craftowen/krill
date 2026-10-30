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

    fn render_machine_overview(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let mut list = div()
            .id("admin-machines-list")
            .debug_selector(|| "admin-machines-list".into())
            .flex()
            .flex_col()
            .gap_2();
        for machine in self.machine_catalog(cx) {
            let selector = format!(
                "admin-machine-{}",
                machine.routes.first().map_or("local", |r| r.host.as_key())
            );
            let page = if machine.is_local() {
                MachinePage::ThisComputer
            } else {
                MachinePage::Details(
                    machine
                        .routes
                        .first()
                        .map_or(HostId::Local, |r| r.host.clone()),
                )
            };
            let keyboard_page = page.clone();
            let (color, state) = machine_status(machine.status(), p);
            let methods = machine
                .routes
                .iter()
                .filter(|r| r.kind != RouteKind::Local)
                .map(|r| route_label(r.kind))
                .collect::<Vec<_>>()
                .join(", ");
            let description = if machine.is_local() {
                text::MACHINES_LOCAL_DESC.get().to_owned()
            } else if machine.routes.iter().all(|r| r.identity.is_none()) {
                format!("{methods}. {}", text::MACHINES_UNKNOWN.get())
            } else {
                methods
            };
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
                    .child(layout::label(p, machine.label().to_owned(), description))
                    .child(
                        layout::controls()
                            .child(div().size(crate::ui::STATE_DOT).rounded_full().bg(color))
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(color)
                                    .child(state),
                            ),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div().flex().justify_end().child(
                    layout::primary("admin-machines-add", text::MACHINES_ADD.get())
                        .debug_selector(|| "admin-machines-add".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_machine_add(AddMethod::Invite, cx);
                        })),
                ),
            )
            .child(list)
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
