use super::super::*;

impl SettingsView {
    pub(super) fn render_ssh_add(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if !self.tailscale_loaded {
            self.refresh_tailscale_peers(cx);
        }
        self.ensure_ssh_form(window, cx);
        self.refresh_ssh_duplicates(cx);
        let p = crate::ui::palette(cx);
        let mut root = div().flex().flex_col().gap_3().child(
            layout::row(p)
                .child(layout::label(
                    p,
                    text::SSH_TITLE.get(),
                    Some(text::SSH_DESC.get()),
                ))
                .child(
                    layout::controls()
                        .child(
                            layout::secondary("admin-ssh-import", text::SSH_IMPORT.get())
                                .debug_selector(|| "admin-ssh-import".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.import_ssh_hosts(cx);
                                })),
                        )
                        .child(
                            layout::secondary("admin-ssh-open-toml", text::SSH_OPEN_TOML.get())
                                .debug_selector(|| "admin-ssh-open-toml".into())
                                .on_click(|_, _, cx| {
                                    open_file("hosts.toml", "", cx);
                                }),
                        ),
                ),
        );

        // 같은 기기 중복 등록(별칭·IP·Orca 가져오기) 정리 — 두 번 눌러 확인.
        if !self.ssh_duplicates.is_empty() {
            let label_of = |id: &str| {
                self.ssh_hosts
                    .iter()
                    .find(|h| h.id == id)
                    .map_or_else(|| id.to_owned(), |h| h.label.clone())
            };
            let list = self
                .ssh_duplicates
                .iter()
                .map(|(dup, kept)| format!("{} → {}", label_of(dup), label_of(kept)))
                .collect::<Vec<_>>()
                .join(", ");
            let remove = self.confirm_button(
                "admin-ssh-dedupe",
                text::SSH_DUPES_REMOVE.get(),
                cx,
                Self::remove_ssh_duplicates,
            );
            root = root.child(
                layout::card(p)
                    .id("admin-ssh-duplicates")
                    .debug_selector(|| "admin-ssh-duplicates".into())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(layout::label(
                        p,
                        text::SSH_DUPES_TITLE
                            .format(&[("count", &self.ssh_duplicates.len().to_string())]),
                        format!("{} {list}", text::SSH_DUPES_DESC.get()),
                    ))
                    .child(remove),
            );
        }

        // 1. Tailscale 자동 감지 피어("발견된 기기") — 없거나 꺼져 있으면 조용히 숨김
        if !self.tailscale_peers.is_empty() {
            let mut peer_card = layout::card(p)
                .id("admin-ssh-discovered")
                .debug_selector(|| "admin-ssh-discovered".into())
                .flex()
                .flex_col()
                .gap_2()
                .child(layout::label(
                    p,
                    text::SSH_DISCOVERED_TITLE.get(),
                    Some(text::SSH_DISCOVERED_DESC.get()),
                ));
            for (idx, peer) in self.tailscale_peers.iter().enumerate() {
                let already_added = self.ssh_hosts.iter().any(|h| {
                    h.destination == peer.address
                        || h.destination.ends_with(&format!("@{}", peer.address))
                        || h.label == peer.hostname
                });
                let btn_id = format!("admin-ssh-add-peer-{idx}");
                let peer_cl = peer.clone();
                let addr_detail = match &peer.ipv4 {
                    Some(ip) if ip != &peer.address => format!("{} ({ip})", peer.address),
                    _ => peer.address.clone(),
                };
                let action_btn = if already_added {
                    layout::ghost(
                        gpui::ElementId::Name(btn_id.clone().into()),
                        text::SSH_DISCOVERED_ADDED.get(),
                    )
                    .debug_selector(move || btn_id.clone())
                } else {
                    layout::secondary(
                        gpui::ElementId::Name(btn_id.clone().into()),
                        text::SSH_DISCOVERED_ADD.get(),
                    )
                    .debug_selector(move || btn_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.add_tailscale_peer(&peer_cl, cx);
                    }))
                };
                peer_card = peer_card.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .p_2p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .border_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::ui::TEXT_SM)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(peer.hostname.clone()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_SM)
                                        .text_color(p.muted_foreground)
                                        .child(addr_detail),
                                ),
                        )
                        .child(layout::controls().child(action_btn)),
                );
            }
            root = root.child(peer_card);
        }

        root.child(self.render_ssh_form(cx))
            .child(self.render_ssh_status(cx))
    }

    pub(super) fn render_ssh_form(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let mut root = div().flex().flex_col().gap_3();
        // 2. SSH 호스트 추가·편집 2열 그리드 폼(OWE-800): [표시 이름 | 호스트], [포트 | SSH 키 경로]
        if let Some(form) = &self.ssh_form {
            let label_input = form.label.clone();
            let addr_input = form.address.clone();
            let port_input = form.port.clone();
            let key_input = form.identity_file.clone();
            let is_editing = form.editing_id.is_some();
            let form_error = form.error.clone();
            let form_title = if is_editing {
                text::SSH_EDIT_TITLE.get()
            } else {
                text::SSH_ADD_TITLE.get()
            };
            let submit_label = if is_editing {
                text::SSH_EDIT_SUBMIT.get()
            } else {
                text::SSH_ADD_SUBMIT.get()
            };

            let mut form_card = layout::card(p)
                .id("admin-ssh-form")
                .debug_selector(|| "admin-ssh-form".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(form_title),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .gap_2p5()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_NAME_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-label")
                                        .debug_selector(|| "admin-ssh-input-label".into())
                                        .child(
                                            gpui_component::input::Input::new(&label_input).small(),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_ADDRESS_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-address")
                                        .debug_selector(|| "admin-ssh-input-address".into())
                                        .child(
                                            gpui_component::input::Input::new(&addr_input).small(),
                                        ),
                                ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .gap_2p5()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_PORT_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-port")
                                        .debug_selector(|| "admin-ssh-input-port".into())
                                        .child(
                                            gpui_component::input::Input::new(&port_input).small(),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_KEY_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-key")
                                        .debug_selector(|| "admin-ssh-input-key".into())
                                        .child(
                                            gpui_component::input::Input::new(&key_input).small(),
                                        ),
                                ),
                        ),
                );

            if let Some(err_msg) = form_error {
                form_card = form_card.child(
                    div()
                        .id("admin-ssh-form-error")
                        .debug_selector(|| "admin-ssh-form-error".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(err_msg),
                );
            }

            let mut form_controls = layout::controls();
            if is_editing {
                form_controls = form_controls.child(
                    layout::ghost("admin-ssh-edit-cancel", text::SSH_EDIT_CANCEL.get())
                        .small()
                        .debug_selector(|| "admin-ssh-edit-cancel".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.cancel_edit_ssh_host(window, cx);
                        })),
                );
            }
            form_controls = form_controls.child(
                layout::primary("admin-ssh-add-submit", submit_label)
                    .small()
                    .debug_selector(|| "admin-ssh-add-submit".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit_ssh_host_form(window, cx);
                    })),
            );

            form_card = form_card.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(form_controls),
            );
            root = root.child(form_card);
        }

        root
    }

    pub(super) fn render_ssh_status(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let mut root = div().flex().flex_col().gap_3();
        // 3. 상태 배너 및 첫 연결 도우미("이 기기 등록하기" / 키 생성 확인)
        if let Some(status) = &self.ssh_status {
            let mut status_box = div()
                .id("admin-ssh-status")
                .debug_selector(|| "admin-ssh-status".into())
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .p_2p5()
                .rounded(crate::ui::RADIUS_SM)
                .bg(p.card)
                .border_1()
                .border_color(p.border)
                .child(div().text_size(crate::ui::TEXT_XS).child(status.clone()));
            if let Some(failed_host) = &self.ssh_auth_failed_host {
                let host_for_reg = failed_host.clone();
                status_box = status_box.child(
                    layout::controls().child(
                        layout::primary(
                            "admin-ssh-register-device",
                            text::SSH_REGISTER_BUTTON.get(),
                        )
                        .debug_selector(|| "admin-ssh-register-device".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_register_ssh_host(host_for_reg.clone(), cx);
                        })),
                    ),
                );
            }
            if self.ssh_keygen_confirm_host.is_some() {
                status_box = status_box.child(
                    div()
                        .id("admin-ssh-keygen-confirm")
                        .debug_selector(|| "admin-ssh-keygen-confirm".into())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .border_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .child(text::SSH_KEYGEN_CONFIRM_PROMPT.get()),
                        )
                        .child(
                            layout::controls()
                                .child(
                                    layout::primary(
                                        "admin-ssh-keygen-confirm-yes",
                                        text::SSH_KEYGEN_CONFIRM_YES.get(),
                                    )
                                    .debug_selector(|| "admin-ssh-keygen-confirm-yes".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.confirm_keygen_and_register(cx);
                                        },
                                    )),
                                )
                                .child(
                                    layout::ghost(
                                        "admin-ssh-keygen-cancel",
                                        text::SSH_KEYGEN_CANCEL.get(),
                                    )
                                    .debug_selector(|| "admin-ssh-keygen-cancel".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.ssh_keygen_confirm_host = None;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        ),
                );
            }
            root = root.child(status_box);
        }

        root
    }

    #[expect(
        clippy::too_many_lines,
        reason = "SSH route card retains connection, test, install and edit controls"
    )]
    pub(super) fn render_ssh_host_card(
        &mut self,
        host: &crate::ssh_hosts::SshHost,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let status = self.ssh_host_status(&host.id);
        let (dot_color, status_label, status_text_color) = match status {
            SshHostCardStatus::Connected => {
                (p.success, text::SSH_STATUS_CONNECTED.get(), p.success)
            }
            SshHostCardStatus::Disconnected => (
                p.muted_foreground.opacity(0.4),
                text::SSH_STATUS_DISCONNECTED.get(),
                p.muted_foreground,
            ),
            SshHostCardStatus::Error => {
                (p.destructive, text::SSH_STATUS_ERROR.get(), p.destructive)
            }
        };
        let card_id = format!("admin-ssh-card-{}", host.id);
        let dot_id = format!("admin-ssh-status-dot-{}", host.id);
        let remote_id = format!("admin-ssh-remote-{}", host.id);
        let conn_id = format!("admin-ssh-connect-{}", host.id);
        let disc_id = format!("admin-ssh-disconnect-{}", host.id);
        let test_id = format!("admin-ssh-test-{}", host.id);
        let probe_id = format!("admin-ssh-probe-{}", host.id);
        let install_id = format!("admin-ssh-install-{}", host.id);
        let edit_id = format!("admin-ssh-edit-{}", host.id);
        let rm_id = format!("admin-ssh-remove-{}", host.id);
        let host_conn = host.clone();
        let host_disc = host.clone();
        let host_cl = host.clone();
        let host_probe = host.clone();
        let host_install = host.clone();
        let host_edit = host.clone();
        let rm_host_id = host.id.clone();
        let card_state = self.ssh_remote_infos.get(&host.id);
        let (remote_text, remote_is_error) = remote_line_text(card_state);
        // OWE-897: 설치·업데이트 버튼은 프로브 완료 + 지원 플랫폼 + 구버전/미설치일 때만.
        let install_label: Option<&'static str> = card_state.and_then(|st| match &st.probe {
            RemoteProbeState::Ready(info) if info.platform.is_supported() => {
                match info.version_status(krill_proto::BUILD) {
                    krill_hosts::managed::VersionStatus::NotInstalled
                    | krill_hosts::managed::VersionStatus::Unknown => {
                        Some(text::SSH_REMOTE_INSTALL.get())
                    }
                    krill_hosts::managed::VersionStatus::UpdateAvailable { .. } => {
                        Some(text::SSH_REMOTE_UPDATE.get())
                    }
                    krill_hosts::managed::VersionStatus::UpToDate => None,
                }
            }
            _ => None,
        });
        let install_running =
            card_state.is_some_and(|st| matches!(st.install, RemoteInstallState::Running));
        let probe_loading =
            card_state.is_some_and(|st| matches!(st.probe, RemoteProbeState::Loading));
        let probe_label = match card_state {
            Some(st)
                if matches!(
                    st.probe,
                    RemoteProbeState::Ready(_) | RemoteProbeState::Failed(_)
                ) =>
            {
                text::SSH_REMOTE_REFRESH.get()
            }
            _ => text::SSH_REMOTE_PROBE.get(),
        };

        let connect_or_disconnect_btn = if status == SshHostCardStatus::Connected {
            layout::secondary(
                gpui::ElementId::Name(disc_id.clone().into()),
                text::SSH_DISCONNECT.get(),
            )
            .small()
            .debug_selector(move || disc_id.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.disconnect_ssh_host(&host_disc, cx);
            }))
        } else {
            layout::secondary(
                gpui::ElementId::Name(conn_id.clone().into()),
                text::SSH_CONNECT.get(),
            )
            .small()
            .debug_selector(move || conn_id.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.connect_ssh_host(host_conn.clone(), cx);
            }))
        };

        let probe_btn = if probe_loading {
            layout::ghost(
                gpui::ElementId::Name(probe_id.clone().into()),
                text::SSH_REMOTE_CHECKING.get(),
            )
            .small()
            .debug_selector(move || probe_id.clone())
        } else {
            layout::secondary(gpui::ElementId::Name(probe_id.clone().into()), probe_label)
                .small()
                .debug_selector(move || probe_id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.refresh_remote_info(host_probe.clone(), cx);
                }))
        };

        let mut controls = layout::controls()
            .w_full()
            .flex_wrap()
            .flex_shrink_1()
            .child(connect_or_disconnect_btn)
            .child({
                let testing = self.is_ssh_test_pending(&host.id);
                crate::ui::button_with_reserved_label(
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
                    this.test_ssh_host(host_cl.clone(), cx);
                }))
            })
            .child(probe_btn);
        if install_running {
            let running_id = install_id.clone();
            controls = controls.child(
                layout::ghost(
                    gpui::ElementId::Name(install_id.clone().into()),
                    text::SSH_REMOTE_INSTALLING.get(),
                )
                .small()
                .debug_selector(move || running_id.clone()),
            );
        } else if let Some(label) = install_label {
            let install_id_cl = install_id.clone();
            controls = controls.child(
                layout::secondary(gpui::ElementId::Name(install_id.clone().into()), label)
                    .small()
                    .debug_selector(move || install_id_cl.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.install_remote(host_install.clone(), cx);
                    })),
            );
        }
        controls = controls
            .child(
                layout::ghost(
                    gpui::ElementId::Name(edit_id.clone().into()),
                    text::SSH_EDIT.get(),
                )
                .small()
                .debug_selector(move || edit_id.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_edit_ssh_host(&host_edit, window, cx);
                })),
            )
            // OWE-897: 삭제는 두 번 눌러 확인(파괴적 동작 규칙).
            .child(
                self.confirm_button(
                    rm_id.clone(),
                    text::SSH_REMOVE.get(),
                    cx,
                    move |this, cx| {
                        this.remove_ssh_host(&rm_host_id, cx);
                    },
                )
                .small(),
            );

        layout::row(p)
            .id(gpui::ElementId::Name(card_id.clone().into()))
            .debug_selector(move || card_id.clone())
            .flex_col()
            .items_start()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(crate::ui::icons::svg_box(
                        crate::ui::icons::SERVER_SVG,
                        crate::ui::ICON_MD,
                        p.muted_foreground,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(crate::ui::TEXT_SM)
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(host.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .id(gpui::ElementId::Name(
                                                        dot_id.clone().into(),
                                                    ))
                                                    .debug_selector(move || dot_id.clone())
                                                    .size(crate::ui::STATE_DOT)
                                                    .rounded_full()
                                                    .bg(dot_color),
                                            )
                                            .child(
                                                div()
                                                    .text_size(crate::ui::TEXT_XS)
                                                    .text_color(status_text_color)
                                                    .child(status_label),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(host.format_summary()),
                            )
                            .child(
                                div()
                                    .id(gpui::ElementId::Name(remote_id.clone().into()))
                                    .debug_selector(move || remote_id.clone())
                                    .min_w_0()
                                    .truncate()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .text_color(if remote_is_error {
                                        p.destructive
                                    } else {
                                        p.muted_foreground
                                    })
                                    .child(remote_text),
                            ),
                    ),
            )
            .child(controls)
    }
}
