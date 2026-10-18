//! MK-076 보안 강화 UI 연결(OSC 8 URL 미리보기, 제목 제어문자 필터, 여러 줄 붙여넣기 경고, Secure Keyboard Entry).
//! `shell.rs` 병합 충돌을 줄이려고 동작·그리기를 이 모듈에 둔다.

use gpui::{AnyElement, App, Context, Pixels, Point, Window, actions, div, prelude::*};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use krill_app::keys;
use krill_app::layout::PaneId;
use krill_app::security::{PastePlan, format_link_preview, plan_paste, text as sec_text};
use krill_app::terminal::link_target;
use krill_proto::input::Now;

use super::Shell;

actions!(security, [ToggleSecureInput]);

pub(super) fn bind_keys(cx: &mut App) {
    crate::set_menus(&krill_app::security::State::default(), cx);
}

impl Shell {
    pub(crate) fn on_toggle_secure_input(
        &mut self,
        _: &ToggleSecureInput,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_secure_keyboard(cx);
    }

    pub(super) fn toggle_secure_keyboard(&mut self, cx: &mut Context<Self>) {
        self.security.toggle_secure_keyboard();
        crate::set_menus(&self.security, cx);
        cx.notify();
    }

    pub(super) fn on_window_active_changed(&mut self, active: bool, cx: &mut Context<Self>) {
        self.security.set_window_active(active);
        krill_app::alert::set_active(active, cx);
        self.sidebar.update(cx, |s, cx| s.set_active(active, cx));
        self.diff.update(cx, |d, cx| d.set_active(active, cx));
    }

    /// 클립보드 항목(텍스트·Finder 복사 파일 경로·스크린샷/복사 이미지)을 터미널 주입 바이트로 변환한다(MK-033·MK-111).
    pub(super) fn paste_clipboard_item(
        &mut self,
        item: &gpui::ClipboardItem,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bytes) = clipboard_paste_payload(item) {
            self.paste_bytes(&bytes, window, cx);
        }
    }

    /// 포커스 칸에 텍스트를 붙여넣는다(MK-033·MK-076).
    /// bracketed paste가 꺼진 상태에서 줄바꿈이 들어 있으면 바로 보내지 않고 경고 배너를 띄운다(MK-076).
    pub(super) fn paste_bytes(&mut self, text: &[u8], window: &Window, cx: &mut Context<Self>) {
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(l) = self.live.get_mut(&pane) else {
            return;
        };
        let modes = l.pane.modes();
        match plan_paste(pane, text, modes) {
            PastePlan::Immediate(bytes) => {
                let mut out = l.ime.commit();
                out.pty = keys::tui::text(&out.pty, modes);
                out.pty.extend(bytes);
                self.send(&out, Some(window), cx);
            }
            PastePlan::Warn(pending) => {
                self.security.set_pending_paste(Some(pending));
                cx.notify();
            }
        }
    }

    pub(super) fn confirm_pending_paste(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(pending) = self.security.take_pending_paste() else {
            return;
        };
        if self.layout.focused() == Some(pending.pane) {
            let Some(l) = self.live.get_mut(&pending.pane) else {
                cx.notify();
                return;
            };
            let modes = l.pane.modes();
            let mut out = l.ime.commit();
            out.pty = keys::tui::text(&out.pty, modes);
            out.pty.extend(pending.payload);
            self.send(&out, Some(window), cx);
        } else if let Some(l) = self.live.get_mut(&pending.pane) {
            let n = l.pane.input(&pending.payload, Now::get());
            self.show(n.as_ref(), cx);
            cx.notify();
        } else {
            cx.notify();
        }
    }

    pub(super) fn cancel_pending_paste(&mut self, cx: &mut Context<Self>) {
        self.security.set_pending_paste(None);
        cx.notify();
    }

    /// 마우스가 가리키는 칸의 OSC 8 링크 미리보기를 갱신한다.
    pub(super) fn update_hover_link(
        &mut self,
        pane: PaneId,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let next = self.live.get(&pane).and_then(|l| {
            let screen = l.pane.shown();
            link_target(&screen, &l.painted, position).map(|u| (pane, u.to_owned()))
        });
        if self.security.set_hover_link(next) {
            cx.notify();
        }
    }

    /// 칸 안의 OSC 8 URL 미리보기 툴팁 바(UI-1 디자인 토큰 적용).
    pub(super) fn render_pane_security(&self, id: PaneId, cx: &App) -> Option<AnyElement> {
        let uri = self.security.hover_uri(id)?;
        let p = krill_app::ui::palette(cx);
        Some(
            div()
                .debug_selector(|| "osc8-preview".into())
                .flex_none()
                .px_2()
                .py_0p5()
                .rounded(krill_app::ui::RADIUS_XS)
                .bg(p.popover)
                .text_color(p.popover_foreground)
                .border_1()
                .border_color(p.border)
                .shadow_lg()
                .font_family(krill_app::ui::FONT_MONO)
                .text_size(krill_app::ui::TEXT_XS)
                .child(format_link_preview(uri))
                .into_any_element(),
        )
    }

    /// 여러 줄 붙여넣기 경고 배너(붙여넣기 확인 / 취소, UI-1 디자인 토큰 적용).
    pub(super) fn render_security_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.security.pending_paste()?;
        let p = krill_app::ui::palette(cx);
        Some(
            div()
                .id("multiline-paste-warning")
                .debug_selector(|| "multiline-paste-warning".into())
                .px_3()
                .py_1p5()
                .bg(p.warning)
                .text_color(p.warning_foreground)
                .text_size(krill_app::ui::TEXT_XS)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().flex_1().child(pending.warning_text()))
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            Button::new("paste-confirm")
                                .label(sec_text::PASTE_CONFIRM.get())
                                .debug_selector(|| "paste-confirm".into())
                                .primary()
                                .xsmall()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_pending_paste(window, cx);
                                })),
                        )
                        .child(
                            Button::new("paste-cancel")
                                .label(sec_text::PASTE_CANCEL.get())
                                .debug_selector(|| "paste-cancel".into())
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_pending_paste(cx);
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 사이드바 하단 푸터(설정·보안/도움말 + 활성 워크트리 이동·보드 대시보드).
    pub(super) fn render_security_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::{Icon, IconName, Sizable as _};
        use krill_app::ui::icons;
        let p = krill_app::ui::palette(cx);
        let on = self.security.is_secure_keyboard_enabled();
        // OWE-886: 상태(켜기/끄기)는 접근성 라벨에 유지하고, 툴팁은 공용 헬퍼로 통일한다.
        let sec_label = if on {
            sec_text::SECURE_KEYBOARD_ON.get()
        } else {
            sec_text::SECURE_KEYBOARD_OFF.get()
        };
        let btn = krill_app::ui::button_with_tooltip(
            Button::new("secure-keyboard-entry")
                .icon(Icon::default().data(icons::SHIELD_SVG))
                .xsmall()
                .debug_selector(|| "secure-keyboard-entry".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_secure_keyboard(cx))),
            "secure-keyboard-entry",
            sec_text::SECURE_KEYBOARD_TOOLTIP.get(),
        )
        .accessibility_label(sec_label);
        div()
            .h(krill_app::ui::SIDEBAR_HEADER_HEIGHT)
            .px_2()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(p.sidebar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .child(krill_app::ui::button_with_tooltip(
                        Button::new("sidebar-settings")
                            .icon(IconName::Settings)
                            .ghost()
                            .xsmall()
                            .debug_selector(|| "sidebar-settings".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings(
                                    krill_app::settings::AdminTab::General,
                                    Some(window),
                                    cx,
                                );
                            })),
                        "sidebar-settings",
                        krill_app::i18n::catalog::general::SETTINGS_HINT.get(),
                    ))
                    .child(if on { btn.primary() } else { btn.ghost() }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .child(krill_app::ui::button_with_tooltip(
                        Button::new("sidebar-reveal-active")
                            .icon(Icon::default().data(icons::CROSSHAIR_SVG))
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar
                                    .update(cx, krill_app::sidebar::Sidebar::reveal_focused);
                            })),
                        "sidebar-reveal-active",
                        krill_app::i18n::catalog::sidebar::REVEAL_ACTIVE.get(),
                    ))
                    .child(krill_app::ui::button_with_tooltip(
                        Button::new("sidebar-board-view")
                            .icon(Icon::default().data(icons::KANBAN_SVG))
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| this.dashboard(cx))),
                        "sidebar-board-view",
                        krill_app::i18n::catalog::sidebar::NAV_AGENT_DASHBOARD.get(),
                    )),
            )
    }
}

/// 클립보드 이미지 최대 허용 크기(64 MiB). 비정상 대용량 버퍼로 인한 UI 지연을 막는다.
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// 클립보드 항목에서 붙여넣을 바이트열을 추출한다(MK-111).
/// - Finder 등에서 복사한 파일(`ExternalPaths`)이 있으면(함께 들어오는 단순 파일명 문자열보다 우선)
///   셸 이스케이프된 경로 목록을 반환한다.
/// - 비어 있지 않은 문자열 항목이 있으면 텍스트를 그대로 반환한다.
/// - 이미지 항목(`ClipboardEntry::Image`)이 있으면 `$TMPDIR/minikrill-paste/` 아래에 `0600` 파일로 저장하고
///   셸 이스케이프된 파일 경로를 반환한다.
pub(crate) fn clipboard_paste_payload(item: &gpui::ClipboardItem) -> Option<Vec<u8>> {
    use krill_proto::os::ffi::OsStrExt as _;

    for entry in item.entries() {
        if let gpui::ClipboardEntry::ExternalPaths(paths) = entry
            && !paths.paths().is_empty()
        {
            return Some(krill_term::paste::dropped(
                paths.paths().iter().map(|p| p.as_os_str().as_bytes()),
            ));
        }
    }

    let has_non_empty_string = item
        .entries()
        .iter()
        .any(|entry| matches!(entry, gpui::ClipboardEntry::String(s) if !s.text().is_empty()));
    if has_non_empty_string
        && let Some(text) = item.text()
        && !text.is_empty()
    {
        return Some(text.into_bytes());
    }

    for entry in item.entries() {
        if let gpui::ClipboardEntry::Image(img) = entry
            && !img.bytes.is_empty()
            && img.bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES
            && let Ok(path) = save_clipboard_image(img)
        {
            return Some(krill_term::paste::dropped([path.as_os_str().as_bytes()]));
        }
    }

    None
}

pub(crate) fn save_clipboard_image(img: &gpui::Image) -> std::io::Result<std::path::PathBuf> {
    use std::io::{self, Write as _};

    if img.bytes.is_empty() || img.bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clipboard image size out of bounds",
        ));
    }
    let dir = std::env::temp_dir().join("minikrill-paste");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok();
    }
    let ext = img.format.extension();
    let filename = format!("clipboard-{:016x}.{ext}", img.id);
    let path = dir.join(filename);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use krill_proto::os::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&img.bytes)?;
    file.sync_all()?;
    Ok(path)
}
