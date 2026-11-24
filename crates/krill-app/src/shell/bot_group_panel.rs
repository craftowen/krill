// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Multi-bot group right sidebar (`BotGroupItemsPanel`, `BotGroupPanelHeader`,
//! `BotItemsFilterButton`): clickable face cluster with member roster dropdown, status line,
//! multi-bot/chat-scope/needs-you filter popover, settings gear, more menu, and merged items.

use gpui::{AnyElement, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_app::bots::{GroupChatScope, GroupItemsFilter, SidebarBot, group_faces};
use krill_app::i18n::catalog::bots as text;
use krill_app::ui;

use super::Shell;
use super::bot_section::SidebarGroup;

fn menu_heading(label: impl Into<SharedString>) -> PopupMenuItem {
    let label = label.into();
    PopupMenuItem::element(move |_, cx| {
        let p = ui::palette(cx);
        div()
            .py_0p5()
            .text_size(ui::TEXT_XS)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.muted_foreground)
            .child(label.clone())
    })
    .disabled(true)
}

impl Shell {
    /// Returns the group summary if `pane` hosts a Group Chat room page.
    pub(super) fn group_for_pane(&self, pane: krill_app::layout::PaneId) -> Option<&SidebarGroup> {
        let target = self.layout.group_pane(pane)?;
        let host = self.layout.host(pane);
        self.sidebar_groups
            .iter()
            .find(|s| &s.host == host && s.group.id == target.group_id)
    }

    /// Returns the focused Group Chat room page, if any (`BotGroupItemsPanel` target).
    pub(super) fn focused_bot_group(&self) -> Option<&SidebarGroup> {
        let pane = self.layout.focused()?;
        self.group_for_pane(pane)
    }

    pub(super) fn render_bot_group_panel(
        &mut self,
        summary: &SidebarGroup,
        width: f32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = width;
        let p = ui::palette(cx);
        let host = summary.host.clone();
        let group = &summary.group;
        let thread_ids: Vec<String> = summary
            .threads
            .iter()
            .map(|t| t.thread_id.clone())
            .collect();

        // Switching the shared panel view to a group clears single-bot tracking so returning
        // to the previously shown bot always invokes `show_in_panel` again.
        self.bot_ui.panel.shown = None;
        let view = self.bot_panel_view_on(&host, cx).inspect(|view| {
            if std::mem::take(&mut self.bot_ui.panel.stale) {
                view.update(cx, krill_app::bots::View::reload);
            }
            let mut states = std::collections::BTreeMap::new();
            for bid in &group.bot_ids {
                if let Some(bot) = self
                    .sidebar_bots
                    .iter()
                    .find(|b| b.host == host && &b.id == bid)
                {
                    for s in &bot.sessions {
                        if let Some(sess) = self.sidebar.read(cx).session_on(&host, s.session_id) {
                            states.insert(s.session_id, sess.status.state);
                        }
                    }
                }
            }
            for t in &summary.active_turns {
                if let Some(sess) = self.sidebar.read(cx).session_on(&host, t.session_id) {
                    states.insert(t.session_id, sess.status.state);
                }
            }
            view.update(cx, |v, cx| {
                v.set_session_states(states, cx);
                v.show_group_in_panel(&group.id, &group.bot_ids, &thread_ids, cx);
            });
        });

        let item_open = view.as_ref().is_some_and(|v| v.read(cx).home_item_open());

        div()
            .id("bot-group-panel")
            .debug_selector(|| "bot-group-panel".into())
            .size_full()
            .min_w_0()
            .bg(p.sidebar)
            .text_color(p.sidebar_foreground)
            .flex()
            .flex_col()
            .child(Self::render_bot_group_panel_top_bar(p, cx))
            .when(!item_open, |d| {
                d.child(self.render_bot_group_panel_header(summary, view.as_ref(), p, cx))
            })
            .child(div().flex_1().min_h_0().children(view))
            .into_any_element()
    }

    fn render_bot_group_panel_top_bar(p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .h(ui::TAB_BAR_HEIGHT)
            .px_2()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(p.sidebar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.sidebar_foreground)
                    .child(ui::icons::svg_box(
                        ui::icons::USERS_SVG,
                        ui::ICON_SM,
                        p.muted_foreground,
                    ))
                    .child(text::GRP_PANEL_TITLE.get()),
            )
            .child(
                Button::new("bot-group-panel-collapse")
                    .debug_selector(|| "bot-group-panel-collapse".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelRightClose)
                    .tooltip(krill_app::i18n::catalog::diff::COLLAPSE.get())
                    .accessibility_label(krill_app::i18n::catalog::diff::COLLAPSE.get())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(cx))),
            )
    }

    fn render_bot_group_panel_header(
        &self,
        summary: &SidebarGroup,
        view: Option<&Entity<krill_app::bots::View>>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let host = summary.host.clone();
        let group = &summary.group;
        let host_bots: Vec<SidebarBot> = self
            .sidebar_bots
            .iter()
            .filter(|b| b.host == host)
            .cloned()
            .collect();
        let members: Vec<SidebarBot> = group
            .bot_ids
            .iter()
            .filter_map(|id| host_bots.iter().find(|b| &b.id == id).cloned())
            .collect();
        let need_count: usize = members.iter().map(|b| b.attention).sum();
        let working_count: usize = {
            let session_working: usize = members.iter().map(SidebarBot::working).sum();
            session_working.max(summary.active_turns.len())
        };
        let status_text = match (need_count, working_count) {
            (n, m) if n > 0 && m > 0 => format!(
                "{} · {}",
                if n == 1 {
                    text::SB_NEEDS_YOU_ONE.format(&[("n", &n.to_string())])
                } else {
                    text::SB_NEEDS_YOU_MANY.format(&[("n", &n.to_string())])
                },
                text::SB_WORKING.format(&[("n", &m.to_string())])
            ),
            (n, _) if n > 0 => {
                if n == 1 {
                    text::SB_NEEDS_YOU_ONE.format(&[("n", &n.to_string())])
                } else {
                    text::SB_NEEDS_YOU_MANY.format(&[("n", &n.to_string())])
                }
            }
            (_, m) if m > 0 => text::SB_WORKING.format(&[("n", &m.to_string())]),
            _ => text::BP_IDLE.get().to_owned(),
        };

        let face_members = group_faces::resolve_members(&group.bot_ids, &host_bots, &|id| {
            self.bot_ui
                .avatars
                .get(&(host.clone(), id.clone()))
                .and_then(|(_, img)| img.clone())
        });
        let faces = group_faces::render_group_faces(&face_members, 36., p.sidebar, p);

        let this_for_members = cx.weak_entity();
        let member_rows: Vec<(SidebarBot, String)> = members
            .iter()
            .map(|b| {
                let st = if b.paused {
                    text::PAUSED_BADGE.get().to_owned()
                } else if b.attention == 1 {
                    text::SB_NEEDS_YOU_ONE.format(&[("n", "1")])
                } else if b.attention > 1 {
                    text::SB_NEEDS_YOU_MANY.format(&[("n", &b.attention.to_string())])
                } else if b.working() > 0 {
                    text::SB_WORKING.format(&[("n", &b.working().to_string())])
                } else {
                    text::BP_IDLE.get().to_owned()
                };
                (b.clone(), st)
            })
            .collect();
        let members_count_label =
            text::GRP_PANEL_MEMBERS.format(&[("n", &member_rows.len().to_string())]);
        let faces_btn = Button::new("bot-group-panel-members-btn")
            .debug_selector(|| "bot-group-panel-members-btn".into())
            .ghost()
            .p_0()
            .size(px(36.))
            .rounded(px(9.))
            .tooltip(members_count_label.clone())
            .accessibility_label(members_count_label.clone())
            .child(faces)
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.item(menu_heading(members_count_label.clone()));
                for (bot, st) in &member_rows {
                    let bid = bot.id.clone();
                    let bhost = bot.host.clone();
                    let name = bot.name.clone();
                    let st = st.clone();
                    let this = this_for_members.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            let p = ui::palette(cx);
                            div()
                                .w_full()
                                .min_w(px(180.))
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.popover_foreground)
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(st.clone()),
                                )
                        })
                        .on_click(move |_, _, cx| {
                            this.update(cx, |s, cx| {
                                s.open_bot_home_on(&bhost, bid.clone(), None, cx);
                                if s.panel_collapsed {
                                    s.toggle_panel(cx);
                                }
                                s.bot_ui.panel.tab = crate::shell::bot_panel::BotPanelTab::Home;
                                s.bot_ui.panel.settings = None;
                                cx.notify();
                            })
                            .ok();
                        }),
                    );
                }
                menu
            });

        let filter_btn = view.map(|v| {
            let v = v.clone();
            let filter: GroupItemsFilter = v.read(cx).group_items_filter();
            let (by_bot, needs_you_count) = v.read(cx).group_filter_counts();
            let active_dot = filter.has_non_default_filters();
            let members_for_menu = members.clone();
            let threads_for_menu = summary.threads.clone();

            div()
                .relative()
                .flex_none()
                .child(
                    Button::new("bot-group-panel-filter")
                        .debug_selector(|| "bot-group-panel-filter".into())
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(gpui_kit_assets::IconName::ListFilter))
                        .tooltip(text::HM_FILTER_LABEL.get())
                        .accessibility_label(text::HM_FILTER_LABEL.get())
                        .dropdown_menu(move |mut menu, _, _| {
                            let v_all = v.clone();
                            let all_bots_checked = filter.bot_ids.is_empty();
                            menu = menu
                                .item(menu_heading(text::GRP_PANEL_FILTER_BOT.get()))
                                .item(
                                    PopupMenuItem::new(text::GRP_PANEL_FILTER_ALL_BOTS.get())
                                        .checked(all_bots_checked)
                                        .on_click(move |_, _, cx| {
                                            v_all.update(
                                                cx,
                                                krill_app::bots::View::clear_group_bot_filter,
                                            );
                                        }),
                                );
                            for m in &members_for_menu {
                                let bid = m.id.clone();
                                let v_bot = v.clone();
                                let checked = filter.bot_ids.contains(&bid);
                                let count = by_bot
                                    .iter()
                                    .find(|(id, _)| id == &bid)
                                    .map_or(0, |(_, c)| *c)
                                    .to_string();
                                let name = m.name.clone();
                                menu = menu.item(
                                    PopupMenuItem::element(move |_, cx| {
                                        let p = ui::palette(cx);
                                        div()
                                            .w_full()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(div().flex_1().truncate().child(name.clone()))
                                            .child(
                                                div()
                                                    .text_size(ui::TEXT_XS)
                                                    .text_color(p.muted_foreground)
                                                    .child(count.clone()),
                                            )
                                    })
                                    .checked(checked)
                                    .on_click(
                                        move |_, _, cx| {
                                            v_bot.update(cx, |v, cx| {
                                                v.toggle_group_filter_bot(bid.clone(), cx);
                                            });
                                        },
                                    ),
                                );
                            }
                            menu = menu
                                .separator()
                                .item(menu_heading(text::GRP_PANEL_FILTER_CHAT.get()));
                            let v_all_work = v.clone();
                            let v_this_group = v.clone();
                            menu = menu
                                .item(
                                    PopupMenuItem::new(text::GRP_PANEL_SCOPE_THIS_GROUP.get())
                                        .checked(filter.chat_scope == GroupChatScope::ThisGroup)
                                        .on_click(move |_, _, cx| {
                                            v_this_group.update(cx, |v, cx| {
                                                v.set_group_chat_scope(
                                                    GroupChatScope::ThisGroup,
                                                    cx,
                                                );
                                            });
                                        }),
                                )
                                .item(
                                    PopupMenuItem::new(text::GRP_PANEL_SCOPE_ALL_WORK.get())
                                        .checked(filter.chat_scope == GroupChatScope::AllTheirWork)
                                        .on_click(move |_, _, cx| {
                                            v_all_work.update(cx, |v, cx| {
                                                v.set_group_chat_scope(
                                                    GroupChatScope::AllTheirWork,
                                                    cx,
                                                );
                                            });
                                        }),
                                );
                            for thr in &threads_for_menu {
                                let tid = thr.thread_id.clone();
                                let title = thr.title.clone();
                                let v_thr = v.clone();
                                let checked =
                                    filter.chat_scope == GroupChatScope::Thread(tid.clone());
                                menu =
                                    menu.item(PopupMenuItem::new(title).checked(checked).on_click(
                                        move |_, _, cx| {
                                            v_thr.update(cx, |v, cx| {
                                                v.set_group_chat_scope(
                                                    GroupChatScope::Thread(tid.clone()),
                                                    cx,
                                                );
                                            });
                                        },
                                    ));
                            }
                            let v_needs = v.clone();
                            let needs_count = needs_you_count.to_string();
                            menu = menu.separator().item(
                                PopupMenuItem::element(move |_, cx| {
                                    let p = ui::palette(cx);
                                    div()
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div().flex_1().child(text::HM_FILTER_NEEDS_YOU.get()),
                                        )
                                        .child(
                                            div()
                                                .text_size(ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(needs_count.clone()),
                                        )
                                })
                                .checked(filter.needs_you_only)
                                .on_click(move |_, _, cx| {
                                    v_needs.update(cx, |v, cx| {
                                        v.set_group_needs_you_only(!filter.needs_you_only, cx);
                                    });
                                }),
                            );
                            if filter.has_non_default_filters() {
                                let v_clear = v.clone();
                                menu = menu.separator().item(
                                    PopupMenuItem::new(text::GRP_PANEL_CLEAR_FILTERS.get())
                                        .on_click(move |_, _, cx| {
                                            v_clear.update(
                                                cx,
                                                krill_app::bots::View::clear_group_filters,
                                            );
                                        }),
                                );
                            }
                            menu
                        }),
                )
                .when(active_dot, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .right(px(4.))
                            .size(px(6.))
                            .rounded_full()
                            .bg(p.foreground),
                    )
                })
        });

        let this_more = cx.weak_entity();
        let host_edit = host.clone();
        let host_more = host;
        let group_edit = group.clone();
        let group_more = group.clone();
        let group_thread = group.clone();
        let group_del = group.clone();
        let reply_aud = summary.reply_audience.clone();
        let reply_aud_more = summary.reply_audience.clone();

        div()
            .id("bot-group-panel-header")
            .debug_selector(|| "bot-group-panel-header".into())
            .flex_none()
            .pl_3()
            .pr_2()
            .pt_3p5()
            .pb_2()
            .flex()
            .items_center()
            .gap_2()
            .child(faces_btn)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_MD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.sidebar_foreground)
                            .child(group.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .text_color(if need_count > 0 {
                                p.sidebar_foreground
                            } else {
                                p.muted_foreground
                            })
                            .child(status_text),
                    ),
            )
            .children(filter_btn)
            .child(
                Button::new("bot-group-panel-settings")
                    .debug_selector(|| "bot-group-panel-settings".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Settings)
                    .tooltip(text::GRP_MANAGE_MEMBERS.get())
                    .accessibility_label(text::GRP_MANAGE_MEMBERS.get())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_manage_group_dialog_on(
                            &host_edit,
                            group_edit.clone(),
                            reply_aud.clone(),
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("bot-group-panel-more")
                    .debug_selector(|| "bot-group-panel-more".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Ellipsis)
                    .tooltip(text::BP_MORE.get())
                    .accessibility_label(text::BP_MORE.get())
                    .dropdown_menu(move |menu, _, _| {
                        let t1 = this_more.clone();
                        let t2 = this_more.clone();
                        let t3 = this_more.clone();
                        let h1 = host_more.clone();
                        let h2 = host_more.clone();
                        let h3 = host_more.clone();
                        let g1 = group_more.clone();
                        let g2 = group_thread.clone();
                        let g3 = group_del.clone();
                        let aud = reply_aud_more.clone();
                        let new_thread_label = text::GRP_NEW_THREAD.format(&[("name", &g2.name)]);
                        menu.item(
                            PopupMenuItem::new(text::GRP_MANAGE_MEMBERS_MENU.get())
                                .icon(gpui_component::Icon::default().data(ui::icons::USERS_SVG))
                                .on_click(move |_, window, cx| {
                                    t1.update(cx, |s, cx| {
                                        s.open_manage_group_dialog_on(
                                            &h1,
                                            g1.clone(),
                                            aud.clone(),
                                            window,
                                            cx,
                                        );
                                    })
                                    .ok();
                                }),
                        )
                        .item(
                            PopupMenuItem::new(new_thread_label)
                                .icon(IconName::Plus)
                                .on_click(move |_, window, cx| {
                                    t2.update(cx, |s, cx| {
                                        s.create_and_open_group_thread_on(
                                            &h2,
                                            g2.id.clone(),
                                            window,
                                            cx,
                                        );
                                    })
                                    .ok();
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(text::GRP_DELETE_MENU.get())
                                .icon(IconName::Delete)
                                .on_click(move |_, _, cx| {
                                    t3.update(cx, |s, cx| {
                                        s.bot_ui.delete_group = Some((h3.clone(), g3.clone()));
                                        cx.notify();
                                    })
                                    .ok();
                                }),
                        )
                    }),
            )
    }
}
