// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License):
// src/renderer/src/components/bots/settings-view/BotSettingsRoot.tsx and BotSettingsRows.tsx.
//! Bot settings as Orca draws them in the right panel: the bot's card, then inset groups of
//! rows (Agent, Schedule, Soul and memory, Access, Sharing, Notifications), then Delete. A row
//! with a switch changes the bot at once; a row with a chevron opens the page that edits it.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
    px,
};
use gpui_component::button::Button;
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::agent::Permission;
use krill_proto::bots::{
    BotConfig, BotRpcRequest, BotRpcResponse, ExternalSource, FieldPatch, UpdateBotParams,
};

use crate::i18n::catalog::bots as text;
use crate::i18n::catalog::chat as chat_text;
use crate::ui;

use super::rpc::{call_checked, runtime_display_label};
use super::{DetailTab, View, avatar};

/// Request the host `Shell` to show another bot settings page (Soul and memory, Connectors,
/// Tools and sharing) in the panel, as Orca's settings rows open their subpages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSettingsPage(pub DetailTab);

/// Request the host `Shell` to confirm deleting a bot (Orca's Delete bot… at the list's end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDeleteBot(pub krill_proto::bots::BotId);

/// Which settings page the panel's Settings tab shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SettingsPage {
    /// Orca's root list.
    #[default]
    Root,
    /// Agent and model: identity, runtime, permission, folder and schedules.
    Agent,
}

/// What a row does when clicked.
#[derive(Clone, Copy)]
enum Open {
    Agent,
    Page(DetailTab),
}

/// A switch row's state and the change it makes when flipped.
type SwitchPatch = (bool, fn(&BotConfig, bool) -> UpdateBotParams);

struct Row {
    id: &'static str,
    icon: Option<&'static [u8]>,
    title: SharedString,
    detail: Option<SharedString>,
    open: Option<Open>,
    switch: Option<SwitchPatch>,
}

impl Row {
    fn new(id: &'static str, icon: Option<&'static [u8]>, title: &str) -> Self {
        Self {
            id,
            icon,
            title: title.to_owned().into(),
            detail: None,
            open: None,
            switch: None,
        }
    }

    fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        let detail = detail.into();
        if !detail.trim().is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    const fn opens(mut self, open: Open) -> Self {
        self.open = Some(open);
        self
    }

    const fn switch(mut self, on: bool, patch: fn(&BotConfig, bool) -> UpdateBotParams) -> Self {
        self.switch = Some((on, patch));
        self
    }
}

fn patch(bot: &BotConfig) -> UpdateBotParams {
    UpdateBotParams {
        bot_id: bot.id.clone(),
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

fn every(secs: u64) -> String {
    let (key, n) = match secs {
        0 => return text::SV_OFF.get().to_owned(),
        s if s % 86_400 == 0 => (&text::SV_EVERY_DAYS, s / 86_400),
        s if s % 3_600 == 0 => (&text::SV_EVERY_HOURS, s / 3_600),
        s => (&text::SV_EVERY_MINUTES, s.div_ceil(60)),
    };
    key.format(&[("n", &n.to_string())])
}

fn count(n: usize) -> String {
    if n == 0 {
        text::SV_NONE.get().to_owned()
    } else {
        text::SV_COUNT.format(&[("n", &n.to_string())])
    }
}

impl View {
    /// Applies one change from a settings row and shows the bot as krilld returns it.
    fn patch_bot(&mut self, params: UpdateBotParams, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.error = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::UpdateBot(Box::new(params))) },
                )
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.settings.sync_from_bot(&updated);
                        this.upsert_bot_config(*updated);
                        this.emit_badge(cx);
                    }
                    Ok(_) => {}
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Whether the Settings tab shows a subpage (the host's Back returns to the list first).
    #[must_use]
    pub fn settings_subpage(&self) -> Option<&'static str> {
        match self.settings.page {
            SettingsPage::Root => None,
            SettingsPage::Agent => Some(text::SV_AGENT_MODEL.get()),
        }
    }

    /// Back to the settings list from a subpage.
    pub fn close_settings_subpage(&mut self, cx: &mut Context<Self>) {
        self.settings.page = SettingsPage::Root;
        cx.notify();
    }

    fn render_settings_row(
        bot: &BotConfig,
        row: Row,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Row {
            id,
            icon,
            title,
            detail,
            open,
            switch,
        } = row;
        let has_detail = detail.is_some();
        let text_col = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .truncate()
                    .text_size(ui::TEXT_MD)
                    .font_weight(FontWeight::MEDIUM)
                    .line_height(px(18.))
                    .text_color(p.foreground)
                    .child(title.clone()),
            )
            .children(detail.map(|d| {
                div()
                    .truncate()
                    .text_size(ui::TEXT_XS)
                    .line_height(px(16.))
                    .text_color(p.muted_foreground)
                    .child(d)
            }));
        let base = div()
            .id(SharedString::from(id))
            .debug_selector(move || id.into())
            .w_full()
            .min_w_0()
            .min_h(px(36.))
            .py_1()
            .pl_3()
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .children(icon.map(|icon| {
                div()
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .when(has_detail, |d| d.self_start().pt(px(2.)))
                    .child(
                        Icon::default()
                            .data(icon)
                            .size(px(ui::ICON_MD))
                            .text_color(p.muted_foreground),
                    )
            }))
            .child(text_col);
        if let Some((on, make)) = switch {
            let ent = cx.entity();
            let bot = bot.clone();
            return base
                .child(
                    ui::switch(
                        p,
                        SharedString::from(format!("{id}-switch")),
                        on,
                        false,
                        move |checked, _, cx| {
                            let params = make(&bot, checked);
                            ent.update(cx, |this, cx| this.patch_bot(params, cx));
                        },
                    )
                    .aria_label(title),
                )
                .into_any_element();
        }
        match open {
            Some(open) => base
                .cursor_pointer()
                .hover(|d| d.bg(p.accent.opacity(0.6)))
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(ui::ICON_MD))
                        .text_color(p.muted_foreground),
                )
                .on_click(cx.listener(move |this, _, _, cx| match open {
                    Open::Agent => {
                        this.settings.page = SettingsPage::Agent;
                        cx.notify();
                    }
                    Open::Page(tab) => cx.emit(OpenSettingsPage(tab)),
                }))
                .into_any_element(),
            None => base.into_any_element(),
        }
    }

    /// Orca's `SettingsInsetGroup`: a small heading, then a muted card of hairline-split rows.
    fn render_settings_group(
        bot: &BotConfig,
        title: &str,
        rows: Vec<Row>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut card = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .overflow_hidden()
            .bg(p.foreground.opacity(0.035))
            .flex()
            .flex_col();
        let mut previous_icon = false;
        for (i, row) in rows.into_iter().enumerate() {
            let icon = row.icon.is_some();
            let inset = if icon && previous_icon { 38. } else { 12. };
            if i > 0 {
                card = card.child(
                    div()
                        .w_full()
                        .pl(px(inset))
                        .child(div().h(px(1.)).w_full().bg(p.border)),
                );
            }
            previous_icon = icon;
            card = card.child(Self::render_settings_row(bot, row, p, cx));
        }
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .min_h(px(20.))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_size(ui::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(title.to_owned()),
            )
            .child(card)
    }

    pub(super) fn render_settings_root(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let runtime = runtime_display_label(&bot.agent_id);
        let status = if bot.paused {
            text::PAUSED_BADGE.get()
        } else {
            text::ACTIVE_BADGE.get()
        };
        let upload = self.settings.avatar_cache.get(&bot.id).cloned();
        let card = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .bg(p.foreground.opacity(0.035))
            .p_3()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(avatar::render(
                &avatar::Avatar::of(bot),
                upload,
                &bot.name,
                px(32.),
                ui::RADIUS_MD,
                p,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(bot.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(format!("{runtime} · {status}")),
                    ),
            )
            .child(
                Button::new("bot-settings-share")
                    .debug_selector(|| "bot-settings-share".into())
                    .icon(
                        Icon::default()
                            .data(ui::icons::SHARE_2_SVG)
                            .size(px(ui::ICON_SM)),
                    )
                    .label(text::SV_SHARE.get())
                    .outline()
                    .xsmall()
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(OpenSettingsPage(DetailTab::ToolsSharing));
                    })),
            );

        let model = bot
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map_or_else(
                || text::SV_DEFAULT_MODEL.get().to_owned(),
                ToOwned::to_owned,
            );
        let permission = match bot.permission_mode {
            Permission::Yolo => chat_text::PERMISSION_FULL_ACCESS.get(),
            Permission::Manual => chat_text::PERMISSION_ASK.get(),
        };
        let computer = if self.host.is_local() {
            text::HOST_LOCAL.get().to_owned()
        } else {
            self.available_hosts
                .iter()
                .find(|(id, _, _)| id == &self.host)
                .map_or_else(|| "?".to_owned(), |(_, label, _)| label.clone())
        };
        let folder = bot
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map_or_else(
                || format!("~/…/bots/{}", bot.folder),
                |w| match krill_proto::os::home_dir() {
                    Some(home) => krill_proto::bots::tilde_display(std::path::Path::new(w), &home),
                    None => w.to_owned(),
                },
            );
        let agent = vec![
            Row::new(
                "bot-settings-agent",
                Some(ui::icons::BOT_SVG),
                text::SV_AGENT_MODEL.get(),
            )
            .detail(format!("{runtime} · {model}"))
            .opens(Open::Agent),
            Row::new(
                "bot-settings-permissions",
                Some(ui::icons::SHIELD_SVG),
                text::SV_PERMISSIONS.get(),
            )
            .detail(permission)
            .opens(Open::Agent),
            Row::new(
                "bot-settings-computer",
                Some(if self.host.is_local() {
                    ui::icons::LAPTOP_SVG
                } else {
                    ui::icons::SERVER_SVG
                }),
                text::SV_COMPUTER.get(),
            )
            .detail(format!("{computer} · {folder}"))
            .opens(Open::Agent),
            Row::new(
                "bot-settings-active",
                Some(ui::icons::ACTIVITY_SVG),
                text::SV_ACTIVE.get(),
            )
            .detail(text::SV_ACTIVE_DESC.get())
            .switch(!bot.paused, |bot, on| UpdateBotParams {
                paused: Some(!on),
                ..patch(bot)
            }),
        ];

        let look = &bot.look_around;
        let look_detail = if look.weekdays_only {
            format!(
                "{} · {}",
                every(look.interval_secs),
                text::SV_WEEKDAYS.get()
            )
        } else {
            every(look.interval_secs)
        };
        let schedules = bot.schedules.len();
        let schedule = vec![
            Row::new(
                "bot-settings-look-around",
                Some(ui::icons::EYE_SVG),
                text::SV_LOOK_AROUND.get(),
            )
            .detail(look_detail)
            .switch(look.enabled, |bot, on| {
                let mut look = bot.look_around.clone();
                look.enabled = on;
                UpdateBotParams {
                    look_around: Some(look),
                    ..patch(bot)
                }
            }),
            Row::new(
                "bot-settings-memory-upkeep",
                Some(ui::icons::MOON_SVG),
                text::SV_MEMORY_UPKEEP.get(),
            )
            .detail(every(bot.memory_tidy.interval_secs))
            .switch(bot.memory_tidy.enabled, |bot, on| {
                let mut tidy = bot.memory_tidy;
                tidy.enabled = on;
                UpdateBotParams {
                    memory_tidy: Some(tidy),
                    ..patch(bot)
                }
            }),
            Row::new(
                "bot-settings-schedules",
                Some(ui::icons::CALENDAR_CLOCK_SVG),
                text::SV_SCHEDULES.get(),
            )
            .detail(count(schedules))
            .opens(Open::Agent),
        ];

        let soul = bot.description.trim();
        let soul_memory = vec![
            Row::new(
                "bot-settings-soul",
                Some(ui::icons::SMILE_SVG),
                text::SV_SOUL.get(),
            )
            .detail(soul.to_owned())
            .opens(Open::Page(DetailTab::SoulMemory)),
            Row::new(
                "bot-settings-memory",
                Some(ui::icons::BRAIN_SVG),
                text::SV_MEMORY.get(),
            )
            .detail("MEMORY.md")
            .opens(Open::Page(DetailTab::SoulMemory)),
        ];

        let connectors = self
            .all_connector_statuses
            .iter()
            .filter(|s| s.bot_id == bot.id && s.enabled)
            .count();
        let people = bot
            .members
            .iter()
            .filter(|m| m.source != ExternalSource::Google)
            .count();
        let who = if people == 0 {
            text::SV_ONLY_YOU.get().to_owned()
        } else {
            text::SV_PEOPLE.format(&[("n", &people.to_string())])
        };
        let access = vec![
            Row::new(
                "bot-settings-connectors",
                Some(ui::icons::PLUG_SVG),
                text::SV_CONNECTORS.get(),
            )
            .detail(count(connectors))
            .opens(Open::Page(DetailTab::Connectors)),
            Row::new(
                "bot-settings-who-can-ask",
                Some(ui::icons::USERS_SVG),
                text::SV_WHO_CAN_ASK.get(),
            )
            .detail(who)
            .opens(Open::Page(DetailTab::Connectors)),
            Row::new(
                "bot-settings-tools",
                Some(ui::icons::WRENCH_SVG),
                text::SV_TOOLS.get(),
            )
            .detail(count(bot.mcp_tools.len()))
            .opens(Open::Page(DetailTab::ToolsSharing)),
            Row::new(
                "bot-settings-skills",
                Some(ui::icons::BOOK_OPEN_SVG),
                text::SV_SKILLS.get(),
            )
            .detail(count(bot.skills.len()))
            .opens(Open::Page(DetailTab::ToolsSharing)),
        ];

        let sharing = vec![
            Row::new(
                "bot-settings-sharing",
                Some(ui::icons::SHARE_2_SVG),
                text::SV_SHARING.get(),
            )
            .detail(text::SV_SHARING_DESC.get())
            .opens(Open::Page(DetailTab::ToolsSharing)),
        ];

        let notifications = vec![
            Row::new(
                "bot-settings-notify-permission",
                None,
                text::SV_NOTIFY_PERMISSION.get(),
            )
            .detail(text::SV_NOTIFY_PERMISSION_DESC.get())
            .switch(bot.notifications.needs_permission, |bot, on| {
                let mut n = bot.notifications;
                n.needs_permission = on;
                UpdateBotParams {
                    notifications: Some(n),
                    ..patch(bot)
                }
            }),
            Row::new(
                "bot-settings-notify-complete",
                None,
                text::SV_NOTIFY_COMPLETE.get(),
            )
            .detail(text::SV_NOTIFY_COMPLETE_DESC.get())
            .switch(bot.notifications.task_complete, |bot, on| {
                let mut n = bot.notifications;
                n.task_complete = on;
                UpdateBotParams {
                    notifications: Some(n),
                    ..patch(bot)
                }
            }),
            Row::new(
                "bot-settings-notify-failed",
                None,
                text::SV_NOTIFY_FAILED.get(),
            )
            .detail(text::SV_NOTIFY_FAILED_DESC.get())
            .switch(bot.notifications.could_not_finish, |bot, on| {
                let mut n = bot.notifications;
                n.could_not_finish = on;
                UpdateBotParams {
                    notifications: Some(n),
                    ..patch(bot)
                }
            }),
        ];

        let delete_id = bot.id.clone();
        div()
            .id("bot-settings-root")
            .debug_selector(|| "bot-settings-root".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_4()
            .child(card)
            .child(Self::render_settings_group(
                bot,
                text::SV_AGENT_GROUP.get(),
                agent,
                p,
                cx,
            ))
            .child(Self::render_settings_group(
                bot,
                text::SV_SCHEDULE.get(),
                schedule,
                p,
                cx,
            ))
            .child(Self::render_settings_group(
                bot,
                text::SV_SOUL_MEMORY.get(),
                soul_memory,
                p,
                cx,
            ))
            .child(Self::render_settings_group(
                bot,
                text::SV_ACCESS.get(),
                access,
                p,
                cx,
            ))
            .child(Self::render_settings_group(
                bot,
                text::SV_SHARING.get(),
                sharing,
                p,
                cx,
            ))
            .child(Self::render_settings_group(
                bot,
                text::SV_NOTIFICATIONS.get(),
                notifications,
                p,
                cx,
            ))
            .child(
                div().child(
                    ui::ghost("bot-settings-delete", text::BP_DELETE.get())
                        .debug_selector(|| "bot-settings-delete".into())
                        .icon(
                            Icon::default()
                                .data(ui::icons::TRASH_SVG)
                                .size(px(ui::ICON_SM)),
                        )
                        .text_color(p.destructive)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(RequestDeleteBot(delete_id.clone()));
                        })),
                ),
            )
    }
}
