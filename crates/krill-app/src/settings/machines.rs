use super::*;
use crate::hosts::{HostId, HostStatus};
use crate::link_ui::ServerPanel;
use crate::machines::{AddMethod, Machine, MachineRoute, RouteKind};
use gpui_component::IconName;

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
        if self.machine_add != Some(method) {
            self.machine_add_focus = Some(method);
        }
        self.machine_add = Some(method);
        cx.notify();
    }

    /// 검색 결과로 들어오면 그 항목이 있는 곳을 펼친다.
    pub(super) fn reveal_machine_search_hit(
        &mut self,
        title: crate::i18n::Message,
        cx: &mut Context<Self>,
    ) {
        if title == text::SSH_TITLE {
            self.show_machine_add(AddMethod::Ssh, cx);
        } else if title == text::TAB_REMOTE_KRILL {
            self.set_machine_hub_open(true, cx);
        }
    }

    fn set_machine_hub_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.machine_hub_open = open;
        self.disarm(cx);
        if let Some(view) = &self.remote_krill {
            let server = view.read(cx).server.clone();
            server.update(cx, |server, cx| {
                server.set_hub_open(open);
                server.clear_row_state(cx);
            });
        }
        cx.notify();
    }

    fn toggle_machine_row(
        &mut self,
        host: HostId,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.machine_open = (!open).then_some(host);
        self.disarm(cx);
        if self.editing_ssh_host_id().is_some() {
            self.cancel_edit_ssh_host(window, cx);
        }
        if let Some(view) = &self.remote_krill {
            let server = view.read(cx).server.clone();
            server.update(cx, ServerPanel::clear_row_state);
        }
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
        let hub_open = self.machine_hub_open;
        server.update(cx, |server, _| server.set_hub_open(hub_open));
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

    /// 들어오는 연결 카드. 권한을 여는 일은 여기 한곳에만 있고 초대 QR은 펼쳤을 때만 보인다.
    fn render_machine_hub(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = crate::ui::palette(cx);
        let Some(view) = &self.remote_krill else {
            return crate::ui::error_banner(p, crate::i18n::link_ui::ERR_UNAVAILABLE.get())
                .into_any_element();
        };
        let open = self.machine_hub_open;
        let toggle = layout::ghost(
            "admin-local-open-invite",
            text::MACHINES_MANAGE_INVITE.get(),
        )
        .icon(if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .debug_selector(|| "admin-local-open-invite".into())
        .on_click(cx.listener(move |this, _, _, cx| this.set_machine_hub_open(!open, cx)));
        let server = view.read(cx).server.clone();
        let hub = server.update(cx, |server, cx| server.render_hub(p, open, toggle, cx));
        self.search_focus
            .anchored(
                p,
                text::TAB_REMOTE_KRILL.get(),
                div()
                    .id("admin-machine-local")
                    .debug_selector(|| "admin-machine-local".into()),
            )
            .child(hub)
            .into_any_element()
    }

    /// 나가는 머신 한 줄. 상세는 페이지를 옮기지 않고 행 아래로 펼친다.
    fn render_machine_row(
        &mut self,
        machine: &Machine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let host = machine
            .routes
            .first()
            .map_or(HostId::Local, |r| r.host.clone());
        let selector = format!("admin-machine-{}", host.as_key());
        let open = self
            .machine_open
            .as_ref()
            .is_some_and(|open| machine.routes.iter().any(|r| r.host == *open));
        let (color, state) = machine_status(machine.status(), p);

        let ssh_hosts: Vec<crate::ssh_hosts::SshHost> = machine
            .routes
            .iter()
            .filter(|r| r.kind == RouteKind::Ssh)
            .filter_map(|r| {
                self.ssh_hosts
                    .iter()
                    .find(|h| Some(h.id.as_str()) == r.host.remote_id())
                    .cloned()
            })
            .collect();

        let address_line = ssh_hosts.first().map_or_else(
            || text::MACHINES_INVITE_ROUTE_DESC.get().to_owned(),
            |host| home_relative(&host.format_summary()),
        );

        let mut title_row = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .flex_wrap()
            .child(
                div()
                    .min_w_0()
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(machine.label().to_owned()),
            );
        for route in machine.routes.iter().filter(|r| r.kind != RouteKind::Local) {
            title_row = title_row.child(crate::ui::badge(
                p,
                crate::ui::BadgeTone::Muted,
                route_label(route.kind),
            ));
        }
        title_row = title_row.child(
            div()
                .flex_none()
                .text_size(crate::ui::TEXT_XS)
                .text_color(color)
                .child(state),
        );

        let quick_connect = ssh_hosts.first().map(|ssh| {
            let target = ssh.clone();
            if self.ssh_host_status(&ssh.id) == SshHostCardStatus::Connected {
                let id = format!("admin-machine-disconnect-{}", ssh.id);
                layout::secondary(
                    gpui::ElementId::Name(id.clone().into()),
                    text::SSH_DISCONNECT.get(),
                )
                .debug_selector(move || id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.disconnect_ssh_host(&target, cx);
                }))
            } else {
                let id = format!("admin-machine-connect-{}", ssh.id);
                layout::secondary(
                    gpui::ElementId::Name(id.clone().into()),
                    text::SSH_CONNECT.get(),
                )
                .debug_selector(move || id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.connect_ssh_host(target.clone(), cx);
                }))
            }
        });

        let keyboard_host = host.clone();
        let head = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .role(gpui::Role::Button)
            .aria_label(machine.label().to_owned())
            .aria_expanded(open)
            .tab_index(0)
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_2()
            .py_2p5()
            .cursor_pointer()
            .hover(|d| d.bg(p.accent))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.toggle_machine_row(keyboard_host.clone(), open, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_machine_row(host.clone(), open, window, cx);
            }))
            .child(
                gpui_component::Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(p.muted_foreground),
            )
            .child(
                div()
                    .flex_none()
                    .size(crate::ui::STATE_DOT)
                    .rounded_full()
                    .bg(color),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(170.))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(title_row)
                    .child(
                        div()
                            .min_w_0()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(address_line),
                    ),
            )
            .child(layout::controls().children(quick_connect));

        let expand = open.then(|| self.render_machine_expand(machine, &ssh_hosts, window, cx));
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(p.border)
            .child(head)
            .children(expand)
    }

    /// 펼친 행 안: 일반 동작과 지우는 동작을 점선으로 나눈다.
    fn render_machine_expand(
        &mut self,
        machine: &Machine,
        ssh_hosts: &[crate::ssh_hosts::SshHost],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .p_2p5()
            .border_t_1()
            .border_color(p.border);
        let mut danger = layout::danger_zone(p);
        for (index, host) in ssh_hosts.iter().enumerate() {
            content = content.child(self.render_ssh_route(host, index > 0, cx));
            if self.editing_ssh_host_id() == Some(host.id.as_str()) {
                self.ensure_ssh_form(window, cx);
                content = content.child(self.render_ssh_form(cx));
            }
            danger = danger.child(self.render_ssh_remove(host, cx));
        }
        let paired = machine
            .routes
            .iter()
            .filter(|r| r.kind == RouteKind::Invite)
            .filter_map(|r| r.host.remote_id()?.strip_prefix("iroh:").map(str::to_owned));
        if let Some(view) = &self.remote_krill {
            let server = view.read(cx).server.clone();
            for id in paired {
                let (route, remove) = server.update(cx, |server, cx| {
                    (
                        server.render_remote_route(p, &id, cx),
                        server.render_remote_danger(p, &id, cx),
                    )
                });
                content = content.children(route);
                danger = danger.children(remove);
            }
        }
        content.child(danger)
    }

    /// 머신 추가: 방법을 고르는 자리는 여기 하나다.
    fn render_machine_add(
        &mut self,
        method: AddMethod,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let content = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(layout::label(
                p,
                text::MACHINES_ADD.get(),
                text::MACHINES_ADD_DESC.get(),
            ))
            .child(
                div().flex().child(crate::ui::segmented_control(
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
                )),
            );
        match method {
            AddMethod::Ssh => content.child(self.render_ssh_add(window, cx)),
            AddMethod::Invite => {
                if let Some(view) = &self.remote_krill {
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

    pub(super) fn render_machines_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        self.ensure_machine_pairing(window, cx);
        if let Some(method) = self.machine_add_focus.take()
            && self.machine_add == Some(method)
        {
            if method == AddMethod::Ssh {
                self.ensure_ssh_form(window, cx);
            }
            cx.defer_in(window, move |this, window, cx| {
                if this.machine_add != Some(method) {
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
        let adding = self.machine_add;
        let add_button = if adding.is_some() {
            layout::secondary("admin-machines-add", text::MACHINES_ADD.get())
                .icon(IconName::ChevronDown)
        } else {
            layout::primary("admin-machines-add", text::MACHINES_ADD.get()).icon(IconName::Plus)
        }
        .debug_selector(|| "admin-machines-add".into())
        .on_click(cx.listener(move |this, _, _, cx| {
            if adding.is_some() {
                this.machine_add = None;
                cx.notify();
            } else {
                this.show_machine_add(AddMethod::Invite, cx);
            }
        }));

        let mut list = div()
            .id("admin-machines-list")
            .debug_selector(|| "admin-machines-list".into())
            .flex()
            .flex_col();
        let catalog = self.machine_catalog(cx);
        let remote: Vec<&Machine> = catalog.iter().filter(|m| !m.is_local()).collect();
        if remote.is_empty() {
            list = list.child(
                div()
                    .id("admin-machines-empty")
                    .debug_selector(|| "admin-machines-empty".into())
                    .py_3()
                    .px_2()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::MACHINES_EMPTY_REMOTE.get()),
            );
        }
        for machine in remote {
            list = list.child(self.render_machine_row(machine, window, cx));
        }

        let add_panel = adding.map(|method| self.render_machine_add(method, window, cx));
        let status = adding.is_none().then(|| self.render_ssh_status(cx));
        div()
            .flex()
            .flex_col()
            .gap_3p5()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(layout::section_title(
                        p,
                        "admin-machines-local-section",
                        text::MACHINES_LOCAL_SECTION.get(),
                    ))
                    .child(self.render_machine_hub(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_wrap()
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
                                    .flex_1()
                                    .min_w(px(150.))
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(p.muted_foreground)
                                    .child(text::MACHINES_REMOTE_SECTION.get()),
                            )
                            .child(layout::controls().child(add_button)),
                    )
                    .children(add_panel)
                    .child(list)
                    .children(status),
            )
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

/// 표시용으로 홈 폴더 경로를 `~`로 줄인다.
fn home_relative(summary: &str) -> String {
    krill_proto::os::home_dir()
        .and_then(|home| home.to_str().map(str::to_owned))
        .filter(|home| home.len() > 1)
        .map_or_else(|| summary.to_owned(), |home| summary.replace(&home, "~"))
}
