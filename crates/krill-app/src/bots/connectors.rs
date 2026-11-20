// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 3 — Connectors & Access: setup and live health status for all 7 connectors
//! (Telegram, Slack Socket Mode, Discord Gateway, Google BYO OAuth, GitHub, Linear, Composio)
//! plus Pending Access Requests approval/decline.

use gpui::{
    App, ClipboardItem, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::bots::{
    ActsFor, BotAccessRequest, BotAccessRequestStatus, BotConfig, BotConnectorConfig,
    BotConnectorStatus, BotId, BotRpcRequest, BotRpcResponse, COMPOSIO_CURATED_APPS,
    ConnectorHealthState, DEFAULT_CONNECTOR_POLL_INTERVAL_SECS, ExternalSource,
    generate_slack_socket_mode_manifest,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::{
    connector_secret_ref, format_csv_i64s, format_csv_strings, parse_csv_i64s, parse_csv_strings,
};
use super::{View, opt_input_composing};

#[derive(Default)]
pub(super) struct ConnectorsState {
    pub(super) bot_statuses: Vec<BotConnectorStatus>,
    pub(super) bot_access_requests: Vec<BotAccessRequest>,
    pub(super) needs_sync_from_bot: bool,
    // Telegram
    pub(super) tg_token: Option<Entity<InputState>>,
    pub(super) tg_chats: Option<Entity<InputState>>,
    pub(super) tg_users: Option<Entity<InputState>>,
    // Slack
    pub(super) slack_app_token: Option<Entity<InputState>>,
    pub(super) slack_bot_token: Option<Entity<InputState>>,
    pub(super) slack_channels: Option<Entity<InputState>>,
    pub(super) slack_users: Option<Entity<InputState>>,
    // Discord
    pub(super) discord_token: Option<Entity<InputState>>,
    pub(super) discord_guilds: Option<Entity<InputState>>,
    pub(super) discord_channels: Option<Entity<InputState>>,
    pub(super) discord_users: Option<Entity<InputState>>,
    // Google
    pub(super) google_oauth_json: Option<Entity<InputState>>,
    pub(super) google_poll: Option<Entity<InputState>>,
    // GitHub
    pub(super) github_token: Option<Entity<InputState>>,
    pub(super) github_repos: Option<Entity<InputState>>,
    pub(super) github_poll: Option<Entity<InputState>>,
    // Linear
    pub(super) linear_api_key: Option<Entity<InputState>>,
    pub(super) linear_teams: Option<Entity<InputState>>,
    pub(super) linear_poll: Option<Entity<InputState>>,
    // Composio
    pub(super) composio_api_key: Option<Entity<InputState>>,
}

impl ConnectorsState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        [
            &self.tg_token,
            &self.tg_chats,
            &self.tg_users,
            &self.slack_app_token,
            &self.slack_bot_token,
            &self.slack_channels,
            &self.slack_users,
            &self.discord_token,
            &self.discord_guilds,
            &self.discord_channels,
            &self.discord_users,
            &self.google_oauth_json,
            &self.google_poll,
            &self.github_token,
            &self.github_repos,
            &self.github_poll,
            &self.linear_api_key,
            &self.linear_teams,
            &self.linear_poll,
            &self.composio_api_key,
        ]
        .into_iter()
        .any(|slot| opt_input_composing(slot.as_ref(), window, app))
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        let secret_ph = text::SECRET_PLACEHOLDER.get();
        let updates = [
            (&self.tg_token, secret_ph),
            (&self.tg_chats, text::TELEGRAM_CHATS_PLACEHOLDER.get()),
            (&self.tg_users, text::TELEGRAM_USERS_PLACEHOLDER.get()),
            (&self.slack_app_token, secret_ph),
            (&self.slack_bot_token, secret_ph),
            (&self.discord_token, secret_ph),
            (&self.discord_guilds, text::DISCORD_GUILDS_PLACEHOLDER.get()),
            (&self.google_oauth_json, secret_ph),
            (&self.github_token, secret_ph),
            (&self.github_repos, text::GITHUB_REPOS_PLACEHOLDER.get()),
            (&self.linear_api_key, secret_ph),
            (&self.linear_teams, text::LINEAR_TEAMS_PLACEHOLDER.get()),
            (&self.composio_api_key, secret_ph),
        ];
        for (slot, ph) in updates {
            if let Some(i) = slot {
                i.update(cx, |s, cx| s.set_placeholder(ph, window, cx));
            }
        }
    }
}

#[derive(Clone, Copy)]
struct SecretRowSpec {
    id_prefix: &'static str,
    label: &'static str,
    configured: bool,
}

fn health_badge_spec(state: ConnectorHealthState) -> (ui::BadgeTone, &'static str) {
    match state {
        ConnectorHealthState::Disabled => (ui::BadgeTone::Muted, text::HEALTH_DISABLED.get()),
        ConnectorHealthState::Disconnected => {
            (ui::BadgeTone::Neutral, text::HEALTH_DISCONNECTED.get())
        }
        ConnectorHealthState::Connecting => (ui::BadgeTone::Warning, text::HEALTH_CONNECTING.get()),
        ConnectorHealthState::Connected => (ui::BadgeTone::Success, text::HEALTH_CONNECTED.get()),
        ConnectorHealthState::Error => (ui::BadgeTone::Destructive, text::HEALTH_ERROR.get()),
    }
}

fn access_status_badge(status: BotAccessRequestStatus) -> (ui::BadgeTone, &'static str) {
    match status {
        BotAccessRequestStatus::Pending => (ui::BadgeTone::Warning, text::ACCESS_PENDING.get()),
        BotAccessRequestStatus::Approved => (ui::BadgeTone::Success, text::ACCESS_APPROVED.get()),
        BotAccessRequestStatus::Declined => (ui::BadgeTone::Muted, text::ACCESS_DECLINED.get()),
    }
}

impl View {
    fn ensure_connector_inputs(
        &mut self,
        bot: &BotConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        let mk_secret = |window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(text::SECRET_PLACEHOLDER.get())
                    .masked(true)
            })
        };
        let mk_plain = |ph: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };

        if self.connectors.tg_token.is_none() {
            self.connectors.tg_token = Some(mk_secret(window, cx));
            self.connectors.tg_chats =
                Some(mk_plain(text::TELEGRAM_CHATS_PLACEHOLDER.get(), window, cx));
            self.connectors.tg_users =
                Some(mk_plain(text::TELEGRAM_USERS_PLACEHOLDER.get(), window, cx));
            self.connectors.slack_app_token = Some(mk_secret(window, cx));
            self.connectors.slack_bot_token = Some(mk_secret(window, cx));
            self.connectors.slack_channels =
                Some(mk_plain(text::ALLOWED_CHANNELS_LABEL.get(), window, cx));
            self.connectors.slack_users =
                Some(mk_plain(text::ALLOWED_USERS_LABEL.get(), window, cx));
            self.connectors.discord_token = Some(mk_secret(window, cx));
            self.connectors.discord_guilds =
                Some(mk_plain(text::DISCORD_GUILDS_PLACEHOLDER.get(), window, cx));
            self.connectors.discord_channels =
                Some(mk_plain(text::ALLOWED_CHANNELS_LABEL.get(), window, cx));
            self.connectors.discord_users =
                Some(mk_plain(text::ALLOWED_USERS_LABEL.get(), window, cx));
            self.connectors.google_oauth_json = Some(mk_secret(window, cx));
            self.connectors.google_poll =
                Some(mk_plain(text::POLL_INTERVAL_LABEL.get(), window, cx));
            self.connectors.github_token = Some(mk_secret(window, cx));
            self.connectors.github_repos =
                Some(mk_plain(text::GITHUB_REPOS_PLACEHOLDER.get(), window, cx));
            self.connectors.github_poll =
                Some(mk_plain(text::POLL_INTERVAL_LABEL.get(), window, cx));
            self.connectors.linear_api_key = Some(mk_secret(window, cx));
            self.connectors.linear_teams =
                Some(mk_plain(text::LINEAR_TEAMS_PLACEHOLDER.get(), window, cx));
            self.connectors.linear_poll =
                Some(mk_plain(text::POLL_INTERVAL_LABEL.get(), window, cx));
            self.connectors.composio_api_key = Some(mk_secret(window, cx));
            self.connectors.needs_sync_from_bot = true;
        }

        if self.connectors.needs_sync_from_bot {
            let c = &bot.connectors;
            let set = |slot: &Option<Entity<InputState>>,
                       val: String,
                       window: &mut Window,
                       cx: &mut Context<Self>| {
                if let Some(i) = slot {
                    i.update(cx, |s, cx| s.set_value(val, window, cx));
                }
            };
            set(
                &self.connectors.tg_chats,
                format_csv_i64s(&c.telegram.allowed_chat_ids),
                window,
                cx,
            );
            set(
                &self.connectors.tg_users,
                format_csv_i64s(&c.telegram.allowed_user_ids),
                window,
                cx,
            );
            set(
                &self.connectors.slack_channels,
                format_csv_strings(&c.slack.allowed_channel_ids),
                window,
                cx,
            );
            set(
                &self.connectors.slack_users,
                format_csv_strings(&c.slack.allowed_user_ids),
                window,
                cx,
            );
            set(
                &self.connectors.discord_guilds,
                format_csv_strings(&c.discord.allowed_guild_ids),
                window,
                cx,
            );
            set(
                &self.connectors.discord_channels,
                format_csv_strings(&c.discord.allowed_channel_ids),
                window,
                cx,
            );
            set(
                &self.connectors.discord_users,
                format_csv_strings(&c.discord.allowed_user_ids),
                window,
                cx,
            );
            set(
                &self.connectors.google_poll,
                c.google.poll_interval_secs.to_string(),
                window,
                cx,
            );
            set(
                &self.connectors.github_repos,
                format_csv_strings(&c.github.repos),
                window,
                cx,
            );
            set(
                &self.connectors.github_poll,
                c.github.poll_interval_secs.to_string(),
                window,
                cx,
            );
            set(
                &self.connectors.linear_teams,
                format_csv_strings(&c.linear.team_keys),
                window,
                cx,
            );
            set(
                &self.connectors.linear_poll,
                c.linear.poll_interval_secs.to_string(),
                window,
                cx,
            );
            self.connectors.needs_sync_from_bot = false;
        }
    }

    pub fn save_connectors_config(
        &mut self,
        bot_id: BotId,
        connectors: BotConnectorConfig,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = rpc(&BotRpcRequest::SetConnectorConfig {
                        bot_id: bot_id_bg.clone(),
                        connectors: Box::new(connectors),
                    })?;
                    let statuses = match rpc(&BotRpcRequest::ConnectorStatus {
                        bot_id: Some(bot_id_bg),
                    }) {
                        Ok(BotRpcResponse::ConnectorStatuses(s)) => Some(s),
                        _ => None,
                    };
                    Ok::<_, String>((resp, statuses))
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok((BotRpcResponse::Bot(updated), statuses)) => {
                        this.upsert_bot_config(*updated);
                        if let Some(st) = statuses {
                            this.connectors.bot_statuses = st;
                        }
                        this.notice = Some(SharedString::from(text::CONNECTOR_SAVED.get()));
                    }
                    Ok((BotRpcResponse::Error(msg), _)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn save_connector_secret(
        &mut self,
        bot_id: BotId,
        secret_ref: String,
        value: &str,
        updated_connectors: BotConnectorConfig,
        cx: &mut Context<Self>,
    ) {
        let trimmed = value.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    match rpc(&BotRpcRequest::SetSecret {
                        secret_ref,
                        value: trimmed,
                    })? {
                        BotRpcResponse::Ok => {}
                        BotRpcResponse::Error(msg) => return Err(msg),
                        other => return Err(format!("unexpected SetSecret response: {other:?}")),
                    }
                    rpc(&BotRpcRequest::SetConnectorConfig {
                        bot_id,
                        connectors: Box::new(updated_connectors),
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SECRET_SAVED.get()));
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn reload_connectors(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let _ = rpc(&BotRpcRequest::ReloadConnectors {
                        bot_id: Some(bot_id_bg.clone()),
                    })?;
                    rpc(&BotRpcRequest::ConnectorStatus {
                        bot_id: Some(bot_id_bg),
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::ConnectorStatuses(list)) => {
                        this.connectors.bot_statuses = list;
                        this.notice = Some(SharedString::from(text::CONNECTOR_RELOADED.get()));
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn trigger_sync_now(
        &mut self,
        bot_id: BotId,
        source: Option<ExternalSource>,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::TriggerSyncNow { bot_id, source }) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::SyncTriggered { .. } | BotRpcResponse::Ok) => {
                        this.notice = Some(SharedString::from(text::SYNC_TRIGGERED.get()));
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn start_google_oauth(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::StartGoogleOAuth { bot_id }) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::GoogleOAuthStarted { auth_url, .. }) => {
                        if auth_url.starts_with("https://") || auth_url.starts_with("http://") {
                            cx.open_url(&auth_url);
                        }
                        this.notice = Some(SharedString::from(text::GOOGLE_OAUTH_OPENED.get()));
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn copy_slack_manifest(&mut self, bot_name: &str, cx: &mut Context<Self>) {
        let yaml = generate_slack_socket_mode_manifest(bot_name);
        cx.write_to_clipboard(ClipboardItem::new_string(yaml));
        self.notice = Some(SharedString::from(text::SLACK_MANIFEST_COPIED.get()));
        cx.notify();
    }

    pub fn decide_access_request(&mut self, token: String, approve: bool, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::DecideAccessRequest { token, approve }) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::AccessRequestDecided(decided)) => {
                        for list in [
                            &mut this.connectors.bot_access_requests,
                            &mut this.all_access_requests,
                        ] {
                            if let Some(slot) = list.iter_mut().find(|r| r.token == decided.token) {
                                *slot = decided.clone();
                            }
                        }
                        this.emit_badge(cx);
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn status_for(
        &self,
        source: ExternalSource,
        enabled: bool,
    ) -> (ConnectorHealthState, Option<String>) {
        if let Some(st) = self
            .connectors
            .bot_statuses
            .iter()
            .find(|s| s.source == source)
        {
            (st.state, st.last_error.clone())
        } else if enabled {
            (ConnectorHealthState::Disconnected, None)
        } else {
            (ConnectorHealthState::Disabled, None)
        }
    }

    fn render_acts_for_control(
        p: &ui::Palette,
        prefix: &'static str,
        current: &ActsFor,
        on_pick: impl Fn(&mut View, ActsFor, &mut Context<View>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let choices = [
            (
                "owner",
                text::ACTS_FOR_OWNER.get(),
                ActsFor::Owner,
                matches!(current, ActsFor::Owner),
            ),
            (
                "anyone",
                text::ACTS_FOR_ANYONE.get(),
                ActsFor::Anyone,
                matches!(current, ActsFor::Anyone),
            ),
        ];
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::ACTS_FOR_LABEL.get()),
            )
            .child(ui::segmented_control(
                p,
                choices
                    .into_iter()
                    .map(move |(suffix, label, val, active)| {
                        let cb = on_pick.clone();
                        let id = format!("bots-{prefix}-acts-{suffix}");
                        let sel = id.clone();
                        ui::segment(p, SharedString::from(id), label, active)
                            .debug_selector(move || sel.clone())
                            .on_click(cx.listener(move |this, _, _, cx| cb(this, val.clone(), cx)))
                    }),
            ))
    }

    fn render_secret_row(
        p: &ui::Palette,
        spec: SecretRowSpec,
        input: Option<&Entity<InputState>>,
        busy: bool,
        on_save: impl Fn(&mut View, &str, &mut Window, &mut Context<View>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let input_clone = input.cloned();
        let btn_id = format!("bots-secret-save-{}", spec.id_prefix);
        let btn_sel = btn_id.clone();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(ui::field_label(spec.label, None, p))
                    .child(ui::badge(
                        p,
                        if spec.configured {
                            ui::BadgeTone::Success
                        } else {
                            ui::BadgeTone::Muted
                        },
                        if spec.configured {
                            text::SECRET_CONFIGURED.get()
                        } else {
                            text::SECRET_NOT_SET.get()
                        },
                    )),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .children(input.map(|i| div().flex_1().min_w_0().child(Input::new(i).small())))
                    .child(
                        ui::secondary(SharedString::from(btn_id), text::SAVE_SECRET_BTN.get())
                            .debug_selector(move || btn_sel.clone())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let value = input_clone
                                    .as_ref()
                                    .map(|i| {
                                        let v = i.read(cx).value().to_string();
                                        i.update(cx, |s, cx| {
                                            s.set_value(String::new(), window, cx);
                                        });
                                        v
                                    })
                                    .unwrap_or_default();
                                on_save(this, &value, window, cx);
                            })),
                    ),
            )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "All 7 connector cards and Pending Access Requests section"
    )]
    pub(super) fn render_connectors_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_connector_inputs(bot, window, cx);
        let c = bot.connectors.clone();
        let bot_id = bot.id.clone();
        let bot_name = bot.name.clone();

        // Top toolbar
        let bot_reload = bot_id.clone();
        let bot_sync = bot_id.clone();
        let toolbar = div()
            .w_full()
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
                    .child(text::CONNECTORS_HEADING.get()),
            )
            .child(
                ui::controls()
                    .child(
                        ui::secondary("bots-connectors-reload", text::RELOAD_CONNECTORS_BTN.get())
                            .debug_selector(|| "bots-connectors-reload".into())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.reload_connectors(bot_reload.clone(), cx);
                            })),
                    )
                    .child(
                        ui::secondary("bots-connectors-sync-all", text::SYNC_NOW_BTN.get())
                            .debug_selector(|| "bots-connectors-sync-all".into())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.trigger_sync_now(bot_sync.clone(), None, cx);
                            })),
                    ),
            );

        // 1. Telegram card
        let (tg_state, tg_err) = self.status_for(ExternalSource::Telegram, c.telegram.enabled);
        let (tg_tone, tg_label) = health_badge_spec(tg_state);
        let tg_card = {
            let bot_toggle = bot_id.clone();
            let c_toggle = c.clone();
            let bot_secret = bot_id.clone();
            let c_secret = c.clone();
            let bot_acts = bot_id.clone();
            let c_acts = c.clone();
            let bot_save = bot_id.clone();
            let c_save = c.clone();
            let chats_in = self.connectors.tg_chats.clone();
            let users_in = self.connectors.tg_users.clone();
            let entity = cx.entity();
            ui::card(p)
                .id("bots-conn-telegram")
                .debug_selector(|| "bots-conn-telegram".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(ui::label(
                            p,
                            text::CONN_TELEGRAM.get(),
                            Some(text::CONN_TELEGRAM_DESC.get()),
                        ))
                        .child(ui::controls().child(ui::badge(p, tg_tone, tg_label)).child(
                            ui::switch(
                                p,
                                "bots-tg-enable",
                                c.telegram.enabled,
                                self.busy,
                                move |next, _, cx| {
                                    let mut next_c = c_toggle.clone();
                                    next_c.telegram.enabled = next;
                                    let bid = bot_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            ),
                        )),
                )
                .children(tg_err.map(|e| ui::error_banner(p, e)))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "telegram-token",
                        label: text::TELEGRAM_TOKEN_LABEL.get(),
                        configured: c.telegram.secret_ref.is_some(),
                    },
                    self.connectors.tg_token.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_secret.clone();
                        let sref = next_c.telegram.secret_ref.clone().unwrap_or_else(|| {
                            connector_secret_ref(bot_secret.as_str(), "telegram")
                        });
                        next_c.telegram.secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_secret.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::ALLOWED_CHATS_LABEL.get(), None, p))
                                .children(chats_in.as_ref().map(|i| Input::new(i).small())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::ALLOWED_USERS_LABEL.get(), None, p))
                                .children(users_in.as_ref().map(|i| Input::new(i).small())),
                        ),
                )
                .child(Self::render_acts_for_control(
                    p,
                    "tg",
                    &c.telegram.acts_for,
                    move |this, val, cx| {
                        let mut next_c = c_acts.clone();
                        next_c.telegram.acts_for = val;
                        this.save_connectors_config(bot_acts.clone(), next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div().flex().justify_end().child(
                        ui::secondary("bots-tg-save", text::SAVE_CONNECTOR_BTN.get())
                            .debug_selector(|| "bots-tg-save".into())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut next_c = c_save.clone();
                                if let Some(i) = &this.connectors.tg_chats {
                                    next_c.telegram.allowed_chat_ids =
                                        parse_csv_i64s(&i.read(cx).value());
                                }
                                if let Some(i) = &this.connectors.tg_users {
                                    next_c.telegram.allowed_user_ids =
                                        parse_csv_i64s(&i.read(cx).value());
                                }
                                this.save_connectors_config(bot_save.clone(), next_c, cx);
                            })),
                    ),
                )
        };

        // 2. Slack Socket Mode card
        let (sl_state, sl_err) = self.status_for(ExternalSource::Slack, c.slack.enabled);
        let (sl_tone, sl_label) = health_badge_spec(sl_state);
        let slack_card = {
            let bot_toggle = bot_id.clone();
            let c_toggle = c.clone();
            let bot_mirror = bot_id.clone();
            let c_mirror = c.clone();
            let bot_app = bot_id.clone();
            let c_app = c.clone();
            let bot_tok = bot_id.clone();
            let c_tok = c.clone();
            let bot_acts = bot_id.clone();
            let c_acts = c.clone();
            let bot_save = bot_id.clone();
            let c_save = c.clone();
            let manifest_bot_name = bot_name.clone();
            let entity_enable = cx.entity();
            let entity_mirror = cx.entity();
            ui::card(p)
                .id("bots-conn-slack")
                .debug_selector(|| "bots-conn-slack".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(ui::label(
                            p,
                            text::CONN_SLACK.get(),
                            Some(text::CONN_SLACK_DESC.get()),
                        ))
                        .child(ui::controls().child(ui::badge(p, sl_tone, sl_label)).child(
                            ui::switch(
                                p,
                                "bots-slack-enable",
                                c.slack.enabled,
                                self.busy,
                                move |next, _, cx| {
                                    let mut next_c = c_toggle.clone();
                                    next_c.slack.enabled = next;
                                    let bid = bot_toggle.clone();
                                    entity_enable.update(cx, |this, cx| {
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            ),
                        )),
                )
                .children(sl_err.map(|e| ui::error_banner(p, e)))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "slack-app-token",
                        label: text::SLACK_APP_TOKEN_LABEL.get(),
                        configured: c.slack.app_token_secret_ref.is_some(),
                    },
                    self.connectors.slack_app_token.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_app.clone();
                        let sref = next_c
                            .slack
                            .app_token_secret_ref
                            .clone()
                            .unwrap_or_else(|| connector_secret_ref(bot_app.as_str(), "slack-app"));
                        next_c.slack.app_token_secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_app.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "slack-bot-token",
                        label: text::SLACK_BOT_TOKEN_LABEL.get(),
                        configured: c.slack.bot_token_secret_ref.is_some(),
                    },
                    self.connectors.slack_bot_token.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_tok.clone();
                        let sref = next_c
                            .slack
                            .bot_token_secret_ref
                            .clone()
                            .unwrap_or_else(|| connector_secret_ref(bot_tok.as_str(), "slack-bot"));
                        next_c.slack.bot_token_secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_tok.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::SLACK_MIRROR_LABEL.get()),
                        )
                        .child(ui::switch(
                            p,
                            "bots-slack-mirror",
                            c.slack.mirror_desktop,
                            self.busy,
                            move |next, _, cx| {
                                let mut next_c = c_mirror.clone();
                                next_c.slack.mirror_desktop = next;
                                let bid = bot_mirror.clone();
                                entity_mirror.update(cx, |this, cx| {
                                    this.save_connectors_config(bid, next_c, cx);
                                });
                            },
                        )),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::ALLOWED_CHANNELS_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .slack_channels
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::ALLOWED_USERS_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .slack_users
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        ),
                )
                .child(Self::render_acts_for_control(
                    p,
                    "slack",
                    &c.slack.acts_for,
                    move |this, val, cx| {
                        let mut next_c = c_acts.clone();
                        next_c.slack.acts_for = val;
                        this.save_connectors_config(bot_acts.clone(), next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            ui::secondary(
                                "bots-slack-copy-manifest",
                                text::SLACK_COPY_MANIFEST_BTN.get(),
                            )
                            .debug_selector(|| "bots-slack-copy-manifest".into())
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.copy_slack_manifest(&manifest_bot_name, cx);
                                },
                            )),
                        )
                        .child(
                            ui::secondary("bots-slack-save", text::SAVE_CONNECTOR_BTN.get())
                                .debug_selector(|| "bots-slack-save".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let mut next_c = c_save.clone();
                                    if let Some(i) = &this.connectors.slack_channels {
                                        next_c.slack.allowed_channel_ids =
                                            parse_csv_strings(&i.read(cx).value());
                                    }
                                    if let Some(i) = &this.connectors.slack_users {
                                        next_c.slack.allowed_user_ids =
                                            parse_csv_strings(&i.read(cx).value());
                                    }
                                    this.save_connectors_config(bot_save.clone(), next_c, cx);
                                })),
                        ),
                )
        };

        // 3. Discord Gateway card
        let (dc_state, dc_err) = self.status_for(ExternalSource::Discord, c.discord.enabled);
        let (dc_tone, dc_label) = health_badge_spec(dc_state);
        let discord_card = {
            let bot_toggle = bot_id.clone();
            let c_toggle = c.clone();
            let bot_secret = bot_id.clone();
            let c_secret = c.clone();
            let bot_acts = bot_id.clone();
            let c_acts = c.clone();
            let bot_save = bot_id.clone();
            let c_save = c.clone();
            let entity = cx.entity();
            ui::card(p)
                .id("bots-conn-discord")
                .debug_selector(|| "bots-conn-discord".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(ui::label(
                            p,
                            text::CONN_DISCORD.get(),
                            Some(text::CONN_DISCORD_DESC.get()),
                        ))
                        .child(ui::controls().child(ui::badge(p, dc_tone, dc_label)).child(
                            ui::switch(
                                p,
                                "bots-discord-enable",
                                c.discord.enabled,
                                self.busy,
                                move |next, _, cx| {
                                    let mut next_c = c_toggle.clone();
                                    next_c.discord.enabled = next;
                                    let bid = bot_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            ),
                        )),
                )
                .children(dc_err.map(|e| ui::error_banner(p, e)))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "discord-token",
                        label: text::DISCORD_TOKEN_LABEL.get(),
                        configured: c.discord.bot_token_secret_ref.is_some(),
                    },
                    self.connectors.discord_token.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_secret.clone();
                        let sref =
                            next_c
                                .discord
                                .bot_token_secret_ref
                                .clone()
                                .unwrap_or_else(|| {
                                    connector_secret_ref(bot_secret.as_str(), "discord")
                                });
                        next_c.discord.bot_token_secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_secret.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(140.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::ALLOWED_GUILDS_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .discord_guilds
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(140.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::ALLOWED_CHANNELS_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .discord_channels
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(140.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::ALLOWED_USERS_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .discord_users
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        ),
                )
                .child(Self::render_acts_for_control(
                    p,
                    "discord",
                    &c.discord.acts_for,
                    move |this, val, cx| {
                        let mut next_c = c_acts.clone();
                        next_c.discord.acts_for = val;
                        this.save_connectors_config(bot_acts.clone(), next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div().flex().justify_end().child(
                        ui::secondary("bots-discord-save", text::SAVE_CONNECTOR_BTN.get())
                            .debug_selector(|| "bots-discord-save".into())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut next_c = c_save.clone();
                                if let Some(i) = &this.connectors.discord_guilds {
                                    next_c.discord.allowed_guild_ids =
                                        parse_csv_strings(&i.read(cx).value());
                                }
                                if let Some(i) = &this.connectors.discord_channels {
                                    next_c.discord.allowed_channel_ids =
                                        parse_csv_strings(&i.read(cx).value());
                                }
                                if let Some(i) = &this.connectors.discord_users {
                                    next_c.discord.allowed_user_ids =
                                        parse_csv_strings(&i.read(cx).value());
                                }
                                this.save_connectors_config(bot_save.clone(), next_c, cx);
                            })),
                    ),
                )
        };

        // 4. Google (BYO OAuth) card
        let (gg_state, gg_err) = self.status_for(ExternalSource::Google, c.google.enabled);
        let (gg_tone, gg_label) = health_badge_spec(gg_state);
        let google_card = {
            let bot_toggle = bot_id.clone();
            let c_toggle = c.clone();
            let bot_secret = bot_id.clone();
            let c_secret = c.clone();
            let bot_oauth = bot_id.clone();
            let bot_save = bot_id.clone();
            let c_save = c.clone();
            let entity = cx.entity();
            let svc = c.google.services;
            let services_grid = [
                ("gmail", text::GOOGLE_SVC_GMAIL.get(), svc.gmail),
                ("calendar", text::GOOGLE_SVC_CALENDAR.get(), svc.calendar),
                ("drive", text::GOOGLE_SVC_DRIVE.get(), svc.drive),
                ("docs", text::GOOGLE_SVC_DOCS.get(), svc.docs),
                ("sheets", text::GOOGLE_SVC_SHEETS.get(), svc.sheets),
                ("contacts", text::GOOGLE_SVC_CONTACTS.get(), svc.contacts),
            ];
            let mut svc_chips = div().flex().flex_wrap().items_center().gap_1p5();
            for (key, lbl, active) in services_grid {
                let bot_svc = bot_id.clone();
                let c_svc = c.clone();
                let chip_id = format!("bots-google-svc-{key}");
                let chip_sel = chip_id.clone();
                svc_chips = svc_chips.child(
                    ui::segment(p, SharedString::from(chip_id), lbl, active)
                        .debug_selector(move || chip_sel.clone())
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut next_c = c_svc.clone();
                            match key {
                                "gmail" => next_c.google.services.gmail = !active,
                                "calendar" => next_c.google.services.calendar = !active,
                                "drive" => next_c.google.services.drive = !active,
                                "docs" => next_c.google.services.docs = !active,
                                "sheets" => next_c.google.services.sheets = !active,
                                "contacts" => next_c.google.services.contacts = !active,
                                _ => {}
                            }
                            this.save_connectors_config(bot_svc.clone(), next_c, cx);
                        })),
                );
            }
            ui::card(p)
                .id("bots-conn-google")
                .debug_selector(|| "bots-conn-google".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(ui::label(
                            p,
                            text::CONN_GOOGLE.get(),
                            Some(text::CONN_GOOGLE_DESC.get()),
                        ))
                        .child(ui::controls().child(ui::badge(p, gg_tone, gg_label)).child(
                            ui::switch(
                                p,
                                "bots-google-enable",
                                c.google.enabled,
                                self.busy,
                                move |next, _, cx| {
                                    let mut next_c = c_toggle.clone();
                                    next_c.google.enabled = next;
                                    let bid = bot_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            ),
                        )),
                )
                .children(gg_err.map(|e| ui::error_banner(p, e)))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "google-oauth-json",
                        label: text::GOOGLE_OAUTH_JSON_LABEL.get(),
                        configured: c.google.oauth_client_secret_ref.is_some(),
                    },
                    self.connectors.google_oauth_json.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_secret.clone();
                        let sref = next_c
                            .google
                            .oauth_client_secret_ref
                            .clone()
                            .unwrap_or_else(|| {
                                connector_secret_ref(bot_secret.as_str(), "google-oauth")
                            });
                        next_c.google.oauth_client_secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_secret.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(ui::field_label(text::GOOGLE_SERVICES_LABEL.get(), None, p))
                        .child(svc_chips),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(ui::field_label(text::POLL_INTERVAL_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .google_poll
                                        .as_ref()
                                        .map(|i| div().w(px(90.)).child(Input::new(i).small())),
                                ),
                        )
                        .child(
                            ui::controls()
                                .child(
                                    ui::primary(
                                        "bots-google-start-oauth",
                                        text::GOOGLE_START_OAUTH_BTN.get(),
                                    )
                                    .debug_selector(|| "bots-google-start-oauth".into())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.start_google_oauth(bot_oauth.clone(), cx);
                                        },
                                    )),
                                )
                                .child(
                                    ui::secondary(
                                        "bots-google-save",
                                        text::SAVE_CONNECTOR_BTN.get(),
                                    )
                                    .debug_selector(|| "bots-google-save".into())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let mut next_c = c_save.clone();
                                            if let Some(i) = &this.connectors.google_poll {
                                                next_c.google.poll_interval_secs = i
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .parse::<u64>()
                                                    .unwrap_or(DEFAULT_CONNECTOR_POLL_INTERVAL_SECS)
                                                    .max(15);
                                            }
                                            this.save_connectors_config(
                                                bot_save.clone(),
                                                next_c,
                                                cx,
                                            );
                                        },
                                    )),
                                ),
                        ),
                )
        };

        // 5. GitHub Tracker card
        let (gh_state, gh_err) = self.status_for(ExternalSource::GitHub, c.github.enabled);
        let (gh_tone, gh_label) = health_badge_spec(gh_state);
        let github_card = {
            let bot_toggle = bot_id.clone();
            let c_toggle = c.clone();
            let bot_ghcli = bot_id.clone();
            let c_ghcli = c.clone();
            let bot_secret = bot_id.clone();
            let c_secret = c.clone();
            let bot_sync_gh = bot_id.clone();
            let bot_save = bot_id.clone();
            let c_save = c.clone();
            let entity_enable = cx.entity();
            let entity_cli = cx.entity();
            ui::card(p)
                .id("bots-conn-github")
                .debug_selector(|| "bots-conn-github".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(ui::label(
                            p,
                            text::CONN_GITHUB.get(),
                            Some(text::CONN_GITHUB_DESC.get()),
                        ))
                        .child(ui::controls().child(ui::badge(p, gh_tone, gh_label)).child(
                            ui::switch(
                                p,
                                "bots-github-enable",
                                c.github.enabled,
                                self.busy,
                                move |next, _, cx| {
                                    let mut next_c = c_toggle.clone();
                                    next_c.github.enabled = next;
                                    let bid = bot_toggle.clone();
                                    entity_enable.update(cx, |this, cx| {
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            ),
                        )),
                )
                .children(gh_err.map(|e| ui::error_banner(p, e)))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "github-token",
                        label: text::GITHUB_TOKEN_LABEL.get(),
                        configured: c.github.token_secret_ref.is_some(),
                    },
                    self.connectors.github_token.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_secret.clone();
                        let sref =
                            next_c.github.token_secret_ref.clone().unwrap_or_else(|| {
                                connector_secret_ref(bot_secret.as_str(), "github")
                            });
                        next_c.github.token_secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_secret.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::GITHUB_USE_GH_CLI_LABEL.get()),
                        )
                        .child(ui::switch(
                            p,
                            "bots-github-use-cli",
                            c.github.use_gh_cli,
                            self.busy,
                            move |next, _, cx| {
                                let mut next_c = c_ghcli.clone();
                                next_c.github.use_gh_cli = next;
                                let bid = bot_ghcli.clone();
                                entity_cli.update(cx, |this, cx| {
                                    this.save_connectors_config(bid, next_c, cx);
                                });
                            },
                        )),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(200.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::GITHUB_REPOS_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .github_repos
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        )
                        .child(
                            div()
                                .w(px(110.))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::POLL_INTERVAL_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .github_poll
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            ui::ghost("bots-github-sync", text::SYNC_NOW_BTN.get())
                                .debug_selector(|| "bots-github-sync".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.trigger_sync_now(
                                        bot_sync_gh.clone(),
                                        Some(ExternalSource::GitHub),
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            ui::secondary("bots-github-save", text::SAVE_CONNECTOR_BTN.get())
                                .debug_selector(|| "bots-github-save".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let mut next_c = c_save.clone();
                                    if let Some(i) = &this.connectors.github_repos {
                                        next_c.github.repos =
                                            parse_csv_strings(&i.read(cx).value());
                                    }
                                    if let Some(i) = &this.connectors.github_poll {
                                        next_c.github.poll_interval_secs = i
                                            .read(cx)
                                            .value()
                                            .trim()
                                            .parse::<u64>()
                                            .unwrap_or(DEFAULT_CONNECTOR_POLL_INTERVAL_SECS)
                                            .max(15);
                                    }
                                    this.save_connectors_config(bot_save.clone(), next_c, cx);
                                })),
                        ),
                )
        };

        // 6. Linear Tracker card
        let (ln_state, ln_err) = self.status_for(ExternalSource::Linear, c.linear.enabled);
        let (ln_tone, ln_label) = health_badge_spec(ln_state);
        let linear_card = {
            let bot_toggle = bot_id.clone();
            let c_toggle = c.clone();
            let bot_secret = bot_id.clone();
            let c_secret = c.clone();
            let bot_sync_ln = bot_id.clone();
            let bot_save = bot_id.clone();
            let c_save = c.clone();
            let entity = cx.entity();
            ui::card(p)
                .id("bots-conn-linear")
                .debug_selector(|| "bots-conn-linear".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(ui::label(
                            p,
                            text::CONN_LINEAR.get(),
                            Some(text::CONN_LINEAR_DESC.get()),
                        ))
                        .child(ui::controls().child(ui::badge(p, ln_tone, ln_label)).child(
                            ui::switch(
                                p,
                                "bots-linear-enable",
                                c.linear.enabled,
                                self.busy,
                                move |next, _, cx| {
                                    let mut next_c = c_toggle.clone();
                                    next_c.linear.enabled = next;
                                    let bid = bot_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            ),
                        )),
                )
                .children(ln_err.map(|e| ui::error_banner(p, e)))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "linear-api-key",
                        label: text::LINEAR_API_KEY_LABEL.get(),
                        configured: c.linear.api_key_secret_ref.is_some(),
                    },
                    self.connectors.linear_api_key.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_secret.clone();
                        let sref =
                            next_c.linear.api_key_secret_ref.clone().unwrap_or_else(|| {
                                connector_secret_ref(bot_secret.as_str(), "linear")
                            });
                        next_c.linear.api_key_secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_secret.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(200.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::LINEAR_TEAMS_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .linear_teams
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        )
                        .child(
                            div()
                                .w(px(110.))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::POLL_INTERVAL_LABEL.get(), None, p))
                                .children(
                                    self.connectors
                                        .linear_poll
                                        .as_ref()
                                        .map(|i| Input::new(i).small()),
                                ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            ui::ghost("bots-linear-sync", text::SYNC_NOW_BTN.get())
                                .debug_selector(|| "bots-linear-sync".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.trigger_sync_now(
                                        bot_sync_ln.clone(),
                                        Some(ExternalSource::Linear),
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            ui::secondary("bots-linear-save", text::SAVE_CONNECTOR_BTN.get())
                                .debug_selector(|| "bots-linear-save".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let mut next_c = c_save.clone();
                                    if let Some(i) = &this.connectors.linear_teams {
                                        next_c.linear.team_keys =
                                            parse_csv_strings(&i.read(cx).value());
                                    }
                                    if let Some(i) = &this.connectors.linear_poll {
                                        next_c.linear.poll_interval_secs = i
                                            .read(cx)
                                            .value()
                                            .trim()
                                            .parse::<u64>()
                                            .unwrap_or(DEFAULT_CONNECTOR_POLL_INTERVAL_SECS)
                                            .max(15);
                                    }
                                    this.save_connectors_config(bot_save.clone(), next_c, cx);
                                })),
                        ),
                )
        };

        // 7. Composio (BYO API Key + 24 Curated Apps) card
        let (cp_state, cp_err) = self.status_for(ExternalSource::Composio, c.composio.enabled);
        let (cp_tone, cp_label) = health_badge_spec(cp_state);
        let composio_card = {
            let bot_toggle = bot_id.clone();
            let c_toggle = c.clone();
            let bot_secret = bot_id.clone();
            let c_secret = c.clone();
            let entity = cx.entity();
            let apps_heading = text::COMPOSIO_APPS_LABEL.format(&[
                ("selected", &c.composio.enabled_apps.len().to_string()),
                ("total", &COMPOSIO_CURATED_APPS.len().to_string()),
            ]);
            let mut apps_grid = div()
                .id("bots-composio-apps-grid")
                .debug_selector(|| "bots-composio-apps-grid".into())
                .w_full()
                .flex()
                .flex_wrap()
                .gap_1p5();
            for app in COMPOSIO_CURATED_APPS {
                let selected = c.composio.enabled_apps.iter().any(|slug| slug == app.slug);
                let bot_app = bot_id.clone();
                let c_app = c.clone();
                let slug = app.slug;
                let aid = format!("bots-composio-app-{slug}");
                let asel = aid.clone();
                apps_grid = apps_grid.child(
                    ui::segment(p, SharedString::from(aid), app.name, selected)
                        .debug_selector(move || asel.clone())
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut next_c = c_app.clone();
                            if selected {
                                next_c.composio.enabled_apps.retain(|s| s != slug);
                            } else {
                                next_c.composio.enabled_apps.push(slug.to_owned());
                            }
                            this.save_connectors_config(bot_app.clone(), next_c, cx);
                        })),
                );
            }
            ui::card(p)
                .id("bots-conn-composio")
                .debug_selector(|| "bots-conn-composio".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(ui::label(
                            p,
                            text::CONN_COMPOSIO.get(),
                            Some(text::CONN_COMPOSIO_DESC.get()),
                        ))
                        .child(ui::controls().child(ui::badge(p, cp_tone, cp_label)).child(
                            ui::switch(
                                p,
                                "bots-composio-enable",
                                c.composio.enabled,
                                self.busy,
                                move |next, _, cx| {
                                    let mut next_c = c_toggle.clone();
                                    next_c.composio.enabled = next;
                                    let bid = bot_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            ),
                        )),
                )
                .children(cp_err.map(|e| ui::error_banner(p, e)))
                .child(Self::render_secret_row(
                    p,
                    SecretRowSpec {
                        id_prefix: "composio-api-key",
                        label: text::COMPOSIO_API_KEY_LABEL.get(),
                        configured: c.composio.api_key_secret_ref.is_some(),
                    },
                    self.connectors.composio_api_key.as_ref(),
                    self.busy,
                    move |this, val, _, cx| {
                        let mut next_c = c_secret.clone();
                        let sref =
                            next_c
                                .composio
                                .api_key_secret_ref
                                .clone()
                                .unwrap_or_else(|| {
                                    connector_secret_ref(bot_secret.as_str(), "composio")
                                });
                        next_c.composio.api_key_secret_ref = Some(sref.clone());
                        this.save_connector_secret(bot_secret.clone(), sref, val, next_c, cx);
                    },
                    cx,
                ))
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.muted_foreground)
                        .child(apps_heading),
                )
                .child(apps_grid)
        };

        // Pending Access Requests section
        let pending_count = self
            .connectors
            .bot_access_requests
            .iter()
            .filter(|r| r.status == BotAccessRequestStatus::Pending)
            .count();
        let access_heading =
            text::ACCESS_REQUESTS_HEADING.format(&[("n", &pending_count.to_string())]);
        let mut access_card = ui::card(p)
            .id("bots-access-requests")
            .debug_selector(|| "bots-access-requests".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(access_heading),
            );
        if self.connectors.bot_access_requests.is_empty() {
            access_card = access_card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::ACCESS_REQUESTS_EMPTY.get()),
            );
        } else {
            for (ridx, req) in self
                .connectors
                .bot_access_requests
                .iter()
                .cloned()
                .enumerate()
            {
                let (tone, st_lbl) = access_status_badge(req.status);
                let token_approve = req.token.clone();
                let token_decline = req.token.clone();
                let meta = format!(
                    "{} ({}) · {}",
                    req.sender_name, req.sender_id, req.channel_id
                );
                access_card = access_card.child(
                    div()
                        .id(SharedString::from(format!("bots-access-req-{ridx}")))
                        .debug_selector(move || format!("bots-access-req-{ridx}"))
                        .w_full()
                        .p_2p5()
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.35))
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(ui::badge(
                                    p,
                                    ui::BadgeTone::Neutral,
                                    Self::source_label(req.source),
                                ))
                                .child(ui::badge(p, tone, st_lbl))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.foreground)
                                        .child(meta),
                                ),
                        )
                        .when(req.status == BotAccessRequestStatus::Pending, |d| {
                            d.child(
                                ui::controls()
                                    .child(
                                        ui::primary(
                                            SharedString::from(format!(
                                                "bots-access-approve-{ridx}"
                                            )),
                                            text::ACCESS_APPROVE_BTN.get(),
                                        )
                                        .debug_selector(move || {
                                            format!("bots-access-approve-{ridx}")
                                        })
                                        .disabled(self.busy)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.decide_access_request(
                                                    token_approve.clone(),
                                                    true,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .child(
                                        ui::ghost(
                                            SharedString::from(format!(
                                                "bots-access-decline-{ridx}"
                                            )),
                                            text::ACCESS_DECLINE_BTN.get(),
                                        )
                                        .debug_selector(move || {
                                            format!("bots-access-decline-{ridx}")
                                        })
                                        .disabled(self.busy)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.decide_access_request(
                                                    token_decline.clone(),
                                                    false,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            )
                        }),
                );
            }
        }

        div()
            .id("bots-tab-connectors-body")
            .debug_selector(|| "bots-tab-connectors-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(toolbar)
            .child(access_card)
            .child(telegram_card_wrap(tg_card))
            .child(slack_card)
            .child(discord_card)
            .child(google_card)
            .child(github_card)
            .child(linear_card)
            .child(composio_card)
    }
}

fn telegram_card_wrap(card: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
    card
}
