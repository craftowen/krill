// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Group Chats tab/screen for Krill Bots modal (`ListGroups`/`CreateGroup`/`UpdateGroup`/`DeleteGroup`/
//! `ReorderGroups`/`NewGroupThread`/`ReadGroup`/`SendGroupMessage`/`StopGroup`).

use std::time::Duration;

use gpui::{
    App, Context, Entity, Focusable as _, FontWeight, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::bots::{
    BotGroupChat, BotGroupReplyMode, BotGroupView, BotId, BotRpcRequest, BotRpcResponse,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::{call_checked, expect_ok, expect_variant};
use super::{Close, Screen, View, clear_input_on_window, opt_input_composing};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum GroupSendState {
    #[default]
    Idle,
    Sending,
}

#[derive(Default)]
pub(super) struct GroupsState {
    pub(super) groups: Vec<BotGroupChat>,
    pub(super) selected_group_id: Option<String>,
    pub(super) active_view: Option<BotGroupView>,
    pub(super) selected_thread_id: Option<String>,
    pub(super) editing_form: bool,
    pub(super) reply_mode: BotGroupReplyMode,
    pub(super) member_bot_ids: Vec<BotId>,
    pub(super) name_input: Option<Entity<InputState>>,
    pub(super) desc_input: Option<Entity<InputState>>,
    pub(super) thread_title_input: Option<Entity<InputState>>,
    pub(super) composer_input: Option<Entity<InputState>>,
    pub(super) composer_sub: Option<Subscription>,
    pub(super) needs_sync_form: bool,
    pub(super) busy: bool,
    pub(super) send_state: GroupSendState,
}

impl GroupsState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.name_input.as_ref(), window, app)
            || opt_input_composing(self.desc_input.as_ref(), window, app)
            || opt_input_composing(self.thread_title_input.as_ref(), window, app)
            || opt_input_composing(self.composer_input.as_ref(), window, app)
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        let updates = [
            (&self.name_input, text::GROUP_NAME_PLACEHOLDER.get()),
            (&self.desc_input, text::GROUP_DESC_PLACEHOLDER.get()),
            (
                &self.thread_title_input,
                text::GROUP_NEW_THREAD_PLACEHOLDER.get(),
            ),
            (&self.composer_input, text::GROUP_MESSAGE_PLACEHOLDER.get()),
        ];
        for (slot, placeholder) in updates {
            if let Some(input) = slot {
                input.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            }
        }
    }
}

impl View {
    pub fn open_groups(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.error = None;
        self.notice = None;
        self.screen = Screen::Groups;
        self.reload_groups(cx);
        cx.notify();
    }

    pub fn reload_groups(&mut self, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.groups.busy = true;
        self.error = None;
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    expect_variant(
                        &rpc,
                        &BotRpcRequest::ListGroups,
                        |r| match r {
                            BotRpcResponse::Groups(list) => Some(list),
                            _ => None,
                        },
                        "Groups",
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.groups.busy = false;
                match res {
                    Ok(groups) => {
                        this.groups.groups = groups;
                        let target_id = this
                            .groups
                            .selected_group_id
                            .clone()
                            .filter(|id| this.groups.groups.iter().any(|g| &g.id == id))
                            .or_else(|| this.groups.groups.first().map(|g| g.id.clone()));
                        if let Some(gid) = target_id {
                            this.groups.selected_group_id = Some(gid.clone());
                            this.load_group_view(gid, this.groups.selected_thread_id.clone(), cx);
                        } else {
                            this.groups.selected_group_id = None;
                            this.groups.active_view = None;
                        }
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn select_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        self.screen = Screen::Groups;
        self.groups.selected_group_id = Some(group_id.clone());
        self.groups.selected_thread_id = None;
        self.groups.editing_form = false;
        self.groups.needs_sync_form = true;
        self.load_group_view(group_id, None, cx);
    }

    pub fn start_new_group_form(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.screen = Screen::Groups;
        self.groups.editing_form = true;
        self.groups.selected_group_id = None;
        self.groups.active_view = None;
        self.groups.selected_thread_id = None;
        self.groups.member_bot_ids = self.bots.iter().take(2).map(|b| b.id.clone()).collect();
        if let Some(window) = window {
            if let Some(i) = &self.groups.name_input {
                i.update(cx, |s, cx| s.set_value(String::new(), window, cx));
            }
            if let Some(i) = &self.groups.desc_input {
                i.update(cx, |s, cx| s.set_value(String::new(), window, cx));
            }
        } else {
            let handle = self.window_handle;
            clear_input_on_window(&mut self.groups.name_input, handle, cx);
            clear_input_on_window(&mut self.groups.desc_input, handle, cx);
        }
        cx.notify();
    }

    pub fn load_group_view(
        &mut self,
        group_id: String,
        thread_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        self.groups.busy = true;
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    expect_variant(
                        &rpc,
                        &BotRpcRequest::ReadGroup {
                            group_id,
                            thread_id,
                            limit: Some(100),
                        },
                        |r| match r {
                            BotRpcResponse::GroupView(view) => Some(view),
                            _ => None,
                        },
                        "GroupView",
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.groups.busy = false;
                match res {
                    Ok(view) => {
                        let gid = view.group.id.clone();
                        let active_run = view.active_run;
                        let latest_thread = this
                            .groups
                            .selected_thread_id
                            .clone()
                            .filter(|tid| view.threads.iter().any(|t| &t.thread_id == tid))
                            .or_else(|| view.threads.first().map(|t| t.thread_id.clone()));
                        this.groups.selected_thread_id.clone_from(&latest_thread);
                        this.groups.member_bot_ids.clone_from(&view.group.bot_ids);
                        this.groups.needs_sync_form = true;
                        this.groups.active_view = Some(view);
                        if active_run {
                            this.start_group_poll(gid, latest_thread, cx);
                        } else {
                            this.group_poll_task = None;
                        }
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn start_group_poll(
        &mut self,
        group_id: String,
        thread_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        self.group_poll_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::ReadGroup {
                            group_id,
                            thread_id,
                            limit: Some(100),
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(BotRpcResponse::GroupView(view)) = res {
                    let active_run = view.active_run;
                    let gid = view.group.id.clone();
                    let tid = this.groups.selected_thread_id.clone();
                    if this.groups.selected_group_id.as_ref() == Some(&gid) {
                        this.groups.active_view = Some(view);
                        if active_run && this.screen == Screen::Groups {
                            this.start_group_poll(gid, tid, cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn submit_create_or_update_group(&mut self, cx: &mut Context<Self>) {
        if self.groups.busy {
            return;
        }
        let name = self
            .groups
            .name_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        if name.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_NAME.get()));
            cx.notify();
            return;
        }
        if self.groups.member_bot_ids.is_empty() {
            self.error = Some(SharedString::from(text::ERR_GROUP_MEMBERS.get()));
            cx.notify();
            return;
        }
        let description = self
            .groups
            .desc_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let bot_ids = self.groups.member_bot_ids.clone();
        let existing_id = self
            .groups
            .active_view
            .as_ref()
            .filter(|_| !self.groups.editing_form || self.groups.selected_group_id.is_some())
            .map(|v| v.group.id.clone())
            .or_else(|| self.groups.selected_group_id.clone());

        let rpc = self.rpc.clone();
        self.groups.busy = true;
        self.error = None;
        self.notice = None;
        let is_create = existing_id.is_none();
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let req = if let Some(group_id) = existing_id {
                        BotRpcRequest::UpdateGroup {
                            group_id,
                            name: Some(name),
                            description: Some(description),
                            bot_ids: Some(bot_ids),
                        }
                    } else {
                        BotRpcRequest::CreateGroup {
                            name,
                            bot_ids,
                            description,
                        }
                    };
                    expect_variant(
                        &rpc,
                        &req,
                        |r| match r {
                            BotRpcResponse::Group(g) => Some(g),
                            _ => None,
                        },
                        "Group",
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.groups.busy = false;
                match res {
                    Ok(group) => {
                        let gid = group.id.clone();
                        if let Some(slot) = this.groups.groups.iter_mut().find(|g| g.id == gid) {
                            *slot = group;
                        } else {
                            this.groups.groups.push(group);
                        }
                        this.groups.selected_group_id = Some(gid.clone());
                        this.groups.editing_form = false;
                        if is_create {
                            let handle = this.window_handle;
                            clear_input_on_window(&mut this.groups.name_input, handle, cx);
                            clear_input_on_window(&mut this.groups.desc_input, handle, cx);
                        }
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                        this.load_group_view(gid, this.groups.selected_thread_id.clone(), cx);
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn delete_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if self.groups.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.groups.busy = true;
        self.error = None;
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let gid = group_id.clone();
            let res = cx
                .background_executor()
                .spawn(
                    async move { expect_ok(&rpc, &BotRpcRequest::DeleteGroup { group_id: gid }) },
                )
                .await;
            this.update(cx, |this, cx| {
                this.groups.busy = false;
                match res {
                    Ok(()) => {
                        this.groups.groups.retain(|g| g.id != group_id);
                        if this.groups.selected_group_id.as_ref() == Some(&group_id) {
                            this.groups.selected_group_id =
                                this.groups.groups.first().map(|g| g.id.clone());
                            this.groups.active_view = None;
                            this.groups.selected_thread_id = None;
                            if let Some(next_gid) = this.groups.selected_group_id.clone() {
                                this.load_group_view(next_gid, None, cx);
                            }
                        }
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn reorder_group(&mut self, idx: usize, delta: isize, cx: &mut Context<Self>) {
        let len = self.groups.groups.len();
        let target = idx.checked_add_signed(delta);
        let Some(new_idx) = target.filter(|&i| i < len) else {
            return;
        };
        let mut reordered = self.groups.groups.clone();
        reordered.swap(idx, new_idx);
        let group_ids: Vec<String> = reordered.iter().map(|g| g.id.clone()).collect();
        let rpc = self.rpc.clone();
        self.groups.busy = true;
        self.error = None;
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::ReorderGroups { group_ids }) },
                )
                .await;
            this.update(cx, |this, cx| {
                this.groups.busy = false;
                match res {
                    Ok(BotRpcResponse::Groups(list)) => {
                        this.groups.groups = list;
                    }
                    Ok(_) => {
                        this.groups.groups = reordered;
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn create_group_thread(&mut self, group_id: String, cx: &mut Context<Self>) {
        if self.groups.busy {
            return;
        }
        let title = self
            .groups
            .thread_title_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        let rpc = self.rpc.clone();
        self.groups.busy = true;
        self.error = None;
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let gid = group_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::NewGroupThread {
                            group_id: gid,
                            title,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.groups.busy = false;
                match res {
                    Ok(BotRpcResponse::GroupView(view)) => {
                        this.groups.selected_thread_id =
                            view.threads.first().map(|t| t.thread_id.clone());
                        this.groups.active_view = Some(view);
                        let handle = this.window_handle;
                        clear_input_on_window(&mut this.groups.thread_title_input, handle, cx);
                    }
                    Ok(_) => {
                        let handle = this.window_handle;
                        clear_input_on_window(&mut this.groups.thread_title_input, handle, cx);
                        this.load_group_view(group_id, None, cx);
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn send_group_message(&mut self, group_id: String, cx: &mut Context<Self>) {
        if self.groups.send_state == GroupSendState::Sending {
            return;
        }
        let msg = self
            .groups
            .composer_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        if msg.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let thread_id = self.groups.selected_thread_id.clone();
        let reply_mode = Some(self.groups.reply_mode);
        let rpc = self.rpc.clone();
        self.groups.send_state = GroupSendState::Sending;
        self.error = None;
        let gid_clone = group_id.clone();
        let tid_clone = thread_id.clone();
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SendGroupMessage {
                            group_id: gid_clone,
                            thread_id: tid_clone,
                            sender: "owner".to_owned(),
                            text: msg,
                            reply_mode,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.groups.send_state = GroupSendState::Idle;
                match res {
                    Ok(_) => {
                        let handle = this.window_handle;
                        clear_input_on_window(&mut this.groups.composer_input, handle, cx);
                        this.load_group_view(group_id, thread_id, cx);
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn stop_group_run(&mut self, group_id: String, cx: &mut Context<Self>) {
        let thread_id = self.groups.selected_thread_id.clone();
        let rpc = self.rpc.clone();
        self.group_poll_task = None;
        self.group_task = Some(cx.spawn(async move |this, cx| {
            let gid = group_id.clone();
            let tid = thread_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    expect_ok(
                        &rpc,
                        &BotRpcRequest::StopGroup {
                            group_id: gid,
                            thread_id: tid,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(()) => this.load_group_view(group_id, thread_id, cx),
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn ensure_group_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.groups.name_input.is_none() {
            self.groups.name_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::GROUP_NAME_PLACEHOLDER.get())
            }));
        }
        if self.groups.desc_input.is_none() {
            self.groups.desc_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::GROUP_DESC_PLACEHOLDER.get())
            }));
        }
        if self.groups.thread_title_input.is_none() {
            let i = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::GROUP_NEW_THREAD_PLACEHOLDER.get())
            });
            self.groups.thread_title_input = Some(i);
        }
        if self.groups.composer_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::GROUP_MESSAGE_PLACEHOLDER.get())
            });
            self.groups.composer_sub =
                Some(
                    cx.subscribe_in(&input, window, |this, inp, ev: &InputEvent, window, cx| {
                        if matches!(ev, InputEvent::PressEnter { .. })
                            && !crate::jump::entity_input_is_composing(inp, window, cx)
                            && let Some(gid) = this.groups.selected_group_id.clone()
                        {
                            this.send_group_message(gid, cx);
                        }
                    }),
                );
            self.groups.composer_input = Some(input);
        }
        if self.groups.needs_sync_form {
            self.groups.needs_sync_form = false;
            if let Some(view) = &self.groups.active_view {
                if let Some(i) = &self.groups.name_input {
                    let name = view.group.name.clone();
                    i.update(cx, |s, cx| s.set_value(name, window, cx));
                }
                if let Some(i) = &self.groups.desc_input {
                    let desc = view.group.description.clone();
                    i.update(cx, |s, cx| s.set_value(desc, window, cx));
                }
            }
        }
    }

    pub(super) fn render_groups_view(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_group_inputs(window, cx);

        let header = div()
            .flex_none()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(p.border)
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(text::GROUP_CHATS_HEADING.get()),
            )
            .child(
                ui::controls()
                    .child(
                        ui::secondary("bots-group-new-btn", text::NEW_GROUP_BTN.get())
                            .debug_selector(|| "bots-group-new-btn".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_new_group_form(Some(window), cx);
                            })),
                    )
                    .when(!self.bots.is_empty(), |d| {
                        d.child(ui::ghost("bots-groups-back", text::BACK.get()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Detail;
                                cx.notify();
                            }),
                        ))
                    })
                    .child(ui::button_with_tooltip(
                        Button::new("bots-close")
                            .small()
                            .ghost()
                            .icon(IconName::Close)
                            .debug_selector(|| "bots-close".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Close))),
                        "bots-close",
                        text::CLOSE.get(),
                    )),
            );

        let mut body = div()
            .id("bots-groups-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_4();

        // Group selector strip with reorder + delete controls.
        if !self.groups.groups.is_empty() {
            let mut strip = div().flex().flex_wrap().items_center().gap_1p5();
            let count = self.groups.groups.len();
            for (idx, group) in self.groups.groups.iter().enumerate() {
                let gid = group.id.clone();
                let gid_up = group.id.clone();
                let gid_down = group.id.clone();
                let gid_del = group.id.clone();
                let active = self.groups.selected_group_id.as_ref() == Some(&gid);
                strip = strip.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(if active { p.ring } else { p.border })
                        .bg(if active { p.accent } else { p.card })
                        .child(
                            ui::ghost(format!("bots-group-select-{idx}"), group.name.clone())
                                .debug_selector(move || format!("bots-group-select-{idx}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_group(gid.clone(), cx);
                                })),
                        )
                        .when(idx > 0, |d| {
                            d.child(
                                ui::ghost(
                                    format!("bots-group-up-{idx}"),
                                    text::GROUP_MOVE_UP.get(),
                                )
                                .disabled(self.groups.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let _ = &gid_up;
                                        this.reorder_group(idx, -1, cx);
                                    },
                                )),
                            )
                        })
                        .when(idx + 1 < count, |d| {
                            d.child(
                                ui::ghost(
                                    format!("bots-group-down-{idx}"),
                                    text::GROUP_MOVE_DOWN.get(),
                                )
                                .disabled(self.groups.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let _ = &gid_down;
                                        this.reorder_group(idx, 1, cx);
                                    },
                                )),
                            )
                        })
                        .child(
                            ui::ghost(format!("bots-group-del-{idx}"), text::DELETE_BOT.get())
                                .disabled(self.groups.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_group(gid_del.clone(), cx);
                                })),
                        ),
                );
            }
            body = body.child(strip);
        }

        // Create/Edit group membership card.
        if self.groups.editing_form || self.groups.groups.is_empty() {
            let mut member_chips = div().flex().flex_wrap().gap_1p5();
            for bot in &self.bots {
                let bid = bot.id.clone();
                let selected = self.groups.member_bot_ids.contains(&bid);
                let label = match bot.emoji.as_deref() {
                    Some(e) if !e.is_empty() => format!("{e} {}", bot.name),
                    _ => bot.name.clone(),
                };
                member_chips = member_chips.child(
                    ui::segment(
                        p,
                        format!("bots-group-member-{}", bot.id.as_str()),
                        label,
                        selected,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(pos) =
                            this.groups.member_bot_ids.iter().position(|id| id == &bid)
                        {
                            this.groups.member_bot_ids.remove(pos);
                        } else {
                            this.groups.member_bot_ids.push(bid.clone());
                        }
                        cx.notify();
                    })),
                );
            }

            let is_creating = self.groups.selected_group_id.is_none();
            body = body.child(
                ui::card(p)
                    .debug_selector(|| "bots-group-form-card".into())
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .children(
                        self.groups
                            .name_input
                            .as_ref()
                            .map(|i| Input::new(i).small()),
                    )
                    .children(
                        self.groups
                            .desc_input
                            .as_ref()
                            .map(|i| Input::new(i).small()),
                    )
                    .child(ui::field_label(text::GROUP_MEMBERS_LABEL.get(), None, p))
                    .child(member_chips)
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .when(!self.groups.groups.is_empty() && !is_creating, |d| {
                                d.child(
                                    ui::ghost("bots-group-form-cancel", text::CANCEL.get())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.groups.editing_form = false;
                                            cx.notify();
                                        })),
                                )
                            })
                            .child(
                                ui::primary(
                                    "bots-group-save-btn",
                                    if is_creating {
                                        text::GROUP_CREATE_BTN.get()
                                    } else {
                                        text::GROUP_UPDATE_BTN.get()
                                    },
                                )
                                .debug_selector(|| "bots-group-save-btn".into())
                                .disabled(self.groups.busy)
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.submit_create_or_update_group(cx);
                                    },
                                )),
                            ),
                    ),
            );
        }

        // Active group conversation thread & messages.
        if let Some(view) = self.groups.active_view.clone() {
            let gid = view.group.id.clone();
            let gid_thread = gid.clone();
            let gid_send = gid.clone();
            let gid_stop = gid.clone();

            let mut thread_pills = div().flex().flex_wrap().items_center().gap_1p5();
            for (tidx, th) in view.threads.iter().enumerate() {
                let tid = th.thread_id.clone();
                let gid_sel = gid.clone();
                let selected = self.groups.selected_thread_id.as_ref() == Some(&tid);
                let title = if th.title.trim().is_empty() {
                    format!("#{tid}")
                } else {
                    th.title.clone()
                };
                thread_pills = thread_pills.child(
                    ui::segment(p, format!("bots-group-thread-{tidx}"), title, selected).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.groups.selected_thread_id = Some(tid.clone());
                            this.load_group_view(gid_sel.clone(), Some(tid.clone()), cx);
                        }),
                    ),
                );
            }

            let mut mention_chips = div().flex().flex_wrap().items_center().gap_1();
            for member_id in &view.group.bot_ids {
                if let Some(bot) = self.bots.iter().find(|b| &b.id == member_id) {
                    let mention_token = bot.slug.as_str().to_owned();
                    mention_chips = mention_chips.child(
                        ui::secondary(
                            format!("bots-mention-{}", bot.id.as_str()),
                            format!("@{}", bot.slug.as_str()),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if let Some(input) = &this.groups.composer_input {
                                    input.update(cx, |s, cx| {
                                        let mut cur = s.value().to_string();
                                        if !cur.is_empty() && !cur.ends_with(' ') {
                                            cur.push(' ');
                                        }
                                        cur.push('@');
                                        cur.push_str(&mention_token);
                                        cur.push(' ');
                                        s.set_value(cur, window, cx);
                                    });
                                    let focus = input.read(cx).focus_handle(cx);
                                    window.focus(&focus, cx);
                                }
                            },
                        )),
                    );
                }
            }

            let reply_mode_pills = ui::segmented_control(
                p,
                [
                    (
                        BotGroupReplyMode::Mentions,
                        "mentions",
                        text::GROUP_MODE_MENTIONS.get(),
                    ),
                    (BotGroupReplyMode::All, "all", text::GROUP_MODE_ALL.get()),
                    (
                        BotGroupReplyMode::Selected,
                        "selected",
                        text::GROUP_MODE_SELECTED.get(),
                    ),
                ]
                .into_iter()
                .map(|(mode, suffix, label)| {
                    ui::segment(
                        p,
                        format!("bots-group-mode-{suffix}"),
                        label,
                        self.groups.reply_mode == mode,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.groups.reply_mode = mode;
                        cx.notify();
                    }))
                }),
            );

            let mut msg_list = div().flex().flex_col().gap_2();
            if view.messages.is_empty() {
                msg_list = msg_list.child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::GROUP_MESSAGES_EMPTY.get()),
                );
            } else {
                for m in &view.messages {
                    msg_list = msg_list.child(
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded(ui::RADIUS_SM)
                            .bg(p.muted.opacity(0.45))
                            .border_1()
                            .border_color(p.border)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(m.sender_name.clone()),
                                    )
                                    .when(m.in_progress, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Warning,
                                            text::GROUP_RUNNING_BADGE.get(),
                                        ))
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.foreground)
                                    .child(m.body.clone()),
                            ),
                    );
                }
            }

            body = body.child(
                ui::card(p)
                    .debug_selector(|| "bots-group-active-card".into())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(view.group.name.clone()),
                            )
                            .child(
                                ui::controls()
                                    .child(
                                        ui::secondary(
                                            "bots-group-edit-toggle",
                                            text::ITEM_EDIT_BTN.get(),
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.groups.editing_form =
                                                    !this.groups.editing_form;
                                                this.groups.needs_sync_form = true;
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .when(view.active_run, |d| {
                                        d.child(
                                            ui::secondary(
                                                "bots-group-stop",
                                                text::GROUP_STOP_BTN.get(),
                                            )
                                            .debug_selector(|| "bots-group-stop".into())
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.stop_group_run(gid_stop.clone(), cx);
                                                }),
                                            ),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(thread_pills)
                            .children(
                                self.groups
                                    .thread_title_input
                                    .as_ref()
                                    .map(|i| div().w(px(180.)).child(Input::new(i).small())),
                            )
                            .child(
                                ui::secondary(
                                    "bots-group-new-thread",
                                    text::GROUP_NEW_THREAD_BTN.get(),
                                )
                                .disabled(self.groups.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.create_group_thread(gid_thread.clone(), cx);
                                    },
                                )),
                            ),
                    )
                    .child(msg_list)
                    .child(
                        div()
                            .pt_2()
                            .border_t_1()
                            .border_color(p.border)
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(mention_chips)
                                    .child(reply_mode_pills),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .children(self.groups.composer_input.as_ref().map(|i| {
                                        div().flex_1().min_w_0().child(Input::new(i).small())
                                    }))
                                    .child(
                                        ui::primary("bots-group-send", text::GROUP_SEND_BTN.get())
                                            .debug_selector(|| "bots-group-send".into())
                                            .disabled(
                                                self.groups.send_state == GroupSendState::Sending,
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.send_group_message(gid_send.clone(), cx);
                                            })),
                                    ),
                            ),
                    ),
            );
        } else if !self.groups.editing_form && self.groups.groups.is_empty() {
            body = body.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::NO_GROUPS_EMPTY.get()),
            );
        }

        div()
            .id("bots-groups-view")
            .debug_selector(|| "bots-groups-view".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .children(
                self.error
                    .as_ref()
                    .map(|err| div().px_5().pt_3().child(ui::error_banner(p, err.clone()))),
            )
            .children(self.notice.as_ref().map(|msg| {
                div().px_5().pt_3().child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(p.success.opacity(0.3))
                        .bg(p.success.opacity(0.1))
                        .text_size(ui::TEXT_XS)
                        .text_color(p.success)
                        .child(msg.clone()),
                )
            }))
            .child(body)
    }
}
