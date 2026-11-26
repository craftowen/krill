// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! A bot's chats under its row (Orca `BotSidebarChats`): Home Chat, then its other chats, each
//! a 24 px row whose chevron unfolds the workers it started (`AttachedWorktreeCards`, compact
//! lines), capped with "Show N more".

use gpui::{Context, FontWeight, SharedString, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::{IconName, Sizable as _};
use krill_app_core::i18n::catalog::bots as text;
use krill_app_core::ui;
use krill_bots_ui::bots::SidebarBot;
use krill_proto::SessionId;
use krill_proto::control::AgentState;

use super::Shell;
use super::now_secs;
use super::rows::{BotFacts, ChatFacts, WorkerFacts};

/// Orca `CHAT_TREE_CAPS.finishedShown` / `.step`.
const FINISHED_SHOWN: usize = 2;
const STEP: usize = 8;
/// Orca `BOT_WORKSPACE_LINE_INDENT_NEW_CARD_STYLE` (its default card style): the chat glyphs
/// land under the bot's picture.
const LINE_INDENT: f32 = 27.;

impl Shell {
    pub(super) fn render_bot_sidebar_chats(
        &self,
        bot: &SidebarBot,
        facts: &BotFacts,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut col = div().ml(px(LINE_INDENT)).flex().flex_col().gap_1();
        for chat in &facts.chats {
            col = col.child(self.render_bot_chat(bot, chat, p, cx));
        }
        for worker in &facts.stray {
            col = col.child(self.render_worker_line(
                &bot.host,
                worker,
                Some(text::SB_FROM_CLOSED_CHAT.get()),
                p,
                cx,
            ));
        }
        col
    }

    fn render_bot_chat(
        &self,
        bot: &SidebarBot,
        chat: &ChatFacts,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let foldable = !chat.workers.is_empty();
        let host = bot.host.clone();
        let chat_key = (host.clone(), chat.session);
        let open = foldable && !self.bot_ui.folded_chats.contains(&chat_key);
        let live = chat.workers.iter().filter(|w| w.live).count();
        let waiting = chat.workers.iter().filter(|w| w.waiting()).count();
        let running = live - waiting;
        // Orca `chatTrailing`: folded, what runs or waits; else when it last moved, except
        // while it replies (the spinner says enough).
        let trailing: Option<String> = if foldable && !open && live > 0 {
            Some(
                [
                    (waiting > 0)
                        .then(|| text::SB_WAITING_COUNT.format(&[("n", &waiting.to_string())])),
                    (running > 0)
                        .then(|| text::SB_RUNNING_COUNT.format(&[("n", &running.to_string())])),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
            )
        } else if chat.working && (!foldable || open) {
            None
        } else {
            (chat.at_ms > 0).then(|| ui::format_short_time_ago_secs(chat.at_ms / 1000, now_secs()))
        };
        let display_title: SharedString = if chat.home {
            text::SB_HOME_CHAT_EXACT.get().into()
        } else {
            chat.title.clone()
        };
        let sel = SharedString::from(format!("bot-chat-{}", chat.session.0));
        let elem_id = SharedString::from(if host.is_local() {
            format!("bot-chat-{}", chat.session.0)
        } else {
            format!("bot-chat-{}-{}", host.as_key(), chat.session.0)
        });
        let click_host = host.clone();
        let bot_id = bot.id.clone();
        let session = chat.session;
        let home = chat.home;
        let this = cx.weak_entity();
        let menu_host = host.clone();
        let menu_bot = bot.id.clone();
        let more_this = cx.weak_entity();
        let more_host = host.clone();
        let more_bot = bot.id.clone();
        let group = SharedString::from(if host.is_local() {
            format!("bot-chat-group-{}", chat.session.0)
        } else {
            format!("bot-chat-group-{}-{}", host.as_key(), chat.session.0)
        });
        let row = div()
            .id(elem_id)
            .debug_selector(move || sel.to_string())
            .role(gpui::Role::Button)
            .tab_index(0)
            .focus_visible(|d| d.border_1().border_color(p.ring))
            .aria_label(display_title.clone())
            .aria_expanded(open)
            .group(group.clone())
            // Orca: `h-6 gap-1.5 rounded-md (6px) pr-0.5 pl-1 text-xs (12px) text-muted-foreground`.
            .h(px(24.))
            .min_w_0()
            .pl_1()
            .pr_0p5()
            .rounded(ui::RADIUS_SM)
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .cursor_pointer()
            .when(chat.active, |d| d.bg(p.worktree_sidebar_accent))
            .when(!chat.active, |d| {
                d.hover(|d| d.bg(p.worktree_sidebar_accent))
            })
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                if e.click_count() <= 1 {
                    this.open_bot_session_on(&click_host, &bot_id, session, cx);
                }
            }))
            .child(self.chat_status_slot(chat, p, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(p.foreground)
                    .when(chat.unread || chat.active, |d| {
                        d.font_weight(FontWeight::SEMIBOLD)
                    })
                    .child(display_title.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.6))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .child({
                        let age_sel = format!("bot-chat-age-{}", chat.session.0);
                        let more_sel = format!("bot-chat-more-{}", chat.session.0);
                        let more_btn_id = if host.is_local() {
                            more_sel.clone()
                        } else {
                            format!("bot-chat-more-btn-{}-{}", host.as_key(), chat.session.0)
                        };
                        let more_dbg = more_sel;
                        div()
                            .relative()
                            .min_w(px(24.))
                            .flex()
                            .items_center()
                            .justify_end()
                            .child(
                                div()
                                    .debug_selector(move || age_sel.clone())
                                    .min_w(px(24.))
                                    .truncate()
                                    .text_right()
                                    .text_size(ui::TEXT_3XS)
                                    .line_height(gpui::relative(1.))
                                    .text_color(p.muted_foreground)
                                    .group_hover(group.clone(), gpui::Styled::invisible)
                                    .children(trailing),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .right_0()
                                    .invisible()
                                    .group_hover(group, gpui::Styled::visible)
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .child(
                                        Button::new(SharedString::from(more_btn_id))
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                gpui_component::Icon::new(IconName::Ellipsis)
                                                    .size(px(14.)),
                                            )
                                            .accessibility_label(text::SB_CHAT_ACTIONS.get())
                                            .debug_selector(move || more_dbg.clone())
                                            .dropdown_menu(move |menu, _, _| {
                                                bot_chat_menu(
                                                    menu,
                                                    more_this.clone(),
                                                    more_host.clone(),
                                                    more_bot.clone(),
                                                    session,
                                                    home,
                                                )
                                            }),
                                    ),
                            )
                    })
                    .when(foldable, |d| {
                        let fold_host = host.clone();
                        let fold = chat.session;
                        let name = display_title.to_string();
                        let fold_id = if host.is_local() {
                            format!("bot-chat-fold-{}", chat.session.0)
                        } else {
                            format!("bot-chat-fold-{}-{}", host.as_key(), chat.session.0)
                        };
                        d.child(
                            div()
                                .id(SharedString::from(fold_id))
                                .debug_selector({
                                    let sel = format!("bot-chat-fold-{}", chat.session.0);
                                    move || sel.clone()
                                })
                                .role(gpui::Role::Button)
                                .aria_label(if open {
                                    text::SB_FOLD.format(&[("name", &name)])
                                } else {
                                    text::SB_UNFOLD.format(&[("name", &name)])
                                })
                                .aria_expanded(open)
                                .size_4()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(ui::RADIUS_XS)
                                .text_color(p.muted_foreground)
                                .hover(|d| d.text_color(p.foreground))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let key = (fold_host.clone(), fold);
                                    if !this.bot_ui.folded_chats.remove(&key) {
                                        this.bot_ui.folded_chats.insert(key);
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    gpui_component::Icon::new(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .size(px(14.)),
                                ),
                        )
                    }),
            )
            // Orca `BotChatRowMenu`: Open, then Delete chat (not Home Chat).
            .context_menu(move |menu, _, _| {
                bot_chat_menu(
                    menu,
                    this.clone(),
                    menu_host.clone(),
                    menu_bot.clone(),
                    session,
                    home,
                )
            });
        let mut col = div().flex().flex_col().gap_1().child(row);
        if open {
            let steps = self.bot_ui.chat_steps.get(&chat_key).copied().unwrap_or(0);
            let cap = live + FINISHED_SHOWN + steps * STEP;
            let shown = chat.workers.len().min(cap);
            let remaining = chat.workers.len() - shown;
            let mut lines = div().ml_3().flex().flex_col().gap_1();
            for (i, w) in chat.workers.iter().enumerate() {
                if i < shown || w.active {
                    lines = lines.child(self.render_worker_line(&host, w, None, p, cx));
                }
            }
            let fold = chat.session;
            let more_host = host.clone();
            let more = (remaining > 0).then(|| {
                link(
                    if host.is_local() {
                        format!("bot-chat-more-{}", chat.session.0)
                    } else {
                        format!("bot-chat-more-{}-{}", host.as_key(), chat.session.0)
                    },
                    format!("bot-chat-more-{}", chat.session.0),
                    text::SB_SHOW_MORE.format(&[("n", &remaining.min(STEP).to_string())]),
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    *this
                        .bot_ui
                        .chat_steps
                        .entry((more_host.clone(), fold))
                        .or_default() += 1;
                    cx.notify();
                }))
            });
            let fewer_host = host.clone();
            let fewer = (steps > 0).then(|| {
                link(
                    if host.is_local() {
                        format!("bot-chat-fewer-{}", chat.session.0)
                    } else {
                        format!("bot-chat-fewer-{}-{}", host.as_key(), chat.session.0)
                    },
                    format!("bot-chat-fewer-{}", chat.session.0),
                    text::SB_SHOW_FEWER.get().to_owned(),
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.bot_ui.chat_steps.remove(&(fewer_host.clone(), fold));
                    cx.notify();
                }))
            });
            if more.is_some() || fewer.is_some() {
                lines = lines.child(
                    div()
                        .pr_2()
                        .pb_1()
                        .pl_1()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x_1()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .children(more)
                        .when(remaining > 0 && steps > 0, |d| d.child("·"))
                        .children(fewer),
                );
            }
            col = col.child(lines);
        }
        col
    }

    /// Orca `BotChatStatusSlot`: the chat's kind icon, with a corner badge while it is live
    /// (spinner replying, dot waiting) or stopped on its usage limit.
    fn chat_status_slot(
        &self,
        chat: &ChatFacts,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let icon = if chat.home {
            ui::icons::HOUSE_SVG
        } else {
            ui::icons::MESSAGE_SQUARE_SVG
        };
        let badge = if chat.working {
            Some(ui::status::status_glyph_slot(
                AgentState::Working,
                false,
                ui::status::Size::Sm,
                Some(&self.bot_ui.spinners),
                cx,
            ))
        } else if chat.needs_you {
            Some(div().size(px(7.)).rounded_full().bg(p.agent_question))
        } else {
            chat.stop
                .as_ref()
                .map(|_| ui::icons::svg_box(ui::icons::GAUGE_SVG, 8., p.warning))
        };
        let badge_bg = if chat.active {
            p.worktree_sidebar_accent
        } else {
            p.worktree_sidebar
        };
        div()
            .relative()
            .size_3()
            .flex_none()
            .child(ui::icons::svg_box(icon, ui::ICON_XS, p.muted_foreground))
            .when_some(badge, |d, badge| {
                d.child(
                    div()
                        .absolute()
                        .right(px(-3.))
                        .bottom(px(-3.))
                        .size(px(9.))
                        .rounded_full()
                        .bg(badge_bg)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(badge),
                )
            })
    }

    /// One worker as Orca's compact workspace line (`BotChatTreeWorkerRow` / `AttachedWorktreeCards`):
    /// 24 px height, state glyph, agent, title, inline note (`handed over` / `from closed chat`),
    /// repo badge, and right-aligned relative age.
    fn render_worker_line(
        &self,
        host: &krill_app_core::hosts::HostId,
        worker: &WorkerFacts,
        note: Option<&'static str>,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let sel = format!("bot-worker-{}", worker.key);
        let elem_id = if host.is_local() {
            sel.clone()
        } else {
            format!("bot-worker-{}-{}", host.as_key(), worker.key)
        };
        let click_host = host.clone();
        let session = worker.session;
        let state = if worker.live {
            worker.state
        } else {
            AgentState::Done
        };
        let age = (worker.at_ms > 0)
            .then(|| ui::format_short_time_ago_secs(worker.at_ms / 1000, now_secs()));
        div()
            .id(SharedString::from(elem_id))
            .debug_selector(move || sel.clone())
            .role(gpui::Role::Button)
            .aria_label(worker.title.clone())
            // Orca compact worker line: 24 px tall, 6 px radius, 12 px text.
            .h(px(24.))
            .min_w_0()
            .ml_1()
            .pl(px(6.))
            .pr_1p5()
            .rounded(ui::RADIUS_SM)
            .border_1()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(ui::TEXT_SM)
            .when(worker.active, |d| {
                d.bg(p.worktree_sidebar_active())
                    .border_color(p.worktree_sidebar_active_border())
            })
            .when(!worker.active, |d| {
                d.border_color(ui::TRANSPARENT)
                    .hover(|d| d.bg(p.worktree_sidebar_accent))
            })
            .when(session.is_some(), gpui::Styled::cursor_pointer)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(session) = session {
                    this.open_session_on(click_host.clone(), session, cx);
                }
            }))
            .child(ui::status::status_glyph_slot(
                state,
                worker.unread,
                ui::status::Size::Md,
                Some(&self.bot_ui.spinners),
                cx,
            ))
            .child(ui::status::agent_icon(
                &worker.agent,
                ui::ICON_XS,
                p.muted_foreground,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if worker.unread || worker.live {
                        p.foreground
                    } else {
                        p.foreground.opacity(0.8)
                    })
                    .when(worker.unread, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .child(worker.title.clone()),
            )
            .when_some(note, |d, note| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground.opacity(0.85))
                        .child(note),
                )
            })
            .when_some(worker.repo.clone(), |d, repo| {
                d.child(
                    div()
                        .flex_none()
                        .max_w(px(96.))
                        .h(px(16.))
                        .px_1()
                        .rounded(ui::RADIUS_XS)
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .text_size(ui::TEXT_3XS)
                        .text_color(p.muted_foreground)
                        .child(ui::icons::svg_box(
                            ui::icons::FOLDER_SVG,
                            10.,
                            p.muted_foreground,
                        ))
                        .child(div().min_w_0().truncate().child(repo)),
                )
            })
            .when_some(age, |d, age| {
                let age_sel = format!("bot-worker-age-{}", worker.key);
                d.child(
                    div()
                        .debug_selector(move || age_sel.clone())
                        .flex_none()
                        .text_size(ui::TEXT_3XS)
                        .line_height(gpui::relative(1.))
                        .text_color(p.muted_foreground)
                        .child(age),
                )
            })
    }

    /// Orca "Delete chat": ends that chat's session.
    pub(super) fn stop_bot_chat_on(
        &mut self,
        host: &krill_app_core::hosts::HostId,
        bot_id: krill_proto::bots::BotId,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(host) else {
            return;
        };
        let host_owned = host.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    krill_bots_ui::bots::rpc::call_checked(
                        &rpc,
                        &krill_proto::bots::BotRpcRequest::StopBotSession { bot_id, session_id },
                    )
                })
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
}

/// Orca `BotChatMenuItems`: shared by right-click context menu and hover `⋯` dropdown.
fn bot_chat_menu(
    menu: PopupMenu,
    this: gpui::WeakEntity<Shell>,
    host: krill_app_core::hosts::HostId,
    bot_id: krill_proto::bots::BotId,
    session: SessionId,
    home: bool,
) -> PopupMenu {
    let open_this = this.clone();
    let open_host = host.clone();
    let open_bot = bot_id.clone();
    let menu = menu.item(
        PopupMenuItem::new(text::SB_OPEN.get())
            .icon(gpui_component::Icon::default().data(ui::icons::ARROW_UP_RIGHT_SVG))
            .on_click(move |_, _, cx| {
                open_this
                    .update(cx, |s, cx| {
                        s.open_bot_session_on(&open_host, &open_bot, session, cx);
                    })
                    .ok();
            }),
    );
    if home {
        return menu;
    }
    menu.separator().item(
        PopupMenuItem::new(text::SB_DELETE_CHAT.get())
            .icon(gpui_component::Icon::default().data(ui::icons::TRASH_SVG))
            .on_click(move |_, _, cx| {
                this.update(cx, |s, cx| {
                    s.stop_bot_chat_on(&host, bot_id.clone(), session, cx);
                })
                .ok();
            }),
    )
}

/// Orca `BotChatPaging`'s links.
fn link(
    id: String,
    sel: String,
    label: String,
    p: &'static ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .debug_selector(move || sel.clone())
        .cursor_pointer()
        .whitespace_nowrap()
        .hover(|d| d.text_color(p.foreground))
        .child(label)
}
