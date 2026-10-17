//! 비개발자용 기기 짝짓기 화면·상태기계·모의 백엔드 모듈(MK-133b, OWE-669).
//!
//! 구성:
//! - [`backend`]: [`LinkBackend`] 트레이트와 테스트·미리보기용 [`MockLinkBackend`]. 모든 호출은
//!   `cx.background_executor()`에서 실행해 UI 스레드를 블로킹하지 않는다(CONVENTIONS T7).
//! - [`qr`]: [`QrMatrixSource`] 트레이트(`Vec<Vec<bool>>`)와 gpui 모듈 격자 렌더러.
//! - [`ServerPanel`]: "이 Mac에 다른 기기 접속 허용" 서버 패널(켜기/끄기 토글, 1회용 초대 링크·QR·복사·유효시간,
//!   짝지은 기기 목록 및 연결 끊기 확인 대화상자).
//! - [`ClientDialog`]: "다른 Mac 추가" 클라이언트 대화상자(초대 링크 붙여넣기·클립보드 자동 채움 제안,
//!   이 기기 이름 자동 채움, 연결 진행 상태 및 비개발자 한국어 실패 사유, 같은 Wi-Fi 근처 Mac 목록).
//! - [`PairingView`]: 서버 패널과 클라이언트 대화상자를 한 화면에서 함께 볼 수 있는 통합 뷰.

pub mod backend;
pub mod qr;
pub mod real;

use std::sync::Arc;

use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, Render, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};

pub use backend::{
    DEFAULT_INVITE_TTL_SECS, INVITE_SCHEME_PREFIX, Invite, LinkBackend, MockLinkBackend, NearbyMac,
    PairFailure, PairOutcome, PairedDevice, ServerStatus, current_epoch_secs, default_device_name,
    extract_clipboard_invite, format_device_meta, format_relative_time, format_remaining,
    is_valid_invite_link, sanitize_device_name,
};
pub use qr::{
    DEFAULT_QR_SIZE, DefaultQrMatrix, QrMatrixSource, StaticQrMatrix, build_qr_matrix,
    matrix_dimensions, render_qr_matrix,
};

use crate::i18n::link_ui as text;

/// `ServerPanel`·`ClientDialog`가 외부로 알리는 이벤트.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkUiEvent {
    /// 서버 접속 허용 상태가 변경됨.
    ServerStatusChanged(ServerStatus),
    /// 클라이언트 짝짓기가 성공하여 새 기기가 연결됨.
    Paired(PairedDevice),
    /// 기기 연결 권한이 회수(연결 끊기)됨.
    Revoked(String),
    /// 클라이언트 대화상자 닫기 요청.
    Close,
}

/// "이 Mac에 다른 기기 접속 허용" 서버 패널.
pub struct ServerPanel {
    backend: Arc<dyn LinkBackend>,
    qr_source: Option<Arc<dyn QrMatrixSource>>,
    status: ServerStatus,
    invite: Option<Invite>,
    qr_matrix: Vec<Vec<bool>>,
    copied: bool,
    devices: Vec<PairedDevice>,
    confirm_revoke: Option<PairedDevice>,
    error: Option<SharedString>,
    busy: bool,
    now_secs: u64,
    refresh_task: Option<Task<()>>,
    clock_task: Option<Task<()>>,
    task: Option<Task<()>>,
}

impl EventEmitter<LinkUiEvent> for ServerPanel {}

impl std::fmt::Debug for ServerPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerPanel")
            .field("status", &self.status)
            .field("invite", &self.invite)
            .field("copied", &self.copied)
            .field("devices", &self.devices)
            .field("confirm_revoke", &self.confirm_revoke)
            .field("busy", &self.busy)
            .field("now_secs", &self.now_secs)
            .finish_non_exhaustive()
    }
}

impl ServerPanel {
    /// 백엔드를 연결해 서버 패널을 만들고 초기 상태(`server_status`, `devices`)를 백그라운드에서 조회한다.
    pub fn new(backend: Arc<dyn LinkBackend>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            backend,
            qr_source: None,
            status: ServerStatus::default(),
            invite: None,
            qr_matrix: Vec::new(),
            copied: false,
            devices: Vec::new(),
            confirm_revoke: None,
            error: None,
            busy: false,
            now_secs: current_epoch_secs(),
            refresh_task: None,
            clock_task: None,
            task: None,
        };
        panel.refresh(cx);
        panel.start_clock(cx);
        panel
    }

    fn start_clock(&mut self, cx: &mut Context<Self>) {
        self.clock_task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let mut tick = 0u8;
            loop {
                bg.timer(std::time::Duration::from_secs(1)).await;
                let Ok(backend) = this.update(cx, |panel, cx| {
                    panel.now_secs = current_epoch_secs();
                    cx.notify();
                    (!panel.busy).then(|| panel.backend.clone())
                }) else {
                    return;
                };
                tick = (tick + 1) % 3;
                if tick == 0
                    && let Some(backend) = backend
                {
                    let snapshot = bg
                        .spawn(async move { (backend.server_status(), backend.devices()) })
                        .await;
                    if this
                        .update(cx, |panel, cx| {
                            if !panel.busy {
                                if let Ok(status) = snapshot.0 {
                                    panel.status = status;
                                }
                                if let Ok(devices) = snapshot.1 {
                                    panel.devices = devices;
                                }
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }));
    }

    /// 외부 QR 행렬 생성기(`crates/krill-link/src/qr.rs` 등)를 주입한다.
    #[must_use]
    pub fn with_qr_source(mut self, qr_source: Arc<dyn QrMatrixSource>) -> Self {
        self.qr_source = Some(qr_source);
        self
    }

    #[must_use]
    pub const fn status(&self) -> ServerStatus {
        self.status
    }

    #[must_use]
    pub const fn invite(&self) -> Option<&Invite> {
        self.invite.as_ref()
    }

    #[must_use]
    pub fn is_invite_expired(&self) -> bool {
        self.invite
            .as_ref()
            .is_some_and(|inv| inv.is_expired(self.now_secs))
    }

    #[must_use]
    pub fn qr_matrix(&self) -> &[Vec<bool>] {
        &self.qr_matrix
    }

    #[must_use]
    pub const fn copied(&self) -> bool {
        self.copied
    }

    #[must_use]
    pub fn devices(&self) -> &[PairedDevice] {
        &self.devices
    }

    #[must_use]
    pub const fn confirm_revoke(&self) -> Option<&PairedDevice> {
        self.confirm_revoke.as_ref()
    }

    #[must_use]
    pub const fn now_secs(&self) -> u64 {
        self.now_secs
    }

    /// 현재 기준 시각(Unix epoch 초)을 갱신한다(초대 링크 만료 카운트다운 및 테스트용).
    pub fn set_now_secs(&mut self, now_secs: u64, cx: &mut Context<Self>) {
        self.now_secs = now_secs;
        cx.notify();
    }

    /// 백그라운드 실행기에서 서버 상태와 짝지은 기기 목록을 새로 고친다.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        self.busy = true;
        cx.notify();
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let (status_res, devices_res) = cx
                .background_executor()
                .spawn(async move { (backend.server_status(), backend.devices()) })
                .await;
            this.update(cx, |panel, cx| {
                panel.busy = false;
                match status_res {
                    Ok(status) => panel.status = status,
                    Err(e) => panel.error = Some(e.into()),
                }
                match devices_res {
                    Ok(devices) => panel.devices = devices,
                    Err(e) => panel.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 서버 접속 허용을 켜거나 끈다(백그라운드 실행기 호출).
    pub fn set_server_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let backend = self.backend.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    if enabled {
                        backend.enable_server()
                    } else {
                        backend.disable_server()
                    }
                })
                .await;
            this.update(cx, |panel, cx| {
                panel.busy = false;
                match res {
                    Ok(status) => {
                        panel.status = status;
                        if !status.enabled {
                            panel.invite = None;
                            panel.qr_matrix.clear();
                            panel.copied = false;
                        }
                        cx.emit(LinkUiEvent::ServerStatusChanged(status));
                    }
                    Err(e) => panel.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 현재 서버 접속 허용 상태를 반전시킨다.
    pub fn toggle_server(&mut self, cx: &mut Context<Self>) {
        self.set_server_enabled(!self.status.enabled, cx);
    }

    /// 새 1회용 초대 링크와 QR 코드 행렬을 백그라운드 실행기에서 생성한다.
    pub fn create_invite(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let backend = self.backend.clone();
        let qr_source = self.qr_source.clone();
        self.busy = true;
        self.error = None;
        self.copied = false;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let invite = backend.create_invite()?;
                    let status = backend.server_status()?;
                    let matrix = qr_source.as_ref().map_or_else(
                        || backend.qr_matrix(&invite.link),
                        |src| src.encode(&invite.link),
                    )?;
                    Ok::<_, String>((status, invite, matrix))
                })
                .await;
            this.update(cx, |panel, cx| {
                panel.busy = false;
                match res {
                    Ok((status, invite, matrix)) => {
                        panel.status = status;
                        panel.invite = Some(invite);
                        panel.qr_matrix = matrix;
                        panel.copied = false;
                    }
                    Err(e) => panel.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 현재 초대 링크를 시스템 클립보드에 복사한다.
    pub fn copy_invite(&mut self, cx: &mut Context<Self>) {
        if let Some(inv) = &self.invite {
            cx.write_to_clipboard(ClipboardItem::new_string(inv.link.clone()));
            self.copied = true;
            cx.notify();
        }
    }

    /// 특정 기기의 "연결 끊기(회수)" 확인 대화상자를 연다.
    pub fn request_revoke(&mut self, device: PairedDevice, cx: &mut Context<Self>) {
        self.confirm_revoke = Some(device);
        cx.notify();
    }

    /// "연결 끊기(회수)" 확인 대화상자를 닫는다.
    pub fn cancel_revoke(&mut self, cx: &mut Context<Self>) {
        self.confirm_revoke = None;
        cx.notify();
    }

    /// 확인된 기기의 연결 권한을 백그라운드 실행기에서 회수한다.
    pub fn confirm_revoke_now(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(target) = self.confirm_revoke.take() else {
            return;
        };
        let backend = self.backend.clone();
        let target_id = target.id.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let id_for_call = target_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move { backend.revoke(&id_for_call) })
                .await;
            this.update(cx, |panel, cx| {
                panel.busy = false;
                match res {
                    Ok(devices) => {
                        panel.devices = devices;
                        cx.emit(LinkUiEvent::Revoked(target_id));
                    }
                    Err(e) => panel.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_invite_section(&self, p: &crate::ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut section = div()
            .flex()
            .flex_col()
            .gap_2p5()
            .p_3()
            .rounded(crate::ui::RADIUS_MD)
            .bg(p.background)
            .border_1()
            .border_color(p.border);

        let create_btn = Button::new("link-create-invite")
            .small()
            .primary()
            .disabled(self.busy || !self.status.enabled)
            .label(text::CREATE_INVITE.get())
            .debug_selector(|| "link-create-invite".into())
            .on_click(cx.listener(|this, _, _, cx| this.create_invite(cx)));

        section = section.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(text::INVITE_HEADING.get()),
                        )
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::INVITE_GUIDE.get()),
                        ),
                )
                .child(create_btn),
        );

        if let Some(invite) = &self.invite {
            if invite.is_expired(self.now_secs) {
                section = section.child(
                    div()
                        .id("link-invite-expired")
                        .debug_selector(|| "link-invite-expired".into())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.card)
                        .border_1()
                        .border_color(p.warning)
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.warning)
                                .child(text::INVITE_EXPIRED.get()),
                        )
                        .child(
                            Button::new("link-recreate-invite")
                                .small()
                                .primary()
                                .label(text::RECREATE_INVITE.get())
                                .debug_selector(|| "link-recreate-invite".into())
                                .on_click(cx.listener(|this, _, _, cx| this.create_invite(cx))),
                        ),
                );
            } else {
                let remaining = format_remaining(invite.remaining_secs(self.now_secs));
                let copy_label = if self.copied {
                    text::COPIED_LINK.get()
                } else {
                    text::COPY_LINK.get()
                };
                section = section.child(
                    div()
                        .id("link-invite-card")
                        .debug_selector(|| "link-invite-card".into())
                        .flex()
                        .items_start()
                        .gap_3()
                        .flex_wrap()
                        .pt_1()
                        .child(render_qr_matrix(&self.qr_matrix, 4.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .id("link-invite-text")
                                        .debug_selector(|| "link-invite-text".into())
                                        .p_2()
                                        .rounded(crate::ui::RADIUS_XS)
                                        .bg(p.muted)
                                        .border_1()
                                        .border_color(p.border)
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_XS)
                                        .child(invite.link.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("link-invite-countdown")
                                                .debug_selector(|| "link-invite-countdown".into())
                                                .text_size(crate::ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(remaining),
                                        )
                                        .child(
                                            Button::new("link-copy-invite")
                                                .small()
                                                .outline()
                                                .label(copy_label)
                                                .debug_selector(|| "link-copy-invite".into())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.copy_invite(cx);
                                                })),
                                        ),
                                ),
                        ),
                );
            }
        }

        section
    }

    fn render_devices_section(&self, p: &crate::ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut section = div().flex().flex_col().gap_2().child(
            div()
                .text_size(crate::ui::TEXT_SM)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(text::DEVICES_TITLE.get()),
        );

        if let Some(target) = &self.confirm_revoke {
            let prompt_title = text::REVOKE_CONFIRM_TITLE.format(&[("name", &target.name)]);
            section = section.child(
                div()
                    .id("link-revoke-confirm")
                    .debug_selector(|| "link-revoke-confirm".into())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.background)
                    .border_1()
                    .border_color(p.destructive)
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.destructive)
                            .child(prompt_title),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::REVOKE_CONFIRM_DETAIL.get()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("link-revoke-confirm-yes")
                                    .small()
                                    .danger()
                                    .label(text::REVOKE_CONFIRM_YES.get())
                                    .debug_selector(|| "link-revoke-confirm-yes".into())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_revoke_now(cx);
                                    })),
                            )
                            .child(
                                Button::new("link-revoke-confirm-cancel")
                                    .small()
                                    .ghost()
                                    .label(text::REVOKE_CONFIRM_CANCEL.get())
                                    .debug_selector(|| "link-revoke-confirm-cancel".into())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_revoke(cx);
                                    })),
                            ),
                    ),
            );
        }

        if self.devices.is_empty() {
            section = section.child(
                div()
                    .id("link-devices-empty")
                    .debug_selector(|| "link-devices-empty".into())
                    .p_3()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.background)
                    .border_1()
                    .border_color(p.border)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::DEVICES_EMPTY.get()),
            );
        } else {
            for dev in &self.devices {
                let row_id = format!("link-device-row-{}", dev.id);
                let btn_id = format!("link-revoke-btn-{}", dev.id);
                let meta = format_device_meta(dev, self.now_secs);
                let dev_cl = dev.clone();
                section = section.child(
                    div()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
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
                                        .text_size(crate::ui::TEXT_SM)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(dev.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(meta),
                                ),
                        )
                        .child(
                            Button::new(gpui::ElementId::Name(btn_id.clone().into()))
                                .small()
                                .outline()
                                .label(text::REVOKE_BUTTON.get())
                                .debug_selector(move || btn_id.clone())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.request_revoke(dev_cl.clone(), cx);
                                })),
                        ),
                );
            }
        }

        section
    }
}

impl Render for ServerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        let enabled = self.status.enabled;
        let desc = if enabled {
            text::SERVER_DESC_ON.get()
        } else {
            text::SERVER_DESC_OFF.get()
        };

        let toggle = gpui_component::switch::Switch::new("link-server-toggle")
            .checked(enabled)
            .disabled(self.busy)
            .label(if enabled {
                text::TOGGLE_ON.get()
            } else {
                text::TOGGLE_OFF.get()
            })
            .accessibility_label(text::SERVER_TITLE.get())
            .on_click(cx.listener(|this, enabled, _, cx| this.set_server_enabled(*enabled, cx)));

        div()
            .id("link-server-panel")
            .debug_selector(|| "link-server-panel".into())
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(crate::ui::RADIUS_MD)
            .bg(p.card)
            .text_color(p.card_foreground)
            .border_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_MD)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(text::SERVER_TITLE.get()),
                            )
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(desc),
                            ),
                    )
                    .child(
                        div().flex_none().flex().items_center().gap_1p5().child(
                            div()
                                .debug_selector(|| "link-server-toggle".into())
                                .child(toggle),
                        ),
                    ),
            )
            .when(!enabled, |d| {
                d.child(
                    div()
                        .id("link-server-off-notice")
                        .debug_selector(|| "link-server-off-notice".into())
                        .p_2p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.muted)
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::SERVER_OFF_NOTICE.get()),
                )
            })
            .when_some(self.error.clone(), |d, err| {
                d.child(
                    div()
                        .id("link-server-error")
                        .debug_selector(|| "link-server-error".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(err),
                )
            })
            .when(enabled, |d| d.child(self.render_invite_section(p, cx)))
            .child(self.render_devices_section(p, cx))
    }
}

/// 클라이언트 "다른 Mac 추가" 대화상자의 연결 진행 단계.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientPhase {
    /// 입력 대기.
    Idle,
    /// 상대 Mac과 짝짓기 진행 중.
    Connecting,
    /// 짝짓기 완료.
    Paired(PairedDevice),
    /// 짝짓기 실패 및 비개발자 한국어/영어 안내 사유.
    Failed(PairFailure),
}

/// "다른 Mac 추가" 클라이언트 대화상자.
pub struct ClientDialog {
    language: crate::i18n::Language,
    backend: Arc<dyn LinkBackend>,
    link_input: Entity<InputState>,
    device_name_input: Entity<InputState>,
    clipboard_suggestion: Option<String>,
    phase: ClientPhase,
    nearby: Vec<NearbyMac>,
    task: Option<Task<()>>,
    nearby_task: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<LinkUiEvent> for ClientDialog {}

impl std::fmt::Debug for ClientDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientDialog")
            .field("phase", &self.phase)
            .field("clipboard_suggestion", &self.clipboard_suggestion.is_some())
            .field("nearby", &self.nearby)
            .finish_non_exhaustive()
    }
}

impl ClientDialog {
    /// "다른 Mac 추가" 대화상자를 초기화한다.
    ///
    /// - 이 기기 이름 칸은 컴퓨터 이름으로 미리 채운다.
    /// - 클립보드에 `krill://pair?...` 초대 링크가 있으면 자동 채움 제안을 노출한다.
    /// - 백그라운드 실행기에서 같은 Wi-Fi의 근처 Mac(`nearby`) 목록을 조회한다.
    pub fn new(backend: Arc<dyn LinkBackend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial_name = default_device_name();
        let link_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(text::LINK_PLACEHOLDER.get()));
        let device_name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::DEVICE_NAME_PLACEHOLDER.get())
                .default_value(initial_name)
        });
        let enter_link = cx.subscribe(&link_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. })
                && !matches!(this.phase, ClientPhase::Connecting)
            {
                this.submit_pair(cx);
            }
        });
        let enter_name = cx.subscribe(&device_name_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. })
                && !matches!(this.phase, ClientPhase::Connecting)
            {
                this.submit_pair(cx);
            }
        });
        let clipboard_suggestion = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .and_then(|s| extract_clipboard_invite(&s));

        let mut dialog = Self {
            language: crate::i18n::language(),
            backend,
            link_input,
            device_name_input,
            clipboard_suggestion,
            phase: ClientPhase::Idle,
            nearby: Vec::new(),
            task: None,
            nearby_task: None,
            _subscriptions: [enter_link, enter_name],
        };
        dialog.refresh_nearby(cx);
        dialog
    }

    #[must_use]
    pub const fn phase(&self) -> &ClientPhase {
        &self.phase
    }

    #[must_use]
    pub fn link_value(&self, cx: &App) -> String {
        self.link_input.read(cx).value().to_string()
    }

    #[must_use]
    pub fn device_name_value(&self, cx: &App) -> String {
        self.device_name_input.read(cx).value().to_string()
    }

    #[must_use]
    pub fn clipboard_suggestion(&self) -> Option<&str> {
        self.clipboard_suggestion.as_deref()
    }

    #[must_use]
    pub fn nearby(&self) -> &[NearbyMac] {
        &self.nearby
    }

    /// 초대 링크 입력칸 값을 설정한다.
    pub fn set_link_value(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let val = value.into();
        self.link_input.update(cx, |input, cx| {
            input.set_value(val, window, cx);
        });
        cx.notify();
    }

    /// 이 기기 이름 입력칸 값을 설정한다.
    pub fn set_device_name_value(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let val = value.into();
        self.device_name_input.update(cx, |input, cx| {
            input.set_value(val, window, cx);
        });
        cx.notify();
    }

    /// 시스템 클립보드를 다시 확인해 `krill://pair` 링크가 있으면 자동 채움 제안을 갱신한다.
    pub fn refresh_clipboard_suggestion(&mut self, cx: &mut Context<Self>) {
        self.clipboard_suggestion = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .and_then(|s| extract_clipboard_invite(&s));
        cx.notify();
    }

    /// 클립보드 자동 채움 제안의 초대 링크를 입력칸에 채운다.
    pub fn apply_clipboard_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(link) = self.clipboard_suggestion.clone() {
            self.set_link_value(link, window, cx);
        }
    }

    /// 클립보드 텍스트를 초대 링크 입력칸에 바로 붙여넣는다.
    pub fn paste_from_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let trimmed = text.trim().to_owned();
            if let Some(valid) = extract_clipboard_invite(&trimmed) {
                self.clipboard_suggestion = Some(valid);
            }
            self.set_link_value(trimmed, window, cx);
            self.submit_pair(cx);
        }
    }

    /// 같은 Wi-Fi의 근처 Mac 목록(`nearby`)을 백그라운드 실행기에서 조회한다.
    pub fn refresh_nearby(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        self.nearby_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { backend.nearby() })
                .await;
            if let Ok(list) = res {
                this.update(cx, |dialog, cx| {
                    dialog.nearby = list;
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// 근처 Mac 항목을 선택해 초대 링크 입력칸에 채운다.
    pub fn select_nearby(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mac) = self.nearby.get(index).cloned() {
            self.set_link_value(mac.invite_link, window, cx);
        }
    }

    /// 근처 Mac 항목을 선택해 초대 링크를 채우고 즉시 짝짓기를 시작한다.
    pub fn connect_nearby(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mac) = self.nearby.get(index).cloned() {
            self.set_link_value(mac.invite_link, window, cx);
            self.submit_pair(cx);
        }
    }

    /// 입력된 초대 링크와 기기 이름으로 백그라운드 실행기에서 짝짓기(`pair`)를 실행한다.
    pub fn submit_pair(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, ClientPhase::Connecting) {
            return;
        }
        let link = self.link_input.read(cx).value().trim().to_owned();
        let device_name = sanitize_device_name(&self.device_name_input.read(cx).value());
        self.phase = ClientPhase::Connecting;
        cx.notify();
        let backend = self.backend.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { backend.pair(&link, &device_name) })
                .await;
            this.update(cx, |dialog, cx| {
                match outcome {
                    PairOutcome::Paired(dev) => {
                        dialog.phase = ClientPhase::Paired(dev.clone());
                        cx.emit(LinkUiEvent::Paired(dev));
                    }
                    PairOutcome::Failed(reason) => {
                        dialog.phase = ClientPhase::Failed(reason);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_status_banner(&self, p: &crate::ui::Palette) -> Option<gpui::Stateful<gpui::Div>> {
        match &self.phase {
            ClientPhase::Idle => None,
            ClientPhase::Connecting => Some(
                div()
                    .id("link-client-status-connecting")
                    .debug_selector(|| "link-client-status-connecting".into())
                    .p_2p5()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.muted)
                    .border_1()
                    .border_color(p.border)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::STATUS_CONNECTING.get()),
            ),
            ClientPhase::Paired(dev) => Some(
                div()
                    .id("link-client-status-paired")
                    .debug_selector(|| "link-client-status-paired".into())
                    .p_2p5()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.card)
                    .border_1()
                    .border_color(p.success)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.success)
                    .child(text::STATUS_PAIRED.format(&[("name", &dev.name)])),
            ),
            ClientPhase::Failed(reason) => {
                let code = reason.code();
                let reason_text = reason.text();
                let full_msg = text::STATUS_FAILED_PREFIX.format(&[("reason", reason_text)]);
                let specific_id = format!("link-client-failure-{code}");
                Some(
                    div()
                        .id("link-client-status-failed")
                        .debug_selector(|| "link-client-status-failed".into())
                        .p_2p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.card)
                        .border_1()
                        .border_color(p.destructive)
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(
                            div()
                                .id(SharedString::from(specific_id.clone()))
                                .debug_selector(move || specific_id.clone())
                                .child(full_msg),
                        ),
                )
            }
        }
    }

    fn render_nearby_section(
        &self,
        p: &crate::ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut section = div()
            .id("link-nearby-section")
            .debug_selector(|| "link-nearby-section".into())
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::NEARBY_TITLE.get()),
                    )
                    .child(
                        Button::new("link-nearby-refresh")
                            .small()
                            .ghost()
                            .label(text::NEARBY_REFRESH.get())
                            .debug_selector(|| "link-nearby-refresh".into())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_nearby(cx))),
                    ),
            );

        if self.nearby.is_empty() {
            section = section.child(
                div()
                    .id("link-nearby-empty")
                    .debug_selector(|| "link-nearby-empty".into())
                    .p_2p5()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.background)
                    .border_1()
                    .border_color(p.border)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::NEARBY_EMPTY.get()),
            );
        } else {
            for (idx, mac) in self.nearby.iter().enumerate() {
                let row_id = format!("link-nearby-item-{idx}");
                let conn_id = format!("link-nearby-connect-{idx}");
                section = section.child(
                    div()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .border_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(mac.name.clone()),
                        )
                        .child(
                            div().flex().items_center().gap_1p5().child(
                                Button::new(gpui::ElementId::Name(conn_id.clone().into()))
                                    .small()
                                    .primary()
                                    .label(text::NEARBY_CONNECT.get())
                                    .debug_selector(move || conn_id.clone())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.connect_nearby(idx, window, cx);
                                    })),
                            ),
                        ),
                );
            }
        }

        section
    }
}

impl Render for ClientDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.link_input.update(cx, |input, cx| {
                input.set_placeholder(text::LINK_PLACEHOLDER.get(), window, cx);
            });
            self.device_name_input.update(cx, |input, cx| {
                input.set_placeholder(text::DEVICE_NAME_PLACEHOLDER.get(), window, cx);
            });
        }
        if self.clipboard_suggestion.is_none() {
            self.clipboard_suggestion = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .and_then(|s| extract_clipboard_invite(&s));
        }

        let p = crate::ui::palette(cx);
        let current_link = self.link_input.read(cx).value().trim().to_owned();
        let show_clipboard_suggestion = self
            .clipboard_suggestion
            .as_ref()
            .is_some_and(|sug| sug != &current_link);
        let is_connecting = matches!(self.phase, ClientPhase::Connecting);

        div()
            .id("link-client-dialog")
            .debug_selector(|| "link-client-dialog".into())
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(crate::ui::RADIUS_MD)
            .bg(p.card)
            .text_color(p.card_foreground)
            .border_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_MD)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::CLIENT_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::CLIENT_DESC.get()),
                    ),
            )
            .when(show_clipboard_suggestion, |d| {
                d.child(
                    div()
                        .id("link-clipboard-suggestion")
                        .debug_selector(|| "link-clipboard-suggestion".into())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .border_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.foreground)
                                .child(text::CLIPBOARD_FOUND.get()),
                        )
                        .child(
                            Button::new("link-clipboard-fill")
                                .small()
                                .primary()
                                .label(text::CLIPBOARD_AUTO_FILL.get())
                                .debug_selector(|| "link-clipboard-fill".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply_clipboard_suggestion(window, cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(crate::ui::TEXT_XS)
                    .child(
                        div()
                            .text_color(p.muted_foreground)
                            .child(text::LINK_LABEL.get()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .id("link-client-input-link")
                                    .debug_selector(|| "link-client-input-link".into())
                                    .flex_1()
                                    .child(Input::new(&self.link_input).small()),
                            )
                            .child(
                                Button::new("link-client-paste-btn")
                                    .small()
                                    .outline()
                                    .label(text::PASTE_BUTTON.get())
                                    .debug_selector(|| "link-client-paste-btn".into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.paste_from_clipboard(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(crate::ui::TEXT_XS)
                    .child(
                        div()
                            .text_color(p.muted_foreground)
                            .child(text::DEVICE_NAME_LABEL.get()),
                    )
                    .child(
                        div()
                            .id("link-client-input-device-name")
                            .debug_selector(|| "link-client-input-device-name".into())
                            .child(Input::new(&self.device_name_input).small()),
                    ),
            )
            .children(self.render_status_banner(p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("link-client-connect-btn")
                            .small()
                            .primary()
                            .disabled(is_connecting)
                            .label(text::CONNECT_BUTTON.get())
                            .debug_selector(|| "link-client-connect-btn".into())
                            .on_click(cx.listener(|this, _, _, cx| this.submit_pair(cx))),
                    )
                    .child(
                        Button::new("link-client-close-btn")
                            .small()
                            .ghost()
                            .label(text::CLOSE_BUTTON.get())
                            .debug_selector(|| "link-client-close-btn".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(LinkUiEvent::Close))),
                    ),
            )
            .child(self.render_nearby_section(p, cx))
    }
}

/// 서버 패널("이 Mac에 다른 기기 접속 허용")과 클라이언트 대화상자("다른 Mac 추가")를
/// 한 화면에 묶어 보여주는 통합 뷰.
#[derive(Debug)]
pub struct PairingView {
    pub server: Entity<ServerPanel>,
    pub client: Entity<ClientDialog>,
    _events: Vec<Subscription>,
}

impl PairingView {
    pub fn new(backend: Arc<dyn LinkBackend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_backend = backend.clone();
        let server = cx.new(|cx| ServerPanel::new(server_backend, cx));
        let client = cx.new(|cx| ClientDialog::new(backend, window, cx));
        let events = vec![
            cx.subscribe(&server, |_, _, _: &LinkUiEvent, cx| {
                crate::settings::notify_external_change(cx);
            }),
            cx.subscribe_in(
                &client,
                window,
                |_, _, event: &LinkUiEvent, window, cx| match event {
                    LinkUiEvent::Paired(_) => crate::settings::notify_external_change(cx),
                    LinkUiEvent::Close => window.remove_window(),
                    _ => {}
                },
            ),
        ];
        Self {
            server,
            client,
            _events: events,
        }
    }
}

impl Render for PairingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        div()
            .id("link-pairing-view")
            .debug_selector(|| "link-pairing-view".into())
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .bg(p.background)
            .text_color(p.foreground)
            .flex()
            .flex_col()
            .gap_4()
            .child(self.server.clone())
            .child(self.client.clone())
    }
}

#[cfg(test)]
#[path = "link_ui/tests.rs"]
mod tests;

/// 설정·사이드바가 공유하는 실제 짝짓기 창 진입점.
pub fn open_pairing(cx: &mut App) {
    let Some(state_dir) = crate::settings::state_dir(cx) else {
        return;
    };
    let executable = krill_proto::daemon::sibling_exe("krill-link")
        .unwrap_or_else(|_| std::path::PathBuf::from("krill-link"));
    let backend = Arc::new(real::RealLinkBackend::new(state_dir, executable));
    let options = gpui::WindowOptions {
        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::centered(
            None,
            gpui::size(px(980.), px(780.)),
            cx,
        ))),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(text::CLIENT_TITLE.get().into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| PairingView::new(backend, window, cx));
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })
    .ok();
}
