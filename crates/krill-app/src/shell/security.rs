//! MK-076 보안 강화 UI 연결(링크 팝오버, 제목 제어문자 필터, 여러 줄 붙여넣기 경고, Secure Keyboard Entry).
//! `shell.rs` 병합 충돌을 줄이려고 동작·그리기를 이 모듈에 둔다.

use std::time::Duration;

use gpui::{AnyElement, App, Context, Pixels, Point, Window, actions, div, prelude::*, px};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use krill_app::keys;
use krill_app::layout::PaneId;
use krill_app::security::{LinkMenu, PastePlan, plan_paste, text as sec_text};
use krill_proto::input::Now;

use super::Shell;

actions!(security, [ToggleSecureInput]);

pub(super) fn bind_keys(cx: &mut App) {
    crate::set_menus(&krill_app::security::State::default(), cx);
}

/// 링크 팝오버 최대 폭. 긴 URL은 한 줄로 줄여 보이고 복사·열기는 전체 URL을 쓴다.
const LINK_MENU_WIDTH: Pixels = px(420.);

/// Bound clipboard-owner stalls, image conversion and filesystem preparation together.
const PASTE_PREPARATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Remote clipboard uploads (TIFF->PNG conversion + SSH / tunnel transfer) need a longer budget.
const REMOTE_PASTE_PREPARATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Clone)]
pub(crate) struct RemoteUploadTarget {
    pub ssh_host: Option<krill_app::ssh_hosts::SshHost>,
    pub host_target: Option<krill_app::hosts::HostTarget>,
    pub prefer_target_ipc: bool,
}

#[derive(Default)]
pub(super) struct PasteQueue {
    next: u64,
    entries: std::collections::VecDeque<QueuedInput>,
}
impl PasteQueue {
    pub(super) fn has_pending_for(&self, pane: PaneId) -> bool {
        self.entries.iter().any(|entry| entry.pane == pane)
    }
}

struct QueuedInput {
    pane: PaneId,
    session: krill_proto::SessionId,
    kind: QueuedKind,
}
enum PreparedPaste {
    Pending,
    Ready(Option<Vec<u8>>),
}
enum QueuedKind {
    Paste(u64, PreparedPaste),
    Bytes(Vec<u8>),
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
        if active {
            // 같은 데몬에 붙은 다른 앱이 색을 바꿨을 수 있다. 보이는 창의 팔레트로 다시 맞춘다.
            self.push_appearance(cx);
        }
        if !active {
            self.layout.cancel_mru();
            if let Some(pane) = self.layout.focused() {
                self.cancel_pending_paste_for(pane, cx);
            }
        }
        if let Some(settings) = &self.settings {
            let visible = active && self.dashboard.is_none() && self.artifacts.is_none();
            settings.update(cx, |view, cx| view.set_visible(visible, cx));
        }
        let resources_visible = active && !self.page_open() && self.dashboard.is_none();
        self.resources
            .update(cx, |view, cx| view.set_visible(resources_visible, cx));
        self.security.set_window_active(active);
        // 비활성화 뒤 다음 렌더를 기다리지 않고 커서 타이머를 끊는다.
        self.sync_cursor_blink(active, cx);
        krill_app::alert::set_active(active, cx);
        self.sidebar.update(cx, |s, cx| s.set_active(active, cx));
        self.diff.update(cx, |d, cx| d.set_active(active, cx));
        cx.notify();
    }

    fn begin_clipboard_paste(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<u64> {
        let pane = self.layout.focused()?;
        let session = self.live.get(&pane)?.pane.session()?;
        let timeout = if self.layout.host(pane).is_remote() {
            REMOTE_PASTE_PREPARATION_TIMEOUT
        } else {
            PASTE_PREPARATION_TIMEOUT
        };
        // Existing composition precedes the paste; later keystrokes enter slots
        // behind it, so a slow image or clipboard owner cannot reorder input.
        self.commit_ime(pane, Some(window), cx);
        self.paste_queue.next = self.paste_queue.next.wrapping_add(1);
        let id = self.paste_queue.next;
        self.paste_queue.entries.push_back(QueuedInput {
            pane,
            session,
            kind: QueuedKind::Paste(id, PreparedPaste::Pending),
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(timeout).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.complete_clipboard_paste(id, None, window, cx);
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
        Some(id)
    }

    pub(super) fn ssh_host_for_remote_id(
        &self,
        id: &str,
        cx: &App,
    ) -> Option<krill_app::ssh_hosts::SshHost> {
        if let Some(h) = self.ssh_hosts.iter().find(|h| h.id == id) {
            return Some(h.clone());
        }
        if let Some(canon) = self.host_aliases.get(id)
            && let Some(h) = self.ssh_hosts.iter().find(|h| h.id == *canon)
        {
            return Some(h.clone());
        }
        if let Some(h) = self
            .ssh_hosts
            .iter()
            .find(|h| self.host_aliases.get(&h.id).is_some_and(|c| c == id))
        {
            return Some(h.clone());
        }
        if let Some(h) = self.ssh_hosts.iter().find(|h| h.destination == id) {
            return Some(h.clone());
        }
        let remote_label = self
            .sidebar
            .read(cx)
            .remote_hosts()
            .iter()
            .find(|h| h.id == id)
            .map(|h| h.label.clone())
            .or_else(|| {
                self.host_statuses
                    .iter()
                    .find(|(s, _)| s.id == id)
                    .map(|(s, _)| s.display_name().to_owned())
            });
        if let Some(label) = remote_label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && let Some(h) = self.ssh_hosts.iter().find(|h| {
                h.label.trim().eq_ignore_ascii_case(label)
                    || h.destination.trim().eq_ignore_ascii_case(label)
            })
        {
            return Some(h.clone());
        }
        if !id.starts_with("iroh:")
            && let Some((spec, _)) = self
                .host_statuses
                .iter()
                .find(|(s, _)| s.id == id && !s.address.trim().is_empty())
        {
            return Some(krill_app::ssh_hosts::SshHost {
                id: spec.id.clone(),
                label: spec.label.clone(),
                destination: spec.address.clone(),
                port: spec.port,
                identity_file: spec.identity_file.clone(),
                remote_socket: spec.remote_socket.clone(),
                imported: false,
                copy_id_first: false,
            });
        }
        None
    }

    pub(super) fn remote_upload_target_for_pane(
        &self,
        pane: PaneId,
        cx: &App,
    ) -> Option<RemoteUploadTarget> {
        match self.layout.host(pane) {
            krill_app::hosts::HostId::Remote(id) => {
                let ssh_host = self.ssh_host_for_remote_id(id, cx);
                let host_target = self.target_for_pane(pane);
                if ssh_host.is_none() && host_target.is_none() {
                    return None;
                }
                Some(RemoteUploadTarget {
                    ssh_host,
                    host_target,
                    prefer_target_ipc: id.starts_with("iroh:"),
                })
            }
            krill_app::hosts::HostId::Local => None,
        }
    }

    pub(super) fn paste_clipboard(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(id) = self.begin_clipboard_paste(window, cx) else {
            return;
        };
        // 원격 칸이면 로컬 경로를 그대로 보내지 않고 SSH 또는 원격 제어 소켓으로 파일을 올려
        // 원격 경로를 붙여넣는다. 앱(맥북)이 가진 클립보드를 원격 데몬(맥미니) PTY가 읽을 수 있게 한다.
        let remote_pane = self
            .layout
            .focused()
            .is_some_and(|pane| self.layout.host(pane).is_remote());
        let remote_target = self
            .layout
            .focused()
            .and_then(|pane| self.remote_upload_target_for_pane(pane, cx));
        let read = cx.read_from_clipboard_async();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let item = read.await.ok().flatten();
            #[cfg(not(test))]
            let item = augment_clipboard_item_from_pasteboard(item);
            let payload = cx
                .background_executor()
                .spawn(async move {
                    item.as_ref().and_then(|item| {
                        if remote_pane && remote_target.is_none() {
                            remote_text_without_upload(item)
                        } else {
                            clipboard_paste_payload_for_target(item, remote_target.as_ref())
                        }
                    })
                })
                .await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.complete_clipboard_paste(id, payload, window, cx);
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    pub(super) fn paste_dropped_paths(
        &mut self,
        paths: &gpui::ExternalPaths,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        use krill_proto::os::ffi::OsStrExt as _;
        let remote_pane = self
            .layout
            .focused()
            .is_some_and(|pane| self.layout.host(pane).is_remote());
        if !remote_pane {
            let text =
                krill_term::paste::dropped(paths.paths().iter().map(|p| p.as_os_str().as_bytes()));
            self.paste_bytes(&text, window, cx);
            return;
        }
        let Some(id) = self.begin_clipboard_paste(window, cx) else {
            return;
        };
        let remote_target = self
            .layout
            .focused()
            .and_then(|pane| self.remote_upload_target_for_pane(pane, cx));
        let item = gpui::ClipboardItem {
            entries: vec![gpui::ClipboardEntry::ExternalPaths(paths.clone())],
        };
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let payload = cx
                .background_executor()
                .spawn(async move {
                    if remote_target.is_none() {
                        remote_text_without_upload(&item)
                    } else {
                        clipboard_paste_payload_for_target(&item, remote_target.as_ref())
                    }
                })
                .await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.complete_clipboard_paste(id, payload, window, cx);
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    #[cfg(all(test, unix))]
    pub(super) fn paste_clipboard_item(
        &mut self,
        item: &gpui::ClipboardItem,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.begin_clipboard_paste(window, cx) else {
            return;
        };
        let remote_pane = self
            .layout
            .focused()
            .is_some_and(|pane| self.layout.host(pane).is_remote());
        let remote_target = self
            .layout
            .focused()
            .and_then(|pane| self.remote_upload_target_for_pane(pane, cx));
        let item = item.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let payload = cx
                .background_executor()
                .spawn(async move {
                    if remote_pane && remote_target.is_none() {
                        remote_text_without_upload(&item)
                    } else {
                        clipboard_paste_payload_for_target(&item, remote_target.as_ref())
                    }
                })
                .await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.complete_clipboard_paste(id, payload, window, cx);
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    pub(super) fn queue_input_behind_paste(&mut self, pane: PaneId, bytes: &[u8]) -> bool {
        if !self
            .paste_queue
            .entries
            .iter()
            .any(|entry| entry.pane == pane)
        {
            return false;
        }
        let Some(session) = self.live.get(&pane).and_then(|l| l.pane.session()) else {
            return false;
        };
        self.paste_queue.entries.push_back(QueuedInput {
            pane,
            session,
            kind: QueuedKind::Bytes(bytes.to_vec()),
        });
        true
    }

    pub(super) fn cancel_pending_paste_for(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.paste_queue.entries);
        for entry in pending {
            if entry.pane != pane {
                self.paste_queue.entries.push_back(entry);
                continue;
            }
            // A focus change cancels clipboard navigation intent. Keystrokes
            // already submitted to the still-existing session keep their order.
            if self.layout.pane_exists(pane)
                && let QueuedKind::Bytes(bytes) = entry.kind
                && let Some(live) = self
                    .live
                    .get_mut(&pane)
                    .filter(|live| live.pane.session() == Some(entry.session))
            {
                let notice = live.pane.input(&bytes, Now::get());
                self.show(notice.as_ref(), cx);
            }
        }
    }

    pub(super) fn cancel_closed_pastes(&mut self, cx: &mut Context<Self>) {
        let closed = self
            .paste_queue
            .entries
            .iter()
            .filter(|entry| !self.layout.pane_exists(entry.pane))
            .map(|entry| entry.pane)
            .collect::<std::collections::BTreeSet<_>>();
        for pane in closed {
            self.cancel_pending_paste_for(pane, cx);
        }
    }

    fn complete_clipboard_paste(
        &mut self,
        id: u64,
        payload: Option<Vec<u8>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mut found = false;
        for entry in &mut self.paste_queue.entries {
            if let QueuedKind::Paste(request, ready) = &mut entry.kind
                && *request == id
            {
                *ready = PreparedPaste::Ready(payload);
                found = true;
                break;
            }
        }
        // Expired/cancelled requests cannot reinsert payloads into the stream.
        if !found {
            return;
        }
        let pending = std::mem::take(&mut self.paste_queue.entries);
        let mut blocked = std::collections::BTreeSet::new();
        for entry in pending {
            if !self.layout.pane_exists(entry.pane)
                || self
                    .live
                    .get(&entry.pane)
                    .is_none_or(|live| live.pane.session() != Some(entry.session))
            {
                continue;
            }
            if blocked.contains(&entry.pane)
                || matches!(entry.kind, QueuedKind::Paste(_, PreparedPaste::Pending))
            {
                blocked.insert(entry.pane);
                self.paste_queue.entries.push_back(entry);
                continue;
            }
            if self.page_open()
                || self.dashboard.is_some()
                || self.layout.focused() != Some(entry.pane)
            {
                continue;
            }
            let Some(live) = self
                .live
                .get_mut(&entry.pane)
                .filter(|live| live.pane.session() == Some(entry.session))
            else {
                continue;
            };
            let bytes = match entry.kind {
                QueuedKind::Bytes(bytes) => bytes,
                QueuedKind::Paste(_, PreparedPaste::Ready(Some(bytes))) => {
                    match plan_paste(entry.pane, &bytes, live.pane.modes()) {
                        PastePlan::Immediate(bytes) => bytes,
                        PastePlan::Warn(pending) => {
                            self.security.set_pending_paste(Some(pending));
                            cx.notify();
                            continue;
                        }
                    }
                }
                QueuedKind::Paste(_, PreparedPaste::Pending | PreparedPaste::Ready(None)) => {
                    continue;
                }
            };
            live.clear_selection();
            live.echo.key_sent(std::time::Instant::now());
            let notice = live.pane.input(&bytes, Now::get());
            self.show(notice.as_ref(), cx);
            cx.notify();
        }
        let _ = window;
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

    /// 맨 클릭한 링크의 팝오버를 클릭한 자리에 연다.
    pub(super) fn open_link_menu(
        &mut self,
        pane: PaneId,
        uri: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.security.set_link_menu(Some(LinkMenu {
            pane,
            uri,
            position,
        })) {
            cx.notify();
        }
    }

    /// 링크 팝오버를 닫는다. 열려 있었으면 `true`.
    pub(super) fn close_link_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.security.set_link_menu(None);
        if closed {
            cx.notify();
        }
        closed
    }

    /// 팝오버의 대상 URL을 클립보드로 복사하고 닫는다.
    pub(super) fn copy_link_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.security.link_menu() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(menu.uri.clone()));
        }
        self.close_link_menu(cx);
    }

    /// 팝오버의 대상 URL을 브라우저로 열고 닫는다.
    pub(super) fn open_link_menu_in_browser(&mut self, cx: &mut Context<Self>) {
        if let Some((pane, uri)) = self
            .security
            .link_menu()
            .map(|menu| (menu.pane, menu.uri.clone()))
        {
            self.open_terminal_url(pane, &uri, cx);
        }
        self.close_link_menu(cx);
    }

    /// 링크 팝오버(Orca식): 실제 대상 URL과 복사·브라우저 열기. 바깥 클릭·Esc로 닫는다.
    /// 클릭한 줄 바로 아래에 띄우고, 아래 공간이 모자라면 위로 올린다.
    pub(super) fn render_link_menu(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let menu = self.security.link_menu()?;
        let p = krill_app::ui::palette(cx);
        let viewport = window.viewport_size();
        let gap = px(14.);
        let height = px(76.);
        let width = LINK_MENU_WIDTH.min(viewport.width - px(16.)).max(px(160.));
        let x = menu
            .position
            .x
            .min((viewport.width - width - px(8.)).max(px(8.)))
            .max(px(8.));
        let below = menu.position.y + gap;
        let y = if below + height + px(8.) <= viewport.height {
            below
        } else {
            (menu.position.y - gap - height).max(px(8.))
        };
        let card = div()
            .id("terminal-link-menu")
            .debug_selector(|| "terminal-link-menu".into())
            .occlude()
            .absolute()
            .left(x)
            .top(y)
            .w(width)
            .p_2()
            .rounded(krill_app::ui::RADIUS_MD)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .font_family(krill_app::ui::FONT_UI)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .debug_selector(|| "terminal-link-menu-url".into())
                    .w_full()
                    .min_w_0()
                    .truncate()
                    .font_family(krill_app::ui::FONT_MONO)
                    .text_size(krill_app::ui::TEXT_SM)
                    .child(menu.display_uri()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        krill_app::ui::secondary("terminal-link-copy", sec_text::LINK_COPY.get())
                            .xsmall()
                            .debug_selector(|| "terminal-link-copy".into())
                            .on_click(cx.listener(|this, _, _, cx| this.copy_link_menu(cx))),
                    )
                    .child(
                        krill_app::ui::primary("terminal-link-open", sec_text::LINK_OPEN.get())
                            .xsmall()
                            .debug_selector(|| "terminal-link-open".into())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.open_link_menu_in_browser(cx)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .text_size(krill_app::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(sec_text::LINK_HINT.get()),
                    ),
            );
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(px(0.), px(0.)))
                    .child(
                        div()
                            .id("terminal-link-menu-backdrop")
                            .occlude()
                            .w(viewport.width)
                            .h(viewport.height)
                            .relative()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.close_link_menu(cx);
                                }),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(|this, _, _, cx| {
                                    this.close_link_menu(cx);
                                }),
                            )
                            .child(card),
                    ),
            )
            .with_priority(2),
        )
    }

    /// Persistent safety confirmation inside the bottom-right toast stack.
    pub(super) fn render_security_notice(
        pending: &krill_app::security::PendingPaste,
        shell: gpui::WeakEntity<Self>,
        cx: &App,
    ) -> AnyElement {
        let confirm_shell = shell.clone();
        let confirm = pending.clone();
        let cancel = pending.clone();
        let p = krill_app::ui::palette(cx);
        div()
            .id("multiline-paste-warning")
            .debug_selector(|| "multiline-paste-warning".into())
            .w_full()
            .min_w_0()
            .px_3()
            .py_1p5()
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .text_size(krill_app::ui::TEXT_XS)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .child(pending.warning_text()),
            )
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
                            .on_click(move |_, window, cx| {
                                confirm_shell
                                    .update(cx, |this, cx| {
                                        if this.security.pending_paste() == Some(&confirm) {
                                            this.confirm_pending_paste(window, cx);
                                        }
                                    })
                                    .ok();
                            }),
                    )
                    .child(
                        Button::new("paste-cancel")
                            .label(sec_text::PASTE_CANCEL.get())
                            .debug_selector(|| "paste-cancel".into())
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                shell
                                    .update(cx, |this, cx| {
                                        if this.security.pending_paste() == Some(&cancel) {
                                            this.cancel_pending_paste(cx);
                                        }
                                    })
                                    .ok();
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 사이드바 하단 푸터(설정 + 활성 워크트리 이동·보드 대시보드). Secure Keyboard Entry는 앱 메뉴 토글만 둔다.
    pub(super) fn render_sidebar_footer(cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::{Icon, IconName, Sizable as _};
        use krill_app::ui::icons;
        let p = krill_app::ui::palette(cx);
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
                    )),
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

const MAX_CLIPBOARD_PREPARATIONS: usize = 2;
static CLIPBOARD_PREPARATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

struct PreparationPermit<'a>(&'a std::sync::atomic::AtomicUsize);
impl<'a> PreparationPermit<'a> {
    fn acquire(count: &'a std::sync::atomic::AtomicUsize) -> Option<Self> {
        use std::sync::atomic::Ordering;
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CLIPBOARD_PREPARATIONS).then_some(active + 1)
            })
            .ok()?;
        Some(Self(count))
    }
}
impl Drop for PreparationPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// 클립보드 이미지 최대 허용 크기(64 MiB). 비정상 대용량 버퍼로 인한 UI 지연을 막는다.
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// 클립보드 이미지 바이트의 매직 넘버를 먼저 검사해 실제 포맷 확장자를 판정한다(#27345, #26739).
/// `CleanShot` X 등이 `public.png` 타입으로 JPEG 바이트를 올릴 때 잘못된 `.png` 확장자로 저장되어
/// Claude Code API가 MIME 불일치로 거부하는 문제를 막는다.
#[must_use]
pub(crate) fn sniff_clipboard_image_extension(
    bytes: &[u8],
    declared: gpui::ImageFormat,
) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "jpg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        "tiff"
    } else {
        declared.extension()
    }
}

/// 클립보드 항목에서 붙여넣을 바이트열을 추출한다(MK-111, OWE-927).
/// - Finder 등에서 복사한 파일(`ExternalPaths`)이 있으면(함께 들어오는 단순 파일명 문자열보다 우선)
///   셸 이스케이프된 경로 목록을 반환한다. 단, 이미지 항목이 함께 있는데 `ExternalPaths`가 가리키는
///   파일이 디스크에 존재하지 않는 죽은 스크린샷 임시 경로면(#28245) 경로와 파일명 문자열을 건너뛰고
///   실제 이미지 바이트(`ClipboardEntry::Image`)를 저장해 붙여넣는다.
/// - 비어 있지 않은 문자열 항목이 있으면 텍스트를 그대로 반환한다(이미지가 함께 있을 때는 공백만 있는 문자열은 건너뛴다).
/// - 이미지 항목(`ClipboardEntry::Image`)이 있으면 `$TMPDIR/krill-paste/` 아래에 `0600` 파일로 저장하고
///   셸 이스케이프된 파일 경로를 반환한다.
pub(crate) fn clipboard_paste_payload(item: &gpui::ClipboardItem) -> Option<Vec<u8>> {
    use krill_proto::os::ffi::OsStrExt as _;
    // Admission is held until native conversion/filesystem work really returns,
    // independently of a UI deadline or the originating window's lifetime.
    let _permit = PreparationPermit::acquire(&CLIPBOARD_PREPARATIONS)?;

    let has_valid_image = item.entries().iter().any(|entry| {
        matches!(
            entry,
            gpui::ClipboardEntry::Image(img)
                if !img.bytes.is_empty() && img.bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES
        )
    });
    let mut skipped_dead_external_paths = false;

    for entry in item.entries() {
        if let gpui::ClipboardEntry::ExternalPaths(paths) = entry
            && !paths.paths().is_empty()
        {
            if has_valid_image {
                let existing: Vec<&std::path::PathBuf> =
                    paths.paths().iter().filter(|p| p.exists()).collect();
                if !existing.is_empty() {
                    return Some(krill_term::paste::dropped(
                        existing.into_iter().map(|p| p.as_os_str().as_bytes()),
                    ));
                }
                skipped_dead_external_paths = true;
            } else {
                return Some(krill_term::paste::dropped(
                    paths.paths().iter().map(|p| p.as_os_str().as_bytes()),
                ));
            }
        }
    }

    if !skipped_dead_external_paths {
        let has_usable_string = item.entries().iter().any(|entry| match entry {
            gpui::ClipboardEntry::String(s) => {
                if has_valid_image {
                    !s.text().trim().is_empty()
                } else {
                    !s.text().is_empty()
                }
            }
            _ => false,
        });
        if has_usable_string
            && let Some(text) = item.text()
            && !text.is_empty()
        {
            return Some(text.into_bytes());
        }
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

#[cfg(not(test))]
fn augment_clipboard_item_from_pasteboard(
    item: Option<gpui::ClipboardItem>,
) -> Option<gpui::ClipboardItem> {
    if let Some(ref existing) = item {
        let has_image = existing
            .entries()
            .iter()
            .any(|e| matches!(e, gpui::ClipboardEntry::Image(img) if !img.bytes.is_empty()));
        if has_image {
            return item;
        }
        let has_dead_paths = existing.entries().iter().any(|e| {
            matches!(
                e,
                gpui::ClipboardEntry::ExternalPaths(p)
                    if !p.paths().is_empty() && p.paths().iter().all(|path| !path.exists())
            )
        });
        let has_only_blank_text = existing.text().is_some_and(|t| t.trim().is_empty());
        let prefer_over_text = has_dead_paths || has_only_blank_text;
        if let Some((fmt, bytes)) = krill_app::platform::read_pasteboard_image(prefer_over_text) {
            let img = gpui::Image::from_bytes(fmt, bytes);
            if has_dead_paths {
                let mut entries = existing.entries().to_vec();
                entries.push(gpui::ClipboardEntry::Image(img));
                return Some(gpui::ClipboardItem { entries });
            }
            return Some(gpui::ClipboardItem::new_image(&img));
        }
        return item;
    }
    let (fmt, bytes) = krill_app::platform::read_pasteboard_image(true)?;
    let img = gpui::Image::from_bytes(fmt, bytes);
    Some(gpui::ClipboardItem::new_image(&img))
}

// A disconnected remote without an upload target cannot read files saved on this computer.
fn remote_text_without_upload(item: &gpui::ClipboardItem) -> Option<Vec<u8>> {
    if item.entries().iter().any(|entry| {
        matches!(
            entry,
            gpui::ClipboardEntry::ExternalPaths(_) | gpui::ClipboardEntry::Image(_)
        )
    }) {
        return None;
    }
    item.text()
        .filter(|text| !text.is_empty())
        .map(String::into_bytes)
}

#[cfg(all(test, unix))]
pub(crate) fn clipboard_paste_payload_for_host(
    item: &gpui::ClipboardItem,
    remote: Option<&krill_app::ssh_hosts::SshHost>,
) -> Option<Vec<u8>> {
    let target = remote.cloned().map(|h| RemoteUploadTarget {
        ssh_host: Some(h),
        host_target: None,
        prefer_target_ipc: false,
    });
    clipboard_paste_payload_for_target(item, target.as_ref())
}

pub(crate) fn clipboard_paste_payload_for_target(
    item: &gpui::ClipboardItem,
    remote: Option<&RemoteUploadTarget>,
) -> Option<Vec<u8>> {
    let Some(target) = remote else {
        return clipboard_paste_payload(item);
    };
    let has_valid_image = item.entries().iter().any(|entry| {
        matches!(entry, gpui::ClipboardEntry::Image(img) if !img.bytes.is_empty() && img.bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES)
    });
    let _permit = PreparationPermit::acquire(&CLIPBOARD_PREPARATIONS)?;
    remote_payload_inner(item, target, has_valid_image)
}

fn remote_payload_inner(
    item: &gpui::ClipboardItem,
    target: &RemoteUploadTarget,
    has_valid_image: bool,
) -> Option<Vec<u8>> {
    let mut skipped_dead = false;
    for entry in item.entries() {
        if let gpui::ClipboardEntry::ExternalPaths(paths) = entry {
            if paths.paths().is_empty() {
                continue;
            }
            let existing: Vec<&std::path::PathBuf> =
                paths.paths().iter().filter(|p| p.exists()).collect();
            if existing.is_empty() {
                if has_valid_image {
                    skipped_dead = true;
                    continue;
                }
                return None;
            }
            return upload_local_paths(target, &existing);
        }
    }
    if !skipped_dead {
        let has_string = item.entries().iter().any(|entry| match entry {
            gpui::ClipboardEntry::String(s) => {
                if has_valid_image {
                    !s.text().trim().is_empty()
                } else {
                    !s.text().is_empty()
                }
            }
            _ => false,
        });
        if has_string
            && let Some(text) = item.text()
            && !text.is_empty()
        {
            return Some(text.into_bytes());
        }
    }
    for entry in item.entries() {
        if let gpui::ClipboardEntry::Image(img) = entry {
            if img.bytes.is_empty() || img.bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
                continue;
            }
            if let Some(payload) = upload_clipboard_image(target, img) {
                return Some(payload);
            }
        }
    }
    None
}

fn sanitize_remote_filename(name: &str) -> String {
    let trimmed = name.trim();
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let mut out = String::new();
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+') {
            out.push(ch);
        } else if ch == ' ' {
            out.push('_');
        }
    }
    let out = out.trim_matches('.').trim();
    if out.is_empty() {
        return "pasted-file".to_owned();
    }
    if out.len() > 64 {
        out.chars().take(64).collect()
    } else {
        out.to_owned()
    }
}

fn upload_bytes_to_remote_target(
    target: &RemoteUploadTarget,
    filename: &str,
    bytes: &[u8],
) -> Option<String> {
    if target.prefer_target_ipc {
        if let Some(ht) = &target.host_target
            && let Some(path) =
                upload_bytes_via_target(ht, filename, bytes, Duration::from_secs(15))
        {
            return Some(path);
        }
        if let Some(host) = &target.ssh_host {
            return upload_bytes_to_remote(host, filename, bytes);
        }
        None
    } else {
        if let Some(host) = &target.ssh_host
            && let Some(path) = upload_bytes_to_remote(host, filename, bytes)
        {
            return Some(path);
        }
        if let Some(ht) = &target.host_target {
            return upload_bytes_via_target(ht, filename, bytes, Duration::from_secs(15));
        }
        None
    }
}

fn upload_bytes_to_remote(
    host: &krill_app::ssh_hosts::SshHost,
    filename: &str,
    bytes: &[u8],
) -> Option<String> {
    upload_bytes_with_program(
        host,
        filename,
        bytes,
        "ssh".as_ref(),
        Duration::from_secs(15),
    )
}

fn base64_encode(input: &[u8]) -> Vec<u8> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len().div_ceil(3).saturating_mul(4));
    for chunk in input.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let i0 = ((n >> 18) & 0x3f) as usize;
        let i1 = ((n >> 12) & 0x3f) as usize;
        let i2 = ((n >> 6) & 0x3f) as usize;
        let i3 = (n & 0x3f) as usize;
        out.push(TABLE.get(i0).copied().unwrap_or(b'A'));
        out.push(TABLE.get(i1).copied().unwrap_or(b'A'));
        out.push(if chunk.len() > 1 {
            TABLE.get(i2).copied().unwrap_or(b'A')
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            TABLE.get(i3).copied().unwrap_or(b'A')
        } else {
            b'='
        });
    }
    out
}

fn run_custom_and_wait_marker(
    stream: &mut krill_proto::os::net::UnixStream,
    args: Vec<Vec<u8>>,
    marker_prefix: &str,
    marker_suffix: &str,
    error_marker: &str,
    deadline: std::time::Instant,
) -> Option<String> {
    use krill_proto::control::{CaptureFormat, Create, Program, Reply, Request};
    use krill_proto::ipc;
    use krill_proto::screen::Msg as _;
    use std::io::Write as _;

    let create = Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args,
        },
        cwd: b"/tmp".to_vec(),
        cols: 1024,
        rows: 24,
    });
    stream.write_all(&create.encode().ok()?).ok()?;
    let (kind, body) = ipc::read_frame(stream).ok()??;
    let Reply::Created { session, .. } = Reply::decode(kind, &body).ok()? else {
        return None;
    };

    let mut result = None;
    while std::time::Instant::now() < deadline {
        let cap_req = Request::Capture {
            session,
            format: CaptureFormat::Plain,
        };
        if stream
            .write_all(&cap_req.encode().unwrap_or_default())
            .is_err()
        {
            break;
        }
        let Ok(Some((k, b))) = ipc::read_frame(stream) else {
            break;
        };
        match Reply::decode(k, &b) {
            Ok(Reply::Captured { bytes: cap, .. }) => {
                let text = String::from_utf8_lossy(&cap);
                let flat: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                if flat.contains(error_marker) {
                    break;
                }
                if let Some((_, after)) = flat.split_once(marker_prefix)
                    && let Some((inner, _)) = after.split_once(marker_suffix)
                {
                    result = Some(inner.trim().to_owned());
                    break;
                }
            }
            Ok(Reply::Failed(_)) | Err(_) => break,
            Ok(_) => {}
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    if let Ok(close_bytes) = (Request::Close { session }).encode()
        && stream.write_all(&close_bytes).is_ok()
    {
        ipc::read_frame(stream).ok();
    }
    result
}

pub(crate) fn upload_bytes_via_target(
    target: &krill_app::hosts::HostTarget,
    filename: &str,
    bytes: &[u8],
    timeout: Duration,
) -> Option<String> {
    use krill_proto::control::{FEAT_CLI, FEAT_CONTROL};
    use krill_proto::ipc::{self, ClientRole};
    use std::io::Write as _;

    // 96 KiB is a multiple of 3 (so each Base64 chunk is 128 KiB with no mid-stream '=' padding)
    // and stays well below ARG_MAX (1 MiB) and ipc::MAX_FRAME (1 MiB).
    const RAW_CHUNK_BYTES: usize = 96 * 1024;

    if bytes.is_empty() || bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return None;
    }
    let filename = sanitize_remote_filename(filename);
    let required = FEAT_CONTROL | FEAT_CLI;
    let hello = krill_proto::daemon::client_hello(ClientRole::App, "krill-app-clipboard");
    let mut stream = if target.allow_local_launch() {
        let mut s = krill_proto::os::net::UnixStream::connect(target.socket()).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        s.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
        let (kind, body) = ipc::read_frame(&mut s).ok()??;
        let server = ipc::ServerHello::decode(kind, &body).ok()?;
        let negotiated = ipc::negotiate(&server, &hello)?;
        if negotiated.features & required != required {
            return None;
        }
        s.write_all(&hello.encode().ok()?).ok()?;
        s
    } else {
        let c = target.connect(&hello).ok()?;
        if c.negotiated.features & required != required {
            return None;
        }
        c.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .ok()?;
        c.stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok()?;
        c.stream
    };

    let deadline = std::time::Instant::now() + timeout;
    let mut chunks = bytes.chunks(RAW_CHUNK_BYTES);
    let first_chunk = chunks.next()?;
    let first_b64 = base64_encode(first_chunk);
    let create_script = format!(
        "umask 077; \
         d=$(mktemp -d \"${{TMPDIR:-/tmp}}/krill-paste.XXXXXXXX\" 2>/dev/null || mktemp -d /tmp/krill-paste.XXXXXXXX) || exit 1; \
         f=\"$d/{filename}\"; \
         if printf '%s' \"$1\" | (base64 -d 2>/dev/null || base64 -D 2>/dev/null || openssl base64 -d -A 2>/dev/null) > \"$f\" && [ -s \"$f\" ]; then \
           printf 'KRILL_PASTE_OK:%s:END\\n' \"$f\"; \
         else \
           rm -rf \"$d\"; printf 'KRILL_PASTE_ERR\\n'; \
         fi; \
         sleep 3"
    );
    let remote_path = run_custom_and_wait_marker(
        &mut stream,
        vec![
            b"-c".to_vec(),
            create_script.into_bytes(),
            b"sh".to_vec(),
            first_b64,
        ],
        "KRILL_PASTE_OK:",
        ":END",
        "KRILL_PASTE_ERR",
        deadline,
    )?;
    if !remote_path.starts_with('/') || remote_path.contains(['\n', '\r', '\0']) {
        return None;
    }

    let append_script = b"umask 077; \
        if printf '%s' \"$2\" | (base64 -d 2>/dev/null || base64 -D 2>/dev/null || openssl base64 -d -A 2>/dev/null) >> \"$1\"; then \
          printf 'KRILL_APPEND_OK:ok:END\\n'; \
        else \
          rm -f \"$1\"; printf 'KRILL_APPEND_ERR\\n'; \
        fi; \
        sleep 3"
        .to_vec();

    for chunk in chunks {
        let chunk_b64 = base64_encode(chunk);
        run_custom_and_wait_marker(
            &mut stream,
            vec![
                b"-c".to_vec(),
                append_script.clone(),
                b"sh".to_vec(),
                remote_path.as_bytes().to_vec(),
                chunk_b64,
            ],
            "KRILL_APPEND_OK:",
            ":END",
            "KRILL_APPEND_ERR",
            deadline,
        )?;
    }

    Some(remote_path)
}

fn upload_bytes_with_program(
    host: &krill_app::ssh_hosts::SshHost,
    filename: &str,
    bytes: &[u8],
    program: &std::ffi::OsStr,
    timeout: Duration,
) -> Option<String> {
    use std::io::{Read as _, Write as _};
    use std::process::Stdio;
    if bytes.is_empty() || bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return None;
    }
    let destination = host.destination.trim();
    if destination.is_empty() || destination.starts_with('-') {
        return None;
    }
    let filename = sanitize_remote_filename(filename);
    let mut args = host.common_ssh_args(10);
    args.extend([
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "StrictHostKeyChecking=accept-new".to_owned(),
        destination.to_owned(),
        // A private, unique directory prevents collisions and follows the remote TMPDIR.
        format!(
            "umask 077; d=$(mktemp -d \"${{TMPDIR:-/tmp}}/krill-paste.XXXXXXXX\" 2>/dev/null || mktemp -d /tmp/krill-paste.XXXXXXXX) || exit 1; \
             if cat > \"$d/{filename}\"; then printf '%s\\n' \"$d/{filename}\"; \
             else rm -rf \"$d\"; exit 1; fi"
        ),
    ]);
    let mut child = krill_app::platform::background_command(program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    // The writer owns stdin and drops it before wait: remote cat needs EOF to finish.
    // Poll on the caller thread so a blocked write or stalled SSH is also bounded.
    let success = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(bytes).is_ok());
        let deadline = std::time::Instant::now() + timeout;
        let exited = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    child.kill().ok();
                    child.wait().ok();
                    break false;
                }
            }
        };
        writer.join().unwrap_or(false) && exited
    });
    if !success {
        return None;
    }
    let mut output = Vec::new();
    child
        .stdout
        .take()?
        .take(4097)
        .read_to_end(&mut output)
        .ok()?;
    if output.len() > 4096 {
        return None;
    }
    let path = String::from_utf8(output)
        .ok()?
        .trim_end_matches('\n')
        .to_owned();
    if !path.starts_with('/') || path.contains(['\n', '\r', '\0']) {
        return None;
    }
    Some(path)
}

fn upload_local_paths(
    target: &RemoteUploadTarget,
    paths: &[&std::path::PathBuf],
) -> Option<Vec<u8>> {
    let mut remote_paths: Vec<String> = Vec::new();
    for path in paths {
        use std::io::Read as _;
        let file = std::fs::File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.is_empty() || bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            return None;
        }
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| "pasted-file".to_owned(), sanitize_remote_filename);
        remote_paths.push(upload_bytes_to_remote_target(target, &filename, &bytes)?);
    }
    if remote_paths.is_empty() {
        return None;
    }
    Some(krill_term::paste::dropped(
        remote_paths.iter().map(String::as_bytes),
    ))
}

fn upload_clipboard_image(target: &RemoteUploadTarget, img: &gpui::Image) -> Option<Vec<u8>> {
    if img.bytes.is_empty() || img.bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return None;
    }
    let sniffed_ext = sniff_clipboard_image_extension(&img.bytes, img.format);
    let (ext, payload): (&str, Vec<u8>) =
        if sniffed_ext == "tiff" || img.format == gpui::ImageFormat::Tiff {
            if let Some(png) = krill_app::platform::tiff_to_png(&img.bytes) {
                ("png", png)
            } else {
                (sniffed_ext, img.bytes.clone())
            }
        } else {
            (sniffed_ext, img.bytes.clone())
        };
    let filename = format!("clipboard-{:016x}.{}", img.id, ext);
    let remote_path = upload_bytes_to_remote_target(target, &filename, &payload)?;
    Some(krill_term::paste::dropped([remote_path.as_bytes()]))
}

pub(crate) fn save_clipboard_image(img: &gpui::Image) -> std::io::Result<std::path::PathBuf> {
    use std::borrow::Cow;
    use std::io::{self, Write as _};

    if img.bytes.is_empty() || img.bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clipboard image size out of bounds",
        ));
    }
    let dir = std::env::temp_dir().join("krill-paste");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok();
    }
    let sniffed_ext = sniff_clipboard_image_extension(&img.bytes, img.format);
    let (ext, payload): (&str, Cow<'_, [u8]>) =
        if sniffed_ext == "tiff" || img.format == gpui::ImageFormat::Tiff {
            if let Some(png) = krill_app::platform::tiff_to_png(&img.bytes) {
                ("png", Cow::Owned(png))
            } else {
                (sniffed_ext, Cow::Borrowed(img.bytes.as_slice()))
            }
        } else {
            (sniffed_ext, Cow::Borrowed(img.bytes.as_slice()))
        };
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
    file.write_all(&payload)?;
    file.sync_all()?;
    Ok(path)
}

#[cfg(all(test, unix))]
mod tests;
