// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! New/Manage multi-bot group dialog (`BotGroupMembersDialog`, `BotPickList`,
//! `BotNameSuggestion`, `bot-group-pick`, `bot-group-members-dialog-roster`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Image,
    KeyDownEvent, MouseButton, Render, SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::bots::{BotGroupChat, BotGroupReplyAudience, BotId};

use super::{SidebarBot, avatar, group_faces};
use crate::i18n::catalog::bots as text;
use crate::ui;

const HERO_CLUSTER_SIZE: f32 = 112.;
const FILTER_MIN_BOTS: usize = 10;

/// Minimum bots required when creating vs editing a group (`Orca BotGroupMembersDialog`).
#[must_use]
pub const fn min_members(is_create: bool) -> usize {
    if is_create { 2 } else { 1 }
}

/// Suggests a readable group title once 2 or more bots are picked (`bot-group-pick.ts`).
#[must_use]
pub fn suggested_group_name(picked_names: &[&str]) -> Option<String> {
    match picked_names {
        [] | [_] => None,
        [a, b] => Some(text::GRP_NAME_TWO.format(&[("a", a), ("b", b)])),
        [a, b, c] => Some(text::GRP_NAME_THREE.format(&[("a", a), ("b", b), ("c", c)])),
        [a, b, rest @ ..] => {
            let n = rest.len().to_string();
            Some(text::GRP_NAME_MANY.format(&[("a", a), ("b", b), ("n", &n)]))
        }
    }
}

/// Orders bots with existing members first (preserving group member order) then remaining bots,
/// and applies optional case-insensitive search filtering.
#[must_use]
pub fn filtered_roster<'a>(
    bots: &'a [SidebarBot],
    initial_members: &[BotId],
    query: &str,
) -> Vec<&'a SidebarBot> {
    let mut ordered: Vec<&'a SidebarBot> = Vec::with_capacity(bots.len());
    let mut seen: HashSet<&BotId> = HashSet::new();
    for id in initial_members {
        if let Some(bot) = bots.iter().find(|b| &b.id == id)
            && seen.insert(&bot.id)
        {
            ordered.push(bot);
        }
    }
    for bot in bots {
        if seen.insert(&bot.id) {
            ordered.push(bot);
        }
    }
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return ordered;
    }
    ordered
        .into_iter()
        .filter(|bot| {
            bot.name.to_ascii_lowercase().contains(&needle)
                || bot.description.to_ascii_lowercase().contains(&needle)
        })
        .collect()
}

/// Events emitted by [`BotGroupMembersDialog`] to the host `Shell`.
#[derive(Debug, Clone)]
pub enum Event {
    Close,
    Create {
        name: String,
        bot_ids: Vec<BotId>,
    },
    Save {
        group_id: String,
        name: String,
        bot_ids: Vec<BotId>,
        reset_reply_audience: bool,
    },
    RequestDelete(BotGroupChat),
}

#[derive(Debug, Clone)]
enum Mode {
    Create,
    Edit {
        group: BotGroupChat,
        reply_audience: BotGroupReplyAudience,
        initial_members: Vec<BotId>,
    },
}

/// Workspace modal for creating a new group (`>= 2` bots) or managing an existing group's
/// name and members (`BotGroupMembersDialog`).
pub struct BotGroupMembersDialog {
    mode: Mode,
    bots: Vec<SidebarBot>,
    avatars: HashMap<BotId, Option<Arc<Image>>>,
    picked: Vec<BotId>,
    name_input: Entity<InputState>,
    name_value: String,
    filter_input: Entity<InputState>,
    filter_value: String,
    _subs: Vec<Subscription>,
    focus: FocusHandle,
}

impl std::fmt::Debug for BotGroupMembersDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotGroupMembersDialog")
            .field("bots", &self.bots.len())
            .field("picked", &self.picked.len())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<Event> for BotGroupMembersDialog {}

impl Focusable for BotGroupMembersDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BotGroupMembersDialog {
    #[must_use]
    pub fn new_group(
        bots: Vec<SidebarBot>,
        avatars: HashMap<BotId, Option<Arc<Image>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(
            Mode::Create,
            bots,
            avatars,
            Vec::new(),
            String::new(),
            window,
            cx,
        )
    }

    #[must_use]
    pub fn manage_members(
        group: BotGroupChat,
        reply_audience: BotGroupReplyAudience,
        bots: Vec<SidebarBot>,
        avatars: HashMap<BotId, Option<Arc<Image>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_name = group.name.clone();
        let picked = group.bot_ids.clone();
        let initial_members = group.bot_ids.clone();
        Self::build(
            Mode::Edit {
                group,
                reply_audience,
                initial_members,
            },
            bots,
            avatars,
            picked,
            initial_name,
            window,
            cx,
        )
    }

    fn build(
        mode: Mode,
        bots: Vec<SidebarBot>,
        avatars: HashMap<BotId, Option<Arc<Image>>>,
        picked: Vec<BotId>,
        initial_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(text::GRP_NAME_PLACEHOLDER.get());
            if !initial_name.is_empty() {
                s.set_value(initial_name.clone(), window, cx);
            }
            s
        });
        let filter_input = cx
            .new(|cx| InputState::new(window, cx).placeholder(text::GRP_FILTER_PLACEHOLDER.get()));
        let sub_name = cx.subscribe_in(
            &name_input,
            window,
            |this, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.name_value = input.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. }
                    if !crate::jump::entity_input_is_composing(input, window, cx) =>
                {
                    this.submit(cx);
                }
                _ => {}
            },
        );
        let sub_filter = cx.subscribe(&filter_input, |this, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.filter_value = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        name_input.update(cx, |s, cx| s.focus(window, cx));
        Self {
            mode,
            bots,
            avatars,
            picked,
            name_input,
            name_value: initial_name,
            filter_input,
            filter_value: String::new(),
            _subs: vec![sub_name, sub_filter],
            focus: cx.focus_handle(),
        }
    }

    #[must_use]
    pub fn is_create(&self) -> bool {
        matches!(self.mode, Mode::Create)
    }

    #[must_use]
    pub fn picked_ids(&self) -> &[BotId] {
        &self.picked
    }

    fn suggested_name(&self) -> Option<String> {
        let names: Vec<&str> = self
            .picked
            .iter()
            .filter_map(|id| self.bots.iter().find(|b| &b.id == id))
            .map(|b| b.name.as_str())
            .collect();
        suggested_group_name(&names)
    }

    fn effective_name(&self) -> Option<String> {
        let trimmed = self.name_value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
        self.suggested_name()
    }

    pub fn toggle_bot(&mut self, bot_id: &BotId, cx: &mut Context<Self>) {
        if let Some(pos) = self.picked.iter().position(|id| id == bot_id) {
            self.picked.remove(pos);
        } else {
            self.picked.push(bot_id.clone());
        }
        cx.notify();
    }

    pub fn apply_suggested_name(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.name_value.trim().is_empty() {
            return false;
        }
        let Some(sug) = self.suggested_name() else {
            return false;
        };
        self.name_value.clone_from(&sug);
        self.name_input.update(cx, |s, cx| {
            s.set_value(sug, window, cx);
        });
        cx.notify();
        true
    }

    fn removing_default_reply_bot(&self) -> Option<&SidebarBot> {
        let Mode::Edit {
            reply_audience: BotGroupReplyAudience::Bot { bot_id },
            ..
        } = &self.mode
        else {
            return None;
        };
        if self.picked.contains(bot_id) {
            return None;
        }
        self.bots.iter().find(|b| &b.id == bot_id)
    }

    fn can_submit(&self) -> bool {
        self.picked.len() >= min_members(self.is_create()) && self.effective_name().is_some()
    }

    fn input_is_composing(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        crate::jump::entity_input_is_composing(&self.name_input, window, cx)
            || crate::jump::entity_input_is_composing(&self.filter_input, window, cx)
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.can_submit() {
            return;
        }
        let Some(name) = self.effective_name() else {
            return;
        };
        let bot_ids = self.picked.clone();
        match &self.mode {
            Mode::Create => cx.emit(Event::Create { name, bot_ids }),
            Mode::Edit {
                group,
                reply_audience,
                ..
            } => {
                let reset_reply_audience = matches!(
                    reply_audience,
                    BotGroupReplyAudience::Bot { bot_id } if !bot_ids.contains(bot_id)
                );
                cx.emit(Event::Save {
                    group_id: group.id.clone(),
                    name,
                    bot_ids,
                    reset_reply_audience,
                });
            }
        }
    }
}

impl Render for BotGroupMembersDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::sync_input_placeholder(
            &self.name_input,
            text::GRP_NAME_PLACEHOLDER.get(),
            window,
            cx,
        );
        crate::i18n::sync_input_placeholder(
            &self.filter_input,
            text::GRP_FILTER_PLACEHOLDER.get(),
            window,
            cx,
        );
        let p = ui::palette(cx);
        let is_create = self.is_create();
        let min_req = min_members(is_create);
        let title = if is_create {
            text::GRP_NEW_TITLE.get()
        } else {
            text::GRP_MANAGE_MEMBERS.get()
        };
        let submit_label = if is_create {
            text::GRP_CREATE_BTN.get()
        } else {
            text::GRP_SAVE_BTN.get()
        };
        let initial_members: &[BotId] = match &self.mode {
            Mode::Create => &[],
            Mode::Edit {
                initial_members, ..
            } => initial_members,
        };
        let picked_members = group_faces::resolve_members(&self.picked, &self.bots, &|id| {
            self.avatars.get(id).and_then(Clone::clone)
        });
        let cluster =
            group_faces::render_group_faces(&picked_members, HERO_CLUSTER_SIZE, p.popover, p);
        let suggestion = self
            .name_value
            .trim()
            .is_empty()
            .then(|| self.suggested_name())
            .flatten();
        let roster = filtered_roster(&self.bots, initial_members, &self.filter_value);
        let can_submit = self.can_submit();
        let picked_count_label =
            text::GRP_PICKED_COUNT.format(&[("n", &self.picked.len().to_string())]);
        let need_more_label = (self.picked.len() < min_req).then(|| {
            if is_create {
                text::GRP_PICK_TWO.get()
            } else {
                text::GRP_PICK_ONE.get()
            }
        });
        let removal_warn = self
            .removing_default_reply_bot()
            .map(|b| text::GRP_REMOVE_REPLY_WARN.format(&[("name", &b.name)]));
        let edit_group = match &self.mode {
            Mode::Create => None,
            Mode::Edit { group, .. } => Some(group.clone()),
        };

        let mut card = div()
            .id("bot-group-members-dialog")
            .debug_selector(|| "bot-group-members-dialog".into())
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .occlude()
            .w(px(460.))
            .max_w(ui::DIALOG_WIDTH)
            .p_6()
            .rounded(ui::RADIUS_XL)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .font_family(ui::FONT_UI)
            .flex()
            .flex_col()
            .gap_4()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.input_is_composing(window, cx) {
                    return;
                }
                if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    cx.emit(Event::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(text::GRP_SHARED_ROOM_DESC.get()),
                            ),
                    )
                    .child(
                        Button::new("group-dialog-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label(text::CLOSE.get())
                            .tooltip(text::CLOSE.get())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(cluster)
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .child(Input::new(&self.name_input))
                            .child(super::pickers::render_bot_name_suggestion(
                                "group-name-suggestion",
                                suggestion,
                                None,
                                |this, _, window, cx| {
                                    let _ = this.apply_suggested_name(window, cx);
                                },
                                p,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(text::GRP_MEMBERS_LABEL.get()),
                            )
                            .child(picked_count_label),
                    )
                    .when(self.bots.len() > FILTER_MIN_BOTS, |d| {
                        d.child(
                            Input::new(&self.filter_input)
                                .small()
                                .prefix(Icon::new(IconName::Search)),
                        )
                    })
                    .child(self.render_roster(roster, p, cx)),
            )
            .children(need_more_label.map(|msg| {
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(msg)
            }))
            .children(removal_warn.map(|msg| {
                div()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_MD)
                    .bg(p.warning.opacity(0.12))
                    .text_size(ui::TEXT_SM)
                    .text_color(p.warning)
                    .child(msg)
            }));

        let footer = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().children(edit_group.map(|g| {
                Button::new("group-dialog-delete")
                    .debug_selector(|| "group-dialog-delete".into())
                    .ghost()
                    .small()
                    .label(text::GRP_DELETE_BTN.get())
                    .text_color(p.destructive)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(Event::RequestDelete(g.clone()));
                    }))
            })))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("group-dialog-cancel")
                            .debug_selector(|| "group-dialog-cancel".into())
                            .ghost()
                            .small()
                            .label(text::CANCEL.get())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    )
                    .child(
                        Button::new("group-dialog-submit")
                            .debug_selector(|| "group-dialog-submit".into())
                            .primary()
                            .small()
                            .disabled(!can_submit)
                            .label(submit_label)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            );
        card = card.child(footer);

        ui::modal_backdrop(
            window.viewport_size(),
            div()
                .id("group-dialog-backdrop")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "tab"
                        && !ev.keystroke.modifiers.modified()
                        && !this.input_is_composing(window, cx)
                        && this.apply_suggested_name(window, cx)
                    {
                        cx.stop_propagation();
                        window.prevent_default();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, _, cx| cx.emit(Event::Close)),
                )
                .child(ui::modal_focus_scope(card, &self.focus)),
        )
        .into_any_element()
    }
}

impl BotGroupMembersDialog {
    fn render_roster(
        &self,
        roster: Vec<&SidebarBot>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut box_div = div()
            .id("group-members-roster")
            .debug_selector(|| "group-members-roster".into())
            .w_full()
            .max_h(px(248.))
            .overflow_y_scroll()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(p.border)
            .p_1()
            .flex()
            .flex_col()
            .gap_0p5();

        if roster.is_empty() {
            return box_div
                .child(
                    div()
                        .py_6()
                        .px_3()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .flex()
                        .justify_center()
                        .child(
                            text::GRP_NO_BOTS_FOUND.format(&[("query", self.filter_value.trim())]),
                        ),
                )
                .into_any_element();
        }

        for bot in roster {
            let selected = self.picked.contains(&bot.id);
            let bot_id = bot.id.clone();
            let upload = self.avatars.get(&bot.id).and_then(Clone::clone);
            let avatar_el = avatar::render(&bot.avatar, upload, &bot.name, px(24.), px(7.), p);
            let check = div()
                .flex_none()
                .size(px(18.))
                .rounded_full()
                .border_1()
                .border_color(if selected { p.primary } else { p.border })
                .when(selected, |d| d.bg(p.primary))
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |d| {
                    d.child(
                        Icon::new(IconName::Check)
                            .with_size(ui::ICON_XS)
                            .text_color(p.primary_foreground),
                    )
                });
            let row = div()
                .id(SharedString::from(format!(
                    "group-member-pick-{}",
                    bot.id.as_str()
                )))
                .debug_selector({
                    let id = bot.id.as_str().to_owned();
                    move || format!("group-member-pick-{id}")
                })
                .w_full()
                .px_2p5()
                .py_1p5()
                .rounded(ui::RADIUS_SM)
                .cursor_pointer()
                .when(selected, |d| d.bg(p.accent.opacity(0.45)))
                .hover(|d| d.bg(p.accent.opacity(0.65)))
                .flex()
                .items_center()
                .gap_2p5()
                .child(avatar_el)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(ui::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.popover_foreground)
                                .child(bot.name.clone()),
                        )
                        .when(!bot.description.trim().is_empty(), |d| {
                            d.child(
                                div()
                                    .truncate()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(bot.description.clone()),
                            )
                        }),
                )
                .child(check)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_bot(&bot_id, cx);
                }));
            box_div = box_div.child(row);
        }
        box_div.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    #[test]
    fn suggested_name_formats_two_three_and_overflow() {
        assert_eq!(suggested_group_name(&[]), None);
        assert_eq!(suggested_group_name(&["Alice"]), None);
        assert_eq!(
            suggested_group_name(&["Alice", "Bob"]),
            Some("Alice & Bob".to_owned())
        );
        assert_eq!(
            suggested_group_name(&["Alice", "Bob", "Carol"]),
            Some("Alice, Bob & Carol".to_owned())
        );
        assert_eq!(
            suggested_group_name(&["Alice", "Bob", "Carol", "Dave"]),
            Some(
                text::GRP_NAME_MANY
                    .in_language(crate::i18n::language())
                    .replace("{a}", "Alice")
                    .replace("{b}", "Bob")
                    .replace("{n}", "2")
            )
        );
        let en = text::GRP_NAME_MANY
            .format_in(Language::En, &[("a", "Alice"), ("b", "Bob"), ("n", "2")]);
        assert_eq!(en, "Alice, Bob & 2 more");
    }

    #[gpui::test]
    fn modal_focus_scope_traps_tab_within_group_dialog(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let bot_a = krill_proto::bots::BotConfig::new(
            BotId::new("bot-a").unwrap(),
            "Alice",
            "claude",
            1_700_000_000_000,
        )
        .unwrap();
        let bot_b = krill_proto::bots::BotConfig::new(
            BotId::new("bot-b").unwrap(),
            "Bob",
            "claude",
            1_700_000_000_000,
        )
        .unwrap();
        let bots = super::super::rpc::sidebar_snapshot(&[bot_a, bot_b], &[], &[], &[]).bots;
        let (dialog, vcx) = cx.add_window_view(|window, cx| {
            BotGroupMembersDialog::new_group(bots, HashMap::new(), window, cx)
        });
        vcx.run_until_parked();

        // Tab cycles focus inside the modal focus scope without escaping the dialog.
        vcx.simulate_keystrokes("tab");
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert!(
                dialog.read(cx).focus.contains_focused(window, cx),
                "Tab must keep focus inside BotGroupMembersDialog's modal_focus_scope"
            );
        });
    }
}
