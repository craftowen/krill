// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Home's sections under the items: Orca's Automations (`BotUpcomingSection`, its rows
//! `BotUpcomingRows`/`BotLookAroundRow` on `BotPanelRow`) and Suggestions (`BotUpNextSection`).

use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{
    BotConfig, BotId, BotRpcRequest, BotRpcResponse, BotUpcoming, FieldPatch, UpdateBotParams,
};

use super::home::section_header;
use super::home_time::{every_text, looked_text, quiet_line, ran_text, upcoming_at};
use super::rpc::call_checked;
use super::settings_root::{OpenSettingsPage, SettingsPage};
use super::{DetailTab, View, now_unix_ms};
use crate::i18n::catalog::{bot_home as hm, bots as text};
use crate::ui;

/// A suggestion's source logo (Orca shows Slack, Linear and GitHub).
fn source_logo(source: krill_proto::bots::ExternalSource) -> Option<&'static [u8]> {
    use krill_proto::bots::ExternalSource as S;
    match source {
        S::Slack => Some(super::connector_logos::SLACK_SVG),
        S::Linear => Some(super::connector_logos::LINEAR_SVG),
        S::GitHub => Some(super::connector_logos::GITHUB_SVG),
        S::Discord => Some(super::connector_logos::DISCORD_SVG),
        S::Google => Some(super::connector_logos::GOOGLE_SVG),
        S::Telegram | S::Composio => None,
    }
}

/// Automations shown before "All N automations"; Look around and Tidy memory always show.
const AUTOMATION_CARD_CAP: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutoKind {
    Tidy,
    Look,
    Schedule,
}

/// What a row's leading icon says in place of its kind (Orca `BotRowState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RowState {
    Failed(Option<String>),
    Running(&'static str),
    Paused,
}

impl RowState {
    fn label(&self) -> &'static str {
        match self {
            Self::Failed(_) => hm::FAILED.get(),
            Self::Running(label) => label,
            Self::Paused => hm::PAUSED.get(),
        }
    }
}

/// One Automations row with what the card ranks and says by (Orca `BotAutomationCardRow`).
#[derive(Debug, Clone)]
pub(super) struct AutoRow {
    pub key: String,
    pub kind: AutoKind,
    /// A schedule's id.
    pub id: String,
    pub title: String,
    pub on: bool,
    pub failed: Option<String>,
    pub running: bool,
    /// Its next run, only while ahead (or due now).
    pub next_at: Option<u64>,
    /// Next is an exact time the header may say (Tidy memory runs some time after it is due).
    pub exact_next: bool,
    pub last_at: Option<u64>,
    /// Tidy memory and Look around run on an interval.
    pub interval_secs: u64,
}

impl AutoRow {
    fn state(&self) -> Option<RowState> {
        if !self.on && !self.running {
            Some(RowState::Paused)
        } else if let Some(reason) = &self.failed {
            Some(RowState::Failed(
                Some(reason.clone()).filter(|r| !r.is_empty()),
            ))
        } else if self.running {
            Some(RowState::Running(if self.kind == AutoKind::Look {
                hm::LOOKING_SHORT.get()
            } else {
                hm::RUNNING.get()
            }))
        } else {
            None
        }
    }

    const fn rank(&self) -> u8 {
        if self.failed.is_some() {
            0
        } else if self.running {
            1
        } else if !self.on {
            4
        } else if self.next_at.is_none() {
            3
        } else {
            2
        }
    }
}

/// Orca's `botUpcomingRows` + `botAutomationCardRows`: Tidy memory, Look around, then the
/// schedules, with their runs.
pub(super) fn automation_rows(
    bot: &BotConfig,
    up: Option<&BotUpcoming>,
    looking: bool,
    look_error: Option<&str>,
    now: u64,
) -> Vec<AutoRow> {
    let ahead = |at: Option<u64>| at.map(|at| at.max(now));
    let mut rows = vec![
        AutoRow {
            key: "tidy-up".to_owned(),
            kind: AutoKind::Tidy,
            id: String::new(),
            title: text::HM_TIDY_MEMORY.get().to_owned(),
            on: bot.memory_tidy.enabled,
            failed: None,
            running: false,
            next_at: ahead(up.and_then(|u| u.tidy_next_ms)),
            exact_next: false,
            last_at: up.and_then(|u| u.tidy_last_ms),
            interval_secs: bot.memory_tidy.interval_secs,
        },
        AutoRow {
            key: "look-around".to_owned(),
            kind: AutoKind::Look,
            id: String::new(),
            title: text::SV_LOOK_AROUND.get().to_owned(),
            on: bot.look_around.enabled,
            failed: look_error
                .filter(|_| bot.look_around.enabled && !looking)
                .map(str::to_owned),
            running: looking,
            next_at: if looking {
                None
            } else {
                ahead(up.and_then(|u| u.look_around_next_ms))
            },
            exact_next: true,
            last_at: up.and_then(|u| u.look_around_last_ms),
            interval_secs: bot.look_around.interval_secs,
        },
    ];
    rows.extend(bot.schedules.iter().map(|s| {
        let facts = up.and_then(|u| u.schedules.iter().find(|x| x.id == s.id));
        AutoRow {
            key: format!("automation:{}", s.id),
            kind: AutoKind::Schedule,
            id: s.id.clone(),
            title: s.name.clone(),
            on: s.enabled,
            failed: None,
            running: facts.is_some_and(|f| f.running),
            next_at: facts.and_then(|f| f.next_ms).filter(|&at| at > now),
            exact_next: true,
            last_at: facts.and_then(|f| f.last_ms),
            interval_secs: 0,
        }
    }));
    rows
}

/// Failed, running, then soonest next; rows with no next keep their order, paused ones last.
pub(super) fn rank_rows(mut rows: Vec<AutoRow>) -> Vec<AutoRow> {
    rows.sort_by_key(|r| (r.rank(), r.next_at.unwrap_or(u64::MAX)));
    rows
}

/// The header's line while nothing else is said: the running automation, else the soonest
/// exact next run (Orca `botRunningRow` / `botNextRun`).
pub(super) fn header_quiet_line(rows: &[AutoRow], now: u64) -> (String, Option<String>) {
    let ranked = rank_rows(rows.to_vec());
    let running = ranked.iter().find(|r| r.running).map(|r| r.title.as_str());
    let next = ranked
        .iter()
        .find(|r| r.on && r.failed.is_none() && !r.running && r.exact_next && r.next_at.is_some())
        .and_then(|r| Some((r.title.as_str(), r.next_at?)));
    quiet_line(running, next, now)
}

/// The state's glyph, the only color on a calm row; the kind's icon while it has none.
fn state_icon(
    state: Option<&RowState>,
    kind: IconName,
    p: &ui::Palette,
    cx: &gpui::App,
) -> AnyElement {
    match state {
        None => Icon::new(kind)
            .size(px(14.))
            .text_color(p.muted_foreground)
            .into_any_element(),
        Some(RowState::Failed(_)) => Icon::new(IconName::CircleX)
            .size(px(14.))
            .text_color(p.destructive)
            .into_any_element(),
        Some(RowState::Running(_)) => ui::status::status_glyph(
            krill_proto::control::AgentState::Working,
            false,
            ui::status::Size::Sm,
            cx,
        )
        .into_any_element(),
        Some(RowState::Paused) => Icon::new(IconName::Pause)
            .size(px(14.))
            .text_color(p.muted_foreground)
            .into_any_element(),
    }
}

/// A row's hover card: its state, then its facts (Orca `BotRowCardBody`).
fn row_card(
    state: Option<(Option<RowState>, &'static str)>,
    facts: Vec<(Option<&'static str>, String)>,
) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView {
    move |window, cx| {
        let state = state.clone();
        let facts = facts.clone();
        Tooltip::element(move |_, cx| {
            let p = ui::palette(cx);
            div()
                .max_w(px(280.))
                .flex()
                .flex_col()
                .gap_1()
                .text_size(ui::TEXT_XS)
                .children(state.clone().map(|(glyph, label)| {
                    let reason = match &glyph {
                        Some(RowState::Failed(Some(reason))) => Some(reason.clone()),
                        _ => None,
                    };
                    div()
                        .flex()
                        .items_start()
                        .gap_1p5()
                        .text_color(p.popover_foreground)
                        .children(glyph.as_ref().map(|g| {
                            div().h(px(16.)).flex().items_center().child(state_icon(
                                Some(g),
                                IconName::Clock,
                                p,
                                cx,
                            ))
                        }))
                        .child(
                            div().min_w_0().child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(label)
                                    .children(reason.map(|r| format!(" · {r}"))),
                            ),
                        )
                }))
                .children(facts.iter().map(|(label, value)| {
                    div()
                        .text_color(p.popover_foreground)
                        .children(label.map(|l| {
                            div()
                                .flex_none()
                                .text_color(p.muted_foreground)
                                .child(format!("{l} "))
                        }))
                        .flex()
                        .child(value.clone())
                }))
        })
        .build(window, cx)
    }
}

fn base_params(bot_id: BotId) -> UpdateBotParams {
    UpdateBotParams {
        bot_id,
        name: None,
        description: None,
        emoji: FieldPatch::Unchanged,
        avatar: FieldPatch::Unchanged,
        agent_id: None,
        model: FieldPatch::Unchanged,
        effort: FieldPatch::Unchanged,
        permission_mode: None,
        workspace: FieldPatch::Unchanged,
        paused: None,
        first_run_completed: None,
        notifications: None,
        schedules: None,
        look_around: None,
        memory_tidy: None,
        mcp_tools: None,
    }
}

/// A row's ⋯ / Pause / Run now targets.
#[derive(Clone)]
enum RowAction {
    Toggle(AutoKind, String, bool),
    RunNow(String),
    LookNow,
    Settings(SettingsPage),
}

impl View {
    /// Pauses or resumes Tidy memory, Look around or a schedule from its Home row.
    fn set_automation_on(
        &mut self,
        bot: &BotConfig,
        kind: AutoKind,
        id: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        let mut params = base_params(bot.id.clone());
        match kind {
            AutoKind::Tidy => {
                let mut tidy = bot.memory_tidy;
                tidy.enabled = on;
                params.memory_tidy = Some(tidy);
            }
            AutoKind::Look => {
                let mut look = bot.look_around.clone();
                look.enabled = on;
                params.look_around = Some(look);
            }
            AutoKind::Schedule => {
                let mut schedules = bot.schedules.clone();
                if let Some(s) = schedules.iter_mut().find(|s| s.id == id) {
                    s.enabled = on;
                }
                params.schedules = Some(schedules);
            }
        }
        self.home_rpc(BotRpcRequest::UpdateBot(Box::new(params)), None, cx);
    }

    /// Sends a Home change, shows the bot krilld returns, and reads the automations' runs again.
    pub(super) fn home_rpc(
        &mut self,
        req: BotRpcRequest,
        notice: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        let Some(bot_id) = self.selected_bot_id.clone() else {
            return;
        };
        self.error = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let (res, up) = cx
                .background_executor()
                .spawn(async move {
                    let res = call_checked(&rpc, &req);
                    let up = match call_checked(&rpc, &BotRpcRequest::GetBotUpcoming { bot_id }) {
                        Ok(BotRpcResponse::Upcoming(up)) => Some(up),
                        _ => None,
                    };
                    (res, up)
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.settings.schedules.clone_from(&updated.schedules);
                        this.upsert_bot_config(*updated);
                        this.emit_badge(cx);
                    }
                    Ok(BotRpcResponse::Error(err)) | Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {
                        if let Some(n) = notice {
                            this.notice = Some(SharedString::from(n));
                        }
                    }
                }
                if up.is_some() {
                    this.items.upcoming = up;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn run_row_action(&mut self, bot: &BotConfig, action: RowAction, cx: &mut Context<Self>) {
        match action {
            RowAction::Toggle(kind, id, on) => self.set_automation_on(bot, kind, &id, on, cx),
            RowAction::RunNow(id) => self.home_rpc(
                BotRpcRequest::RunScheduleNow {
                    bot_id: bot.id.clone(),
                    schedule_id: id,
                },
                Some(hm::RUN_QUEUED.get()),
                cx,
            ),
            RowAction::LookNow => self.look_now(bot.id.clone(), cx),
            RowAction::Settings(page) => {
                self.settings.page = page;
                cx.emit(OpenSettingsPage(DetailTab::Settings));
            }
        }
    }

    /// Orca's Look now: looks around, then reads when it looked. A failure shows on its row.
    pub(super) fn look_now(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.soul_memory.look_around_running {
            return;
        }
        let rpc = self.rpc.clone();
        self.soul_memory.look_around_running = true;
        self.items.look_error = None;
        self.look_task = Some(cx.spawn(async move |this, cx| {
            let (res, up) = cx
                .background_executor()
                .spawn(async move {
                    let res = call_checked(
                        &rpc,
                        &BotRpcRequest::RunLookAround {
                            bot_id: bot_id.clone(),
                        },
                    );
                    let up = match call_checked(&rpc, &BotRpcRequest::GetBotUpcoming { bot_id }) {
                        Ok(BotRpcResponse::Upcoming(up)) => Some(up),
                        _ => None,
                    };
                    (res, up)
                })
                .await;
            this.update(cx, |this, cx| {
                this.soul_memory.look_around_running = false;
                match res {
                    Ok(
                        BotRpcResponse::LookAroundResult { suggestions, .. }
                        | BotRpcResponse::Suggestions(suggestions),
                    ) => this.soul_memory.suggestions = suggestions,
                    Ok(BotRpcResponse::Error(err)) | Err(err) => this.items.look_error = Some(err),
                    Ok(other) => this.items.look_error = Some(format!("{other:?}")),
                }
                if up.is_some() {
                    this.items.upcoming = up;
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Orca's "+ New automation": the bot's Home Chat opens with a draft to finish.
    fn draft_in_home_chat(bot_id: BotId, draft: &str, cx: &mut Context<Self>) {
        cx.emit(super::DraftInHomeChat {
            bot_id,
            text: draft.to_owned(),
        });
    }

    /// Starts the minute clock that keeps Home's automation times current.
    pub(super) fn ensure_upcoming_poll(&mut self, cx: &mut Context<Self>) {
        if self.items.upcoming_poll.is_some() {
            return;
        }
        self.items.upcoming_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(60))
                    .await;
                let Ok(Some((rpc, bot_id))) = this.update(cx, |this, _| {
                    (this.detail_tab == DetailTab::Items)
                        .then(|| {
                            this.selected_bot_id
                                .clone()
                                .map(|id| (this.rpc.clone(), id))
                        })
                        .flatten()
                }) else {
                    if this.upgrade().is_none() {
                        return;
                    }
                    continue;
                };
                let up = cx
                    .background_executor()
                    .spawn(async move {
                        match call_checked(&rpc, &BotRpcRequest::GetBotUpcoming { bot_id }) {
                            Ok(BotRpcResponse::Upcoming(up)) => Some(up),
                            _ => None,
                        }
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if up.is_some() {
                            this.items.upcoming = up;
                        }
                        // The ages and times move on even when nothing was read.
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    /// The bot's Automations rows as Home shows them.
    pub(super) fn home_automation_rows(&self, bot: &BotConfig, now: u64) -> Vec<AutoRow> {
        automation_rows(
            bot,
            self.items.upcoming.as_ref(),
            self.soul_memory.look_around_running,
            self.items.look_error.as_deref(),
            now,
        )
    }

    /// The header's quiet line for the selected bot: "Running …" or "Idle · next … · …", its
    /// narrow form, and whether an automation is running.
    #[must_use]
    pub fn home_quiet_line(&self) -> Option<(String, Option<String>, bool)> {
        let bot = self.selected_bot()?;
        let now = now_unix_ms();
        let rows = self.home_automation_rows(bot, now);
        let (line, short) = header_quiet_line(&rows, now);
        Some((line, short, rows.iter().any(|r| r.running)))
    }

    /// Orca's `BotUpcomingSection`: the bot's automations, and how to add one.
    pub(super) fn render_automations(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let now = now_unix_ms();
        let open = !self.items.folds.contains("automations");
        let header = section_header(
            "bot-home-automations",
            text::HM_AUTOMATIONS.get(),
            open,
            None,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if !this.items.folds.remove("automations") {
                this.items.folds.insert("automations");
            }
            cx.notify();
        }));
        let new_bot = bot.id.clone();
        let header = div()
            .relative()
            .flex()
            .items_center()
            .child(div().flex_1().min_w_0().child(header))
            .child(
                Button::new("bot-home-automations-new")
                    .debug_selector(|| "bot-home-automations-new".into())
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Plus).size(px(14.)))
                    .tooltip(hm::NEW_AUTOMATION.get())
                    .accessibility_label(hm::NEW_AUTOMATION.get())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        Self::draft_in_home_chat(
                            new_bot.clone(),
                            hm::NEW_AUTOMATION_DRAFT.get(),
                            cx,
                        );
                    })),
            );
        let mut section = div()
            .debug_selector(|| "bot-home-automations-section".into())
            .flex()
            .flex_col()
            .gap_0p5()
            .child(header);
        let ranked = rank_rows(self.home_automation_rows(bot, now));
        let listed: Vec<&AutoRow> = ranked
            .iter()
            .filter(|r| r.kind == AutoKind::Schedule)
            .collect();
        let kept: Vec<&str> = listed
            .iter()
            .take(AUTOMATION_CARD_CAP)
            .map(|r| r.key.as_str())
            .collect();
        for row in &ranked {
            if row.kind == AutoKind::Schedule && !kept.contains(&row.key.as_str()) {
                continue;
            }
            // Folded, only rows in trouble stay listed.
            if !open && row.failed.is_none() {
                continue;
            }
            section = section.child(self.render_automation_row(bot, row, now, p, cx));
        }
        if !open {
            return section;
        }
        if listed.len() > AUTOMATION_CARD_CAP {
            let paused = listed.iter().filter(|r| !r.on).count();
            let mut label = hm::ALL_AUTOMATIONS.format(&[("n", &listed.len().to_string())]);
            if paused > 0 {
                label.push_str(" · ");
                label.push_str(&hm::PAUSED_COUNT.format(&[("n", &paused.to_string())]));
            }
            section = section.child(
                div()
                    .id("bot-home-automations-all")
                    .debug_selector(|| "bot-home-automations-all".into())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .min_h(px(28.))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(ui::RADIUS_XS)
                    .pr_2()
                    .pl_8()
                    .cursor_pointer()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .hover(|d| d.bg(p.sidebar_accent).text_color(p.foreground))
                    .child(div().min_w_0().truncate().child(label))
                    .child(Icon::new(IconName::ChevronRight).size(px(14.)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.page = SettingsPage::Agent;
                        cx.emit(OpenSettingsPage(DetailTab::Settings));
                    })),
            );
        }
        if bot.schedules.is_empty() {
            section = section.child(
                div()
                    .debug_selector(|| "bot-home-automations-empty".into())
                    .py_1()
                    .pr_2()
                    .pl_8()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::HM_AUTOMATIONS_EMPTY.format(&[("name", &bot.name)])),
            );
        }
        section
    }

    /// Orca's `BotPanelRow` for one automation: its state as the leading icon, its name, the
    /// details in a hover card, Pause/Resume (or Look now) and ⋯ over its end on hover.
    fn render_automation_row(
        &self,
        bot: &BotConfig,
        row: &AutoRow,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let state = row.state();
        let dimmed = matches!(state, Some(RowState::Paused));
        let due_now = row.kind == AutoKind::Look
            && row.on
            && !row.running
            && row.next_at.is_some_and(|at| at <= now);
        let kind_icon = match row.kind {
            AutoKind::Tidy => IconName::Moon,
            AutoKind::Look => IconName::Eye,
            AutoKind::Schedule => IconName::CalendarClock,
        };
        let every = every_text(row.interval_secs);
        let mut facts: Vec<(Option<&'static str>, String)> = Vec::new();
        match row.kind {
            AutoKind::Tidy => facts.push((None, hm::RUNS_EVERY.format(&[("every", &every)]))),
            AutoKind::Look => match row.next_at {
                Some(at) if row.on && !due_now => facts.push((
                    Some(hm::NEXT.get()),
                    format!("{} · {every}", upcoming_at(at, now)),
                )),
                _ => facts.push((None, hm::RUNS_EVERY.format(&[("every", &every)]))),
            },
            AutoKind::Schedule => {
                let when = bot
                    .schedules
                    .iter()
                    .find(|s| s.id == row.id)
                    .map(|s| s.cron.clone())
                    .unwrap_or_default();
                match row.next_at {
                    Some(at) if row.on => facts.push((
                        Some(hm::NEXT.get()),
                        format!("{} · {when}", upcoming_at(at, now)),
                    )),
                    _ => facts.push((None, when)),
                }
            }
        }
        if let Some(at) = row.last_at.filter(|_| row.on) {
            facts.push((None, ran_text(at, now)));
        }
        let card_state = if due_now {
            Some((None, hm::DUE_NOW.get()))
        } else {
            state.clone().map(|s| {
                let label = s.label();
                (Some(s), label)
            })
        };
        let hover_key = format!("auto:{}", row.key);
        let hovered = self.items.hover_uid.as_deref() == Some(hover_key.as_str());
        let busy = row.running && row.kind == AutoKind::Look;
        let sel = format!("bot-home-automation-{}", row.key.replace(':', "-"));
        let debug = sel.clone();
        let open_page = match row.kind {
            AutoKind::Schedule => SettingsPage::Agent,
            AutoKind::Tidy | AutoKind::Look => SettingsPage::Root,
        };
        let open_bot = bot.clone();
        let title_label: SharedString = match &state {
            Some(s) => format!("{}, {}", row.title, s.label()).into(),
            None => row.title.clone().into(),
        };
        let mut el = div()
            .id(SharedString::from(sel))
            .debug_selector(move || debug.clone())
            .relative()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .rounded(ui::RADIUS_XS)
            .px_2()
            .py_1()
            .hover(|d| d.bg(p.sidebar_accent))
            .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                let now_on = this.items.hover_uid.as_deref() == Some(hover_key.as_str());
                if *on != now_on {
                    this.items.hover_uid = on.then(|| hover_key.clone());
                    cx.notify();
                }
            }))
            .child(
                div()
                    .h(px(20.))
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(dimmed, |d| d.opacity(0.6))
                    .child(state_icon(state.as_ref(), kind_icon, p, cx)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{}-title", row.key)))
                    .role(gpui::Role::Button)
                    .aria_label(title_label)
                    .tab_index(0)
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .text_size(ui::TEXT_MD)
                    .line_height(px(20.))
                    .text_color(if dimmed {
                        p.muted_foreground
                    } else {
                        p.foreground
                    })
                    .child(row.title.clone())
                    .tooltip(row_card(card_state, facts))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_row_action(&open_bot, RowAction::Settings(open_page), cx);
                    })),
            );
        if hovered || busy {
            el = el.child(Self::render_automation_actions(bot, row, p, cx));
        }
        el
    }

    fn render_automation_actions(
        bot: &BotConfig,
        row: &AutoRow,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let this = cx.weak_entity();
        let toggle = RowAction::Toggle(row.kind, row.id.clone(), !row.on);
        let toggle_label = if row.on {
            hm::PAUSE_SHORT.get()
        } else {
            hm::RESUME_SHORT.get()
        };
        let toggle_icon = if row.on {
            IconName::Pause
        } else {
            IconName::Play
        };
        let looking = row.running && row.kind == AutoKind::Look;
        // (label, icon, action, disabled) per menu group.
        let mut groups: Vec<Vec<(&'static str, IconName, RowAction, bool)>> = Vec::new();
        match row.kind {
            AutoKind::Tidy => {
                groups.push(vec![(toggle_label, toggle_icon, toggle.clone(), false)]);
                groups.push(vec![(
                    hm::TIDY_SETTINGS.get(),
                    IconName::Settings,
                    RowAction::Settings(SettingsPage::Root),
                    false,
                )]);
            }
            AutoKind::Look => {
                let mut first = Vec::new();
                if row.on {
                    first.push((
                        hm::LOOK_NOW.get(),
                        IconName::RefreshCw,
                        RowAction::LookNow,
                        looking,
                    ));
                }
                first.push((toggle_label, toggle_icon, toggle.clone(), false));
                groups.push(first);
                groups.push(vec![(
                    hm::LOOK_SETTINGS.get(),
                    IconName::Settings,
                    RowAction::Settings(SettingsPage::Root),
                    false,
                )]);
            }
            AutoKind::Schedule => {
                groups.push(vec![
                    (
                        hm::RUN_NOW.get(),
                        IconName::CirclePlay,
                        RowAction::RunNow(row.id.clone()),
                        row.running,
                    ),
                    (toggle_label, toggle_icon, toggle.clone(), false),
                ]);
                groups.push(vec![(
                    hm::OPEN_IN_SETTINGS.get(),
                    IconName::ArrowUpRight,
                    RowAction::Settings(SettingsPage::Agent),
                    false,
                )]);
            }
        }
        let (button_label, button_icon, button_action, button_disabled) =
            if row.kind == AutoKind::Look && row.on {
                (
                    hm::LOOK_NOW.get(),
                    if looking {
                        IconName::LoaderCircle
                    } else {
                        IconName::RefreshCw
                    },
                    RowAction::LookNow,
                    looking,
                )
            } else {
                (toggle_label, toggle_icon, toggle, false)
            };
        let button_bot = bot.clone();
        let menu_bot = bot.clone();
        let key = row.key.replace(':', "-");
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .flex()
            .items_center()
            .gap_0p5()
            .pl_1()
            .pr(px(6.))
            .rounded(ui::RADIUS_XS)
            .bg(p.sidebar_accent)
            .child(
                Button::new(SharedString::from(format!("bot-home-automation-btn-{key}")))
                    .debug_selector({
                        let key = key.clone();
                        move || format!("bot-home-automation-btn-{key}")
                    })
                    .ghost()
                    .xsmall()
                    .disabled(button_disabled)
                    .icon(Icon::new(button_icon).size(px(14.)))
                    .tooltip(button_label)
                    .accessibility_label(button_label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_row_action(&button_bot, button_action.clone(), cx);
                    })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "bot-home-automation-more-{key}"
                )))
                .debug_selector(move || format!("bot-home-automation-more-{key}"))
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Ellipsis).size(px(14.)))
                .accessibility_label(hm::MORE.get())
                .dropdown_menu(move |menu: PopupMenu, _, _| {
                    let mut menu = menu;
                    for (i, group) in groups.iter().enumerate() {
                        if i > 0 {
                            menu = menu.separator();
                        }
                        for (label, icon, action, disabled) in group {
                            let (this, bot, action) =
                                (this.clone(), menu_bot.clone(), action.clone());
                            menu = menu.item(
                                PopupMenuItem::new(*label)
                                    .icon(*icon)
                                    .disabled(*disabled)
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |v, cx| {
                                            v.run_row_action(&bot, action.clone(), cx);
                                        })
                                        .ok();
                                    }),
                            );
                        }
                    }
                    menu
                }),
            )
    }

    /// Orca's `BotUpNextSection` ("Suggestions"): what the last Look around suggests, shown while
    /// it has some or is looking. The header's Look now spins while looking; folded, it says how
    /// many and how many are new. Hovering a row shows Start and Dismiss over its end.
    pub(super) fn render_suggestions(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let rows: Vec<_> = self
            .soul_memory
            .suggestions
            .iter()
            .filter(|s| s.bot_id == bot.id && !s.dismissed)
            .cloned()
            .collect();
        let looking = self.soul_memory.look_around_running;
        if rows.is_empty() && !looking {
            return None;
        }
        let open = !self.items.folds.contains("suggestions");
        // What the owner had seen when this Home opened: newer rows keep their dot.
        let host_seen = self
            .items
            .upcoming
            .as_ref()
            .map_or(0, |u| u.up_next_seen_ms);
        let seen_at = *self
            .items
            .up_next_seen_at_open
            .entry(bot.id.clone())
            .or_insert(host_seen);
        let new_count = rows.iter().filter(|s| s.created_at_ms > seen_at).count();
        if open && rows.iter().any(|s| s.created_at_ms > host_seen) {
            self.mark_up_next_seen(bot.id.clone(), cx);
        }
        let summary = (!open).then(|| {
            if new_count > 0 {
                format!(
                    "{} · {}",
                    rows.len(),
                    hm::NEW_COUNT.format(&[("n", &new_count.to_string())])
                )
            } else {
                rows.len().to_string()
            }
        });
        let header = section_header(
            "bot-home-suggestions",
            text::HM_SUGGESTIONS.get(),
            open,
            summary,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if !this.items.folds.remove("suggestions") {
                this.items.folds.insert("suggestions");
            }
            cx.notify();
        }));
        let look_bot = bot.id.clone();
        let last_looked = self
            .items
            .upcoming
            .as_ref()
            .and_then(|u| u.look_around_last_ms);
        let look_label = if looking {
            hm::LOOKING_SHORT.get()
        } else {
            hm::LOOK_NOW.get()
        };
        let look_tip = match (looking, last_looked) {
            (false, Some(at)) => format!("{look_label}\n{}", looked_text(at, now_unix_ms())),
            _ => look_label.to_owned(),
        };
        let header = div()
            .group("bot-home-suggestions-head")
            .flex()
            .items_center()
            .child(div().flex_1().min_w_0().child(header))
            .child(
                div()
                    .when(!looking, |d| {
                        d.invisible()
                            .group_hover("bot-home-suggestions-head", gpui::Styled::visible)
                    })
                    .child(
                        Button::new("bot-home-look-now")
                            .debug_selector(|| "bot-home-look-now".into())
                            .ghost()
                            .xsmall()
                            .icon(
                                Icon::new(if looking {
                                    IconName::LoaderCircle
                                } else {
                                    IconName::RefreshCw
                                })
                                .size(px(14.)),
                            )
                            .tooltip(look_tip)
                            .accessibility_label(look_label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.soul_memory.look_around_running {
                                    this.look_now(look_bot.clone(), cx);
                                }
                            })),
                    ),
            );
        let mut section = div()
            .debug_selector(|| "bot-home-suggestions-section".into())
            .flex()
            .flex_col()
            .gap_0p5()
            .child(header);
        if !open {
            return Some(section);
        }
        for s in rows {
            let key = format!("suggestion:{}", s.id);
            let hovered = self.items.hover_uid.as_deref() == Some(key.as_str());
            let unseen = s.created_at_ms > seen_at;
            let sel = format!("bot-home-suggestion-{}", s.id);
            let debug = sel.clone();
            let link = s.link.clone();
            let (start_bot, start_id) = (bot.id.clone(), s.id.clone());
            let (dismiss_bot, dismiss_id) = (bot.id.clone(), s.id.clone());
            let card =
                {
                    let (title, why, link) = (s.title.clone(), s.why.clone(), s.link.clone());
                    move |window: &mut Window, cx: &mut gpui::App| {
                        let (title, why, link) = (title.clone(), why.clone(), link.clone());
                        Tooltip::element(move |_, cx| {
                            let p = ui::palette(cx);
                            div()
                                .max_w(px(280.))
                                .flex()
                                .flex_col()
                                .text_size(ui::TEXT_XS)
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.popover_foreground)
                                        .child(title.clone()),
                                )
                                .child(
                                    div()
                                        .mt_1()
                                        .text_color(p.muted_foreground)
                                        .child(why.clone()),
                                )
                                .children(link.clone().map(|l| {
                                    div().mt_1p5().text_color(p.muted_foreground).child(l)
                                }))
                        })
                        .build(window, cx)
                    }
                };
            let mut row = div()
                .id(SharedString::from(sel))
                .debug_selector(move || debug.clone())
                .role(gpui::Role::Button)
                .aria_label(SharedString::from(s.title.clone()))
                .tab_index(0)
                .relative()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .rounded(ui::RADIUS_XS)
                .px_2()
                .py_1()
                .when(link.is_some(), gpui::Styled::cursor_pointer)
                .hover(|d| d.bg(p.sidebar_accent))
                .tooltip(card)
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    let now_on = this.items.hover_uid.as_deref() == Some(key.as_str());
                    if *hovered != now_on {
                        this.items.hover_uid = hovered.then(|| key.clone());
                        cx.notify();
                    }
                }))
                .on_click(move |_, _, cx| {
                    if let Some(url) = link.as_deref().filter(|l| l.starts_with("http")) {
                        cx.open_url(url);
                    }
                })
                // The slot stays without a source so every title in the panel starts at one edge.
                .child(
                    div()
                        .h(px(20.))
                        .w(px(16.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .children(s.source.and_then(source_logo).map(|svg| {
                            Icon::default()
                                .data(svg)
                                .size(px(14.))
                                .text_color(p.muted_foreground)
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ui::TEXT_MD)
                        .line_height(px(20.))
                        .text_color(p.foreground)
                        .child(s.title.clone()),
                )
                .when(unseen, |d| {
                    d.child(
                        div()
                            .debug_selector(|| "bot-home-suggestion-unseen".into())
                            .size(px(6.))
                            .flex_none()
                            .rounded_full()
                            .bg(p.foreground),
                    )
                });
            if hovered {
                row = row.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl_1()
                        .pr(px(6.))
                        .rounded(ui::RADIUS_XS)
                        .bg(p.sidebar_accent)
                        .child(
                            ui::secondary("bot-home-suggestion-start", hm::START.get())
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_suggestion(start_bot.clone(), start_id.clone(), cx);
                                })),
                        )
                        .child(
                            Button::new("bot-home-suggestion-dismiss")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X).size(px(14.)))
                                .tooltip(hm::DISMISS.get())
                                .accessibility_label(hm::DISMISS.get())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.dismiss_suggestion(
                                        dismiss_bot.clone(),
                                        dismiss_id.clone(),
                                        cx,
                                    );
                                })),
                        ),
                );
            }
            section = section.child(row);
        }
        Some(section)
    }

    /// Marks the bot's suggestions seen (once per new batch), leaving this Home's dots as they
    /// opened.
    fn mark_up_next_seen(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let now = now_unix_ms();
        if let Some(up) = self.items.upcoming.as_mut() {
            up.up_next_seen_ms = now;
        }
        let rpc = self.rpc.clone();
        cx.background_executor()
            .spawn(async move {
                call_checked(
                    &rpc,
                    &BotRpcRequest::MarkUpNextSeen {
                        bot_id: Some(bot_id),
                    },
                )
                .ok();
            })
            .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::{BotScheduleConfig, BotScheduleUpcoming};

    fn bot() -> BotConfig {
        let mut bot = BotConfig::new(BotId::new("b1").unwrap(), "Scout", "claude", 0).unwrap();
        for (id, name) in [("s1", "Mentions triage"), ("s2", "Digest")] {
            bot.schedules.push(BotScheduleConfig {
                id: id.to_owned(),
                name: name.to_owned(),
                cron: "0 9 * * *".to_owned(),
                prompt: String::new(),
                enabled: true,
                timezone: None,
            });
        }
        bot
    }

    #[test]
    fn rows_rank_failed_running_soonest_then_paused() {
        let mut bot = bot();
        bot.memory_tidy.enabled = false;
        let now = 1_000_000;
        let up = BotUpcoming {
            look_around_next_ms: Some(now + 50_000),
            schedules: vec![
                BotScheduleUpcoming {
                    id: "s1".to_owned(),
                    next_ms: Some(now + 10_000),
                    ..Default::default()
                },
                BotScheduleUpcoming {
                    id: "s2".to_owned(),
                    running: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let keys: Vec<_> = rank_rows(automation_rows(&bot, Some(&up), false, None, now))
            .into_iter()
            .map(|r| r.key)
            .collect();
        assert_eq!(
            keys,
            ["automation:s2", "automation:s1", "look-around", "tidy-up"]
        );
        let failed = automation_rows(&bot, Some(&up), false, Some("boom"), now);
        assert_eq!(rank_rows(failed)[0].key, "look-around");
    }

    #[test]
    fn header_says_running_else_the_soonest_exact_next() {
        let bot = bot();
        let now = 1_000_000;
        let mut up = BotUpcoming {
            tidy_next_ms: Some(now + 1_000),
            look_around_next_ms: Some(now + 50_000),
            schedules: vec![BotScheduleUpcoming {
                id: "s1".to_owned(),
                next_ms: Some(now + 10_000),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rows = automation_rows(&bot, Some(&up), false, None, now);
        let (line, short) = header_quiet_line(&rows, now);
        // Tidy memory is sooner but runs in a window: the header says the schedule.
        assert!(line.contains("Mentions triage"), "{line}");
        assert!(short.is_some());
        up.schedules[0].running = true;
        let rows = automation_rows(&bot, Some(&up), false, None, now);
        assert_eq!(
            header_quiet_line(&rows, now).0,
            hm::RUNNING_NAMED.format(&[("name", "Mentions triage")])
        );
        let rows = automation_rows(&bot, None, false, None, now);
        assert_eq!(header_quiet_line(&rows, now).0, text::BP_IDLE.get());
    }
}
