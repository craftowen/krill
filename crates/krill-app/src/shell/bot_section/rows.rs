// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! One bot in the Bots section (Orca `BotSidebarEntry`/`BotRow`/`BotSidebarRow`): the 32 px
//! picture with its corner badge, name and age, the activity line (`BotRowActivityLine`), the
//! fold strip, the hover card (`BotHoverDetails`/`BotHoverNeedsYou`) and the right-click menu.

use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants as _;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::{IconName, Sizable as _};
use krill_app::bots::SidebarBot;
use krill_app::bots::sidebar_view::{self as view, LineChat, RowLine};
use krill_app::i18n::catalog::bots as text;
use krill_app::ui;
use krill_proto::SessionId;
use krill_proto::bots::BotId;
use krill_proto::control::AgentState;

use super::{Shell, ShellAction, now_secs};

/// The row picture: 36 px as Orca's sidebar draws it (measured on the reference screenshot; its
/// source says `size-8`, which its root scale renders at 36).
pub(super) const AVATAR: f32 = 36.;

/// Orca `BotRowStatus`: the avatar shows exceptions; working shows at the row's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowStatus {
    Working,
    Permission,
    Active,
    Inactive,
}

/// A chat, worker or run stopped on its usage limit (Orca `BotRowLimit`).
#[derive(Debug, Clone)]
pub(super) struct UsageStop {
    pub place: Option<String>,
    pub auto: bool,
    pub time: Option<String>,
}

impl UsageStop {
    /// Orca `usageLimitStopText`.
    pub fn text(&self) -> String {
        match (&self.time, self.auto) {
            (Some(t), true) => text::SB_LIMIT_CONTINUES.format(&[("time", t)]),
            (Some(t), false) => text::SB_LIMIT_HIT.format(&[("time", t)]),
            (None, _) => text::SB_LIMIT_HIT_NO_TIME.get().to_owned(),
        }
    }

    /// Orca `usageLimitStopTip`.
    pub fn tip(&self, agent: &str) -> String {
        let time = self.time.as_deref().unwrap_or("?");
        if self.auto {
            text::SB_LIMIT_CONTINUES_TIP.format(&[("time", time)])
        } else {
            text::SB_LIMIT_HIT_TIP.format(&[("agent", agent), ("time", time)])
        }
    }
}

/// One of the bot's chats as the sidebar shows it.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent row flags, as Orca's chat"
)]
#[derive(Debug, Clone)]
pub(super) struct ChatFacts {
    pub session: SessionId,
    pub home: bool,
    pub title: SharedString,
    pub working: bool,
    pub needs_you: bool,
    pub unread: bool,
    pub active: bool,
    /// When it last moved, ms.
    pub at_ms: u64,
    pub status: Option<String>,
    pub stop: Option<UsageStop>,
    pub workers: Vec<WorkerFacts>,
}

/// A worker one of its chats started (a krill orchestration dispatch).
#[derive(Debug, Clone)]
pub(super) struct WorkerFacts {
    pub key: String,
    pub session: Option<SessionId>,
    pub title: String,
    pub repo: Option<String>,
    pub agent: String,
    pub state: AgentState,
    /// Still running (or waiting): not finished.
    pub live: bool,
    pub unread: bool,
    pub active: bool,
    pub at_ms: u64,
}

impl WorkerFacts {
    pub fn waiting(&self) -> bool {
        self.live && matches!(self.state, AgentState::Waiting | AgentState::Blocked)
    }
}

/// Everything one bot row reads, gathered once per render.
pub(super) struct BotFacts {
    pub chats: Vec<ChatFacts>,
    /// Running workers of chats no longer listed (Orca "from closed chat").
    pub stray: Vec<WorkerFacts>,
    pub unread: bool,
    pub status: RowStatus,
    pub line: Option<RowLine>,
    pub limit: Option<UsageStop>,
    pub needs_you: bool,
    pub activity_ms: u64,
    pub active: bool,
}

fn ms_of_ns(ns: u64) -> u64 {
    ns / 1_000_000
}

impl Shell {
    pub(super) fn bot_facts(
        &self,
        bot: &SidebarBot,
        focused: Option<SessionId>,
        cx: &gpui::App,
    ) -> BotFacts {
        let host = &self.launch_host;
        let sidebar = self.sidebar.read(cx);
        let hide_sleeping = self.bot_view(cx).hide_sleeping_workers;
        let local = host.is_local();
        let orch = &self.orch_snapshot;
        let worker_of = |d: &krill_proto::orchestration::Dispatch| -> WorkerFacts {
            let session = d
                .session_id
                .map(SessionId)
                .filter(|_| local)
                .and_then(|id| sidebar.session_on(host, id));
            let path = session
                .and_then(|s| s.worktree.as_deref().or(s.cwd.as_deref()))
                .map_or_else(
                    || std::path::PathBuf::from(&d.cwd),
                    std::path::Path::to_path_buf,
                );
            let title = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| d.worker_name.clone());
            let repo = sidebar
                .repo_for_worktree_on(host, &path)
                .and_then(|r| r.file_name().map(|n| n.to_string_lossy().into_owned()));
            let live = d.status == krill_proto::orchestration::DispatchStatus::Active
                && session.is_some_and(|s| !s.is_exited());
            WorkerFacts {
                key: d.id.clone(),
                session: session.map(|s| s.id),
                title,
                repo,
                agent: session
                    .map(|s| s.status.agent.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| d.agent.clone()),
                state: session.map_or(AgentState::Unknown, |s| s.status.state),
                live,
                unread: session.is_some_and(|s| sidebar.is_unread(s.id)),
                active: session.is_some_and(|s| focused == Some(s.id)),
                at_ms: session
                    .map(|s| ms_of_ns(s.status.since_unix_ns))
                    .filter(|&at| at > 0)
                    .unwrap_or(d.ended_at_ms.unwrap_or(d.started_at_ms)),
            }
        };
        let workers_of = |coordinator: SessionId| -> Vec<WorkerFacts> {
            if !local {
                return Vec::new();
            }
            let mut out: Vec<WorkerFacts> = orch
                .runs
                .iter()
                .filter(|r| r.coordinator_session_id == Some(coordinator.0))
                .flat_map(|r| orch.dispatches.iter().filter(move |d| d.run_id == r.id))
                .map(worker_of)
                .filter(|w| !hide_sleeping || w.live || w.active)
                .collect();
            // Orca: a worker waiting on the owner leads, then running ones, then newest.
            out.sort_by_key(|w| (!w.waiting(), !w.live, std::cmp::Reverse(w.at_ms)));
            out
        };
        let chats: Vec<ChatFacts> = bot
            .sessions
            .iter()
            .map(|s| {
                let session = sidebar.session_on(host, s.session_id);
                let state = session.map(|x| x.status.state);
                let working = s.working || state == Some(AgentState::Working);
                let needs_you = matches!(state, Some(AgentState::Waiting | AgentState::Blocked));
                let home = bot
                    .home_session()
                    .is_some_and(|h| h.session_id == s.session_id);
                ChatFacts {
                    session: s.session_id,
                    home,
                    title: crate::shell::bot_panel::chat_title(bot, s),
                    working,
                    needs_you,
                    unread: sidebar.is_unread(s.session_id),
                    active: focused == Some(s.session_id),
                    at_ms: session
                        .map(|x| ms_of_ns(x.status.since_unix_ns))
                        .filter(|&at| at > 0)
                        .unwrap_or(s.created_at_ms),
                    status: session.and_then(|x| status_text(x, &bot.name)),
                    stop: self.chat_usage_stop(&(host.clone(), s.session_id), cx).map(
                        |(auto, time)| UsageStop {
                            place: None,
                            auto,
                            time,
                        },
                    ),
                    workers: workers_of(s.session_id),
                }
            })
            .collect();
        // Workers of this bot's closed chats that still run.
        let listed: std::collections::HashSet<u64> = chats.iter().map(|c| c.session.0).collect();
        let stray: Vec<WorkerFacts> = if local {
            orch.runs
                .iter()
                .filter_map(|r| r.coordinator_session_id.map(|sid| (r, sid)))
                .filter(|(_, sid)| !listed.contains(sid))
                .filter(|(_, sid)| {
                    sidebar
                        .bot_session_meta_on(host, SessionId(*sid))
                        .is_some_and(|m| m.bot_id == bot.id)
                })
                .flat_map(|(r, _)| orch.dispatches.iter().filter(move |d| d.run_id == r.id))
                .map(worker_of)
                .filter(|w| w.live || w.active)
                .collect()
        } else {
            Vec::new()
        };
        let workers = chats.iter().flat_map(|c| &c.workers).chain(&stray);
        let waiting_workers = workers.clone().filter(|w| w.waiting()).count();
        let working_workers = workers
            .clone()
            .filter(|w| w.live && !w.waiting() && w.state == AgentState::Working)
            .count();
        let line_chats: Vec<LineChat> = chats
            .iter()
            .map(|c| LineChat {
                place: (!c.home).then(|| c.title.to_string()),
                working: c.working,
                needs_you: c.needs_you,
                status: c.status.clone(),
                updated_at: c.at_ms,
            })
            .collect();
        let line = view::select_line(
            bot.paused,
            &line_chats,
            bot.attention + waiting_workers,
            working_workers,
        );
        let busy =
            chats.iter().any(|c| c.working || c.needs_you) || waiting_workers + working_workers > 0;
        let live_line = matches!(
            line,
            Some(
                RowLine::Paused
                    | RowLine::Replying(_)
                    | RowLine::NeedYouElsewhere(_)
                    | RowLine::WorkingElsewhere(_)
            )
        );
        let limit = (!busy && !live_line)
            .then(|| {
                chats.iter().find_map(|c| {
                    c.stop.clone().map(|mut s| {
                        s.place = Some(c.title.to_string());
                        s
                    })
                })
            })
            .flatten();
        let status = if chats.iter().any(|c| c.needs_you) {
            RowStatus::Permission
        } else if chats.iter().any(|c| c.working) {
            RowStatus::Working
        } else if bot.paused {
            RowStatus::Inactive
        } else {
            RowStatus::Active
        };
        let activity_ms = chats
            .iter()
            .map(|c| c.at_ms)
            .chain(workers.clone().map(|w| w.at_ms))
            .chain(std::iter::once(bot.updated_at_ms))
            .max()
            .unwrap_or(0);
        BotFacts {
            unread: chats.iter().any(|c| c.unread),
            active: chats.iter().any(|c| c.active),
            needs_you: chats.iter().any(|c| c.needs_you) || waiting_workers > 0,
            status,
            line,
            limit,
            activity_ms,
            chats,
            stray,
        }
    }

    /// The whole section, placed between the top navigation and Projects.
    pub(in crate::shell) fn render_bots_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = ui::palette(cx);
        self.bot_ui.spinners.clear();
        let mut section = div()
            .id("bots-section")
            .debug_selector(|| "bots-section".into())
            .role(gpui::Role::Group)
            .aria_label(krill_app::i18n::catalog::sidebar::NAV_BOTS.get())
            .flex_none()
            // Orca caps the section at 40% of the sidebar; the bot list scrolls inside it.
            .max_h(gpui::relative(0.4))
            // Orca SidebarBotsSplit: `border-b pb-2`, the Bots / Projects divider.
            .border_b_1()
            .border_color(p.worktree_sidebar_border)
            .pb_2()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .child(self.render_bots_header(p, cx));
        if self.bot_ui.collapsed {
            return section.into_any_element();
        }
        if self.sidebar_bots.is_empty() {
            // Until the list arrives, show nothing rather than an invitation to make a first bot.
            if self.bot_ui.loaded {
                section = section.child(Self::render_bots_empty(p, cx));
            }
            return section.into_any_element();
        }
        let focused = self
            .focused_session()
            .filter(|(host, _)| *host == self.launch_host)
            .map(|(_, s)| s);
        let facts: Vec<BotFacts> = self
            .sidebar_bots
            .iter()
            .map(|b| self.bot_facts(b, focused, cx))
            .collect();
        let arrange: Vec<view::ArrangeFacts<'_>> = self
            .sidebar_bots
            .iter()
            .zip(&facts)
            .map(|(b, f)| view::ArrangeFacts {
                name: &b.name,
                paused: b.paused,
                needs_you: f.needs_you,
                activity_at: f.activity_ms,
            })
            .collect();
        let order = view::arrange(&arrange, self.bot_view(cx));
        if let Some(search) = self.render_bot_search(&order, p, cx) {
            let searching = !self.bot_ui.query.trim().is_empty();
            section = section.child(search);
            if searching {
                return section.into_any_element();
            }
        }
        let mut list = div()
            .id("bots-section-list")
            .debug_selector(|| "bots-section-list".into())
            .role(gpui::Role::List)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            // Orca: `pb-1 pl-1`; rows carry their own `ml-1 pr-1.5`.
            .pb_1()
            .pl_1()
            .pr_1()
            .flex()
            .flex_col();
        if order.is_empty() {
            list = list.child(
                div()
                    .id("bots-all-hidden")
                    .debug_selector(|| "bots-all-hidden".into())
                    .px_4()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::SBO_ALL_HIDDEN.get()),
            );
        }
        for i in order {
            list = list.child(self.render_bot_entry(&self.sidebar_bots[i], &facts[i], p, cx));
        }
        section.child(list).into_any_element()
    }

    /// Spinners for working bot chats (drawn above the section so it isn't re-laid out per tick).
    pub(in crate::shell) fn bots_spinner_overlay(&self) -> impl IntoElement {
        ui::status::spinner_overlay(&self.bot_ui.spinners)
    }

    /// Orca `BotSidebarEntry`: the row, then (unfolded) its chats and their workers.
    fn render_bot_entry(
        &self,
        bot: &SidebarBot,
        facts: &BotFacts,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let folded = self.bot_ui.folded.contains(&bot.id);
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(self.render_bot_row(bot, facts, folded, p, cx))
            .when(!folded, |d| {
                d.child(
                    div()
                        .when(bot.paused, |d| d.opacity(0.6))
                        .child(self.render_bot_sidebar_chats(bot, facts, p, cx)),
                )
            })
            .into_any_element()
    }

    fn render_bot_row(
        &self,
        bot: &SidebarBot,
        facts: &BotFacts,
        folded: bool,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row_id = format!("bot-row-{}", bot.id.as_str());
        let row_sel = row_id.clone();
        let group = SharedString::from(format!("bot-row-group-{}", bot.id.as_str()));
        let open_id = bot.id.clone();
        let renaming = self
            .bot_ui
            .renaming
            .as_ref()
            .filter(|(id, _, _)| *id == bot.id)
            .map(|(_, input, _)| input.clone());
        let now = now_secs();
        let age = match &facts.line {
            Some(RowLine::Activity { at: Some(at), .. }) => {
                Some(krill_app::jump::format_age_secs(at / 1000, now))
            }
            _ => None,
        };
        let stop_tip = facts.limit.as_ref().map(|l| l.tip(&bot.agent_id));

        // Orca BotSidebarRow title: `text-[13px] leading-5`; unread is semibold, read is 80%.
        let name: AnyElement = match renaming {
            Some(input) => div()
                .flex_1()
                .min_w_0()
                .child(Input::new(&input).xsmall().appearance(false))
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ui::TEXT_MD)
                .line_height(px(20.))
                .when(facts.unread, |d| {
                    d.font_weight(FontWeight::SEMIBOLD).text_color(p.foreground)
                })
                .when(!facts.unread, |d| d.text_color(p.foreground.opacity(0.8)))
                .child(bot.name.clone())
                .into_any_element(),
        };
        let line = self.render_bot_line(bot, facts, p);
        let trailing = self.render_fold_strip(bot, facts, folded, &group, p, cx);
        let hover = self.bot_hover_card(bot, facts, cx.weak_entity());

        div()
            .id(SharedString::from(row_id))
            .debug_selector(move || row_sel.clone())
            .group(group.clone())
            .role(gpui::Role::Button)
            .tab_index(0)
            .focus_visible(|d| d.border_color(p.ring))
            .aria_label(bot.name.clone())
            .aria_expanded(!folded)
            // Orca workspace card surface: `ml-1 pr-1.5 rounded-lg border`, `pt/pb/pl-1.25`.
            .ml_1()
            .pl(px(5.))
            .pr_1p5()
            .py(px(5.))
            .rounded(ui::RADIUS_LG)
            .border_1()
            .overflow_hidden()
            .flex()
            .items_center()
            .cursor_pointer()
            .when(bot.paused, |d| d.opacity(0.6))
            .when(facts.active, |d| {
                d.bg(p.worktree_sidebar)
                    .border_color(p.worktree_sidebar_border)
            })
            .when(!facts.active, |d| {
                d.border_color(ui::TRANSPARENT)
                    .hover(|d| d.bg(p.sidebar_accent.opacity(0.4)))
            })
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                // A double click's second click renames, as Orca's.
                if e.click_count() > 1 {
                    this.start_bot_rename(&open_id, window, cx);
                } else {
                    this.open_bot_home(open_id.clone(), None, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .mr_2()
                    .flex_none()
                    .child(self.bot_avatar(bot, AVATAR, ui::RADIUS_MD, p))
                    .children(avatar_badge(facts.status, stop_tip.is_some(), p, cx)),
            )
            .child(
                div()
                    .id(SharedString::from(format!(
                        "bot-row-body-{}",
                        bot.id.as_str()
                    )))
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
                                        d.child(
                                            div()
                                                .ml_auto()
                                                .flex_none()
                                                .text_size(ui::TEXT_SM)
                                                .line_height(px(16.))
                                                .text_color(p.muted_foreground)
                                                .child(age),
                                        )
                                    }),
                            )
                            .children(line),
                    )
                    .child(trailing),
            )
            .context_menu(self.bot_row_menu(bot, facts, cx.weak_entity()))
    }

    /// Orca `BotRowLineView`: the line's words (place in the foreground), or the bot's purpose.
    fn render_bot_line(
        &self,
        bot: &SidebarBot,
        facts: &BotFacts,
        p: &'static ui::Palette,
    ) -> Option<gpui::Div> {
        let (place, words): (Option<String>, String) = if let Some(limit) = &facts.limit {
            (limit.place.clone(), limit.text())
        } else {
            facts
                .line
                .as_ref()
                .map_or_else(|| (None, bot.description.clone()), line_words)
        };
        let remote = !self.launch_host.is_local();
        if words.is_empty() && place.is_none() && !remote {
            return None;
        }
        Some(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_SM)
                .line_height(px(16.))
                .text_color(p.muted_foreground)
                // A paired server's bot: a server glyph, its name in the hover card.
                .when(remote, |d| {
                    d.child(ui::icons::svg_box(
                        ui::icons::SERVER_SVG,
                        ui::ICON_XS,
                        p.muted_foreground,
                    ))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(facts.unread, |d| d.text_color(p.foreground))
                        .child(match place {
                            Some(place) => format!("{place} · {words}"),
                            None => words,
                        }),
                ),
        )
    }

    /// Orca `BotRowFoldToggle`: the row's whole right edge, its chevron hidden until hover while
    /// unfolded, with the one live mark (a folded worker's ask, else the bot's own work).
    fn render_fold_strip(
        &self,
        bot: &SidebarBot,
        facts: &BotFacts,
        folded: bool,
        group: &SharedString,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let fold_id = bot.id.clone();
        let name = bot.name.clone();
        let any_waiting = facts
            .chats
            .iter()
            .flat_map(|c| &c.workers)
            .any(WorkerFacts::waiting);
        let mark = if folded && any_waiting {
            Some(AgentState::Waiting)
        } else if facts.status == RowStatus::Working && !bot.paused {
            Some(AgentState::Working)
        } else {
            None
        };
        div()
            .id(SharedString::from(format!("bot-fold-{}", bot.id.as_str())))
            .debug_selector({
                let sel = format!("bot-fold-{}", bot.id.as_str());
                move || sel.clone()
            })
            .role(gpui::Role::Button)
            .aria_label(if folded {
                text::SB_UNFOLD.format(&[("name", &name)])
            } else {
                text::SB_FOLD.format(&[("name", &name)])
            })
            .aria_expanded(!folded)
            // `-mt/-mb-1.25 -mr-1.5 min-w-10 px-1.5 gap-0.5`: full height, flush right.
            .my(px(-5.))
            .mr(px(-6.))
            .min_w(px(40.))
            .px_1p5()
            .self_stretch()
            .flex()
            .items_center()
            .justify_end()
            .gap_0p5()
            .text_color(p.muted_foreground)
            .hover(|d| d.text_color(p.foreground))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if !this.bot_ui.folded.remove(&fold_id) {
                    this.bot_ui.folded.insert(fold_id.clone());
                }
                cx.notify();
            }))
            .when_some(mark, |d, state| {
                d.child(ui::status::status_glyph_slot(
                    state,
                    false,
                    ui::status::Size::Md,
                    Some(&self.bot_ui.spinners),
                    cx,
                ))
            })
            .child(
                div()
                    .size_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!folded, |d| {
                        d.invisible()
                            .group_hover(group.clone(), gpui::Styled::visible)
                    })
                    .child(
                        gpui_component::Icon::new(if folded {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size(px(14.)),
                    ),
            )
    }

    /// Orca's row menu: Rename, Bot settings…, Pause, Copy Path, Mark Read/Unread, Delete bot.
    fn bot_row_menu(
        &self,
        bot: &SidebarBot,
        facts: &BotFacts,
        this: gpui::WeakEntity<Self>,
    ) -> impl Fn(
        gpui_component::menu::PopupMenu,
        &mut Window,
        &mut Context<gpui_component::menu::PopupMenu>,
    ) -> gpui_component::menu::PopupMenu
    + 'static {
        let menu_bot = bot.clone();
        let folder = self.bot_folder(bot);
        let unread = facts.unread;
        let sessions: Vec<SessionId> = facts.chats.iter().map(|c| c.session).collect();
        move |menu, _, _| {
            let bot = menu_bot.clone();
            let act = |f: ShellAction| {
                let this = this.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                    this.update(cx, |s, cx| f(s, window, cx)).ok();
                }
            };
            let id = bot.id.clone();
            let rename = act(Box::new(move |s, window, cx| {
                s.start_bot_rename(&id, window, cx);
            }));
            let id = bot.id.clone();
            let settings = act(Box::new(move |s, window, cx| {
                if !s.open_bot_settings_in_panel(&id, cx) {
                    s.open_bot(Some(id.clone()), Some(window), cx);
                }
            }));
            let id = bot.id.clone();
            let paused = bot.paused;
            let pause = act(Box::new(move |s, _, cx| {
                s.set_bot_paused(id.clone(), !paused, cx);
            }));
            let ids = sessions.clone();
            let read = act(Box::new(move |s, _, cx| {
                let host = s.launch_host.clone();
                let ids = ids.clone();
                s.sidebar.update(cx, |sb, cx| {
                    for id in &ids {
                        if unread {
                            sb.mark_session_read(&host, *id, cx);
                        } else {
                            sb.mark_session_unread(&host, *id, cx);
                        }
                    }
                });
            }));
            let deleting = bot.clone();
            let delete = act(Box::new(move |s, _, cx| {
                s.bot_ui.delete = Some(deleting.clone());
                cx.notify();
            }));
            let mut menu = menu
                .item(
                    PopupMenuItem::new(text::SB_RENAME.get())
                        .icon(gpui_component::Icon::default().data(ui::icons::PENCIL_SVG))
                        .on_click(rename),
                )
                .item(
                    PopupMenuItem::new(text::SB_BOT_SETTINGS.get())
                        .icon(IconName::Settings)
                        .on_click(settings),
                )
                .item(
                    PopupMenuItem::new(if paused {
                        text::SB_RESUME.get()
                    } else {
                        text::SB_PAUSE.get()
                    })
                    .icon(if paused {
                        IconName::Play
                    } else {
                        IconName::Pause
                    })
                    .on_click(pause),
                );
            if let Some(path) = folder.clone() {
                menu = menu.separator().item(
                    PopupMenuItem::new(text::SB_COPY_PATH.get())
                        .icon(IconName::Copy)
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                path.to_string_lossy().into_owned(),
                            ));
                        }),
                );
            }
            menu.item(
                PopupMenuItem::new(if unread {
                    text::SB_MARK_READ.get()
                } else {
                    text::SB_MARK_UNREAD.get()
                })
                .icon(gpui_component::Icon::default().data(ui::icons::BELL_SVG))
                .disabled(sessions.is_empty())
                .on_click(read),
            )
            .separator()
            .item(
                PopupMenuItem::new(text::SB_DELETE.get())
                    .icon(IconName::Delete)
                    .on_click(delete),
            )
        }
    }

    /// Orca `WorktreeTitleInlineRename` on the bot row: Enter saves, Escape or leaving cancels.
    pub(super) fn start_bot_rename(
        &mut self,
        bot_id: &BotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bot) = self.sidebar_bots.iter().find(|b| &b.id == bot_id) else {
            return;
        };
        let name = bot.name.clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let id = bot_id.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, ev: &InputEvent, _, cx| match ev {
                InputEvent::PressEnter { .. } => {
                    let name = input.read(cx).value().trim().to_owned();
                    this.bot_ui.renaming = None;
                    this.rename_bot(id.clone(), name, cx);
                }
                InputEvent::Blur => {
                    this.bot_ui.renaming = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.bot_ui.renaming = Some((bot_id.clone(), input, sub));
        cx.notify();
    }

    fn rename_bot(&mut self, bot_id: BotId, name: String, cx: &mut Context<Self>) {
        let Some(rpc) = self.bot_rpc() else {
            return;
        };
        let unchanged = self
            .sidebar_bots
            .iter()
            .any(|b| b.id == bot_id && b.name == name);
        if name.is_empty() || unchanged {
            cx.notify();
            return;
        }
        if let Some(b) = self.sidebar_bots.iter_mut().find(|b| b.id == bot_id) {
            b.name.clone_from(&name);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    krill_app::bots::rpc::call_checked(
                        &rpc,
                        &krill_proto::bots::BotRpcRequest::UpdateBot(Box::new(
                            krill_proto::bots::UpdateBotParams {
                                bot_id,
                                name: Some(name),
                                description: None,
                                emoji: krill_proto::bots::FieldPatch::Unchanged,
                                avatar: krill_proto::bots::FieldPatch::Unchanged,
                                agent_id: None,
                                model: krill_proto::bots::FieldPatch::Unchanged,
                                effort: krill_proto::bots::FieldPatch::Unchanged,
                                permission_mode: None,
                                workspace: krill_proto::bots::FieldPatch::Unchanged,
                                paused: None,
                                first_run_completed: None,
                                notifications: None,
                                schedules: None,
                                look_around: None,
                                memory_tidy: None,
                                mcp_tools: None,
                            },
                        )),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = res {
                    this.notice = Some(err);
                }
                this.refresh_bots_badge(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Orca `BotHoverDetails` + `BotHoverNeedsYou`, opened on pointer rest.
    fn bot_hover_card(
        &self,
        bot: &SidebarBot,
        facts: &BotFacts,
        this: gpui::WeakEntity<Self>,
    ) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView + 'static {
        let bot = bot.clone();
        let host_label = (!self.launch_host.is_local()).then(|| self.bot_host_label());
        let state: String = if bot.paused {
            text::PAUSED_BADGE.get().to_owned()
        } else {
            let working = facts.chats.iter().find(|c| c.working);
            match (facts.status, working.and_then(|c| c.status.clone())) {
                (RowStatus::Working, Some(action)) => action,
                (RowStatus::Working, None) => text::SB_REPLYING.get().to_owned(),
                (RowStatus::Permission, _) => text::SB_WAITING_FOR_YOU.get().to_owned(),
                _ => text::BP_IDLE.get().to_owned(),
            }
        };
        let working = facts.status == RowStatus::Working && !bot.paused;
        // Orca `BotHoverLatestLine`: the row's line whole; Needs you names its asks below.
        let latest = match &facts.line {
            None | Some(RowLine::Paused | RowLine::NeedYouElsewhere(_)) => None,
            Some(line) => {
                let age = match line {
                    RowLine::Activity { at: Some(at), .. } => {
                        Some(krill_app::jump::format_age_secs(at / 1000, now_secs()))
                    }
                    _ => None,
                };
                Some(line_words(line)).map(|(place, words)| {
                    (
                        age,
                        match place {
                            Some(place) => format!("{place} · {words}"),
                            None => words,
                        },
                    )
                })
            }
        };
        // Orca's asks: chats waiting on the owner, workers wanting approval, access requests.
        let mut asks: Vec<(&'static [u8], String, &'static str)> = Vec::new();
        for c in facts.chats.iter().filter(|c| c.needs_you) {
            let title = if c.home {
                text::SB_YOUR_CHAT.format(&[("name", &bot.name)])
            } else {
                c.title.to_string()
            };
            asks.push((
                ui::icons::CIRCLE_HELP_SVG,
                title,
                text::SB_ASKS_QUESTION.get(),
            ));
        }
        for w in facts
            .chats
            .iter()
            .flat_map(|c| &c.workers)
            .chain(&facts.stray)
            .filter(|w| w.waiting())
        {
            asks.push((
                ui::icons::SHIELD_SVG,
                w.title.clone(),
                text::SB_WANTS_APPROVAL.get(),
            ));
        }
        for who in &bot.access_requesters {
            asks.push((
                ui::icons::USER_PLUS_SVG,
                who.clone(),
                text::SB_ASKED_ACCESS.get(),
            ));
        }
        let items = bot.items_need_you;
        let total = asks.len() + items;
        let show_needs = !bot.paused && total > 0;
        let home_id = bot.id.clone();
        move |window, cx| {
            let bot = bot.clone();
            let host_label = host_label.clone();
            let state = state.clone();
            let latest = latest.clone();
            let asks = asks.clone();
            let this = this.clone();
            let home_id = home_id.clone();
            gpui_component::tooltip::Tooltip::element(move |_, cx| {
                let p = ui::palette(cx);
                let open_home = {
                    let this = this.clone();
                    let id = home_id.clone();
                    move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                        this.update(cx, |s, cx| s.open_bot_home_panel(&id, cx)).ok();
                    }
                };
                let home_label = text::SB_OPEN_HOME.format(&[("name", &bot.name)]);
                let shown = asks.len().min(3);
                div()
                    .id("bot-hover-card")
                    .debug_selector(|| "bot-hover-card".into())
                    .w(px(280.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(avatar_tile(&bot, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .pt_0p5()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(ui::TEXT_MD)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(bot.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .min_w_0()
                                            .items_center()
                                            .gap_1p5()
                                            .text_size(ui::TEXT_SM)
                                            .text_color(p.muted_foreground)
                                            .when(working, |d| {
                                                d.child(ui::status::status_glyph(
                                                    AgentState::Working,
                                                    false,
                                                    ui::status::Size::Md,
                                                    cx,
                                                ))
                                            })
                                            .child(div().min_w_0().truncate().child(state.clone())),
                                    )
                                    .when_some(host_label.clone(), |d, host| {
                                        d.child(
                                            div()
                                                .flex()
                                                .min_w_0()
                                                .items_center()
                                                .gap_1()
                                                .text_size(ui::TEXT_SM)
                                                .text_color(p.muted_foreground)
                                                .child(ui::icons::svg_box(
                                                    ui::icons::SERVER_SVG,
                                                    ui::ICON_XS,
                                                    p.muted_foreground,
                                                ))
                                                .child(
                                                    div().truncate().child(
                                                        krill_app::i18n::catalog::bots::NB_RUNS_LINE
                                                            .format(&[
                                                                ("host", &host),
                                                                ("agent", &bot.agent_id),
                                                            ]),
                                                    ),
                                                ),
                                        )
                                    }),
                            )
                            .child(
                                gpui_component::button::Button::new("bot-hover-open-home")
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        gpui_component::Icon::default().data(ui::icons::HOUSE_SVG),
                                    )
                                    .accessibility_label(home_label.clone())
                                    .tooltip(home_label)
                                    .on_click(open_home.clone()),
                            ),
                    )
                    .when_some(latest.clone(), |d, (age, line)| {
                        d.child(
                            div()
                                .mt(px(10.))
                                .pt_2()
                                .border_t_1()
                                .border_color(p.border)
                                .text_size(ui::TEXT_SM)
                                .text_color(p.foreground)
                                .line_clamp(3)
                                .when_some(age, |d, age| {
                                    d.child(
                                        div()
                                            .text_color(p.muted_foreground)
                                            .child(format!("{age} · ")),
                                    )
                                })
                                .child(line),
                        )
                    })
                    .when(show_needs, |d| {
                        let heading = if total == 1 {
                            text::SB_NEEDS_HEADING_ONE.get().to_owned()
                        } else {
                            text::SB_NEEDS_HEADING_MANY.format(&[("n", &total.to_string())])
                        };
                        let mut col = div()
                            .mt(px(10.))
                            .pt_2()
                            .border_t_1()
                            .border_color(p.border)
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(
                                div()
                                    .id("bot-hover-review")
                                    .debug_selector(|| "bot-hover-review".into())
                                    .group("bot-hover-review")
                                    .mx(px(-4.))
                                    .px_1()
                                    .py_0p5()
                                    .rounded(ui::RADIUS_SM)
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(p.accent))
                                    .on_click(open_home.clone())
                                    .text_size(ui::TEXT_SM)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.foreground)
                                            .child(heading),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_0p5()
                                            .text_color(p.muted_foreground)
                                            .group_hover("bot-hover-review", |d| {
                                                d.text_color(p.foreground)
                                            })
                                            .child(text::SB_REVIEW.get())
                                            .child(ui::icons::svg_box(
                                                ui::icons::ARROW_RIGHT_SVG,
                                                ui::ICON_XS,
                                                p.muted_foreground,
                                            )),
                                    ),
                            );
                        for (icon, title, detail) in asks.iter().take(shown) {
                            col = col.child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(ui::TEXT_SM)
                                    .line_height(px(16.))
                                    .child(ui::icons::svg_box(icon, ui::ICON_XS, p.agent_question))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(p.foreground)
                                            .child(title.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(p.muted_foreground)
                                            .child(format!("· {detail}")),
                                    ),
                            );
                        }
                        if asks.len() > shown {
                            col = col.child(
                                div()
                                    .pl(px(18.))
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(
                                        text::SB_MORE_ASKS
                                            .format(&[("n", &(asks.len() - shown).to_string())]),
                                    ),
                            );
                        }
                        if items > 0 {
                            col = col.child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(ui::TEXT_SM)
                                    .line_height(px(16.))
                                    .child(ui::icons::svg_box(
                                        ui::icons::INBOX_SVG,
                                        ui::ICON_XS,
                                        p.muted_foreground,
                                    ))
                                    .child(
                                        div().min_w_0().truncate().text_color(p.foreground).child(
                                            text::SB_ITEMS_FLAGGED
                                                .format(&[("n", &items.to_string())]),
                                        ),
                                    ),
                            );
                        }
                        d.child(col)
                    })
            })
            .build(window, cx)
        }
    }

    /// Opens the bot's page with its Home tab showing, where Needs you leads (Orca
    /// `openBotHomePanelFromSidebar`).
    fn open_bot_home_panel(&mut self, bot_id: &BotId, cx: &mut Context<Self>) {
        self.open_bot_home(bot_id.clone(), None, cx);
        if self.panel_collapsed {
            self.toggle_panel(cx);
        }
        self.bot_ui.panel.tab = crate::shell::bot_panel::BotPanelTab::Home;
        self.bot_ui.panel.settings = None;
        cx.notify();
    }
}

/// The 56 px picture of the hover card (`size-14 rounded-lg`).
fn avatar_tile(bot: &SidebarBot, cx: &gpui::App) -> AnyElement {
    krill_app::bots::avatar::render(
        &bot.avatar,
        None,
        &bot.name,
        px(56.),
        ui::RADIUS_LG,
        ui::palette(cx),
    )
}

/// Orca `BotAvatarStatusBadge`: needs-you, paused or a usage-limit stop on the picture's
/// corner, centred 3 px in from it, on a disc of the row's own colour.
// ponytail: a painted disc stands in for Orca's mask cutout (gpui has no masks), so a hovered
// row shows the sidebar colour around the badge; draw a mask if that ever reads wrong.
fn avatar_badge(
    status: RowStatus,
    stopped: bool,
    p: &'static ui::Palette,
    cx: &gpui::App,
) -> Option<gpui::Div> {
    let glyph = if stopped {
        ui::icons::svg_box(ui::icons::GAUGE_SVG, ui::ICON_XS, p.warning)
    } else {
        match status {
            RowStatus::Permission => {
                ui::status::status_glyph(AgentState::Waiting, false, ui::status::Size::Md, cx)
            }
            RowStatus::Inactive => {
                ui::status::status_glyph(AgentState::Exited, false, ui::status::Size::Md, cx)
            }
            RowStatus::Working | RowStatus::Active => return None,
        }
    };
    Some(
        div()
            .absolute()
            .right(px(-3.))
            .bottom(px(-3.))
            .size(px(14.))
            .rounded_full()
            .bg(p.sidebar)
            .flex()
            .items_center()
            .justify_center()
            .child(glyph),
    )
}

/// Orca `botRowLineText`: the line's place and words.
fn line_words(line: &RowLine) -> (Option<String>, String) {
    match line {
        RowLine::Paused => (None, text::PAUSED_BADGE.get().to_owned()),
        RowLine::Replying(Some(place)) => (None, text::SB_REPLYING_IN.format(&[("place", place)])),
        RowLine::Replying(None) => (None, text::SB_REPLYING.get().to_owned()),
        RowLine::Activity { place, text, .. } => (place.clone(), text.clone()),
        RowLine::NeedYouElsewhere(n) => (
            None,
            if *n == 1 {
                text::SB_NEEDS_YOU_ONE.format(&[("n", "1")])
            } else {
                text::SB_NEEDS_YOU_MANY.format(&[("n", &n.to_string())])
            },
        ),
        RowLine::WorkingElsewhere(n) => (None, text::SB_WORKING.format(&[("n", &n.to_string())])),
    }
}

/// What the agent last said for the line: its terminal title, unless that only names the agent
/// or the bot.
fn status_text(session: &krill_app::sidebar::Session, bot_name: &str) -> Option<String> {
    let title = session.status.title.as_deref()?.trim();
    let generic = ["claude", "claude code", "codex", "shell", "zsh", "bash"];
    (!title.is_empty()
        && !generic.iter().any(|g| title.eq_ignore_ascii_case(g))
        && title != bot_name.trim())
    .then(|| title.to_owned())
}
