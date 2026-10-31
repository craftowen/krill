//! MK-092 셸 연결. 잠정 결정 의존: MK-030 gpui 셸·포커스 칸. 거부 시 이 파일과 shell.rs의 연결만 되돌린다.

use gpui::{ClipboardItem, Context, Entity, Subscription, WeakEntity, Window, div, prelude::*};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::PopupMenu;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_app::commands::{QuickCommand, text};
use std::ops::Range;
use std::sync::Arc;

use super::Shell;

const QUICK_MENU_THRESHOLD: usize = 32;
const QUICK_VISIBLE_ROWS: usize = 8;

/// Keep source indices so filtering never changes which command is injected.
fn filtered_commands(commands: &[QuickCommand], query: &str) -> Vec<usize> {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            let haystack = format!("{}\n{}", command.name, command.text).to_lowercase();
            terms
                .iter()
                .all(|term| haystack.contains(term))
                .then_some(index)
        })
        .collect()
}

fn command_window(indices: &[usize], range: Range<usize>) -> &[usize] {
    let start = range.start.min(indices.len());
    let end = range.end.min(indices.len()).max(start);
    indices.get(start..end).unwrap_or_default()
}

/// Created when a large menu opens; only visible rows become GPUI elements.
struct CommandList {
    commands: Arc<Vec<QuickCommand>>,
    filtered: Vec<usize>,
    input: Entity<InputState>,
    shell: WeakEntity<Shell>,
    menu: WeakEntity<PopupMenu>,
    can_inject: bool,
    selected: Option<usize>,
    scroll: gpui::UniformListScrollHandle,
    _input: Subscription,
}

impl CommandList {
    fn new(
        commands: Arc<Vec<QuickCommand>>,
        shell: WeakEntity<Shell>,
        menu: WeakEntity<PopupMenu>,
        can_inject: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::QUICK.get()));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, input, event, window, cx| match event {
                    InputEvent::Change => {
                        this.filtered = filtered_commands(&this.commands, &input.read(cx).value());
                        this.selected = None;
                        this.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        let composing = input.update(cx, |input, cx| {
                            gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some()
                        });
                        if !composing {
                            this.inject(this.selected.unwrap_or(0), window, cx);
                        }
                    }
                    _ => {}
                },
            );
        // DropdownMenu focuses its menu after building; defer search focus until then.
        cx.defer_in(window, |this, window, cx| {
            this.input.update(cx, |input, cx| input.focus(window, cx));
        });
        Self {
            filtered: filtered_commands(&commands, ""),
            commands,
            input,
            shell,
            menu,
            can_inject,
            selected: None,
            scroll: gpui::UniformListScrollHandle::new(),
            _input: subscription,
        }
    }

    fn inject(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(command) = self.filtered.get(row).and_then(|&i| self.commands.get(i)) else {
            return;
        };
        if !self.can_inject || command.text.is_empty() {
            return;
        }
        self.shell
            .update(cx, |shell, cx| shell.inject_command(command, window, cx))
            .ok();
        self.menu
            .update(cx, |_, cx| cx.emit(gpui::DismissEvent))
            .ok();
    }
}

impl Render for CommandList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = krill_app::ui::palette(cx);
        let rows = self.filtered.len().clamp(1, QUICK_VISIBLE_ROWS);
        let height = gpui::px(32. * f32::from(u16::try_from(rows).unwrap_or(8)));
        div()
            .id("quick-command-search-list")
            .debug_selector(|| "quick-command-search-list".into())
            .w(krill_app::ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - krill_app::ui::SPACE_4 * 2.).max(gpui::px(0.)))
            .flex()
            .flex_col()
            .gap_1()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                let next = match event.keystroke.key.as_str() {
                    "down" => Some(this.selected.map_or(0, |row| {
                        (row + 1).min(this.filtered.len().saturating_sub(1))
                    })),
                    "up" => Some(this.selected.unwrap_or(0).saturating_sub(1)),
                    _ => None,
                };
                if let Some(next) = next {
                    this.selected = Some(next);
                    this.scroll
                        .scroll_to_item(next, gpui::ScrollStrategy::Nearest);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(Input::new(&self.input).small())
            .when(self.filtered.is_empty(), |d| {
                d.child(
                    div()
                        .p_2()
                        .text_size(krill_app::ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .child(text::EMPTY.get()),
                )
            })
            .child(
                gpui::uniform_list(
                    "quick-command-rows",
                    self.filtered.len(),
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        let p = krill_app::ui::palette(cx);
                        let start = range.start;
                        command_window(&this.filtered, range)
                            .iter()
                            .enumerate()
                            .filter_map(|(offset, &index)| {
                                let command = this.commands.get(index)?;
                                let row = start + offset;
                                let enabled = this.can_inject && !command.text.is_empty();
                                Some(
                                    div()
                                        .id(("quick-command", index))
                                        .debug_selector(move || format!("quick-command-{index}"))
                                        .role(gpui::Role::ListBoxOption)
                                        .aria_selected(this.selected == Some(row))
                                        .h(gpui::px(32.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .text_size(krill_app::ui::TEXT_SM)
                                        .text_color(p.popover_foreground)
                                        .when(this.selected == Some(row), |d| d.bg(p.accent))
                                        .when(!enabled, |d| d.opacity(0.5))
                                        .when(enabled, |d| {
                                            d.cursor_pointer().hover(|d| d.bg(p.accent))
                                        })
                                        .child(div().truncate().child(command.name.clone()))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.inject(row, window, cx);
                                        })),
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .h(height),
            )
    }
}

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
        let commands = Arc::new(settings.quick_commands);
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
        .dropdown_menu(move |mut menu, window, cx| {
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
                let parent = cx.weak_entity();
                let list = cx.new(|cx| {
                    CommandList::new(
                        Arc::clone(&commands),
                        this.clone(),
                        parent,
                        can_inject,
                        window,
                        cx,
                    )
                });
                return menu
                    .item(PopupMenuItem::element(move |_, _| list.clone()))
                    .separator()
                    .item(edit);
            }
            menu = menu.scrollable(true).max_h(krill_app::ui::DIALOG_WIDTH);
            for command in commands.iter() {
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

#[cfg(test)]
mod window_tests {
    use super::*;

    #[test]
    fn commands_filter_matches_name_body_case_and_multiple_terms() {
        let commands = vec![
            QuickCommand {
                name: "상태 Git".into(),
                text: "git STATUS".into(),
            },
            QuickCommand {
                name: "배포".into(),
                text: "cargo run".into(),
            },
        ];
        assert_eq!(filtered_commands(&commands, " 상태 status "), vec![0]);
        assert_eq!(filtered_commands(&commands, "RUN"), vec![1]);
        assert_eq!(filtered_commands(&commands, "  "), vec![0, 1]);
        assert!(filtered_commands(&commands, "missing").is_empty());
    }

    #[test]
    fn commands_large_windows_keep_source_identity_and_boundaries() {
        let commands: Vec<_> = (0..10_000)
            .map(|i| QuickCommand {
                name: format!("command {i}"),
                text: format!("echo {i}"),
            })
            .collect();
        let indices = filtered_commands(&commands, "command");
        assert_eq!(
            command_window(&indices, 500..508),
            &[500, 501, 502, 503, 504, 505, 506, 507]
        );
        assert_eq!(command_window(&indices, 9999..usize::MAX), &[9999]);
        assert!(command_window(&indices, 20_000..usize::MAX).is_empty());
        assert!(command_window(&[], 0..8).is_empty());
        assert_eq!(filtered_commands(&commands, "9999"), vec![9999]);
    }
}
