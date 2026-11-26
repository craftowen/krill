// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Group row in the sidebar Bots section (`BotGroupRow`, `BotGroupHoverDetails`):
//! 36 px multi-bot face cluster + status corner dot, title + age, latest activity line,
//! double-click inline rename, hover card with members & latest line, right-click menu,
//! and the "Delete group" workspace confirmation modal + `BotGroupMembersDialog` hosting.

use gpui::{
    AnyElement, Context, FontWeight, KeyDownEvent, MouseButton, SharedString, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::{IconName, Sizable as _};
use krill_app_core::hosts::HostId;
use krill_app_core::i18n::catalog::bots as text;
use krill_app_core::ui;
use krill_bots_ui::bots::{
    BotGroupMembersDialog, SidebarBot, group_dialog, group_faces, rpc::call_checked,
};
use krill_proto::bots::{
    BotGroupChat, BotGroupReplyAudience, BotGroupSummary, BotId, BotRpcRequest, BotRpcResponse,
};
use krill_proto::control::AgentState;

use super::rows::{AVATAR, RowStatus, avatar_badge};
use super::{Shell, SidebarGroup, now_secs};

/// Derived sidebar row facts for one group.
pub(super) struct GroupFacts {
    pub active: bool,
    pub all_paused: bool,
    pub status: RowStatus,
    pub preview: Option<String>,
    pub activity_ms: u64,
}

impl Shell {
    /// Picks a host that has at least 2 bots (preferring `launch_host`), for creating a new group.
    pub(super) fn default_group_host(&self) -> Option<HostId> {
        let count_on = |host: &HostId| self.sidebar_bots.iter().filter(|b| &b.host == host).count();
        if count_on(&self.launch_host) >= 2 {
            return Some(self.launch_host.clone());
        }
        let mut seen = Vec::new();
        for b in &self.sidebar_bots {
            if !seen.contains(&b.host) {
                if count_on(&b.host) >= 2 {
                    return Some(b.host.clone());
                }
                seen.push(b.host.clone());
            }
        }
        None
    }

    pub(super) fn bot_group_facts(&self, summary: &SidebarGroup, cx: &gpui::App) -> GroupFacts {
        let group = &summary.group;
        let host = &summary.host;
        let sidebar = self.sidebar.read(cx);
        let focused_pane = self.layout.focused();
        let active = focused_pane.is_some_and(|p| {
            self.layout.host(p) == host
                && self
                    .layout
                    .group_pane(p)
                    .is_some_and(|t| t.group_id == group.id)
        });

        let mut member_bots: Vec<&SidebarBot> = Vec::new();
        for bid in &group.bot_ids {
            if let Some(b) = self
                .sidebar_bots
                .iter()
                .find(|b| &b.host == host && &b.id == bid)
            {
                member_bots.push(b);
            }
        }
        let all_paused = !member_bots.is_empty() && member_bots.iter().all(|b| b.paused);
        let mut needs_you = false;
        let mut working = !summary.active_turns.is_empty();
        for bot in &member_bots {
            if bot.attention > 0 {
                needs_you = true;
            }
            for s in &bot.sessions {
                if s.working {
                    working = true;
                }
                if let Some(sess) = sidebar.session_on(host, s.session_id) {
                    match sess.status.state {
                        AgentState::Waiting | AgentState::Blocked => needs_you = true,
                        AgentState::Working => working = true,
                        _ => {}
                    }
                }
            }
        }
        let status = if needs_you {
            RowStatus::Permission
        } else if working {
            RowStatus::Working
        } else if active {
            RowStatus::Active
        } else {
            RowStatus::Inactive
        };

        let preview = summary.latest_message.as_ref().and_then(|msg| {
            let body = msg.body.trim();
            if body.is_empty() && msg.in_progress {
                return Some(text::GROUP_RUNNING_BADGE.get().to_owned());
            }
            if body.is_empty() || body == "[stopped]" {
                return None;
            }
            let author = if msg.sender_id == "owner" {
                text::GRP_YOU_SENDER.get().to_owned()
            } else {
                member_bots
                    .iter()
                    .find(|b| b.id.as_str() == msg.sender_id)
                    .map_or_else(|| msg.sender_name.clone(), |b| b.name.clone())
            };
            Some(format!("{author}: {body}"))
        });

        let activity_ms = summary
            .latest_message
            .as_ref()
            .map(|m| m.created_at_ms)
            .into_iter()
            .chain(summary.threads.iter().map(|t| t.updated_at_ms))
            .chain(std::iter::once(group.updated_at_ms))
            .max()
            .unwrap_or(group.created_at_ms);

        GroupFacts {
            active,
            all_paused,
            status,
            preview,
            activity_ms,
        }
    }

    pub(super) fn render_group_row(
        &self,
        summary: &SidebarGroup,
        facts: &GroupFacts,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host = summary.host.clone();
        let group = &summary.group;
        let gid = group.id.clone();
        let gid_click = gid.clone();
        let click_host = host.clone();
        let row_sel = format!("bot-group-row-{gid}");
        let row_id = if host.is_local() {
            row_sel.clone()
        } else {
            format!("bot-group-row-{}-{gid}", host.as_key())
        };
        let renaming = self
            .bot_ui
            .renaming_group
            .as_ref()
            .filter(|(h, id, _, _)| *h == host && id == &gid)
            .map(|(_, _, input, _)| input.clone());
        let now = now_secs();
        let age = (facts.activity_ms > 0)
            .then(|| ui::format_short_time_ago_secs(facts.activity_ms / 1000, now));

        let host_bots: Vec<SidebarBot> = self
            .sidebar_bots
            .iter()
            .filter(|b| b.host == host)
            .cloned()
            .collect();
        let face_members = group_faces::resolve_members(&group.bot_ids, &host_bots, &|id| {
            self.bot_ui
                .avatars
                .get(&(host.clone(), id.clone()))
                .and_then(|(_, img)| img.clone())
        });
        let surface_bg = if facts.active {
            p.worktree_sidebar_active()
        } else {
            p.worktree_sidebar
        };
        let faces = group_faces::render_group_faces(&face_members, AVATAR, surface_bg, p);
        let badge = (facts.status == RowStatus::Permission)
            .then(|| avatar_badge(facts.status, None, surface_bg, p, cx))
            .flatten();
        let trailing_working = (!facts.all_paused && facts.status == RowStatus::Working)
            .then(|| self.render_row_working_mark(&format!("group-{gid}"), cx));

        let unread = facts.status == RowStatus::Permission;
        let name: AnyElement = match renaming {
            Some(input) => {
                let esc_input = input.clone();
                let rename_sel = format!("bot-group-row-rename-{gid}");
                div()
                    .id(SharedString::from(rename_sel.clone()))
                    .debug_selector(move || rename_sel.clone())
                    .flex_1()
                    .min_w_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        if e.keystroke.key == "escape" && !e.keystroke.modifiers.modified() {
                            let composing = esc_input.update(cx, |s, cx| {
                                gpui::EntityInputHandler::marked_text_range(s, window, cx).is_some()
                            });
                            if !composing {
                                cx.stop_propagation();
                                window.prevent_default();
                                this.bot_ui.renaming_group = None;
                                cx.notify();
                            }
                        }
                    }))
                    .child(Input::new(&input).xsmall().appearance(false))
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ui::TEXT_MD)
                .line_height(px(20.))
                .when(unread, |d| {
                    d.font_weight(FontWeight::SEMIBOLD).text_color(p.foreground)
                })
                .when(!unread, |d| d.text_color(p.foreground.opacity(0.8)))
                .child(group.name.clone())
                .into_any_element(),
        };

        let remote_label = (!host.is_local()).then(|| self.host_display_label(&host, cx));
        let second_line = if let Some(ref line) = facts.preview {
            Some(line.clone())
        } else if !group.description.trim().is_empty() {
            Some(group.description.clone())
        } else {
            let names: Vec<&str> = face_members.iter().map(|m| m.bot.name.as_str()).collect();
            (!names.is_empty()).then(|| names.join(", "))
        };
        let line_el = match (remote_label.as_ref(), second_line) {
            (None, None) => None,
            (remote, line) => {
                let text = match (remote, line) {
                    (Some(h), Some(l)) => format!("{h} · {l}"),
                    (Some(h), None) => h.to_string(),
                    (None, Some(l)) => l,
                    (None, None) => String::new(),
                };
                Some(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_1p5()
                        .text_size(ui::TEXT_SM)
                        .line_height(px(16.))
                        .text_color(if facts.status == RowStatus::Permission {
                            p.foreground
                        } else {
                            p.muted_foreground
                        })
                        .when(remote.is_some(), |d| {
                            d.child(ui::icons::svg_box(
                                ui::icons::SERVER_SVG,
                                ui::ICON_XS,
                                p.muted_foreground,
                            ))
                        })
                        .child(div().flex_1().min_w_0().truncate().child(text)),
                )
            }
        };

        let hover = self.group_hover_card(summary, facts, cx);

        div()
            .id(SharedString::from(row_id))
            .debug_selector(move || row_sel.clone())
            .role(gpui::Role::Button)
            .tab_index(0)
            .focus_visible(|d| d.border_color(p.ring))
            .aria_label(group.name.clone())
            .ml_1()
            .pl(px(5.))
            .pr_1p5()
            .py(px(5.))
            .rounded(ui::RADIUS_MD)
            .border_1()
            .overflow_hidden()
            .flex()
            .items_center()
            .cursor_pointer()
            .when(facts.all_paused, |d| d.opacity(0.6))
            .when(facts.active, |d| {
                d.bg(p.worktree_sidebar_active())
                    .border_color(p.worktree_sidebar_active_border())
            })
            .when(!facts.active, |d| {
                d.border_color(ui::TRANSPARENT)
                    .hover(|d| d.bg(p.worktree_sidebar_accent))
            })
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                if this
                    .bot_ui
                    .renaming_group
                    .as_ref()
                    .is_some_and(|(h, id, _, _)| *h == click_host && id == &gid_click)
                {
                    return;
                }
                if e.click_count() > 1 {
                    this.start_group_rename_on(&click_host, &gid_click, window, cx);
                } else {
                    this.open_bot_group_on(&click_host, &gid_click, None, Some(window), cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .mr_2()
                    .flex_none()
                    .child(faces)
                    .children(badge),
            )
            .child(
                div()
                    .id(SharedString::from(if host.is_local() {
                        format!("bot-group-row-body-{gid}")
                    } else {
                        format!("bot-group-row-body-{}-{gid}", host.as_key())
                    }))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap_1()
                    .self_stretch()
                    .hoverable_tooltip(hover)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_1p5()
                                    .child(name)
                                    .when_some(age, |d, age| {
                                        let age_sel = format!("bot-group-row-age-{gid}");
                                        d.child(
                                            div()
                                                .debug_selector(move || age_sel.clone())
                                                .ml_auto()
                                                .flex_none()
                                                .text_size(ui::TEXT_3XS)
                                                .line_height(gpui::relative(1.))
                                                .text_color(p.muted_foreground)
                                                .child(age),
                                        )
                                    }),
                            )
                            .children(line_el),
                    )
                    .children(trailing_working),
            )
            .context_menu(Self::group_row_menu(summary, cx.weak_entity()))
            .into_any_element()
    }

    fn group_hover_card(
        &self,
        summary: &SidebarGroup,
        facts: &GroupFacts,
        _cx: &gpui::App,
    ) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView + Clone + 'static {
        let host = &summary.host;
        let group = summary.group.clone();
        let preview = facts.preview.clone();
        let age = (facts.activity_ms > 0)
            .then(|| ui::format_short_time_ago_secs(facts.activity_ms / 1000, now_secs()));
        let members: Vec<SidebarBot> = group
            .bot_ids
            .iter()
            .filter_map(|id| {
                self.sidebar_bots
                    .iter()
                    .find(|b| &b.host == host && &b.id == id)
                    .cloned()
            })
            .collect();
        let avatars: std::collections::HashMap<BotId, Option<std::sync::Arc<gpui::Image>>> =
            members
                .iter()
                .map(|m| {
                    (
                        m.id.clone(),
                        self.bot_ui
                            .avatars
                            .get(&(host.clone(), m.id.clone()))
                            .and_then(|(_, i)| i.clone()),
                    )
                })
                .collect();
        move |window, cx| {
            let group = group.clone();
            let preview = preview.clone();
            let age = age.clone();
            let members = members.clone();
            let avatars = avatars.clone();
            gpui_component::tooltip::Tooltip::element(move |_, cx| {
                let p = ui::palette(cx);
                let mut col = div()
                    .id("bot-group-hover-card")
                    .debug_selector(|| "bot-group-hover-card".into())
                    .w(px(260.))
                    .flex()
                    .flex_col()
                    .gap_2()
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
                                    .text_size(ui::TEXT_MD)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.popover_foreground)
                                    .child(group.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(
                                        text::GRP_PANEL_MEMBERS
                                            .format(&[("n", &members.len().to_string())]),
                                    ),
                            ),
                    );
                let mut member_list = div().flex().flex_col().gap_1();
                for m in members.iter().take(6) {
                    let upload = avatars.get(&m.id).and_then(Clone::clone);
                    let av = krill_bots_ui::bots::avatar::render(
                        &m.avatar,
                        upload,
                        &m.name,
                        px(20.),
                        ui::RADIUS_XS,
                        p,
                    );
                    member_list = member_list.child(
                        div().flex().items_center().gap_2().child(av).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.popover_foreground)
                                .child(m.name.clone()),
                        ),
                    );
                }
                col = col.child(member_list);
                if let Some(line) = preview.clone() {
                    let text_line = match age.clone() {
                        Some(age) => format!("{age} · {line}"),
                        None => line,
                    };
                    col = col.child(
                        div()
                            .pt_2()
                            .border_t_1()
                            .border_color(p.border)
                            .text_size(ui::TEXT_SM)
                            .text_color(p.popover_foreground)
                            .line_clamp(3)
                            .child(text_line),
                    );
                }
                col
            })
            .build(window, cx)
        }
    }

    fn group_row_menu(
        summary: &SidebarGroup,
        this: gpui::WeakEntity<Self>,
    ) -> impl Fn(
        gpui_component::menu::PopupMenu,
        &mut Window,
        &mut Context<gpui_component::menu::PopupMenu>,
    ) -> gpui_component::menu::PopupMenu
    + 'static {
        let host = summary.host.clone();
        let group = summary.group.clone();
        let audience = summary.reply_audience.clone();
        move |menu, _, _| {
            let t1 = this.clone();
            let t2 = this.clone();
            let t3 = this.clone();
            let t4 = this.clone();
            let h1 = host.clone();
            let h2 = host.clone();
            let h3 = host.clone();
            let h4 = host.clone();
            let g1 = group.clone();
            let g2 = group.clone();
            let g3 = group.clone();
            let g4 = group.clone();
            let aud = audience.clone();
            let new_thread = text::GRP_NEW_THREAD.format(&[("name", &g2.name)]);
            menu.item(
                PopupMenuItem::new(text::SB_RENAME.get())
                    .icon(gpui_component::Icon::default().data(ui::icons::PENCIL_SVG))
                    .on_click(move |_, window, cx| {
                        t4.update(cx, |s, cx| s.start_group_rename_on(&h4, &g4.id, window, cx))
                            .ok();
                    }),
            )
            .item(
                PopupMenuItem::new(text::GRP_MANAGE_MEMBERS_MENU.get())
                    .icon(gpui_component::Icon::default().data(ui::icons::USERS_SVG))
                    .on_click(move |_, window, cx| {
                        t1.update(cx, |s, cx| {
                            s.open_manage_group_dialog_on(&h1, g1.clone(), aud.clone(), window, cx);
                        })
                        .ok();
                    }),
            )
            .item(
                PopupMenuItem::new(new_thread)
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| {
                        t2.update(cx, |s, cx| {
                            s.create_and_open_group_thread_on(&h2, g2.id.clone(), window, cx);
                        })
                        .ok();
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new(text::GRP_DELETE_MENU.get())
                    .icon(gpui_component::Icon::default().data(ui::icons::TRASH_SVG))
                    .on_click(move |_, _, cx| {
                        t3.update(cx, |s, cx| {
                            s.bot_ui.delete_group = Some((h3.clone(), g3.clone()));
                            cx.notify();
                        })
                        .ok();
                    }),
            )
        }
    }

    pub(in crate::shell) fn open_new_group_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.default_group_host() else {
            return;
        };
        self.bot_ui.group_dialog = None;
        self.bot_ui.group_dialog_host = Some(host.clone());
        let bots: Vec<SidebarBot> = self
            .sidebar_bots
            .iter()
            .filter(|b| b.host == host)
            .cloned()
            .collect();
        let avatars: std::collections::HashMap<_, _> = self
            .bot_ui
            .avatars
            .iter()
            .filter(|((h, _), _)| h == &host)
            .map(|((_, k), (_, img))| (k.clone(), img.clone()))
            .collect();
        let dialog = cx.new(|cx| BotGroupMembersDialog::new_group(bots, avatars, window, cx));
        let sub = cx.subscribe(&dialog, Self::on_group_dialog_event);
        self.bot_ui.group_dialog = Some((dialog, sub));
        cx.notify();
    }

    pub(in crate::shell) fn open_manage_group_dialog_on(
        &mut self,
        host: &HostId,
        group: BotGroupChat,
        reply_audience: BotGroupReplyAudience,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.bot_ui.group_dialog = None;
        self.bot_ui.group_dialog_host = Some(host.clone());
        let bots: Vec<SidebarBot> = self
            .sidebar_bots
            .iter()
            .filter(|b| &b.host == host)
            .cloned()
            .collect();
        let avatars: std::collections::HashMap<_, _> = self
            .bot_ui
            .avatars
            .iter()
            .filter(|((h, _), _)| h == host)
            .map(|((_, k), (_, img))| (k.clone(), img.clone()))
            .collect();
        let dialog = cx.new(|cx| {
            BotGroupMembersDialog::manage_members(group, reply_audience, bots, avatars, window, cx)
        });
        let sub = cx.subscribe(&dialog, Self::on_group_dialog_event);
        self.bot_ui.group_dialog = Some((dialog, sub));
        cx.notify();
    }

    fn on_group_dialog_event(
        &mut self,
        _: gpui::Entity<BotGroupMembersDialog>,
        ev: &group_dialog::Event,
        cx: &mut Context<Self>,
    ) {
        let dialog_host = self
            .bot_ui
            .group_dialog_host
            .clone()
            .unwrap_or_else(|| self.launch_host.clone());
        match ev {
            group_dialog::Event::Close => {
                self.bot_ui.group_dialog = None;
                self.bot_ui.group_dialog_host = None;
                cx.notify();
            }
            group_dialog::Event::RequestDelete(group) => {
                self.bot_ui.group_dialog = None;
                self.bot_ui.group_dialog_host = None;
                self.bot_ui.delete_group = Some((dialog_host, group.clone()));
                cx.notify();
            }
            group_dialog::Event::Create { name, bot_ids } => {
                self.bot_ui.group_dialog = None;
                self.bot_ui.group_dialog_host = None;
                let Some(rpc) = self.bot_rpc_on(&dialog_host) else {
                    return;
                };
                let name = name.clone();
                let bot_ids = bot_ids.clone();
                cx.spawn(async move |this, cx| {
                    let res = cx
                        .background_executor()
                        .spawn(async move {
                            call_checked(
                                &rpc,
                                &BotRpcRequest::CreateGroup {
                                    name,
                                    bot_ids,
                                    description: String::new(),
                                },
                            )
                        })
                        .await;
                    this.update(cx, |this, cx| {
                        match res {
                            Ok(BotRpcResponse::Group(group)) => {
                                let gid = group.id.clone();
                                let summary = BotGroupSummary {
                                    group,
                                    reply_audience: BotGroupReplyAudience::default(),
                                    threads: Vec::new(),
                                    latest_message: None,
                                    active_turns: Vec::new(),
                                };
                                this.sidebar_groups.push(SidebarGroup {
                                    host: dialog_host.clone(),
                                    summary: summary.clone(),
                                });
                                if let Some(entry) = this.bot_ui.hosts.get_mut(&dialog_host) {
                                    entry.groups.push(summary);
                                }
                                this.open_bot_group_on(&dialog_host, &gid, None, None, cx);
                                this.refresh_bots_for_host(&dialog_host, true, cx);
                            }
                            Ok(_) => {}
                            Err(err) => this.notice = Some(err),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            group_dialog::Event::Save {
                group_id,
                name,
                bot_ids,
                reset_reply_audience,
            } => {
                self.bot_ui.group_dialog = None;
                self.bot_ui.group_dialog_host = None;
                let Some(rpc) = self.bot_rpc_on(&dialog_host) else {
                    return;
                };
                let group_id = group_id.clone();
                let name = name.clone();
                let bot_ids = bot_ids.clone();
                let reset_aud = *reset_reply_audience;
                cx.spawn(async move |this, cx| {
                    let gid = group_id.clone();
                    let res = cx
                        .background_executor()
                        .spawn(async move {
                            let updated = call_checked(
                                &rpc,
                                &BotRpcRequest::UpdateGroup {
                                    group_id: gid.clone(),
                                    name: Some(name),
                                    description: None,
                                    bot_ids: Some(bot_ids),
                                },
                            )?;
                            if reset_aud {
                                call_checked(
                                    &rpc,
                                    &BotRpcRequest::SetGroupReplyAudience {
                                        group_id: gid,
                                        audience: BotGroupReplyAudience::Mentions,
                                    },
                                )
                                .ok();
                            }
                            Ok::<_, String>(updated)
                        })
                        .await;
                    this.update(cx, |this, cx| {
                        match res {
                            Ok(BotRpcResponse::Group(updated)) => {
                                if let Some(s) = this
                                    .sidebar_groups
                                    .iter_mut()
                                    .find(|s| s.host == dialog_host && s.group.id == updated.id)
                                {
                                    s.group = updated.clone();
                                    if reset_aud {
                                        s.reply_audience = BotGroupReplyAudience::Mentions;
                                    }
                                }
                                if let Some(entry) = this.bot_ui.hosts.get_mut(&dialog_host)
                                    && let Some(s) =
                                        entry.groups.iter_mut().find(|s| s.group.id == updated.id)
                                {
                                    s.group = updated;
                                    if reset_aud {
                                        s.reply_audience = BotGroupReplyAudience::Mentions;
                                    }
                                }
                                this.invalidate_group_room(&dialog_host, &group_id);
                                this.refresh_bots_for_host(&dialog_host, true, cx);
                            }
                            Ok(_) => {}
                            Err(err) => this.notice = Some(err),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    pub(in crate::shell) fn start_group_rename_on(
        &mut self,
        host: &HostId,
        group_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self
            .sidebar_groups
            .iter()
            .find(|s| &s.host == host && s.group.id == group_id)
            .map(|s| s.group.clone())
        else {
            return;
        };
        let gid = group.id.clone();
        let initial = group.name;
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(initial, window, cx);
            s
        });
        let host_sub = host.clone();
        let gid_sub = gid.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } => {
                    let composing = input.update(cx, |s, cx| {
                        gpui::EntityInputHandler::marked_text_range(s, window, cx).is_some()
                    });
                    if composing {
                        return;
                    }
                    let next = input.read(cx).value().trim().to_owned();
                    this.bot_ui.renaming_group = None;
                    this.rename_group_rpc_on(&host_sub, &gid_sub, &next, cx);
                }
                InputEvent::Blur => {
                    this.bot_ui.renaming_group = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        input.update(cx, |s, cx| s.focus(window, cx));
        self.bot_ui.renaming_group = Some((host.clone(), gid, input, sub));
        cx.notify();
    }

    pub(super) fn rename_group_rpc_on(
        &mut self,
        host: &HostId,
        group_id: &str,
        new_name: &str,
        cx: &mut Context<Self>,
    ) {
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            cx.notify();
            return;
        }
        let Some(rpc) = self.bot_rpc_on(host) else {
            return;
        };
        let host_owned = host.clone();
        let gid = group_id.to_owned();
        let name = trimmed.to_owned();
        let previous = self
            .sidebar_groups
            .iter_mut()
            .find(|s| s.host == host_owned && s.group.id == gid)
            .map(|s| {
                let prev = s.group.name.clone();
                s.group.name.clone_from(&name);
                prev
            });
        if previous.as_deref() == Some(name.as_str()) {
            cx.notify();
            return;
        }
        let optimistic = name.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let gid_bg = gid.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::UpdateGroup {
                            group_id: gid_bg,
                            name: Some(name),
                            description: None,
                            bot_ids: None,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::Group(updated)) => {
                        if let Some(s) = this
                            .sidebar_groups
                            .iter_mut()
                            .find(|s| s.host == host_owned && s.group.id == gid)
                        {
                            s.group = updated.clone();
                        }
                        if let Some(entry) = this.bot_ui.hosts.get_mut(&host_owned)
                            && let Some(s) = entry.groups.iter_mut().find(|s| s.group.id == gid)
                        {
                            s.group = updated;
                        }
                        this.invalidate_group_room(&host_owned, &gid);
                        this.refresh_bots_for_host(&host_owned, true, cx);
                    }
                    Ok(_) => {
                        this.refresh_bots_for_host(&host_owned, true, cx);
                    }
                    Err(err) => {
                        if let Some(prev) = previous
                            && let Some(s) = this
                                .sidebar_groups
                                .iter_mut()
                                .find(|s| s.host == host_owned && s.group.id == gid)
                            && s.group.name == optimistic
                        {
                            s.group.name = prev;
                        }
                        this.refresh_bots_for_host(&host_owned, true, cx);
                        this.notice = Some(err);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(in crate::shell) fn confirm_delete_group_on(
        &mut self,
        host: &HostId,
        group_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(host) else {
            return;
        };
        let host_owned = host.clone();
        self.bot_ui.delete_group = None;
        self.sidebar_groups
            .retain(|s| !(s.host == host_owned && s.group.id == group_id));
        if let Some(entry) = self.bot_ui.hosts.get_mut(&host_owned) {
            entry.groups.retain(|s| s.group.id != group_id);
        }
        self.close_panes_for_group(&host_owned, &group_id, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { call_checked(&rpc, &BotRpcRequest::DeleteGroup { group_id }) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = res {
                    this.notice = Some(err);
                    cx.notify();
                }
                this.refresh_bots_for_host(&host_owned, true, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_group_delete(
        &mut self,
        host: &HostId,
        group: &BotGroupChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = ui::palette(cx);
        let focus = self
            .bot_ui
            .delete_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        if !focus.contains_focused(window, cx) {
            window.focus(&focus, cx);
        }
        let host_owned = host.clone();
        let id = group.id.clone();
        let card = div()
            .id("bot-group-delete-dialog")
            .debug_selector(|| "bot-group-delete-dialog".into())
            .role(gpui::Role::AlertDialog)
            .aria_label(text::GRP_DELETE_TITLE.format(&[("name", &group.name)]))
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.bot_ui.delete_group = None;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text::GRP_DELETE_TITLE.format(&[("name", &group.name)])),
            )
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::GRP_DELETE_BODY.get()),
            )
            .child(
                ui::controls()
                    .justify_end()
                    .child(
                        ui::ghost("bot-group-delete-cancel", text::CANCEL.get()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.bot_ui.delete_group = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Button::new("bot-group-delete-confirm")
                            .small()
                            .danger()
                            .label(text::GRP_DELETE_BTN.get())
                            .debug_selector(|| "bot-group-delete-confirm".into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_delete_group_on(&host_owned, id.clone(), cx);
                            })),
                    ),
            );
        ui::modal_backdrop(
            window.viewport_size(),
            div()
                .id("bot-group-delete-backdrop")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.bot_ui.delete_group = None;
                        cx.notify();
                    }),
                )
                .child(ui::modal_focus_scope(card, &focus)),
        )
        .into_any_element()
    }
}
