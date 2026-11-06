//! 모바일 탭. IPC는 콜백 뒤에 두고 QR·선택 상태는 창 하나가 소유한다.
//!
//! 레이아웃과 선택형 경로 카드·QR/티켓 복사·기기 목록 구조는
//! `.ref/orca/src/renderer/src/components/settings/{MobilePane,MobilePairingConnectionOptions,MobilePairingPathOption,MobilePairingQrSection,MobilePairedDevicesSection}.tsx`를
//! 참고해 순수 Rust/gpui로 옮겼다.
//! 출처·저작권: Copyright (c) 2026 Lovecast Inc. (MIT License, `.ref/orca/LICENSE`).
mod account;
use super::{SettingsView, layout};
use crate::i18n::catalog::mobile as text;
use gpui::{Context, FontWeight, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::{
    Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
};
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
    backup_address: Option<String>,
    pub pending: bool,
    /// 연결 준비만 별도로 추적해 다른 경로 선택·기기 관리·앱 이동을 막지 않는다.
    pairing: PairingState,
    pub error: Option<String>,
    /// 페어링 링크 복사 피드백(버튼 라벨 '복사됨'). 새 오퍼가 오면 풀린다(OWE-880).
    pub copied: bool,
    pub call: Call,
    timer: Option<Task<()>>,
    generation: u64,
    account: AccountView,
    /// 새 기기 연결을 펼쳤는지. 살아 있는 QR이 있거나 기기가 없으면 이 값과 상관없이 펼친다.
    pair_open: bool,
    /// 폴링이 마지막으로 그리게 한 값.
    last_shown: Option<ShownValues>,
}
/// 탭에 보이는 값 전부. 폴링 결과가 이것과 같으면 다시 그리지 않는다.
#[derive(PartialEq)]
struct ShownValues {
    mode: ConnectMode,
    devices: Vec<PairedMobileDevice>,
    connected: BTreeSet<String>,
    interfaces: Vec<NetworkInterface>,
    backup_address: Option<String>,
    offer: Option<(String, u64)>,
    copied_url: Option<String>,
    error: Option<String>,
    /// 탭이 보일 때 QR 만료 카운트다운(초). 표시가 멈추면 안 된다.
    expires_in: Option<u64>,
    /// 탭이 보일 때 현재 분. 기기 행의 경과 표시가 분 단위로 바뀐다.
    minute: Option<u64>,
}
struct AccountView {
    connect_after_login: Option<ConnectMode>,
    email: Option<String>,
    required: bool,
    pending: bool,
    busy: bool,
    generation: u64,
    timer: Option<Task<()>>,
}
impl Default for AccountView {
    fn default() -> Self {
        Self {
            connect_after_login: None,
            email: None,
            required: true,
            pending: false,
            busy: false,
            generation: 0,
            timer: None,
        }
    }
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
            backup_address: None,
            pending: false,
            pairing: PairingState::Idle,
            error: None,
            copied: false,
            call,
            timer: None,
            generation: 0,
            account: AccountView::default(),
            pair_open: false,
            last_shown: None,
        }
    }
    pub(super) fn reveal_search_hit(&mut self, title: crate::i18n::Message) {
        if title == text::PAIR_SECTION
            || title == text::PAGE_TAILSCALE
            || title == text::PAGE_TUNNEL
        {
            self.pair_open = true;
        }
    }
    fn shown_values(&self, now_ms: u64, on_screen: bool) -> ShownValues {
        ShownValues {
            mode: self.mode,
            devices: self.devices.clone(),
            connected: self.connected.clone(),
            interfaces: self.interfaces.clone(),
            backup_address: self.backup_address.clone(),
            offer: self
                .offer
                .as_ref()
                .map(|(url, expires, _)| (url.clone(), *expires)),
            copied_url: self.copied_url.clone(),
            error: self.error.clone(),
            expires_in: self
                .offer
                .as_ref()
                .filter(|_| on_screen)
                .map(|(_, expires, _)| expires.saturating_sub(now_ms) / 1000),
            minute: on_screen.then_some(now_ms / 60_000),
        }
    }
    /// 보이는 값이 지난번과 달라졌을 때만 참이다.
    fn take_shown_change(&mut self, now_ms: u64, on_screen: bool) -> bool {
        let shown = self.shown_values(now_ms, on_screen);
        if self.last_shown.as_ref() == Some(&shown) {
            return false;
        }
        self.last_shown = Some(shown);
        true
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
            MobileReply::GatewayStatus {
                mode,
                devices,
                bind_addrs,
                ..
            } => {
                self.mode = if mode == ConnectMode::Lan {
                    ConnectMode::Tunnel
                } else {
                    mode
                };
                self.devices = devices;
                self.backup_address = bind_addrs.into_iter().find(|value| {
                    value
                        .parse::<std::net::SocketAddrV4>()
                        .is_ok_and(|address| {
                            (u32::from(*address.ip()) & 0xffc0_0000) == 0x6440_0000
                        })
                });
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
    /// 탭 진입 시 활성 오퍼를 복원하고, 첫 기기 연결·단독 페이지에서는 QR을 자동 발급한다.
    /// 진행 중인 요청이 있으면 그 결과가 오면 폴링이 시작되므로 중복 호출하지 않는다.
    pub(super) fn mobile_enter(&mut self, cx: &mut Context<Self>) {
        self.mobile_account_poll(cx);
        if self.mobile.pending || self.mobile.pairing_pending() {
            return;
        }
        self.mobile.generation = self.mobile.generation.wrapping_add(1);
        let generation = self.mobile.generation;
        self.mobile.pending = true;
        self.mobile.error = None;
        let relay_signed_in = !self.mobile.account.required || self.mobile.account.email.is_some();
        let auto_mint = self.mobile_page;
        let custom_address = self.mobile.address.clone();
        let call = Arc::clone(&self.mobile.call);
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    let status = call(MobileRequest::Status);
                    let offer = match &status {
                        Ok(MobileReply::GatewayStatus {
                            active_offer,
                            mode,
                            devices,
                            ..
                        }) => {
                            if *active_offer {
                                Some(call(MobileRequest::CurrentPairingOffer))
                            } else if (auto_mint || devices.is_empty())
                                && (*mode != ConnectMode::Tunnel || relay_signed_in)
                            {
                                Some(call(MobileRequest::MintPairingTicket {
                                    mode: *mode,
                                    custom_address,
                                    rotate: false,
                                }))
                            } else {
                                None
                            }
                        }
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
        self.mobile_account_poll(cx);
        if self.mobile.timer.is_some() {
            return;
        }
        let call = Arc::clone(&self.mobile.call);
        self.mobile.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok((generation, poll, had_offer, auto_mint, mode, custom_address)) = this
                    .read_with(cx, |this, _| {
                        (
                            this.mobile.generation,
                            this.visible && this.tab == super::AdminTab::Mobile
                                || this.mobile.pairing_pending()
                                || this.mobile.offer.is_some(),
                            this.mobile.offer.is_some(),
                            this.mobile_page
                                && (this.mobile.mode != ConnectMode::Tunnel
                                    || !this.mobile.account.required
                                    || this.mobile.account.email.is_some()),
                            this.mobile.mode,
                            this.mobile.address.clone(),
                        )
                    })
                else {
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
                        let status = call(MobileRequest::Status);
                        let connected = call(MobileRequest::ConnectedDevices);
                        let offer = (had_offer || auto_mint).then(|| {
                            let offer = call(MobileRequest::CurrentPairingOffer);
                            if auto_mint
                                && !matches!(
                                    &offer,
                                    Ok(MobileReply::PairingOffer { expires_at_ms, .. })
                                        if *expires_at_ms > krill_proto::mobile::client::now_ms()
                                )
                            {
                                call(MobileRequest::MintPairingTicket {
                                    mode,
                                    custom_address,
                                    rotate: false,
                                })
                            } else {
                                offer
                            }
                        });
                        (status, connected, offer)
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
                        if let Some(offer) = data.2 {
                            if let Ok(reply) = offer {
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
                        let on_screen = this.visible
                            && (this.mobile_page || this.tab == super::AdminTab::Mobile);
                        let now = krill_proto::mobile::client::now_ms();
                        if this.mobile.take_shown_change(now, on_screen) {
                            cx.notify();
                        }
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
        if mode == ConnectMode::Tunnel
            && self.mobile.account.required
            && self.mobile.account.email.is_none()
        {
            self.mobile.mode = mode;
            self.mobile.address = address;
            self.mobile.account.connect_after_login = Some(mode);
            if !self.mobile.account.pending {
                self.mobile_account_action(false, cx);
            }
            cx.notify();
            return;
        }
        self.mobile.account.connect_after_login = None;
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
    /// 연결 경로 고르기. 탭의 패널과 사이드바 단독 화면이 함께 쓴다.
    fn mobile_mode_switch(
        &self,
        [tailscale, tunnel]: [&'static str; 2],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let pending = self.mobile.pending;
        let segment = |id: &'static str, label, mode| {
            layout::segment(p, id, label, self.mobile.mode == mode)
                .disabled(pending)
                .debug_selector(move || id.into())
                .on_click(cx.listener(move |this, _, _, cx| this.mobile_pair(mode, None, cx)))
        };
        layout::segmented_control(
            p,
            [
                segment(
                    tailscale,
                    text::PAGE_TAILSCALE.get(),
                    ConnectMode::Tailscale,
                ),
                segment(tunnel, text::PAGE_TUNNEL.get(), ConnectMode::Tunnel),
            ],
        )
    }

    fn mobile_mode_desc(&self) -> &'static str {
        if self.mobile.mode == ConnectMode::Tailscale {
            text::TAILSCALE_DESC.get()
        } else {
            text::TUNNEL_DESC.get()
        }
    }

    /// 기기 목록 아래 패널: QR과 경로를 한 묶음으로 둔다.
    #[expect(
        clippy::too_many_lines,
        reason = "The pairing panel keeps QR, path, account, interface and action rows in one place"
    )]
    fn render_pair_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let side = qr_side(px(148.), window.scale_factor());
        let qr_box = if let Some((_, expires, qr)) = &self.mobile.offer {
            let seconds = expires.saturating_sub(krill_proto::mobile::client::now_ms()) / 1000;
            div()
                .id("mobile-qr-card")
                .debug_selector(|| "mobile-qr-card".into())
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .child(qr_canvas(qr.clone()).size(side))
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
        } else {
            let signed_out_relay = self.mobile.mode == ConnectMode::Tunnel
                && self.mobile.account.required
                && self.mobile.account.email.is_none();
            let label = if signed_out_relay {
                text::QR_NEEDS_LOGIN.get()
            } else if self.mobile.pairing == PairingState::Waiting {
                text::PREPARING.get()
            } else {
                text::NEW_QR.get()
            };
            qr_placeholder(p, side, label)
                .id("mobile-qr-empty")
                .debug_selector(|| "mobile-qr-empty".into())
        };

        let mode_switch = div()
            .id("mobile-mode-card")
            .debug_selector(|| "mobile-mode-card".into())
            .flex();
        let mode_switch = self
            .search_focus
            .anchored(p, text::PAGE_TAILSCALE.get(), mode_switch);
        let mode_switch = self
            .search_focus
            .anchored(p, text::PAGE_TUNNEL.get(), mode_switch)
            .child(self.mobile_mode_switch(["mobile-tailscale", "mobile-tunnel"], cx));

        let muted_line = |content: gpui::SharedString| {
            div()
                .min_w_0()
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(content)
        };

        let mut right =
            div()
                .flex_1()
                .min_w(px(200.))
                .flex()
                .flex_col()
                .gap_2()
                .child(mode_switch)
                .child(muted_line(self.mobile_mode_desc().into()))
                .children(self.mobile.backup_address.as_ref().map(|address| {
                    muted_line(format!("{}, {address}", text::TAILSCALE.get()).into())
                }))
                .child(self.mobile_account_line(cx));

        if !self.mobile.interfaces.is_empty() && self.mobile.mode == ConnectMode::Tailscale {
            let mut interfaces = div().flex().flex_wrap().gap_1p5();
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
            right = right.child(interfaces);
        }

        let has_offer = self.mobile.offer.is_some();
        let busy = self.mobile.pending || self.mobile.pairing_pending();
        let mut actions = div()
            .id("mobile-pair-actions-row")
            .debug_selector(|| "mobile-pair-actions-row".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2();
        if let Some((url, _, _)) = &self.mobile.offer {
            let copied = self.mobile.copied_url.as_deref() == Some(url.as_str());
            let link = url.clone();
            // 두 문구의 공간을 함께 예약해 눌러도 옆 버튼이 밀리지 않는다(§7.3).
            actions = actions.child(
                crate::ui::button_with_reserved_label(
                    "mobile-copy-link",
                    text::COPY.get(),
                    text::COPIED.get(),
                    copied,
                )
                .small()
                .outline()
                .icon(if copied {
                    IconName::Check
                } else {
                    IconName::Copy
                })
                .debug_selector(|| "mobile-copy-link".into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
                    // OWE-880 복사 피드백(`copied`)과 URL별 복사 상태(`copied_url`)를 함께 갱신한다.
                    this.mobile.copied = true;
                    this.mobile.copied_url = Some(link.clone());
                    cx.notify();
                })),
            );
        }
        let new_qr = if has_offer {
            layout::secondary("mobile-new-qr", text::NEW_QR.get())
        } else {
            layout::primary("mobile-new-qr", text::NEW_QR.get())
        };
        actions = actions.child(
            new_qr
                .disabled(busy)
                .debug_selector(|| "mobile-new-qr".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.mobile_pair(this.mobile.mode, this.mobile.address.clone(), cx);
                })),
        );
        if has_offer || self.mobile.pairing_pending() {
            actions = actions.child(
                layout::ghost("mobile-cancel-qr", text::CANCEL_QR.get())
                    .disabled(self.mobile.pending)
                    .debug_selector(|| "mobile-cancel-qr".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.mobile_action(MobileRequest::CancelPairingTicket, cx);
                    })),
            );
        }
        right = right.child(actions);
        if let Some((url, _, _)) = &self.mobile.offer {
            right = right.child(
                div()
                    .id("mobile-ticket-url")
                    .debug_selector(|| "mobile-ticket-url".into())
                    .min_w_0()
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(url.clone()),
            );
        }
        let show_progress = self.mobile.pairing == PairingState::Waiting;
        // 두 안내의 공간을 함께 예약해 준비 상태가 나타나도 패널이 흔들리지 않는다.
        right = right.child(
            div()
                .grid()
                .child(
                    muted_line(text::PAIR_DESC.get().into())
                        .row_start(1)
                        .col_start(1)
                        .when(show_progress, Styled::invisible),
                )
                .child(
                    muted_line(text::PREPARING_DESC.get().into())
                        .id("mobile-pairing-progress")
                        .debug_selector(|| "mobile-pairing-progress".into())
                        .row_start(1)
                        .col_start(1)
                        .when(!show_progress, Styled::invisible),
                ),
        );

        let panel = layout::card(p)
            .id("admin-mobile-pair-section")
            .debug_selector(|| "admin-mobile-pair-section".into());
        self.search_focus
            .anchored(p, text::PAIR_SECTION.get(), panel)
            .flex()
            .flex_wrap()
            .items_start()
            .gap_3p5()
            .child(qr_box)
            .child(right)
    }

    /// 기기 먼저: 평소 보는 기기 목록으로 시작하고 QR은 펼쳤을 때만 보인다. 전체 해제는 맨 아래다.
    #[expect(
        clippy::too_many_lines,
        reason = "The tab stacks error, device list, pairing panel and danger zone"
    )]
    pub(super) fn render_mobile_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let now = krill_proto::mobile::client::now_ms();
        let active_devices: Vec<PairedMobileDevice> = self
            .mobile
            .devices
            .iter()
            .filter(|d| d.revoked_at_ms.is_none())
            .cloned()
            .collect();
        let has_offer = self.mobile.offer.is_some();
        let open = self.mobile.pair_open
            || has_offer
            || active_devices.is_empty()
            || self.mobile.pairing_pending();

        let mut root = div()
            .id("admin-section-mobile")
            .debug_selector(|| "admin-section-mobile".into())
            .flex()
            .flex_col()
            .gap_3();
        if let Some(error) = &self.mobile.error {
            root = root.child(
                crate::ui::error_banner(p, crate::i18n::cached_notice(error))
                    .id("mobile-error-banner")
                    .debug_selector(|| "mobile-error-banner".into()),
            );
        }

        let busy = self.mobile.pending || self.mobile.pairing_pending();
        let new_device = if open {
            layout::secondary("mobile-new-device", text::NEW_DEVICE.get())
        } else {
            layout::primary("mobile-new-device", text::NEW_DEVICE.get())
        };
        root = root.child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .id("admin-mobile-devices-section")
                        .debug_selector(|| "admin-mobile-devices-section".into())
                        .flex_1()
                        .min_w(px(150.))
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted_foreground)
                        .child(text::DEVICES_TITLE.get()),
                )
                .child(
                    layout::controls()
                        .child(
                            layout::ghost("mobile-refresh", text::REFRESH.get())
                                .icon(IconName::RefreshCw)
                                .disabled(busy)
                                .debug_selector(|| "mobile-refresh".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.mobile_action(MobileRequest::Status, cx);
                                })),
                        )
                        .child(
                            new_device
                                .icon(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::Plus
                                })
                                .disabled(has_offer || active_devices.is_empty())
                                .debug_selector(|| "mobile-new-device".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mobile.pair_open = !open;
                                    this.disarm(cx);
                                    cx.notify();
                                })),
                        ),
                ),
        );

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
            let mut rows = Vec::with_capacity(active_devices.len());
            for (device_index, device) in active_devices.iter().enumerate() {
                let id = device.device_id.clone();
                let connected = self.mobile.connected.contains(&id);
                let row_id = format!("mobile-device-row-{device_index}");
                let revoke_id = format!("mobile-revoke-{device_index}");
                rows.push(
                    crate::ui::group_row()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .px_2p5()
                        .py_2()
                        .gap_2()
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
                            div()
                                .flex_1()
                                .min_w(px(150.))
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_SM)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(device.label.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(device_line(device, connected, now)),
                                ),
                        )
                        .child(
                            self.confirm_button(
                                revoke_id.clone(),
                                text::REVOKE.get(),
                                cx,
                                move |this, cx| {
                                    this.mobile_action(
                                        MobileRequest::RevokeDevice {
                                            device_id: Some(id.clone()),
                                            all: false,
                                        },
                                        cx,
                                    );
                                },
                            )
                            .accessibility_label(format!(
                                "{}, {}",
                                if self.armed.as_deref() == Some(revoke_id.as_str()) {
                                    super::text::CONFIRM_AGAIN.get()
                                } else {
                                    text::REVOKE.get()
                                },
                                device.label
                            ))
                            .disabled(self.mobile.pending)
                            .debug_selector(move || revoke_id.clone()),
                        ),
                );
            }
            root = root.child(crate::ui::group(p, rows));
        }

        if open {
            root = root.child(self.render_pair_panel(window, cx));
        }

        // 페어링된 기기가 없을 때는 전체 해제 행을 숨긴다. 있을 때는 2단계 확인(위험 색)으로
        // 1클릭 실수를 막는다(OWE-880).
        if !active_devices.is_empty() {
            root = root.child(
                layout::danger_zone(p).child(
                    div()
                        .id("mobile-revoke-all-row")
                        .debug_selector(|| "mobile-revoke-all-row".into())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(layout::label(
                            p,
                            text::REVOKE_ALL.get(),
                            Some(text::REVOKE_ALL_DESC.get()),
                        ))
                        .child(
                            layout::controls().child(
                                self.confirm_button(
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
                                )
                                .disabled(self.mobile.pending),
                            ),
                        ),
                ),
            );
        }
        div().child(root)
    }
}

/// QR이 아직 없을 때의 빈 자리. QR과 같은 크기라 발급돼도 옆 칸이 밀리지 않는다.
fn qr_placeholder(p: &crate::ui::Palette, side: gpui::Pixels, label: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .size(side)
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
        .child(label)
}

/// QR 칸 하나의 최소 기기 픽셀. 1배율 화면에서 칸이 2~3px로 번갈아 그려지면 카메라가 잘 읽지 못한다.
const QR_MIN_MODULE: f32 = 4.;
/// 조용한 영역 4칸씩을 포함한 한 변 칸 수(`SIZE + 8`, 테스트가 확인한다).
const QR_MODULES: f32 = 65.;
/// `target`에 가장 가까운 QR 한 변. 칸을 정수 기기 픽셀(최소 [`QR_MIN_MODULE`])로 맞춘다.
fn qr_side(target: gpui::Pixels, scale: f32) -> gpui::Pixels {
    let scale = if scale > 0. { scale } else { 1. };
    let module = (f32::from(target) * scale / QR_MODULES)
        .round()
        .max(QR_MIN_MODULE);
    px(module * QR_MODULES / scale)
}
/// QR 행렬을 흰 바탕(조용한 영역 4칸 포함)에 그린다. 칸은 정수 기기 픽셀로 그려 폭이 번갈아 바뀌지 않게 한다.
fn qr_canvas(matrix: Qr) -> gpui::Canvas<()> {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            window.paint_quad(gpui::fill(bounds, crate::ui::theme::QR_LIGHT));
            let scale = window.scale_factor();
            let scale = if scale > 0. { scale } else { 1. };
            let module = (f32::from(bounds.size.width) * scale / QR_MODULES)
                .floor()
                .max(1.);
            let left = (f32::from(bounds.origin.x) * scale).round();
            let top = (f32::from(bounds.origin.y) * scale).round();
            let cell = px(module / scale);
            // 이어진 칸은 사각형 하나로 칠해 다시 그릴 때의 일을 줄인다.
            for y in 0..SIZE {
                let row = f32::from(u16::try_from(y).unwrap_or(0)) + 4.;
                let mut x = 0;
                while x < SIZE {
                    if !matrix.dark(x, y) {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < SIZE && matrix.dark(x, y) {
                        x += 1;
                    }
                    let column = f32::from(u16::try_from(start).unwrap_or(0)) + 4.;
                    let run = f32::from(u16::try_from(x - start).unwrap_or(0));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(
                            gpui::point(
                                px((left + module * column) / scale),
                                px((top + module * row) / scale),
                            ),
                            gpui::size(cell * run, cell),
                        ),
                        crate::ui::theme::QR_DARK,
                    ));
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let busy = self.mobile.pending || self.mobile.pairing_pending();

        let modes = self.mobile_mode_switch(["mobile-page-tailscale", "mobile-page-tunnel"], cx);
        let mode_desc = self.mobile_mode_desc();

        let qr_size = qr_side(px(168.), window.scale_factor());
        let qr_box = if let Some((_, _, qr)) = &self.mobile.offer {
            div()
                .id("mobile-page-qr")
                .debug_selector(|| "mobile-page-qr".into())
                .flex_none()
                .size(qr_size + px(16.))
                .p(crate::ui::SPACE_2)
                .rounded(crate::ui::RADIUS_LG)
                .bg(crate::ui::theme::QR_LIGHT)
                .child(qr_canvas(qr.clone()).size(qr_size))
        } else {
            qr_placeholder(
                p,
                qr_size + px(16.),
                if busy {
                    text::PREPARING.get()
                } else {
                    text::NEW_QR.get()
                },
            )
            .id("mobile-page-qr-empty")
            .debug_selector(|| "mobile-page-qr-empty".into())
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

        // 1배율에서 커진 QR만큼 카드를 넓혀 설명 칸 폭을 지킨다.
        let card_width = px(460.)
            + if qr_size > px(168.) {
                qr_size - px(168.)
            } else {
                px(0.)
            };
        let card = layout::card(p)
            .id("mobile-page-card")
            .debug_selector(|| "mobile-page-card".into())
            .flex_none()
            .w(card_width)
            .max_w_full()
            .p(crate::ui::SPACE_4)
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_5()
                    .child(
                        div()
                            .id("mobile-page-description")
                            .debug_selector(|| "mobile-page-description".into())
                            .flex_1()
                            .min_w(px(200.))
                            .max_w_full()
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
                            .children(self.mobile.backup_address.as_ref().map(|address| {
                                div()
                                    .id("mobile-page-address")
                                    .debug_selector(|| "mobile-page-address".into())
                                    .w_full()
                                    .min_w_0()
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(format!("{}, {address}", text::TAILSCALE.get()))
                            }))
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(mode_desc),
                            ),
                    )
                    .child(qr_box),
            )
            .child(self.mobile_account_line(cx))
            .child(home_guide(p))
            .child(actions)
            .children(self.mobile.error.clone().map(|error| {
                crate::ui::error_banner(p, crate::i18n::cached_notice(&error))
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
        let mut devices = div()
            .id("mobile-page-phone-devices")
            .debug_selector(|| "mobile-page-phone-devices".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2();
        let mut any = false;
        for (index, device) in self
            .mobile
            .devices
            .iter()
            .filter(|d| d.revoked_at_ms.is_none())
            .enumerate()
        {
            any = true;
            let connected = self.mobile.connected.contains(&device.device_id);
            devices = devices.child(
                div()
                    .id(("mobile-page-device", index))
                    .debug_selector(move || format!("mobile-page-device-{index}"))
                    .flex_none()
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
                                    .child(device.label.clone()),
                            )
                            .child(
                                div()
                                    .id(("mobile-page-device-status", index))
                                    .debug_selector(move || {
                                        format!("mobile-page-device-status-{index}")
                                    })
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_baseline()
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
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(device_line(device, connected, now)),
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
            .max_w_full()
            .overflow_hidden()
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
                    .px(crate::ui::SPACE_4)
                    .py_8()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_center()
                    .gap_x(px(40.))
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

/// 기기 행 상태 줄. 연결 상태 점은 행이 따로 그린다. 오프라인 표기는 `OFFLINE`을 쓰고(`PAIRED` 중복 금지),
/// 접속 기록이 없으면 `NEVER`, 경과는 분/시간/일 단위로 표기한다(OWE-880).
fn device_line(device: &PairedMobileDevice, connected: bool, now: u64) -> String {
    format!(
        "{}, {}: {}, {}: {}, ID: {}",
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
