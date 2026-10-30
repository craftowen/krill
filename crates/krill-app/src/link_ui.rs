//! 비개발자용 기기 페어링 화면·상태기계·모의 백엔드 모듈(MK-133b, OWE-669).
//!
//! 구성:
//! - [`backend`]: [`LinkBackend`] 트레이트와 테스트·미리보기용 [`MockLinkBackend`]. 모든 호출은
//!   `cx.background_executor()`에서 실행해 UI 스레드를 블로킹하지 않는다(CONVENTIONS T7).
//! - [`qr`]: [`QrMatrixSource`] 트레이트(`Vec<Vec<bool>>`)와 gpui 모듈 격자 렌더러.
//! - [`ServerPanel`]: "이 컴퓨터에 다른 기기 접속 허용" 서버 패널(켜기/끄기 토글, 1회용 초대 링크·QR·복사·유효시간,
//!   페어링된 기기 목록 및 행内 2단계 연결 끊기 확인 버튼).
//! - [`ClientDialog`]: "다른 컴퓨터 추가" 클라이언트 대화상자(초대 링크 붙여넣기·클립보드 자동 채움 제안,
//!   이 기기 이름 자동 채움, 연결 진행 상태 및 비개발자 한국어 실패 사유, 같은 Wi-Fi 근처 컴퓨터 목록).
//! - [`PairingView`]: 서버 패널과 클라이언트 대화상자를 한 화면에서 함께 볼 수 있는 통합 뷰.

pub mod backend;
pub mod qr;
pub mod real;

use std::sync::Arc;

use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, Render, SharedString, Subscription, Task,
    Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};

pub use backend::{
    DEFAULT_INVITE_TTL_SECS, INVITE_SCHEME_PREFIX, Invite, LinkBackend, MockLinkBackend, NearbyMac,
    PairFailure, PairOutcome, PairedDevice, PairedHost, ServerStatus, current_epoch_secs,
    default_device_name, extract_clipboard_invite, format_device_meta, format_relative_time,
    format_remaining, is_valid_invite_link, sanitize_device_name,
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
    /// 클라이언트 페어링이 성공하여 새 기기가 연결됨.
    Paired(PairedDevice),
    /// 기기 연결 권한이 회수(연결 끊기)됨.
    Revoked(String),
    /// 이 Mac에 저장한 원격 Mac 연결이 제거됨.
    HostDisconnected(String),
    /// 원격 Mac 표시 이름이 바뀜.
    HostRenamed(String),
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
    remote_hosts: Vec<PairedHost>,
    renaming: Option<HostRename>,
    confirm_revoke: Option<PairedDevice>,
    confirm_disconnect: Option<PairedHost>,
    /// 업데이트를 요청한 원격 호스트 id. 상대가 새 버전으로 다시 붙으면 목록에서 빠진다.
    updating_hosts: std::collections::BTreeSet<String>,
    confirmation_deadline: Option<std::time::Instant>,
    rename_return_focus: Option<gpui::FocusHandle>,
    focus: gpui::FocusHandle,
    error: Option<SharedString>,
    busy: bool,
    generation: u64,
    now_secs: u64,
    refresh_task: Option<Task<()>>,
    clock_task: Option<Task<()>>,
    visible: bool,
    task: Option<Task<()>>,
}

/// 원격 Mac 목록에서 이름을 바꾸는 중인 행.
struct HostRename {
    id: String,
    input: Entity<InputState>,
    _subscription: Subscription,
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
            remote_hosts: Vec::new(),
            renaming: None,
            confirm_revoke: None,
            confirm_disconnect: None,
            updating_hosts: std::collections::BTreeSet::new(),
            confirmation_deadline: None,
            rename_return_focus: None,
            focus: cx.focus_handle(),
            error: None,
            busy: false,
            generation: 0,
            now_secs: current_epoch_secs(),
            refresh_task: None,
            clock_task: None,
            visible: true,
            task: None,
        };
        panel.refresh(cx);
        panel.start_clock(cx);
        panel
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.confirm_revoke = None;
        self.confirm_disconnect = None;
        self.confirmation_deadline = None;
        if visible {
            self.refresh(cx);
        }
    }

    fn start_clock(&mut self, cx: &mut Context<Self>) {
        self.clock_task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let mut tick = 0u8;
            loop {
                bg.timer(std::time::Duration::from_secs(1)).await;
                let Ok(backend) = this.update(cx, |panel, cx| {
                    if !panel.visible
                        && !panel.busy
                        && panel
                            .invite
                            .as_ref()
                            .is_none_or(|invite| invite.is_expired(current_epoch_secs()))
                    {
                        return None;
                    }
                    if panel
                        .confirmation_deadline
                        .is_some_and(|deadline| std::time::Instant::now() >= deadline)
                    {
                        panel.confirm_revoke = None;
                        panel.confirm_disconnect = None;
                        panel.confirmation_deadline = None;
                    }
                    panel.now_secs = current_epoch_secs();
                    if panel.visible {
                        cx.notify();
                    }
                    (!panel.busy).then(|| (panel.backend.clone(), panel.generation))
                }) else {
                    return;
                };
                tick = (tick + 1) % 3;
                if tick == 0
                    && let Some((backend, generation)) = backend
                {
                    let snapshot = bg
                        .spawn(async move {
                            (
                                backend.server_status(),
                                backend.devices(),
                                backend.remote_hosts(),
                            )
                        })
                        .await;
                    if this
                        .update(cx, |panel, cx| {
                            if !panel.busy && panel.generation == generation {
                                if let Ok(status) = snapshot.0 {
                                    panel.status = status;
                                }
                                if let Ok(devices) = snapshot.1 {
                                    panel.devices = devices;
                                }
                                if let Ok(hosts) = snapshot.2 {
                                    panel.remote_hosts = hosts;
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

    /// 백그라운드 실행기에서 서버 상태와 페어링된 기기 목록을 새로 고친다.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let backend = self.backend.clone();
        self.busy = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        cx.notify();
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let (status_res, devices_res, hosts_res) = cx
                .background_executor()
                .spawn(async move {
                    (
                        backend.server_status(),
                        backend.devices(),
                        backend.remote_hosts(),
                    )
                })
                .await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
                panel.busy = false;
                match status_res {
                    Ok(status) => panel.status = status,
                    Err(e) => panel.error = Some(e.into()),
                }
                match devices_res {
                    Ok(devices) => panel.devices = devices,
                    Err(e) => panel.error = Some(e.into()),
                }
                match hosts_res {
                    Ok(hosts) => panel.remote_hosts = hosts,
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
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
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
                if panel.generation != generation {
                    return;
                }
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
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
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
                if panel.generation != generation {
                    return;
                }
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

    /// 특정 기기의 "연결 끊기(회수)" 버튼을 무장한다. 같은 버튼을 다시 누르면 실행된다.
    pub fn request_revoke(&mut self, device: PairedDevice, cx: &mut Context<Self>) {
        self.confirm_disconnect = None;
        self.confirm_revoke = Some(device);
        self.confirmation_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        cx.notify();
    }

    /// "연결 끊기(회수)" 버튼 무장을 푼다.
    pub fn cancel_revoke(&mut self, cx: &mut Context<Self>) {
        self.confirm_revoke = None;
        cx.notify();
    }

    /// 확인된 기기의 연결 권한을 백그라운드 실행기에서 회수한다.
    pub fn confirm_revoke_now(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self
            .confirmation_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            self.cancel_revoke(cx);
            return;
        }
        let Some(target) = self.confirm_revoke.take() else {
            return;
        };
        let backend = self.backend.clone();
        let target_id = target.id.clone();
        self.busy = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.error = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let id_for_call = target_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move { backend.revoke(&id_for_call) })
                .await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
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

    #[must_use]
    pub fn renaming_host(&self) -> Option<&str> {
        self.renaming.as_ref().map(|r| r.id.as_str())
    }

    /// 원격 Mac 행을 이름 입력칸으로 바꾼다. Enter는 저장, Escape는 취소.
    pub fn start_rename(&mut self, host: &PairedHost, window: &mut Window, cx: &mut Context<Self>) {
        self.rename_return_focus = window.focused(cx);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(host.name.clone()));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_rename(window, cx);
                }
            });
        self.renaming = Some(HostRename {
            id: host.id.clone(),
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn finish_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<HostRename> {
        let rename = self.renaming.take()?;
        let focus = self
            .rename_return_focus
            .take()
            .unwrap_or_else(|| self.focus.clone());
        window.focus(&focus, cx);
        Some(rename)
    }

    pub fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        drop(self.finish_rename(window, cx));
        cx.notify();
    }

    /// 입력한 이름을 저장한다. 비어 있거나 그대로면 저장 없이 닫는다.
    pub fn save_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(rename) = self.finish_rename(window, cx) else {
            return;
        };
        let name = rename.input.read(cx).value().trim().to_owned();
        let unchanged = self
            .remote_hosts
            .iter()
            .any(|h| h.id == rename.id && h.name == name);
        if name.is_empty() || unchanged {
            cx.notify();
            return;
        }
        let backend = self.backend.clone();
        let id = rename.id;
        self.busy = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.error = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let id_for_call = id.clone();
            let res = cx
                .background_executor()
                .spawn(async move { backend.rename_host(&id_for_call, &name) })
                .await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
                panel.busy = false;
                match res {
                    Ok(hosts) => {
                        panel.remote_hosts = hosts;
                        cx.emit(LinkUiEvent::HostRenamed(id));
                    }
                    Err(e) => panel.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn update_host(&mut self, id: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.error = None;
        let backend = self.backend.clone();
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let call_id = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { backend.update_host(&call_id) })
                .await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
                panel.busy = false;
                match result {
                    Ok(()) => {
                        panel.updating_hosts.insert(id);
                    }
                    Err(err) => panel.error = Some(err.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn request_disconnect(&mut self, host: PairedHost, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.confirm_revoke = None;
        self.confirm_disconnect = Some(host);
        self.confirmation_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        cx.notify();
    }

    fn cancel_disconnect(&mut self, cx: &mut Context<Self>) {
        self.confirm_disconnect = None;
        cx.notify();
    }

    fn confirm_disconnect_now(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self
            .confirmation_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            self.cancel_disconnect(cx);
            return;
        }
        let Some(host) = self.confirm_disconnect.take() else {
            return;
        };
        self.busy = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.error = None;
        let backend = self.backend.clone();
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let id = host.id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { backend.disconnect_host(&id) })
                .await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
                panel.busy = false;
                match result {
                    Ok(hosts) => {
                        panel.remote_hosts = hosts;
                        crate::hosts::link::notify_connections_changed(cx);
                        cx.emit(LinkUiEvent::HostDisconnected(host.id));
                    }
                    Err(err) => panel.error = Some(err.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_invite_section(&self, p: &crate::ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        // 서버 카드 안에 또 카드를 얹지 않는다(STYLEGUIDE §8.4). 초대 영역은 카드 안
        // 구분 구역으로 평탄하게 그린다.
        let mut section = div().flex().flex_col().gap_2p5();

        let create_btn = crate::ui::primary("link-create-invite", text::CREATE_INVITE.get())
            .disabled(self.busy || !self.status.enabled)
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
                        .flex_1()
                        .min_w_0()
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
                .child(crate::ui::controls().child(create_btn)),
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
                        .bg(p.warning.opacity(0.1))
                        .border_1()
                        .border_color(p.warning.opacity(0.3))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.warning)
                                .child(text::INVITE_EXPIRED.get()),
                        )
                        .child(
                            crate::ui::controls().child(
                                crate::ui::primary(
                                    "link-recreate-invite",
                                    text::RECREATE_INVITE.get(),
                                )
                                .debug_selector(|| "link-recreate-invite".into())
                                .on_click(cx.listener(|this, _, _, cx| this.create_invite(cx))),
                            ),
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
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .id("link-invite-text")
                                        .debug_selector(|| "link-invite-text".into())
                                        .w_full()
                                        .min_w_0()
                                        .truncate()
                                        .p_2()
                                        .rounded(crate::ui::RADIUS_XS)
                                        .bg(p.muted)
                                        .border_1()
                                        .border_color(p.border)
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_SM)
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
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .font_family(crate::ui::FONT_MONO)
                                                .text_size(crate::ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(remaining),
                                        )
                                        .child(
                                            crate::ui::controls().child(
                                                crate::ui::secondary(
                                                    "link-copy-invite",
                                                    copy_label,
                                                )
                                                .debug_selector(|| "link-copy-invite".into())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.copy_invite(cx);
                                                })),
                                            ),
                                        ),
                                ),
                        ),
                );
            }
        }

        section
    }

    fn render_remote_hosts(
        &self,
        p: &crate::ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use crate::i18n::catalog::sidebar as labels;
        let mut section = div()
            .id("link-remote-hosts")
            .debug_selector(|| "link-remote-hosts".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(text::REMOTE_HOSTS_TITLE.get()),
            );
        if self.remote_hosts.is_empty() {
            return section.child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::REMOTE_HOSTS_EMPTY.get()),
            );
        }
        for host in &self.remote_hosts {
            let (color, label) = match host.status {
                krill_hosts::HostStatus::Connected => (p.success, labels::HOST_CONNECTED.get()),
                krill_hosts::HostStatus::Connecting => (p.warning, labels::HOST_CONNECTING.get()),
                krill_hosts::HostStatus::Disconnected => {
                    (p.muted_foreground, labels::HOST_DISCONNECTED.get())
                }
            };
            let outdated = host.status == krill_hosts::HostStatus::Connected
                && host
                    .version
                    .as_deref()
                    .and_then(|v| krill_proto::control::is_newer(krill_proto::BUILD, v))
                    == Some(true);
            let selector = format!("link-remote-host-{}", host.id);
            let button_id = format!("link-remote-disconnect-{}", host.id);
            let button_selector = button_id.clone();
            let target = host.clone();
            let out_id = host.id.clone();
            let armed = self
                .confirm_disconnect
                .as_ref()
                .is_some_and(|target| target.id == host.id);
            let editing = self.renaming.as_ref().filter(|r| r.id == host.id);
            let name: gpui::AnyElement = match editing {
                Some(rename) => div()
                    .id("link-remote-rename-input")
                    .debug_selector(|| "link-remote-rename-input".into())
                    .flex_1()
                    .min_w_0()
                    .capture_action(cx.listener(
                        |this, _: &gpui_component::input::Escape, window, cx| {
                            cx.stop_propagation();
                            this.cancel_rename(window, cx);
                        },
                    ))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            cx.stop_propagation();
                            this.cancel_rename(window, cx);
                        }
                    }))
                    .child(Input::new(&rename.input).small())
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::ui::TEXT_SM)
                    .child(host.name.clone())
                    .into_any_element(),
            };
            let actions = if editing.is_some() {
                crate::ui::controls()
                    .child(
                        crate::ui::ghost(
                            "link-remote-rename-cancel",
                            text::RENAME_HOST_CANCEL.get(),
                        )
                        .debug_selector(|| "link-remote-rename-cancel".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel_rename(window, cx)),
                        ),
                    )
                    .child(
                        crate::ui::primary("link-remote-rename-save", text::RENAME_HOST_SAVE.get())
                            .disabled(self.busy)
                            .debug_selector(|| "link-remote-rename-save".into())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_rename(window, cx)),
                            ),
                    )
            } else {
                let button_id = format!("link-remote-rename-{}", host.id);
                let button_selector = button_id.clone();
                let target = host.clone();
                crate::ui::controls().child(
                    crate::ui::secondary(
                        SharedString::from(button_id),
                        text::RENAME_HOST_BUTTON.get(),
                    )
                    .disabled(self.busy)
                    .debug_selector(move || button_selector.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_rename(&target, window, cx);
                    })),
                )
            };
            section = section.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(crate::ui::STATE_DOT).rounded_full().bg(color))
                            .child(name)
                            .children(host.version.as_ref().map(|v| {
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(v.clone())
                            }))
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(color)
                                    .child(label),
                            )
                            .when(outdated && editing.is_none(), |d| {
                                if self.updating_hosts.contains(&host.id) {
                                    return d.child(
                                        div()
                                            .text_size(crate::ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(text::UPDATE_HOST_STARTED.get()),
                                    );
                                }
                                let update_id = format!("link-remote-update-{}", host.id);
                                let update_selector = update_id.clone();
                                let target_id = host.id.clone();
                                d.child(
                                    crate::ui::controls().child(
                                        crate::ui::primary(
                                            SharedString::from(update_id),
                                            text::UPDATE_HOST_BUTTON.get(),
                                        )
                                        .disabled(self.busy)
                                        .debug_selector(move || update_selector.clone())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.update_host(target_id.clone(), cx);
                                            }),
                                        ),
                                    ),
                                )
                            })
                            .child(actions)
                            .when(editing.is_none(), |d| {
                                d.child(
                                    crate::ui::controls().child(
                                        crate::ui::danger_confirm(
                                            gpui::ElementId::Name(button_id.into()),
                                            text::REVOKE_BUTTON.get(),
                                            text::REVOKE_CONFIRM_AGAIN.get(),
                                            armed,
                                        )
                                        .disabled(self.busy)
                                        .debug_selector(move || button_selector.clone())
                                        .when(armed, |button| {
                                            button.on_mouse_down_out(cx.listener(
                                                move |this, _, _, cx| {
                                                    if this
                                                        .confirm_disconnect
                                                        .as_ref()
                                                        .is_some_and(|h| h.id == out_id)
                                                    {
                                                        this.cancel_disconnect(cx);
                                                    }
                                                },
                                            ))
                                        })
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                if this
                                                    .confirm_disconnect
                                                    .as_ref()
                                                    .is_some_and(|h| h.id == target.id)
                                                {
                                                    this.confirm_disconnect_now(cx);
                                                } else {
                                                    this.request_disconnect(target.clone(), cx);
                                                }
                                            }),
                                        ),
                                    ),
                                )
                            }),
                    )
                    .when(armed, |row| {
                        row.child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::REMOTE_DISCONNECT_DETAIL.get()),
                        )
                    }),
            );
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

        if self.devices.is_empty() {
            section = section.child(
                div()
                    .id("link-devices-empty")
                    .debug_selector(|| "link-devices-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::DEVICES_EMPTY.get()),
            );
        } else {
            let last = self.devices.len() - 1;
            for (index, dev) in self.devices.iter().enumerate() {
                let row_id = format!("link-device-row-{}", dev.id);
                let btn_id = format!("link-revoke-btn-{}", dev.id);
                let meta = format_device_meta(dev, self.now_secs);
                let armed = self
                    .confirm_revoke
                    .as_ref()
                    .is_some_and(|target| target.id == dev.id);
                let dev_id = dev.id.clone();
                let dev_cl = dev.clone();
                let btn_dbg = btn_id.clone();
                let out_id = dev.id.clone();
                section = section.child(
                    div()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .py_2()
                        .when(index != last, |d| d.border_b_1().border_color(p.border))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(crate::ui::TEXT_SM)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(dev.name.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(crate::ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(meta),
                                ),
                        )
                        .child(
                            crate::ui::controls().child(
                                crate::ui::danger_confirm(
                                    gpui::ElementId::Name(btn_id.clone().into()),
                                    text::REVOKE_BUTTON.get(),
                                    text::REVOKE_CONFIRM_AGAIN.get(),
                                    armed,
                                )
                                .debug_selector(move || btn_dbg.clone())
                                .when(armed, |b| {
                                    b.on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                                        if this
                                            .confirm_revoke
                                            .as_ref()
                                            .is_some_and(|t| t.id == out_id)
                                        {
                                            this.cancel_revoke(cx);
                                        }
                                    }))
                                })
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if this
                                            .confirm_revoke
                                            .as_ref()
                                            .is_some_and(|t| t.id == dev_id)
                                        {
                                            this.confirm_revoke_now(cx);
                                        } else {
                                            this.request_revoke(dev_cl.clone(), cx);
                                        }
                                    },
                                )),
                            ),
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

        let toggle_label = if enabled {
            text::TOGGLE_ON.get()
        } else {
            text::TOGGLE_OFF.get()
        };
        let weak = cx.entity().downgrade();
        let toggle = crate::ui::switch(
            p,
            "link-server-toggle",
            enabled,
            self.busy,
            move |next, _, cx| {
                weak.update(cx, |this, cx| this.set_server_enabled(next, cx))
                    .ok();
            },
        )
        .aria_label(text::SERVER_TITLE.get());

        crate::ui::card(p)
            .id("link-server-panel")
            .debug_selector(|| "link-server-panel".into())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "tab" | "escape")
                    && (this.confirm_revoke.is_some() || this.confirm_disconnect.is_some())
                {
                    this.confirm_revoke = None;
                    this.confirm_disconnect = None;
                    this.confirmation_deadline = None;
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                    }
                    cx.notify();
                }
            }))
            .flex()
            .flex_col()
            .gap_3()
            .text_color(p.card_foreground)
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
                                    .text_size(crate::ui::TEXT_SM)
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
                        crate::ui::controls()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(toggle_label),
                            )
                            .child(toggle),
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
                    crate::ui::error_banner(p, err)
                        .id("link-server-error")
                        .debug_selector(|| "link-server-error".into()),
                )
            })
            .when(enabled, |d| d.child(self.render_invite_section(p, cx)))
            .child(self.render_devices_section(p, cx))
            .child(self.render_remote_hosts(p, cx))
    }
}

/// 클라이언트 "다른 Mac 추가" 대화상자의 연결 진행 단계.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientPhase {
    /// 입력 대기.
    Idle,
    /// 상대 Mac과 페어링 진행 중.
    Connecting,
    /// 페어링 완료.
    Paired(PairedDevice),
    /// 페어링 실패 및 비개발자 한국어/영어 안내 사유.
    Failed(PairFailure),
}

/// "다른 Mac 추가" 클라이언트 대화상자.
pub struct ClientDialog {
    generation: u64,
    nearby_generation: u64,
    clipboard_generation: u64,
    language: crate::i18n::Language,
    backend: Arc<dyn LinkBackend>,
    link_input: Entity<InputState>,
    host_name_input: Entity<InputState>,
    device_name_input: Entity<InputState>,
    clipboard_suggestion: Option<String>,
    phase: ClientPhase,
    nearby: Vec<NearbyMac>,
    task: Option<Task<()>>,
    nearby_task: Option<Task<()>>,
    _subscriptions: [Subscription; 3],
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
        let host_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(text::HOST_NAME_PLACEHOLDER.get()));
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
        let enter_host_name = cx.subscribe(&host_name_input, |this, _, event: &InputEvent, cx| {
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
        let clipboard_suggestion = None;

        let mut dialog = Self {
            language: crate::i18n::language(),
            generation: 0,
            nearby_generation: 0,
            clipboard_generation: 0,
            backend,
            link_input,
            host_name_input,
            device_name_input,
            clipboard_suggestion,
            phase: ClientPhase::Idle,
            nearby: Vec::new(),
            task: None,
            nearby_task: None,
            _subscriptions: [enter_link, enter_host_name, enter_name],
        };
        dialog.refresh_nearby(cx);
        dialog.refresh_clipboard_suggestion(cx);
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
    pub fn host_name_value(&self, cx: &App) -> String {
        self.host_name_input.read(cx).value().to_string()
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

    /// 연결할 Mac 이름 입력칸 값을 설정한다.
    pub fn set_host_name_value(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let val = value.into();
        self.host_name_input.update(cx, |input, cx| {
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
        self.clipboard_generation = self.clipboard_generation.wrapping_add(1);
        let generation = self.clipboard_generation;
        let read = cx.read_from_clipboard_async();
        cx.spawn(async move |this, cx| {
            let suggestion = read
                .await
                .ok()
                .flatten()
                .and_then(|item| item.text())
                .and_then(|text| extract_clipboard_invite(&text));
            this.update(cx, |this, cx| {
                if this.clipboard_generation != generation {
                    return;
                }
                this.clipboard_suggestion = suggestion;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 클립보드 자동 채움 제안의 초대 링크를 입력칸에 채운다.
    pub fn apply_clipboard_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(link) = self.clipboard_suggestion.clone() {
            self.set_link_value(link, window, cx);
        }
    }

    /// 클립보드 텍스트를 초대 링크 입력칸에 채운다. 기기 이름을 확인·수정할 수 있게
    /// 바로 연결하지 않고 사용자가 [연결](또는 Enter)으로 제출한다(OWE-878).
    pub fn paste_from_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clipboard_generation = self.clipboard_generation.wrapping_add(1);
        let generation = self.clipboard_generation;
        let initial = self.link_input.read(cx).value();
        let read = cx.read_from_clipboard_async();
        cx.spawn_in(window, async move |this, cx| {
            let text = read.await.ok().flatten().and_then(|item| item.text());
            if let Some(text) = text {
                this.update_in(cx, |this, window, cx| {
                    if this.clipboard_generation != generation
                        || this.link_input.read(cx).value() != initial
                    {
                        return;
                    }
                    let trimmed = text.trim().to_owned();
                    if let Some(valid) = extract_clipboard_invite(&trimmed) {
                        this.clipboard_suggestion = Some(valid);
                    }
                    this.set_link_value(trimmed, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// 클라이언트 카드 입력 초기화. 통합 페어링 창 안에 있으므로 OS 창을 닫지 않고
    /// 입력칸·진행 상태만 되돌린다(OWE-878). 이 기기 이름은 유지한다.
    pub fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.clipboard_generation = self.clipboard_generation.wrapping_add(1);
        self.task = None;
        self.phase = ClientPhase::Idle;
        self.set_link_value("", window, cx);
        self.set_host_name_value("", window, cx);
        cx.notify();
    }

    /// 같은 Wi-Fi의 근처 Mac 목록(`nearby`)을 백그라운드 실행기에서 조회한다.
    pub fn refresh_nearby(&mut self, cx: &mut Context<Self>) {
        self.nearby_generation = self.nearby_generation.wrapping_add(1);
        let generation = self.nearby_generation;
        let backend = self.backend.clone();
        self.nearby_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { backend.nearby() })
                .await;
            if let Ok(list) = res {
                this.update(cx, |dialog, cx| {
                    if dialog.nearby_generation != generation {
                        return;
                    }
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

    /// 근처 Mac 항목을 선택해 초대 링크를 채우고 즉시 페어링를 시작한다.
    pub fn connect_nearby(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mac) = self.nearby.get(index).cloned() {
            self.set_link_value(mac.invite_link, window, cx);
            self.submit_pair(cx);
        }
    }

    /// 입력된 초대 링크·연결할 Mac 이름·기기 이름으로 백그라운드 실행기에서 페어링(`pair`)를 실행한다.
    pub fn submit_pair(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, ClientPhase::Connecting) {
            return;
        }
        let link = self.link_input.read(cx).value().trim().to_owned();
        let device_name = sanitize_device_name(&self.device_name_input.read(cx).value());
        let host_name = self.host_name_input.read(cx).value().to_string();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.phase = ClientPhase::Connecting;
        cx.notify();
        let backend = self.backend.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { backend.pair(&link, &device_name, &host_name) })
                .await;
            this.update(cx, |dialog, cx| {
                if dialog.generation != generation {
                    return;
                }
                match outcome {
                    PairOutcome::Paired(dev) => {
                        dialog.phase = ClientPhase::Paired(dev.clone());
                        crate::hosts::link::notify_connections_changed(cx);
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
                    .bg(p.success.opacity(0.1))
                    .border_1()
                    .border_color(p.success.opacity(0.3))
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
                        .child(
                            crate::ui::error_banner(p, full_msg)
                                .debug_selector(move || specific_id.clone()),
                        ),
                )
            }
        }
    }

    /// 같은 Wi-Fi 근처 Mac 목록. 실제 탐색이 지원되지 않아 목록이 비어 있으면
    /// 섹션 자체를 그리지 않는다(미구현 크롬 노출 금지, STYLEGUIDE §7.1, OWE-878).
    fn render_nearby_section(
        &self,
        p: &crate::ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if self.nearby.is_empty() {
            return None;
        }
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
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::NEARBY_TITLE.get()),
                    )
                    .child(
                        crate::ui::controls().child(
                            Button::new("link-nearby-refresh")
                                .small()
                                .ghost()
                                .label(text::NEARBY_REFRESH.get())
                                .debug_selector(|| "link-nearby-refresh".into())
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_nearby(cx))),
                        ),
                    ),
            );

        let last = self.nearby.len() - 1;
        for (idx, mac) in self.nearby.iter().enumerate() {
            let row_id = format!("link-nearby-item-{idx}");
            let conn_id = format!("link-nearby-connect-{idx}");
            let conn_dbg = conn_id.clone();
            section = section.child(
                div()
                    .id(SharedString::from(row_id.clone()))
                    .debug_selector(move || row_id.clone())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .py_2()
                    .when(idx != last, |d| d.border_b_1().border_color(p.border))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(mac.name.clone()),
                    )
                    .child(
                        crate::ui::controls().child(
                            crate::ui::primary(
                                gpui::ElementId::Name(conn_id.clone().into()),
                                text::NEARBY_CONNECT.get(),
                            )
                            .debug_selector(move || conn_dbg.clone())
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.connect_nearby(idx, window, cx);
                                },
                            )),
                        ),
                    ),
            );
        }

        Some(section)
    }
}

impl Render for ClientDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.link_input.update(cx, |input, cx| {
                input.set_placeholder(text::LINK_PLACEHOLDER.get(), window, cx);
            });
            self.host_name_input.update(cx, |input, cx| {
                input.set_placeholder(text::HOST_NAME_PLACEHOLDER.get(), window, cx);
            });
            self.device_name_input.update(cx, |input, cx| {
                input.set_placeholder(text::DEVICE_NAME_PLACEHOLDER.get(), window, cx);
            });
        }

        let p = crate::ui::palette(cx);
        let current_link = self.link_input.read(cx).value().trim().to_owned();
        let show_clipboard_suggestion = self
            .clipboard_suggestion
            .as_ref()
            .is_some_and(|sug| sug != &current_link);
        let is_connecting = matches!(self.phase, ClientPhase::Connecting);

        crate::ui::card(p)
            .id("link-client-dialog")
            .debug_selector(|| "link-client-dialog".into())
            .flex()
            .flex_col()
            .gap_3()
            .text_color(p.card_foreground)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
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
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.foreground)
                                .child(text::CLIPBOARD_FOUND.get()),
                        )
                        .child(
                            crate::ui::controls().child(
                                crate::ui::primary(
                                    "link-clipboard-fill",
                                    text::CLIPBOARD_AUTO_FILL.get(),
                                )
                                .debug_selector(|| "link-clipboard-fill".into())
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.apply_clipboard_suggestion(window, cx);
                                    },
                                )),
                            ),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(crate::ui::TEXT_XS)
                    .child(crate::ui::field_label(text::LINK_LABEL.get(), None, p))
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
                                    .min_w_0()
                                    .child(Input::new(&self.link_input).small()),
                            )
                            .child(
                                crate::ui::controls().child(
                                    crate::ui::secondary(
                                        "link-client-paste-btn",
                                        text::PASTE_BUTTON.get(),
                                    )
                                    .debug_selector(|| "link-client-paste-btn".into())
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.paste_from_clipboard(window, cx);
                                        },
                                    )),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(crate::ui::TEXT_XS)
                    .child(crate::ui::field_label(text::HOST_NAME_LABEL.get(), None, p))
                    .child(
                        div()
                            .id("link-client-input-host-name")
                            .debug_selector(|| "link-client-input-host-name".into())
                            .child(Input::new(&self.host_name_input).small()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(crate::ui::TEXT_XS)
                    .child(crate::ui::field_label(
                        text::DEVICE_NAME_LABEL.get(),
                        None,
                        p,
                    ))
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
                    .justify_end()
                    .gap_2()
                    .child(
                        crate::ui::ghost("link-client-close-btn", text::CLIENT_CLEAR.get())
                            .debug_selector(|| "link-client-close-btn".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_form(window, cx);
                            })),
                    )
                    .child(
                        crate::ui::primary("link-client-connect-btn", text::CONNECT_BUTTON.get())
                            .disabled(is_connecting)
                            .debug_selector(|| "link-client-connect-btn".into())
                            .on_click(cx.listener(|this, _, _, cx| this.submit_pair(cx))),
                    ),
            )
            .children(self.render_nearby_section(p, cx))
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
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.server
            .update(cx, |server, cx| server.set_visible(visible, cx));
    }

    pub fn new(backend: Arc<dyn LinkBackend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_backend = backend.clone();
        let server = cx.new(|cx| ServerPanel::new(server_backend, cx));
        let client = cx.new(|cx| ClientDialog::new(backend, window, cx));
        let events = vec![
            cx.subscribe(&server, |_, _, _: &LinkUiEvent, cx| {
                crate::settings::notify_external_change(cx);
            }),
            cx.subscribe_in(&client, window, |_, _, event: &LinkUiEvent, _, cx| {
                if let LinkUiEvent::Paired(_) = event {
                    crate::settings::notify_external_change(cx);
                }
            }),
        ];
        Self {
            server,
            client,
            _events: events,
        }
    }
}

impl Render for PairingView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("link-pairing-view")
            .debug_selector(|| "link-pairing-view".into())
            .w_full()
            .min_w_0()
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

/// 앱 메뉴 등이 공유하는 원격 Krill 설정 페이지 진입점.
///
/// 메인 창(`Shell`) 안 설정의 원격 Krill 탭을 연다.
/// 호스트가 등록돼 있으면 거기에 위임하고, 없으면(테스트·호스트 미기동) 경고만 남긴다.
pub fn open_pairing(cx: &mut App) {
    // 현재 업데이트가 끝난 뒤 디스패치한다. 호출자가 `Shell` 업데이트 안이어도
    // 핸들러가 `Shell`을 다시 읽다가 재진입 panic이 나지 않게 한다.
    cx.defer(|cx| {
        if !dispatch_host(HostCommand::Open, cx) {
            tracing::warn!("remote Krill settings host missing; ignoring open_pairing");
        }
    });
}

/// 메인 창(`Shell`)이 원격 Krill 설정을 인앱으로 열 때 쓰는 명령.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCommand {
    /// 원격 Krill 설정 페이지를 연다.
    Open,
}

type HostHandler = std::rc::Rc<dyn Fn(HostCommand, &mut App) -> Option<bool>>;

struct HostRegistry {
    handlers: Vec<HostHandler>,
}

impl gpui::Global for HostRegistry {}

/// 메인 창(`Shell`)을 원격 Krill 설정 호스트로 등록한다.
/// 콜백은 호스트 엔티티가 이미 해제됐으면 `None`, 이 창이 처리했으면 `Some(true)`,
/// 건너뛰면 `Some(false)`를 돌려준다.
pub fn register_host(
    cx: &mut App,
    handler: impl Fn(HostCommand, &mut App) -> Option<bool> + 'static,
) {
    let handler: HostHandler = std::rc::Rc::new(handler);
    if cx.has_global::<HostRegistry>() {
        cx.global_mut::<HostRegistry>().handlers.push(handler);
    } else {
        cx.set_global(HostRegistry {
            handlers: vec![handler],
        });
    }
}

fn dispatch_host(cmd: HostCommand, cx: &mut App) -> bool {
    let Some(mut handlers) = cx.try_global::<HostRegistry>().map(|r| r.handlers.clone()) else {
        return false;
    };
    let mut handled = false;
    for i in (0..handlers.len()).rev() {
        let Some(handler) = handlers.get_mut(i) else {
            continue;
        };
        match handler(cmd, cx) {
            Some(true) => {
                handled = true;
                break;
            }
            Some(false) => {}
            None => {
                handlers.remove(i);
            }
        }
    }
    if cx.has_global::<HostRegistry>() {
        cx.global_mut::<HostRegistry>().handlers = handlers;
    }
    handled
}
