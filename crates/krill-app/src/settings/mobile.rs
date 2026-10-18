//! 모바일 탭. IPC는 콜백 뒤에 두고 QR·선택 상태는 창 하나가 소유한다.
use super::{SettingsView, layout};
use crate::i18n::catalog::mobile as text;
use gpui::{Context, Task, Window, div, prelude::*, px};
use gpui_component::{Disableable as _, button::Button};
use krill_proto::mobile::{
    ConnectMode, MobileReply, MobileRequest, NetworkInterface, PairedMobileDevice,
    qr::{Qr, SIZE},
};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

pub type Call = Arc<dyn Fn(MobileRequest) -> Result<MobileReply, String> + Send + Sync>;
pub struct Panel {
    pub mode: ConnectMode,
    pub address: Option<String>,
    pub offer: Option<(String, u64, Qr)>,
    pub devices: Vec<PairedMobileDevice>,
    pub interfaces: Vec<NetworkInterface>,
    pub connected: BTreeSet<String>,
    pub pending: bool,
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
            mode: ConnectMode::Lan,
            address: None,
            offer: None,
            devices: Vec::new(),
            interfaces: Vec::new(),
            connected: BTreeSet::new(),
            pending: false,
            error: None,
            copied: false,
            call,
            timer: None,
            generation: 0,
        }
    }
    fn accept(&mut self, reply: MobileReply) {
        match reply {
            MobileReply::PairingOffer {
                url,
                expires_at_ms,
                interfaces,
                ..
            } => {
                match Qr::encode(&url) {
                    Ok(q) => {
                        self.offer = Some((url, expires_at_ms, q));
                        self.copied = false;
                    }
                    Err(e) => self.error = Some(e.into()),
                }
                self.interfaces = interfaces;
            }
            MobileReply::GatewayStatus { devices, .. } | MobileReply::Devices(devices) => {
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
        ) {
            self.mobile.offer = None;
            self.mobile.copied = false;
        }
        self.mobile.generation = self.mobile.generation.wrapping_add(1);
        self.mobile.pending = true;
        self.mobile.error = None;
        let call = Arc::clone(&self.mobile.call);
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move { call(request) })
                .await;
            this.update(cx, |this, cx| {
                this.mobile.pending = false;
                match got {
                    Ok(reply) => this.mobile.accept(reply),
                    Err(e) => this.mobile.error = Some(e),
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
        if self.mobile.pending {
            return;
        }
        self.mobile.generation = self.mobile.generation.wrapping_add(1);
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
                let generation = this
                    .read_with(cx, |this, _| this.mobile.generation)
                    .unwrap_or(0);
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
                        if this.mobile.pending || this.mobile.generation != generation {
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
                            }
                        }
                        if this.mobile.offer.as_ref().is_some_and(|(_, expires, _)| {
                            *expires <= krill_proto::mobile::client::now_ms()
                        }) {
                            this.mobile.offer = None;
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
        if self.mobile.pending {
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
            .child(text::TITLE.get());
        let modes = [
            (ConnectMode::Lan, "mobile-lan", text::LAN.get()),
            (
                ConnectMode::Tailscale,
                "mobile-tailscale",
                text::TAILSCALE.get(),
            ),
            (ConnectMode::Tunnel, "mobile-tunnel", text::TUNNEL.get()),
        ]
        .into_iter()
        .map(|(mode, id, label)| {
            let selected = self.mobile.mode == mode;
            layout::segment(id, label, selected)
                .disabled(self.mobile.pending)
                .debug_selector(move || id.into())
                .on_click(cx.listener(move |this, _, _, cx| this.mobile_pair(mode, None, cx)))
        });
        root = root.child(layout::card(p).child(layout::segmented_control(p, modes)));
        if !self.mobile.interfaces.is_empty() && self.mobile.mode != ConnectMode::Tunnel {
            let mut interfaces = layout::card(p).flex().flex_wrap().gap_2();
            interfaces = interfaces.child(
                Button::new("mobile-network-auto")
                    .label(text::AUTO.get())
                    .disabled(self.mobile.pending)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.mobile_pair(this.mobile.mode, None, cx)),
                    ),
            );
            for (index, interface) in self.mobile.interfaces.iter().enumerate() {
                let address = format!("{}:0", interface.address);
                let label = format!("{} ({})", interface.name, interface.address);
                interfaces = interfaces.child(
                    Button::new(("mobile-interface", index))
                        .label(label)
                        .disabled(self.mobile.pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mobile_pair(this.mobile.mode, Some(address.clone()), cx);
                        })),
                );
            }
            root = root.child(interfaces);
        }
        if let Some((url, expires, qr)) = &self.mobile.offer {
            let seconds = expires.saturating_sub(krill_proto::mobile::client::now_ms()) / 1000;
            let matrix = qr.clone();
            let visual = gpui::canvas(
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
            .size(px(260.));
            let link = url.clone();
            root = root.child(
                layout::card(p)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(visual)
                    .child(format!(
                        "{} {:02}:{:02}",
                        text::EXPIRES.get(),
                        seconds / 60,
                        seconds % 60
                    ))
                    .child(
                        Button::new("mobile-copy-link")
                            .label(if self.mobile.copied {
                                text::COPIED.get()
                            } else {
                                text::COPY.get()
                            })
                            .debug_selector(|| "mobile-copy-link".into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    link.clone(),
                                ));
                                this.mobile.copied = true;
                                cx.notify();
                            })),
                    ),
            );
        }
        root = root.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("mobile-new-qr")
                        .label(text::NEW_QR.get())
                        .disabled(self.mobile.pending)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mobile_pair(this.mobile.mode, this.mobile.address.clone(), cx);
                        })),
                )
                .child(
                    Button::new("mobile-refresh")
                        .label(text::REFRESH.get())
                        .disabled(self.mobile.pending)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mobile_action(MobileRequest::Status, cx);
                        })),
                ),
        );
        // 페어링된 기기가 없을 때는 해제 버튼을 숨긴다. 있을 때는 2단계 확인(위험 색)으로
        // 1클릭 실수를 막는다(OWE-880).
        let active_devices = self
            .mobile
            .devices
            .iter()
            .any(|d| d.revoked_at_ms.is_none());
        if active_devices {
            root = root.child(self.confirm_button(
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
            ));
        }
        let now = krill_proto::mobile::client::now_ms();
        for (device_index, device) in self
            .mobile
            .devices
            .iter()
            .filter(|d| d.revoked_at_ms.is_none())
            .enumerate()
        {
            let id = device.device_id.clone();
            let connected = self.mobile.connected.contains(&id);
            root = root.child(
                layout::card(p)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(device.label.clone())
                    .child(device_line(device, connected, now))
                    .child(
                        Button::new(("mobile-revoke", device_index))
                            .label(text::REVOKE.get())
                            .disabled(self.mobile.pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mobile_action(
                                    MobileRequest::RevokeDevice {
                                        device_id: Some(id.clone()),
                                        all: false,
                                    },
                                    cx,
                                );
                            })),
                    ),
            );
        }
        if let Some(error) = &self.mobile.error {
            root = root.child(div().text_color(p.warning).child(error.clone()));
        }
        div().child(root)
    }
}
/// 기기 행 상태 줄. 오프라인 표기는 `OFFLINE`을 쓰고(`PAIRED` 중복 금지),
/// 접속 기록이 없으면 `NEVER`, 경과는 분/시간/일 단위로 표기한다(OWE-880).
fn device_line(device: &PairedMobileDevice, connected: bool, now: u64) -> String {
    format!(
        "{} {} · {}: {} · {}: {}",
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
        }
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
mod tests {
    use super::*;

    fn device(created_at_ms: u64, last_seen_ms: u64) -> PairedMobileDevice {
        PairedMobileDevice {
            device_id: "dev-1".into(),
            label: "아이폰".into(),
            key_fingerprint: String::new(),
            shared_secret_hash: String::new(),
            created_at_ms,
            last_seen_ms,
            revoked_at_ms: None,
        }
    }

    #[test]
    fn ago_scales_from_just_now_to_days() {
        let now = 10_000_000_000;
        assert_eq!(ago(now, now), text::JUST_NOW.get());
        assert_eq!(ago(now, now + 5_000), text::JUST_NOW.get());
        assert!(
            ago(now, now - 90_000).starts_with("1 "),
            "got {}",
            ago(now, now - 90_000)
        );
        assert!(
            ago(now, now - 3 * 3_600_000).starts_with("3 "),
            "got {}",
            ago(now, now - 3 * 3_600_000)
        );
        assert!(
            ago(now, now - 2 * 86_400_000).starts_with("2 "),
            "got {}",
            ago(now, now - 2 * 86_400_000)
        );
    }

    #[test]
    fn device_line_avoids_paired_duplication_and_handles_never_seen() {
        let now = 10_000_000_000;
        // 오프라인·접속 기록 없음: 상태는 OFFLINE 1회, 마지막 활동은 NEVER.
        let line = device_line(&device(now - 90_000, 0), false, now);
        assert!(line.starts_with("○ "), "got {line}");
        assert!(line.contains(text::OFFLINE.get()), "got {line}");
        assert!(line.contains(text::NEVER.get()), "got {line}");
        assert_eq!(
            line.matches(text::PAIRED.get()).count(),
            1,
            "PAIRED must appear exactly once (paired-at label): {line}"
        );
        // 온라인 기기: CONNECTED 1회 + 경과 시간 표기.
        let line = device_line(
            &device(now - 2 * 86_400_000, now - 3 * 3_600_000),
            true,
            now,
        );
        assert!(line.starts_with("● "), "got {line}");
        assert!(line.contains(text::CONNECTED.get()), "got {line}");
        assert!(!line.contains(text::OFFLINE.get()), "got {line}");
    }
}
