//! MK-092 셸 연결. 잠정 결정 의존: MK-030 gpui 셸·포커스 칸. 거부 시 이 파일과 shell.rs의 연결만 되돌린다.

use gpui::{ClipboardItem, Context, Window};
use gpui_component::IconName;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use krill_app::commands::{QuickCommand, text};
use krill_app::layout::Axis;
use std::sync::Arc;

use super::Shell;

/// 이보다 많은 저장 명령은 메뉴에 다 펼치지 않고 검색되는 명령 팔레트로 보낸다.
const QUICK_MENU_THRESHOLD: usize = 32;

impl Shell {
    pub(super) fn inject_command(
        &mut self,
        command: &QuickCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .focused_live()
            .is_some_and(|l| l.pane.is_linked() && !l.pane.is_closed())
        {
            return;
        }
        self.paste_bytes(command.text.as_bytes(), window, cx);
        window.focus(&self.focus, cx);
    }

    fn copy_context(&mut self, lines: u16, cx: &mut Context<Self>) {
        let Some(l) = self.focused_live() else {
            return;
        };
        match l.pane.copy_context(lines) {
            Ok(Some(text)) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Ok(None) => {}
            Err(_) => {
                self.notice = Some(text::FAILED.get().to_owned());
                cx.notify();
            }
        }
    }

    /// 탭 넘침 메뉴(⌄) 위쪽 항목: 채팅 보기·상하 분할·오른쪽 패널·저장 명령·맥락 복사.
    pub(super) fn toolbar_menu(
        &self,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        use krill_app::i18n::catalog::shell as shell_text;
        let settings = krill_app::settings::current(cx);
        let lines = settings
            .copy_context_lines
            .clamp(1, krill_app::commands::MAX_CONTEXT_LINES);
        let commands = Arc::new(settings.quick_commands);
        let live = self.layout.focused().and_then(|p| self.live.get(&p));
        let can_inject = live.is_some_and(|l| l.pane.is_linked() && !l.pane.is_closed());
        let can_copy = live.is_some_and(|l| !l.pane.screen().rows.is_empty());
        let chat_view = self.is_active_chat_view_in(cx);
        let panel_open = !self.panel_collapsed;
        let this = cx.weak_entity();
        move |menu, window, cx| {
            let commands = Arc::clone(&commands);
            let quick_this = this.clone();
            menu.item(
                PopupMenuItem::new(shell_text::TOGGLE_CHAT_VIEW.get())
                    .checked(chat_view)
                    .on_click(on_shell(this.clone(), |s, window, cx| {
                        s.toggle_chat_view_with_focus(window, cx);
                    })),
            )
            .item(
                PopupMenuItem::new(shell_text::SPLIT_DOWN.get())
                    .icon(IconName::PanelBottom)
                    .on_click(on_shell(this.clone(), |s, _, cx| {
                        s.split_active(Axis::Vertical, cx);
                    })),
            )
            .item(
                PopupMenuItem::new(shell_text::TOGGLE_PANEL.get())
                    .checked(panel_open)
                    .on_click(on_shell(this.clone(), |s, _, cx| s.toggle_panel(cx))),
            )
            .separator()
            .submenu_with_icon(
                Some(IconName::Play.into()),
                text::QUICK.get(),
                window,
                cx,
                move |menu, _, _| quick_menu(menu, &commands, can_inject, &quick_this),
            )
            .item(
                PopupMenuItem::new(text::COPY_HINT.format(&[("n", &lines.to_string())]))
                    .icon(IconName::Copy)
                    .disabled(!can_copy)
                    .on_click(on_shell(this.clone(), move |s, _, cx| {
                        s.copy_context(lines, cx);
                    })),
            )
        }
    }
}

fn on_shell(
    this: gpui::WeakEntity<Shell>,
    f: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static {
    move |_, window, cx| {
        this.update(cx, |s, cx| f(s, window, cx)).ok();
    }
}

/// 저장 명령 하위 메뉴. 목록이 길면 검색되는 명령 팔레트를 연다(팔레트도 저장 명령을 실행한다).
fn quick_menu(
    mut menu: PopupMenu,
    commands: &[QuickCommand],
    can_inject: bool,
    this: &gpui::WeakEntity<Shell>,
) -> PopupMenu {
    let edit = {
        let this = this.clone();
        PopupMenuItem::new(text::EDIT.get()).on_click(move |_, window, cx| {
            this.update(cx, |s, cx| {
                s.open_settings(
                    krill_app::settings::AdminTab::QuickCommands,
                    Some(window),
                    cx,
                );
            })
            .ok();
        })
    };
    if commands.is_empty() {
        return menu.label(text::EMPTY.get()).separator().item(edit);
    }
    if commands.len() > QUICK_MENU_THRESHOLD {
        let this = this.clone();
        return menu
            .item(
                PopupMenuItem::new(text::SEARCH.get()).on_click(move |_, window, cx| {
                    this.update(cx, |s, cx| s.toggle_jump(window, cx)).ok();
                }),
            )
            .separator()
            .item(edit);
    }
    for command in commands {
        let this = this.clone();
        let command = command.clone();
        menu = menu.item(
            PopupMenuItem::new(command.name.clone())
                .disabled(!can_inject || command.text.is_empty())
                .on_click(move |_, window, cx| {
                    this.update(cx, |s, cx| s.inject_command(&command, window, cx))
                        .ok();
                }),
        );
    }
    menu.separator().item(edit)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
