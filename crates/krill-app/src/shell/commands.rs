//! MK-092 셸 연결. 잠정 결정 의존: MK-030 gpui 셸·포커스 칸. 거부 시 이 파일과 shell.rs의 연결만 되돌린다.

use gpui::{ClipboardItem, Context, Window, div, prelude::*};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_app::commands::{QuickCommand, text};

use super::Shell;

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

    /// 탭 바 오른쪽 아이콘 두 개(OWE-653: 따로 차지하던 툴바 줄을 없앴다).
    pub(super) fn render_commands(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = krill_app::settings::current(cx);
        let lines = settings
            .copy_context_lines
            .clamp(1, krill_app::commands::MAX_CONTEXT_LINES);
        let live = self.layout.focused().and_then(|p| self.live.get(&p));
        let can_inject = live.is_some_and(|l| l.pane.is_linked() && !l.pane.is_closed());
        let can_copy = live.is_some_and(|l| !l.pane.screen().rows.is_empty());
        let this = cx.weak_entity();
        let quick_tooltip = format!("{} · {}", text::QUICK.get(), text::QUICK_HINT.get());
        let quick = krill_app::ui::button_with_tooltip(
            Button::new("quick-commands")
                .icon(IconName::Play)
                .ghost()
                .xsmall()
                .debug_selector(|| "quick-commands".into()),
            "quick-commands",
            quick_tooltip,
        )
        .dropdown_menu(move |mut menu, _, _| {
            if settings.quick_commands.is_empty() {
                return menu.label(text::EMPTY.get());
            }
            for command in &settings.quick_commands {
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
            menu
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_0p5()
            .child(quick)
            .child(krill_app::ui::button_with_tooltip(
                Button::new("copy-context")
                    .icon(IconName::Copy)
                    .ghost()
                    .xsmall()
                    .disabled(!can_copy)
                    .debug_selector(|| "copy-context".into())
                    .on_click(cx.listener(move |s, _, _, cx| s.copy_context(lines, cx))),
                "copy-context",
                format!(
                    "{} · {}",
                    text::COPY.get(),
                    text::COPY_HINT.format(&[("n", &lines.to_string())])
                ),
            ))
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
