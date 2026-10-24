//! 모바일 탭. IPC는 콜백 뒤에 두고 QR·선택 상태는 창 하나가 소유한다.
//!
//! 레이아웃과 선택형 경로 카드·QR/티켓 복사·기기 목록 구조는
//! `.ref/orca/src/renderer/src/components/settings/{MobilePane,MobilePairingConnectionOptions,MobilePairingPathOption,MobilePairingQrSection,MobilePairedDevicesSection}.tsx`를
//! 참고해 순수 Rust/gpui로 옮겼다.
//! 출처·저작권: Copyright (c) 2026 Lovecast Inc. (MIT License, `.ref/orca/LICENSE`).
use super::{SettingsView, layout};
use crate::i18n::catalog::mobile as text;
use gpui::{Context, FontWeight, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::{Disableable as _, IconName};
use krill_proto::mobile::{
    ConnectMode, MobileReply, MobileRequest, NetworkInterface, PairedMobileDevice,
    qr::{Qr, SIZE},
};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

pub type Call = Arc<dyn Fn(MobileRequest) -> Result<MobileReply, String> + Send + Sync>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairingState {
    Idle,
    Connecting,
    Waiting,
}

pub struct Panel {
    pub mode: ConnectMode,
    pub address: Option<String>,
    pub offer: Option<(String, u64, Qr)>,
    pub copied_url: Option<String>,
    pub devices: Vec<PairedMobileDevice>,
    pub interfaces: Vec<NetworkInterface>,
    pub connected: BTreeSet<String>,
    pub pending: bool,
    /// 연결 준비만 별도로 추적해 다른 경로 선택·기기 관리·앱 이동을 막지 않는다.
    pairing: PairingState,
    pub error: Option<String>,
    /// 페어링 링크 복사 피드백(버튼 라벨 '복사됨'). 새 오퍼가 오면 풀린다(OWE-880).
    pub copied: bool,
    pub call: Call,
    timer: Option<Task<()>>,
    generation: u64,
}
impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}
impl Default for Panel {
    fn default() -> Self {
        Self::new(Arc::new(|request| {
            krill_proto::mobile::client::request(
                &krill_proto::daemon::Paths::dev().map_err(|e| e.to_string())?,
                &request,
                krill_proto::ipc::ClientRole::App,
            )
            .map_err(|e| e.to_string())
        }))
    }
}
impl Panel {
    pub fn new(call: Call) -> Self {
        Self {
            mode: ConnectMode::Tunnel,
            address: None,
            offer: None,
            copied_url: None,
            devices: Vec::new(),
            interfaces: Vec::new(),
            connected: BTreeSet::new(),
            pending: false,
            pairing: PairingState::Idle,
            error: None,
            copied: false,
            call,
            timer: None,
            generation: 0,
        }
    }
    pub fn pairing_pending(&self) -> bool {
        self.pairing != PairingState::Idle
    }
    fn complete(&mut self, generation: u64, reply: Result<MobileReply, String>) -> bool {
        if self.generation != generation {
            return false;
        }
        self.pending = false;
        self.pairing = PairingState::Idle;
        match reply {
            Ok(reply) => self.accept(reply),
            Err(error) => self.error = Some(error),
        }
        true
    }
    fn accept(&mut self, reply: MobileReply) {
        match reply {
            MobileReply::PairingOffer {
                url,
                expires_at_ms,
                mode,
                interfaces,
                ..
            } => {
                self.mode = if mode == ConnectMode::Lan {
                    ConnectMode::Tunnel
                } else {
                    mode
                };
                if self.copied_url.as_deref() != Some(url.as_str()) {
                    self.copied_url = None;
                }
                if self.offer.as_ref().is_some_and(|(old, _, _)| old == &url) {
                    if let Some((_, expires, _)) = &mut self.offer {
                        *expires = expires_at_ms;
                    }
                    self.interfaces = interfaces;
                    return;
                }
                match Qr::encode(&url) {
                    Ok(q) => {
                        self.offer = Some((url, expires_at_ms, q));
                        self.copied = false;
                    }
                    Err(e) => self.error = Some(e.into()),
                }
                self.interfaces = interfaces;
            }
            MobileReply::GatewayStatus { mode, devices, .. } => {
                self.mode = if mode == ConnectMode::Lan {
                    ConnectMode::Tunnel
                } else {
                    mode
                };
                self.devices = devices;
            }
            MobileReply::Devices(devices) => {
                self.devices = devices;
            }
            MobileReply::ConnectedDevices(ids) => self.connected = ids.into_iter().collect(),
            MobileReply::Failed(e) => self.error = Some(e),
            MobileReply::Revoked { .. } | MobileReply::Workflow(_) => {}
        }
    }
}
impl SettingsView {
    pub(super) fn mobile_action(&mut self, request: MobileRequest, cx: &mut Context<Self>) {
        if self.mobile.pending {
            return;
        }
        if matches!(
            request,
            MobileRequest::MintPairingTicket { .. }
                | MobileRequest::SetConnectMode { .. }
                | MobileRequest::RevokeDevice { all: true, .. }
                | MobileRequest::CancelPairingTicket
        ) {
            self.mobile.offer = None;
            self.mobile.copied_url = None;
            self.mobile.copied = false;
        }
        self.mobile.generation = self.mobile.generation.wrapping_add(1);
        let generation = self.mobile.generation;
        self.mobile.pairing = if matches!(request, MobileRequest::MintPairingTicket { .. }) {
            PairingState::Connecting
        } else {
            PairingState::Idle
        };
        self.mobile.pending = !self.mobile.pairing_pending();
        if self.mobile.pairing_pending() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                this.update(cx, |this, cx| {
                    if this.mobile.generation == generation && this.mobile.pairing_pending() {
                        this.mobile.pairing = PairingState::Waiting;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        self.mobile.error = None;
        let call = Arc::clone(&self.mobile.call);
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move { call(request) })
                .await;
            this.update(cx, |this, cx| {
                if !this.mobile.complete(generation, got) {
                    return;
                }
                this.mobile_poll(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    /// 탭 진입 시 기기 목록·활성 오퍼를 자동 조회한다(Orca 마운트 조회 대응, OWE-880).
    /// 진행 중인 요청이 있으면 그 결과가 오면 폴링이 시작되므로 중복 호출하지 않는다.
    pub(super) fn mobile_enter(&mut self, cx: &mut Context<Self>) {
        if self.mobile.pending || self.mobile.pairing_pending() {
            return;
        }
        self.mobile.generation = self.mobile.generation.wrapping_add(1);
        let generation = self.mobile.generation;
        self.mobile.pending = true;
        self.mobile.error = None;
        let call = Arc::clone(&self.mobile.call);
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    let status = call(MobileRequest::Status);
                    // 활성 오퍼가 있을 때만 복원한다. 티켓이 없으면 데몬이
                    // Err를 돌려주므로 무조건 조회하면 오류가 뜬다.
                    let offer = match &status {
                        Ok(MobileReply::GatewayStatus {
                            active_offer: true, ..
                        }) => Some(call(MobileRequest::CurrentPairingOffer)),
                        _ => None,
                    };
                    (status, offer)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.mobile.generation != generation {
                    return;
                }
                this.mobile.pending = false;
                match got.0 {
                    Ok(reply) => this.mobile.accept(reply),
                    Err(e) => this.mobile.error = Some(e),
                }
                // 오퍼 복원 실패(만료 등)는 조용히 무시한다.
                if let Some(Ok(reply)) = got.1 {
                    this.mobile.accept(reply);
                }
                this.mobile_poll(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    fn mobile_poll(&mut self, cx: &mut Context<Self>) {
        if self.mobile.timer.is_some() {
            return;
        }
        let call = Arc::clone(&self.mobile.call);
        self.mobile.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok((generation, poll)) = this.read_with(cx, |this, _| {
                    (
                        this.mobile.generation,
                        this.visible && this.tab == super::AdminTab::Mobile
                            || this.mobile.pairing_pending()
                            || this.mobile.offer.is_some(),
                    )
                }) else {
                    return;
                };
                if !poll {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    continue;
                }
                let call = Arc::clone(&call);
                let data = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            call(MobileRequest::Status),
                            call(MobileRequest::ConnectedDevices),
                            call(MobileRequest::CurrentPairingOffer),
                        )
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.mobile.pending
                            || this.mobile.pairing_pending()
                            || this.mobile.generation != generation
                        {
                            return;
                        }
                        if let Ok(reply) = data.0 {
                            this.mobile.accept(reply);
                        }
                        if let Ok(reply) = data.1 {
                            this.mobile.accept(reply);
                        }
                        if this.mobile.offer.is_some() {
                            if let Ok(reply) = data.2 {
                                this.mobile.accept(reply);
                            } else {
                                this.mobile.offer = None;
                                this.mobile.copied_url = None;
                            }
                        }
                        if this.mobile.offer.as_ref().is_some_and(|(_, expires, _)| {
                            *expires <= krill_proto::mobile::client::now_ms()
                        }) {
                            this.mobile.offer = None;
                            this.mobile.copied_url = None;
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
    }
    pub(super) fn mobile_pair(
        &mut self,
        mode: ConnectMode,
        address: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.mobile.pending
            || (self.mobile.pairing_pending()
                && self.mobile.mode == mode
                && self.mobile.address == address)
        {
            return;
        }
        self.mobile.mode = mode;
        self.mobile.address.clone_from(&address);
        self.mobile_action(
            MobileRequest::MintPairingTicket {
                mode,
                custom_address: address,
                rotate: true,
            },
            cx,
        );
    }
    pub(super) fn render_mobile_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let mut root = div()
            .id("admin-section-mobile")
            .debug_selector(|| "admin-section-mobile".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(layout::section_title(
                p,
                "admin-mobile-pair-section",
                text::PAIR_SECTION.get(),
            ));

        let mode_options = [
            (
                ConnectMode::Tunnel,
                "mobile-tunnel",
                text::TUNNEL.get(),
                text::TUNNEL_DESC.get(),
            ),
            (
                ConnectMode::Tailscale,
                "mobile-tailscale",
                text::TAILSCALE.get(),
                text::TAILSCALE_DESC.get(),
            ),
        ];
        let mut mode_card = layout::card(p)
            .id("mobile-mode-card")
            .debug_selector(|| "mobile-mode-card".into())
            .p_0()
            .overflow_hidden()
            .flex()
            .flex_col();
        for (idx, (mode, id, title, desc)) in mode_options.into_iter().enumerate() {
            let selected = self.mobile.mode == mode;
            let pending = self.mobile.pending;
            let radio = div()
                .mt_0p5()
                .flex_none()
                .size(px(14.))
                .rounded_full()
                .border_1()
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |d| {
                    d.border_color(p.foreground)
                        .bg(p.foreground)
                        .child(div().size(px(6.)).rounded_full().bg(p.background))
                })
                .when(!selected, |d| {
                    d.border_color(p.muted_foreground.opacity(0.4))
                });
            let option_row = div()
                .id(id)
                .debug_selector(move || id.into())
                .w_full()
                .min_w_0()
                .px_3()
                .py_2p5()
                .flex()
                .items_start()
                .gap_3()
                .when(idx > 0, |d| d.border_t_1().border_color(p.border))
                .when(selected, |d| d.bg(p.accent.opacity(0.45)))
                .when(!selected && !pending, |d| {
                    d.hover(|h| h.bg(p.accent.opacity(0.25)))
                })
                .when(pending, |d| d.opacity(0.5).cursor_default())
                .when(!pending, |d| {
                    d.cursor_pointer().on_click(
                        cx.listener(move |this, _, _, cx| this.mobile_pair(mode, None, cx)),
                    )
                })
                .child(radio)
                .child(layout::label(p, title, Some(desc)));
            mode_card = mode_card.child(option_row);
        }
        root = root
            .child(mode_card)
            .child(layout::label(p, text::MODE_IMMEDIATE.get(), None));

        if !self.mobile.interfaces.is_empty() && self.mobile.mode == ConnectMode::Tailscale {
            let mut interfaces = layout::card(p).flex().flex_wrap().gap_1p5();
            let auto_selected = self.mobile.address.is_none();
            interfaces = interfaces.child(
                layout::segment(p, "mobile-network-auto", text::AUTO.get(), auto_selected)
                    .disabled(self.mobile.pending)
                    .debug_selector(|| "mobile-network-auto".into())
                    .on_click(
                        cx.listener(|this, _, _, cx| this.mobile_pair(this.mobile.mode, None, cx)),
                    ),
            );
            for (index, interface) in
                self.mobile
                    .interfaces
                    .iter()
                    .enumerate()
                    .filter(|(_, interface)| {
                        interface
                            .address
                            .parse::<std::net::Ipv4Addr>()
                            .is_ok_and(|ip| (u32::from(ip) & 0xffc0_0000) == 0x6440_0000)
                    })
            {
                let address = format!("{}:0", interface.address);
                let selected = self.mobile.address.as_deref() == Some(address.as_str());
                let label = format!("{} ({})", interface.name, interface.address);
                let if_id = format!("mobile-if-{index}");
                interfaces = interfaces.child(
                    layout::segment(p, ("mobile-interface", index), label, selected)
                        .disabled(self.mobile.pending)
                        .debug_selector(move || if_id.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mobile_pair(this.mobile.mode, Some(address.clone()), cx);
                        })),
                );
            }
            root = root.child(interfaces);
        }

        let show_progress = self.mobile.pairing == PairingState::Waiting;
        // 두 문구의 공간을 함께 예약해 준비 상태가 나타나도 기기 목록이 밀리지 않는다.
        let pair_label = div()
            .flex_1()
            .min_w_0()
            .min_w(px(200.))
            .grid()
            .child(
                layout::label(p, text::PAIR_TITLE.get(), Some(text::PAIR_DESC.get()))
                    .row_start(1)
                    .col_start(1)
                    .when(show_progress, Styled::invisible),
            )
            .child(
                layout::label(p, text::PREPARING.get(), Some(text::PREPARING_DESC.get()))
                    .id("mobile-pairing-progress")
                    .debug_selector(|| "mobile-pairing-progress".into())
                    .row_start(1)
                    .col_start(1)
                    .when(!show_progress, Styled::invisible),
            );
        let pair_actions = layout::row(p)
            .id("mobile-pair-actions-row")
            .debug_selector(|| "mobile-pair-actions-row".into())
            .flex_wrap()
            .child(pair_label)
            .child(
                layout::controls()
                    .flex_wrap()
                    .child(
                        layout::primary("mobile-new-qr", text::NEW_QR.get())
                            .disabled(self.mobile.pending || self.mobile.pairing_pending())
                            .debug_selector(|| "mobile-new-qr".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mobile_pair(this.mobile.mode, this.mobile.address.clone(), cx);
                            })),
                    )
                    .child(
                        layout::secondary("mobile-refresh", text::REFRESH.get())
                            .icon(IconName::RefreshCw)
                            .disabled(self.mobile.pending || self.mobile.pairing_pending())
                            .debug_selector(|| "mobile-refresh".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mobile_action(MobileRequest::Status, cx);
                            })),
                    ),
            );
        root = root.child(pair_actions);
        if self.mobile.offer.is_some() || self.mobile.pairing_pending() {
            root = root.child(
                layout::secondary("mobile-cancel-qr", text::CANCEL_QR.get())
                    .disabled(self.mobile.pending)
                    .debug_selector(|| "mobile-cancel-qr".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.mobile_action(MobileRequest::CancelPairingTicket, cx);
                    })),
            );
        }

        if let Some((url, expires, qr)) = &self.mobile.offer {
            let seconds = expires.saturating_sub(krill_proto::mobile::client::now_ms()) / 1000;
            let visual = qr_canvas(qr.clone()).size(px(220.));
            let copied = self.mobile.copied_url.as_deref() == Some(url.as_str());
            let copy_label = if copied {
                text::COPIED.get()
            } else {
                text::COPY.get()
            };
            let copy_icon = if copied {
                IconName::Check
            } else {
                IconName::Copy
            };
            let link = url.clone();
            let ticket_row = div()
                .w_full()
                .min_w_0()
                .p_2()
                .rounded(crate::ui::RADIUS_SM)
                .bg(p.background)
                .border_1()
                .border_color(p.border)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .flex_wrap()
                .child(
                    div()
                        .id("mobile-ticket-url")
                        .debug_selector(|| "mobile-ticket-url".into())
                        .flex_1()
                        .min_w_0()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .truncate()
                        .child(url.clone()),
                )
                .child(
                    layout::secondary("mobile-copy-link", copy_label)
                        .icon(copy_icon)
                        .debug_selector(|| "mobile-copy-link".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
                            // OWE-880 복사 피드백(`copied`)과 URL별 복사 상태(`copied_url`)를 함께 갱신한다.
                            this.mobile.copied = true;
                            this.mobile.copied_url = Some(link.clone());
                            cx.notify();
                        })),
                );
            root = root.child(
                layout::card(p)
                    .id("mobile-qr-card")
                    .debug_selector(|| "mobile-qr-card".into())
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(visual)
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(format!(
                                "{} {:02}:{:02}",
                                text::EXPIRES.get(),
                                seconds / 60,
                                seconds % 60
                            )),
                    )
                    .child(ticket_row),
            );
        }

        root = root.child(layout::section_title(
            p,
            "admin-mobile-devices-section",
            text::DEVICES_TITLE.get(),
        ));
        let now = krill_proto::mobile::client::now_ms();
        let active_devices: Vec<&PairedMobileDevice> = self
            .mobile
            .devices
            .iter()
            .filter(|d| d.revoked_at_ms.is_none())
            .collect();
        if active_devices.is_empty() {
            root = root.child(
                layout::card(p)
                    .id("mobile-devices-empty")
                    .debug_selector(|| "mobile-devices-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::DEVICES_EMPTY.get()),
            );
        } else {
            for (device_index, device) in active_devices.into_iter().enumerate() {
                let id = device.device_id.clone();
                let connected = self.mobile.connected.contains(&id);
                let row_id = format!("mobile-device-row-{device_index}");
                let revoke_id = format!("mobile-revoke-{device_index}");
                let status_badge = crate::ui::badge(
                    p,
                    if connected {
                        crate::ui::BadgeTone::Success
                    } else {
                        crate::ui::BadgeTone::Muted
                    },
                    if connected {
                        text::CONNECTED.get()
                    } else {
                        // OWE-880: 오프라인 표기는 `OFFLINE`을 쓴다(`PAIRED` 중복 금지).
                        text::OFFLINE.get()
                    },
                );
                // OWE-880: 상태 줄은 `device_line` 서식(경과 스케일·`NEVER` 처리)을 쓴다.
                let meta = device_line(device, connected, now);
                root = root.child(
                    layout::row(p)
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .flex_wrap()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .flex_wrap()
                                        .child(
                                            div()
                                                .text_size(crate::ui::TEXT_SM)
                                                .font_weight(FontWeight::MEDIUM)
                                                .truncate()
                                                .child(device.label.clone()),
                                        )
                                        .child(status_badge),
                                )
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(meta),
                                ),
                        )
                        .child(
                            layout::controls().child(
                                layout::secondary(
                                    ("mobile-revoke", device_index),
                                    text::REVOKE.get(),
                                )
                                .disabled(self.mobile.pending)
                                .debug_selector(move || revoke_id.clone())
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.mobile_action(
                                            MobileRequest::RevokeDevice {
                                                device_id: Some(id.clone()),
                                                all: false,
                                            },
                                            cx,
                                        );
                                    },
                                )),
                            ),
                        ),
                );
            }
        }
        // 페어링된 기기가 없을 때는 전체 해제 행을 숨긴다. 있을 때는 2단계 확인(위험 색)으로
        // 1클릭 실수를 막는다(OWE-880).
        let has_active_devices = self
            .mobile
            .devices
            .iter()
            .any(|d| d.revoked_at_ms.is_none());
        if has_active_devices {
            root = root.child(
                layout::row(p)
                    .id("mobile-revoke-all-row")
                    .debug_selector(|| "mobile-revoke-all-row".into())
                    .flex_wrap()
                    .child(
                        layout::label(p, text::REVOKE_ALL.get(), Some(text::REVOKE_ALL_DESC.get()))
                            .min_w(px(200.)),
                    )
                    .child(layout::controls().child(self.confirm_button(
                        "mobile-revoke-all",
                        text::REVOKE_ALL.get(),
                        cx,
                        |this, cx| {
                            this.mobile_action(
                                MobileRequest::RevokeDevice {
                                    device_id: None,
                                    all: true,
                                },
                                cx,
                            );
                        },
                    ))),
            );
        }
        if let Some(error) = &self.mobile.error {
            root = root.child(
                crate::ui::error_banner(p, error.clone())
                    .id("mobile-error-banner")
                    .debug_selector(|| "mobile-error-banner".into()),
            );
        }
        div().child(root)
    }
}
/// QR 행렬을 흰 바탕(조용한 영역 4칸 포함)에 그린다.
fn qr_canvas(matrix: Qr) -> gpui::Canvas<()> {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            window.paint_quad(gpui::fill(bounds, crate::ui::theme::QR_LIGHT));
            let cell = bounds.size.width / 65.;
            for y in 0..SIZE {
                for x in 0..SIZE {
                    if matrix.dark(x, y) {
                        let x = f32::from(u16::try_from(x).unwrap_or(0)) + 4.;
                        let y = f32::from(u16::try_from(y).unwrap_or(0)) + 4.;
                        window.paint_quad(gpui::fill(
                            gpui::Bounds::new(
                                bounds.origin + gpui::point(cell * x, cell * y),
                                gpui::size(cell, cell),
                            ),
                            crate::ui::theme::QR_DARK,
                        ));
                    }
                }
            }
        },
    )
}

impl SettingsView {
    /// 사이드바 'Krill 모바일' 단독 페이지(Orca 모바일 페이지 구성 참고). 왼쪽은 경로 선택·QR 카드,
    /// 오른쪽은 실제 페어링된 휴대폰 목록을 휴대폰 틀 안에 보여 준다. 기기 해제 등 관리는 설정 탭으로 넘긴다.
    pub(super) fn render_mobile_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::{
            Sizable as _,
            button::{Button, ButtonVariants as _},
        };
        let p = crate::ui::palette(cx);
        let busy = self.mobile.pending || self.mobile.pairing_pending();

        let mut modes = div().flex().gap_1p5();
        for (mode, id, label) in [
            (
                ConnectMode::Tunnel,
                "mobile-page-tunnel",
                text::PAGE_TUNNEL.get(),
            ),
            (
                ConnectMode::Tailscale,
                "mobile-page-tailscale",
                text::PAGE_TAILSCALE.get(),
            ),
        ] {
            modes = modes.child(
                layout::segment(p, id, label, self.mobile.mode == mode)
                    .disabled(self.mobile.pending)
                    .debug_selector(move || id.into())
                    .on_click(cx.listener(move |this, _, _, cx| this.mobile_pair(mode, None, cx))),
            );
        }
        let mode_desc = if self.mobile.mode == ConnectMode::Tailscale {
            text::TAILSCALE_DESC.get()
        } else {
            text::TUNNEL_DESC.get()
        };

        let qr_size = px(168.);
        let qr_box = if let Some((_, _, qr)) = &self.mobile.offer {
            div()
                .id("mobile-page-qr")
                .debug_selector(|| "mobile-page-qr".into())
                .flex_none()
                .p_2()
                .rounded(crate::ui::RADIUS_LG)
                .bg(crate::ui::theme::QR_LIGHT)
                .child(qr_canvas(qr.clone()).size(qr_size))
        } else {
            div()
                .id("mobile-page-qr-empty")
                .debug_selector(|| "mobile-page-qr-empty".into())
                .flex_none()
                .size(qr_size + px(16.))
                .rounded(crate::ui::RADIUS_LG)
                .border_1()
                .border_dashed()
                .border_color(p.border)
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .text_center()
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(if busy {
                    text::PREPARING.get()
                } else {
                    text::NEW_QR.get()
                })
        };

        let mut actions = div().flex().flex_wrap().items_center().gap_2();
        if let Some((url, expires, _)) = &self.mobile.offer {
            let copied = self.mobile.copied_url.as_deref() == Some(url.as_str());
            let link = url.clone();
            let seconds = expires.saturating_sub(krill_proto::mobile::client::now_ms()) / 1000;
            actions = actions
                .child(
                    layout::secondary(
                        "mobile-page-copy",
                        if copied {
                            text::COPIED.get()
                        } else {
                            text::COPY.get()
                        },
                    )
                    .icon(if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .debug_selector(|| "mobile-page-copy".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
                        this.mobile.copied = true;
                        this.mobile.copied_url = Some(link.clone());
                        cx.notify();
                    })),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(format!(
                            "{} {:02}:{:02}",
                            text::EXPIRES.get(),
                            seconds / 60,
                            seconds % 60
                        )),
                );
        }
        actions = actions.child(div().flex_1()).child(
            layout::primary("mobile-page-new-qr", text::NEW_QR.get())
                .disabled(busy)
                .debug_selector(|| "mobile-page-new-qr".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.mobile_pair(this.mobile.mode, this.mobile.address.clone(), cx);
                })),
        );

        let card = layout::card(p)
            .id("mobile-page-card")
            .debug_selector(|| "mobile-page-card".into())
            .w(px(460.))
            .max_w_full()
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .gap_5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(text::PAGE_TITLE.get()),
                            )
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::PAGE_DESC.get()),
                            )
                            .child(modes)
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(mode_desc),
                            ),
                    )
                    .child(qr_box),
            )
            .child(home_guide(p))
            .child(actions)
            .children(self.mobile.error.clone().map(|error| {
                crate::ui::error_banner(p, error)
                    .id("mobile-page-error")
                    .debug_selector(|| "mobile-page-error".into())
            }))
            .child(
                div().flex().child(
                    layout::ghost("mobile-page-manage", text::PAGE_MANAGE.get())
                        .icon(IconName::Settings)
                        .debug_selector(|| "mobile-page-manage".into())
                        .on_click(|_, _, cx| {
                            super::open_window_at_tab(super::AdminTab::Mobile, cx);
                        }),
                ),
            );

        let now = krill_proto::mobile::client::now_ms();
        let mut devices = div().flex().flex_col().gap_2();
        let mut any = false;
        for device in self
            .mobile
            .devices
            .iter()
            .filter(|d| d.revoked_at_ms.is_none())
        {
            any = true;
            let connected = self.mobile.connected.contains(&device.device_id);
            devices = devices.child(
                div()
                    .p_3()
                    .rounded(crate::ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.background)
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(crate::ui::icons::svg_icon(
                        crate::ui::icons::SMARTPHONE_SVG,
                        crate::ui::ICON_LG,
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
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(device.label.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(
                                        div()
                                            .flex_none()
                                            .size(crate::ui::STATE_DOT)
                                            .rounded_full()
                                            .bg(if connected {
                                                p.success
                                            } else {
                                                p.muted_foreground
                                            }),
                                    )
                                    .child(
                                        div().truncate().child(device_line(device, connected, now)),
                                    ),
                            ),
                    ),
            );
        }
        if !any {
            devices = devices.child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::PAGE_PHONE_EMPTY.get()),
            );
        }
        let phone = div()
            .id("mobile-page-phone")
            .debug_selector(|| "mobile-page-phone".into())
            .flex_none()
            .w(px(300.))
            .h(px(560.))
            .p_2()
            .rounded(px(44.))
            .border_1()
            .border_color(p.border)
            .bg(p.sidebar)
            .child(
                div()
                    .size_full()
                    .rounded(px(36.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.card)
                    .px_4()
                    .py_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(crate::ui::icons::svg_icon(
                                crate::ui::icons::SMARTPHONE_SVG,
                                crate::ui::ICON_LG,
                                p.foreground,
                            ))
                            .child(text::PAGE_BRAND.get()),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.muted_foreground)
                            .child(text::PAGE_PHONE_TITLE.get()),
                    )
                    .child(devices),
            );

        div()
            .id("mobile-page")
            .debug_selector(|| "mobile-page".into())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    super::close_settings_page_for(Some(cx.entity_id()), window, cx);
                }
            }))
            .size_full()
            .relative()
            .overflow_y_scroll()
            .bg(p.background)
            .text_color(p.foreground)
            .font_family(crate::ui::FONT_UI)
            .text_size(crate::ui::TEXT_MD)
            .child(
                div()
                    .min_h_full()
                    .p_8()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_center()
                    .gap_x(px(72.))
                    .gap_y_8()
                    .child(card)
                    .child(phone),
            )
            .child(
                div().absolute().top_3().right_3().child(
                    Button::new("mobile-page-close")
                        .icon(IconName::Close)
                        .ghost()
                        .small()
                        .tooltip(text::PAGE_CLOSE.get())
                        .debug_selector(|| "mobile-page-close".into())
                        .on_click(cx.listener(|_, _, window, cx| {
                            super::close_settings_page_for(Some(cx.entity_id()), window, cx);
                        })),
                ),
            )
    }
}

/// 홈 화면 추가 안내. 모바일 웹은 standalone 매니페스트를 갖고 있어
/// 홈 화면 아이콘으로 열면 주소창 없이 앱처럼 뜬다.
fn home_guide(p: &crate::ui::Palette) -> gpui::Stateful<gpui::Div> {
    let step = |label: &'static str| {
        div()
            .text_size(crate::ui::TEXT_XS)
            .text_color(p.foreground)
            .child(label)
    };
    div()
        .id("mobile-page-home-guide")
        .debug_selector(|| "mobile-page-home-guide".into())
        .p_3()
        .rounded(crate::ui::RADIUS_MD)
        .border_1()
        .border_color(p.border)
        .bg(p.background)
        .flex()
        .gap_3()
        .child(div().flex_none().pt_0p5().child(crate::ui::icons::svg_icon(
            crate::ui::icons::SMARTPHONE_SVG,
            crate::ui::ICON_LG,
            p.muted_foreground,
        )))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .child(text::PAGE_HOME_TITLE.get()),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::PAGE_HOME_DESC.get()),
                )
                .child(step(text::PAGE_HOME_IOS.get()))
                .child(step(text::PAGE_HOME_ANDROID.get())),
        )
}

/// 기기 행 상태 줄. 오프라인 표기는 `OFFLINE`을 쓰고(`PAIRED` 중복 금지),
/// 접속 기록이 없으면 `NEVER`, 경과는 분/시간/일 단위로 표기한다(OWE-880).
fn device_line(device: &PairedMobileDevice, connected: bool, now: u64) -> String {
    format!(
        "{} {} · {}: {} · {}: {} · ID: {}",
        if connected { "●" } else { "○" },
        if connected {
            text::CONNECTED.get()
        } else {
            text::OFFLINE.get()
        },
        text::PAIRED.get(),
        ago(now, device.created_at_ms),
        text::LAST_ACTIVE.get(),
        if device.last_seen_ms == 0 {
            text::NEVER.get().to_owned()
        } else {
            ago(now, device.last_seen_ms)
        },
        device.key_fingerprint.chars().take(12).collect::<String>()
    )
}

fn ago(now: u64, then: u64) -> String {
    let elapsed = now.saturating_sub(then);
    if elapsed < 60_000 {
        return text::JUST_NOW.get().to_owned();
    }
    let mins = elapsed / 60_000;
    if mins < 60 {
        return format!("{} {}", mins, text::MINUTES_AGO.get());
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{} {}", hours, text::HOURS_AGO.get());
    }
    format!("{} {}", hours / 24, text::DAYS_AGO.get())
}

#[cfg(test)]
mod tests;
