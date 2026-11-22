// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 3 — Connectors & Access: setup, connectivity testing, secret rollback/clear,
//! `ActsFor::Member`, Google OAuth `.json` picker, default GitHub/Linear bot routing,
//! Composio account connections, cross-bot access requests, and pairing code redemption.

use gpui::{
    App, ClipboardItem, Context, Entity, FontWeight, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::bots::{
    ActsFor, BotAccessRequest, BotAccessRequestStatus, BotConfig, BotConnectorConfig,
    BotConnectorStatus, BotId, BotRpcRequest, BotRpcResponse, COMPOSIO_CURATED_APPS,
    ComposioConnectionInfo, ConnectorHealthState, DEFAULT_CONNECTOR_POLL_INTERVAL_SECS,
    DefaultTrackerBots, ExternalSource, generate_slack_socket_mode_manifest,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::{
    call_checked, connector_secret_ref, expect_ok, format_csv_i64s, format_csv_strings,
    parse_csv_i64s, parse_csv_strings,
};
use super::{View, clear_input_on_window, opt_input_composing};

#[derive(Default)]
pub(super) struct ConnectorsState {
    /// The bot page's Connectors tab: the connector whose page shows in the list's place.
    pub(super) page: Option<ExternalSource>,
    /// The bot page's Connectors tab: "Add to {bot}" is open.
    pub(super) adding: Option<()>,
    /// "Add to {bot}": the rail's pick (0 all apps, 1 the bot's own, then a category) and search.
    pub(super) add_rail: u8,
    pub(super) add_search: Option<Entity<InputState>>,
    pub(super) bot_statuses: Vec<BotConnectorStatus>,
    pub(super) bot_access_requests: Vec<BotAccessRequest>,
    pub(super) show_all_access_requests: bool,
    pub(super) default_tracker_bots: DefaultTrackerBots,
    pub(super) composio_connections: Vec<ComposioConnectionInfo>,
    pub(super) needs_sync_from_bot: bool,
    pub(super) busy: bool,
    // Telegram
    pub(super) tg_token: Option<Entity<InputState>>,
    pub(super) tg_chats: Option<Entity<InputState>>,
    pub(super) tg_users: Option<Entity<InputState>>,
    pub(super) tg_member: Option<Entity<InputState>>,
    // Slack
    pub(super) slack_app_token: Option<Entity<InputState>>,
    pub(super) slack_bot_token: Option<Entity<InputState>>,
    pub(super) slack_channels: Option<Entity<InputState>>,
    pub(super) slack_users: Option<Entity<InputState>>,
    pub(super) slack_member: Option<Entity<InputState>>,
    // Discord
    pub(super) discord_token: Option<Entity<InputState>>,
    pub(super) discord_guilds: Option<Entity<InputState>>,
    pub(super) discord_channels: Option<Entity<InputState>>,
    pub(super) discord_users: Option<Entity<InputState>>,
    pub(super) discord_member: Option<Entity<InputState>>,
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
    // Pairing code
    pub(super) pairing_code_input: Option<Entity<InputState>>,
    pub(super) pairing_sub: Option<Subscription>,
}

impl ConnectorsState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        [
            &self.tg_token,
            &self.tg_chats,
            &self.tg_users,
            &self.tg_member,
            &self.slack_app_token,
            &self.slack_bot_token,
            &self.slack_channels,
            &self.slack_users,
            &self.slack_member,
            &self.discord_token,
            &self.discord_guilds,
            &self.discord_channels,
            &self.discord_users,
            &self.discord_member,
            &self.google_oauth_json,
            &self.google_poll,
            &self.github_token,
            &self.github_repos,
            &self.github_poll,
            &self.linear_api_key,
            &self.linear_teams,
            &self.linear_poll,
            &self.composio_api_key,
            &self.pairing_code_input,
        ]
        .into_iter()
        .any(|slot| opt_input_composing(slot.as_ref(), window, app))
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        let secret_ph = text::SECRET_PLACEHOLDER.get();
        let member_ph = text::ACTS_FOR_MEMBER_PLACEHOLDER.get();
        let updates = [
            (&self.tg_token, secret_ph),
            (&self.tg_chats, text::TELEGRAM_CHATS_PLACEHOLDER.get()),
            (&self.tg_users, text::TELEGRAM_USERS_PLACEHOLDER.get()),
            (&self.tg_member, member_ph),
            (&self.slack_app_token, secret_ph),
            (&self.slack_bot_token, secret_ph),
            (&self.slack_member, member_ph),
            (&self.discord_token, secret_ph),
            (&self.discord_guilds, text::DISCORD_GUILDS_PLACEHOLDER.get()),
            (&self.discord_member, member_ph),
            (&self.google_oauth_json, secret_ph),
            (&self.github_token, secret_ph),
            (&self.github_repos, text::GITHUB_REPOS_PLACEHOLDER.get()),
            (&self.linear_api_key, secret_ph),
            (&self.linear_teams, text::LINEAR_TEAMS_PLACEHOLDER.get()),
            (&self.composio_api_key, secret_ph),
            (
                &self.pairing_code_input,
                text::PAIRING_CODE_PLACEHOLDER.get(),
            ),
        ];
        for (slot, placeholder) in updates {
            if let Some(input) = slot {
                input.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            }
        }
    }

    /// Snapshots all editable non-secret connector inputs onto `base` so toggles preserve unsaved edits.
    pub(super) fn collect_connector_inputs(
        &self,
        base: &BotConnectorConfig,
        cx: &App,
    ) -> BotConnectorConfig {
        let mut next = base.clone();
        if let Some(i) = &self.tg_chats {
            next.telegram.allowed_chat_ids = parse_csv_i64s(&i.read(cx).value());
        }
        if let Some(i) = &self.tg_users {
            next.telegram.allowed_user_ids = parse_csv_i64s(&i.read(cx).value());
        }
        if let Some(i) = &self.slack_channels {
            next.slack.allowed_channel_ids = parse_csv_strings(&i.read(cx).value());
        }
        if let Some(i) = &self.slack_users {
            next.slack.allowed_user_ids = parse_csv_strings(&i.read(cx).value());
        }
        if let Some(i) = &self.discord_guilds {
            next.discord.allowed_guild_ids = parse_csv_strings(&i.read(cx).value());
        }
        if let Some(i) = &self.discord_channels {
            next.discord.allowed_channel_ids = parse_csv_strings(&i.read(cx).value());
        }
        if let Some(i) = &self.discord_users {
            next.discord.allowed_user_ids = parse_csv_strings(&i.read(cx).value());
        }
        if let Some(i) = &self.google_poll {
            next.google.poll_interval_secs = parse_poll_secs(&i.read(cx).value());
        }
        if let Some(i) = &self.github_repos {
            next.github.repos = parse_csv_strings(&i.read(cx).value());
        }
        if let Some(i) = &self.github_poll {
            next.github.poll_interval_secs = parse_poll_secs(&i.read(cx).value());
        }
        if let Some(i) = &self.linear_teams {
            next.linear.team_keys = parse_csv_strings(&i.read(cx).value());
        }
        if let Some(i) = &self.linear_poll {
            next.linear.poll_interval_secs = parse_poll_secs(&i.read(cx).value());
        }
        next
    }
}

fn parse_poll_secs(raw: &str) -> u64 {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_CONNECTOR_POLL_INTERVAL_SECS)
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

#[derive(Clone, Copy)]
struct SecretRowSpec {
    id_prefix: &'static str,
    label: &'static str,
    configured: bool,
}

type ClearSecretCallback = Box<dyn Fn(&mut View, &mut Context<View>) + 'static>;

impl View {
    pub(super) fn ensure_connector_inputs(
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
            self.connectors.tg_member = Some(mk_plain(
                text::ACTS_FOR_MEMBER_PLACEHOLDER.get(),
                window,
                cx,
            ));
            self.connectors.slack_app_token = Some(mk_secret(window, cx));
            self.connectors.slack_bot_token = Some(mk_secret(window, cx));
            self.connectors.slack_channels =
                Some(mk_plain(text::ALLOWED_CHANNELS_LABEL.get(), window, cx));
            self.connectors.slack_users =
                Some(mk_plain(text::ALLOWED_USERS_LABEL.get(), window, cx));
            self.connectors.slack_member = Some(mk_plain(
                text::ACTS_FOR_MEMBER_PLACEHOLDER.get(),
                window,
                cx,
            ));
            self.connectors.discord_token = Some(mk_secret(window, cx));
            self.connectors.discord_guilds =
                Some(mk_plain(text::DISCORD_GUILDS_PLACEHOLDER.get(), window, cx));
            self.connectors.discord_channels =
                Some(mk_plain(text::ALLOWED_CHANNELS_LABEL.get(), window, cx));
            self.connectors.discord_users =
                Some(mk_plain(text::ALLOWED_USERS_LABEL.get(), window, cx));
            self.connectors.discord_member = Some(mk_plain(
                text::ACTS_FOR_MEMBER_PLACEHOLDER.get(),
                window,
                cx,
            ));
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
            let pairing = mk_plain(text::PAIRING_CODE_PLACEHOLDER.get(), window, cx);
            self.connectors.pairing_sub = Some(cx.subscribe_in(
                &pairing,
                window,
                |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !crate::jump::entity_input_is_composing(inp, window, cx)
                    {
                        let code = inp.read(cx).value().to_string();
                        this.redeem_pairing_code(&code, Some(inp.clone()), cx);
                    }
                },
            ));
            self.connectors.pairing_code_input = Some(pairing);
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
            let member_val = |acts: &ActsFor| match acts {
                ActsFor::Member(id) => id.clone(),
                _ => String::new(),
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
                &self.connectors.tg_member,
                member_val(&c.telegram.acts_for),
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
                &self.connectors.slack_member,
                member_val(&c.slack.acts_for),
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
                &self.connectors.discord_member,
                member_val(&c.discord.acts_for),
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
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.notice = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = call_checked(
                        &rpc,
                        &BotRpcRequest::SetConnectorConfig {
                            bot_id: bot_id_bg.clone(),
                            connectors: Box::new(connectors),
                        },
                    )?;
                    let statuses = match call_checked(
                        &rpc,
                        &BotRpcRequest::ConnectorStatus {
                            bot_id: Some(bot_id_bg),
                        },
                    ) {
                        Ok(BotRpcResponse::ConnectorStatuses(s)) => Some(s),
                        _ => None,
                    };
                    Ok::<_, String>((resp, statuses))
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok((BotRpcResponse::Bot(updated), statuses)) => {
                        this.upsert_bot_config(*updated);
                        if let Some(st) = statuses {
                            this.connectors.bot_statuses = st;
                        }
                        this.notice = Some(SharedString::from(text::CONNECTOR_SAVED.get()));
                    }
                    Ok((_, statuses)) => {
                        if let Some(st) = statuses {
                            this.connectors.bot_statuses = st;
                        }
                        this.notice = Some(SharedString::from(text::CONNECTOR_SAVED.get()));
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

    pub fn save_connector_secret(
        &mut self,
        bot_id: BotId,
        secret_ref: String,
        value: &str,
        updated_connectors: BotConnectorConfig,
        clear_input: Option<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let trimmed = value.trim().to_owned();
        if trimmed.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.notice = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SetSecret {
                            secret_ref: secret_ref.clone(),
                            value: trimmed,
                        },
                    )?;
                    match call_checked(
                        &rpc,
                        &BotRpcRequest::SetConnectorConfig {
                            bot_id,
                            connectors: Box::new(updated_connectors),
                        },
                    ) {
                        Ok(resp) => Ok(resp),
                        Err(err) => {
                            drop(rpc(&BotRpcRequest::DeleteSecret { secret_ref }));
                            Err(text::ROLLBACK_NOTICE.format(&[("err", &err)]))
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        this.notice = Some(SharedString::from(text::SECRET_SAVED.get()));
                    }
                    Ok(_) => {
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        this.notice = Some(SharedString::from(text::SECRET_SAVED.get()));
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

    pub fn delete_connector_secret(
        &mut self,
        bot_id: BotId,
        secret_ref: String,
        updated_connectors: BotConnectorConfig,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.notice = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::DeleteSecret { secret_ref })?;
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SetConnectorConfig {
                            bot_id,
                            connectors: Box::new(updated_connectors),
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SECRET_DELETED.get()));
                    }
                    Ok(_) => {
                        this.notice = Some(SharedString::from(text::SECRET_DELETED.get()));
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

    pub fn reload_connectors(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::ReloadConnectors {
                            bot_id: Some(bot_id_bg.clone()),
                        },
                    )?;
                    call_checked(
                        &rpc,
                        &BotRpcRequest::ConnectorStatus {
                            bot_id: Some(bot_id_bg),
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(BotRpcResponse::ConnectorStatuses(list)) => {
                        this.connectors.bot_statuses = list;
                        this.notice = Some(SharedString::from(text::CONNECTOR_RELOADED.get()));
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected ReloadConnectors response: {other:?}"
                        )));
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

    pub fn trigger_sync_now(
        &mut self,
        bot_id: BotId,
        source: Option<ExternalSource>,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::TriggerSyncNow { bot_id, source })
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(_) => {
                        this.notice = Some(SharedString::from(text::SYNC_TRIGGERED.get()));
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

    pub fn test_connector(
        &mut self,
        bot_id: BotId,
        source: ExternalSource,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.notice = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::TestConnector { bot_id, source })
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(BotRpcResponse::ConnectorTest(result)) => {
                        let detail = match result.account_label {
                            Some(acct) if !acct.is_empty() => {
                                format!("{acct} ({})", result.detail)
                            }
                            _ => result.detail,
                        };
                        if result.ok {
                            this.notice = Some(SharedString::from(
                                text::TEST_CONNECTOR_OK.format(&[("detail", &detail)]),
                            ));
                        } else {
                            this.error = Some(SharedString::from(
                                text::TEST_CONNECTOR_FAIL.format(&[("detail", &detail)]),
                            ));
                        }
                    }
                    Ok(_) => {
                        this.notice = Some(SharedString::from(text::SAVED.get()));
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

    pub fn start_google_oauth(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::StartGoogleOAuth { bot_id }) },
                )
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(BotRpcResponse::GoogleOAuthStarted { auth_url, .. }) => {
                        if auth_url.starts_with("https://") || auth_url.starts_with("http://") {
                            cx.open_url(&auth_url);
                        }
                        this.notice = Some(SharedString::from(text::GOOGLE_OAUTH_OPENED.get()));
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected StartGoogleOAuth response: {other:?}"
                        )));
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

    fn pick_google_oauth_json(
        &mut self,
        bot_id: BotId,
        mut next_c: BotConnectorConfig,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(text::GOOGLE_OAUTH_JSON_LABEL.get().into()),
        });
        let rpc = self.rpc.clone();
        let sref = next_c
            .google
            .oauth_client_secret_ref
            .clone()
            .unwrap_or_else(|| connector_secret_ref(bot_id.as_str(), "google-oauth"));
        next_c.google.oauth_client_secret_ref = Some(sref.clone());
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let res = cx
                .background_executor()
                .spawn(async move {
                    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SetSecret {
                            secret_ref: sref.clone(),
                            value: content,
                        },
                    )?;
                    match call_checked(
                        &rpc,
                        &BotRpcRequest::SetConnectorConfig {
                            bot_id,
                            connectors: Box::new(next_c),
                        },
                    ) {
                        Ok(r) => Ok(r),
                        Err(err) => {
                            drop(rpc(&BotRpcRequest::DeleteSecret { secret_ref: sref }));
                            Err(text::ROLLBACK_NOTICE.format(&[("err", &err)]))
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SECRET_SAVED.get()));
                    }
                    Ok(_) => {
                        this.notice = Some(SharedString::from(text::SECRET_SAVED.get()));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_default_tracker_bot(
        &mut self,
        source: ExternalSource,
        bot_id: Option<BotId>,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SetDefaultTrackerBot { source, bot_id },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(BotRpcResponse::DefaultTrackerBots(defaults)) => {
                        this.connectors.default_tracker_bots = defaults;
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Ok(_) => {
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn start_composio_connect(
        &mut self,
        bot_id: BotId,
        app_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::StartComposioConnect { bot_id, app_id },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(BotRpcResponse::ComposioConnectStarted { redirect_url, .. }) => {
                        if redirect_url.starts_with("https://")
                            || redirect_url.starts_with("http://")
                        {
                            cx.open_url(&redirect_url);
                        }
                        this.notice = Some(SharedString::from(text::GOOGLE_OAUTH_OPENED.get()));
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected StartComposioConnect response: {other:?}"
                        )));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn disconnect_composio_account(
        &mut self,
        bot_id: BotId,
        connection_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let cid = connection_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    expect_ok(
                        &rpc,
                        &BotRpcRequest::DisconnectComposioAccount {
                            bot_id,
                            connection_id: cid,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok(()) => {
                        this.connectors
                            .composio_connections
                            .retain(|c| c.id != connection_id);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
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
        if self.connectors.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::DecideAccessRequest { token, approve })
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
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
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected DecideAccessRequest response: {other:?}"
                        )));
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

    pub fn redeem_pairing_code(
        &mut self,
        code: &str,
        clear_input: Option<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) {
        if self.connectors.busy {
            return;
        }
        let trimmed = code.trim().to_owned();
        if trimmed.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        let selected_bot_id = self.selected_bot_id.clone();
        self.connectors.busy = true;
        self.error = None;
        self.connector_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp =
                        call_checked(&rpc, &BotRpcRequest::RedeemPairingCode { code: trimmed })?;
                    let all_req = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListAccessRequests { bot_id: None },
                    ) {
                        Ok(BotRpcResponse::AccessRequests(list)) => Some(list),
                        _ => None,
                    };
                    Ok::<_, String>((resp, all_req))
                })
                .await;
            this.update(cx, |this, cx| {
                this.connectors.busy = false;
                match res {
                    Ok((resp, all_req)) => {
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        if let BotRpcResponse::AccessRequestDecided(decided) = resp {
                            for list in [
                                &mut this.connectors.bot_access_requests,
                                &mut this.all_access_requests,
                            ] {
                                if let Some(slot) =
                                    list.iter_mut().find(|r| r.token == decided.token)
                                {
                                    *slot = decided.clone();
                                } else {
                                    list.push(decided.clone());
                                }
                            }
                        }
                        if let Some(all) = all_req {
                            this.all_access_requests.clone_from(&all);
                            if let Some(bid) = &selected_bot_id {
                                this.connectors.bot_access_requests =
                                    all.into_iter().filter(|r| &r.bot_id == bid).collect();
                            }
                        }
                        this.emit_badge(cx);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
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

    fn status_for(
        &self,
        source: ExternalSource,
        enabled: bool,
    ) -> (ConnectorHealthState, Option<&BotConnectorStatus>) {
        if let Some(st) = self
            .connectors
            .bot_statuses
            .iter()
            .find(|s| s.source == source)
        {
            (st.state, Some(st))
        } else if enabled {
            (ConnectorHealthState::Disconnected, None)
        } else {
            (ConnectorHealthState::Disabled, None)
        }
    }

    fn render_connector_status_meta(
        status: Option<&BotConnectorStatus>,
        p: &ui::Palette,
    ) -> Option<gpui::Div> {
        let st = status?;
        let mut lines = div().flex().flex_col().gap_1();
        let mut has_any = false;
        let mut meta_row = div().flex().flex_wrap().items_center().gap_1p5();
        if let Some(acct) = &st.account_label
            && !acct.trim().is_empty()
        {
            has_any = true;
            meta_row = meta_row.child(ui::badge(
                p,
                ui::BadgeTone::Neutral,
                text::CONNECTOR_ACCOUNT_LABEL.format(&[("name", acct)]),
            ));
        }
        if let Some(ms) = st.last_sync_ms
            && ms > 0
        {
            has_any = true;
            let secs = i64::try_from(ms / 1000).unwrap_or(i64::MAX);
            let ts = crate::local_time::format_timestamp(secs).unwrap_or_else(|| secs.to_string());
            meta_row = meta_row.child(ui::badge(
                p,
                ui::BadgeTone::Muted,
                text::LAST_SYNC_LABEL.format(&[("ts", &ts)]),
            ));
        }
        if has_any {
            lines = lines.child(meta_row);
        }
        if let Some(err) = &st.last_error {
            has_any = true;
            let formatted = match st.error_category {
                Some(cat) => format!("[{}] {err}", cat.as_str()),
                None => err.clone(),
            };
            lines = lines.child(ui::error_banner(p, formatted));
        }
        has_any.then_some(lines)
    }

    fn render_acts_for_control(
        p: &ui::Palette,
        prefix: &'static str,
        current: &ActsFor,
        member_input: Option<&Entity<InputState>>,
        busy: bool,
        on_pick: impl Fn(&mut View, ActsFor, &mut Context<View>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let is_member = matches!(current, ActsFor::Member(_));
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
        let member_cb = on_pick.clone();
        let member_btn_cb = on_pick.clone();
        let member_in_clone = member_input.cloned();
        let member_in_apply = member_input.cloned();
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
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
                    .child({
                        let entity_choice = cx.entity();
                        let entity_member = cx.entity();
                        ui::segmented_control(
                            p,
                            choices
                                .into_iter()
                                .map(move |(suffix, label, val, active)| {
                                    let cb = on_pick.clone();
                                    let ent = entity_choice.clone();
                                    let id = format!("bots-{prefix}-acts-{suffix}");
                                    let sel = id.clone();
                                    ui::segment(p, SharedString::from(id), label, active)
                                        .debug_selector(move || sel.clone())
                                        .disabled(busy)
                                        .on_click(move |_, _, cx| {
                                            ent.update(cx, |this, cx| cb(this, val.clone(), cx));
                                        })
                                })
                                .chain(std::iter::once({
                                    let id = format!("bots-{prefix}-acts-member");
                                    let sel = id.clone();
                                    ui::segment(
                                        p,
                                        SharedString::from(id),
                                        text::ACTS_FOR_MEMBER.get(),
                                        is_member,
                                    )
                                    .debug_selector(move || sel.clone())
                                    .disabled(busy)
                                    .on_click(
                                        move |_, _, cx| {
                                            entity_member.update(cx, |this, cx| {
                                                let m_id = member_in_clone
                                                    .as_ref()
                                                    .map(|i| i.read(cx).value().trim().to_owned())
                                                    .filter(|s| !s.is_empty())
                                                    .unwrap_or_else(|| "member".to_owned());
                                                member_cb(this, ActsFor::Member(m_id), cx);
                                            });
                                        },
                                    )
                                })),
                        )
                    }),
            )
            .when_some(member_in_apply.filter(|_| is_member), |d, inp| {
                let inp_read = inp.clone();
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(div().flex_1().min_w_0().child(Input::new(&inp).small()))
                        .child(
                            ui::secondary(
                                SharedString::from(format!("bots-{prefix}-acts-member-apply")),
                                text::SAVE.get(),
                            )
                            .disabled(busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let m_id = inp_read.read(cx).value().trim().to_owned();
                                    if m_id.is_empty() {
                                        this.error =
                                            Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
                                        cx.notify();
                                        return;
                                    }
                                    member_btn_cb(this, ActsFor::Member(m_id), cx);
                                },
                            )),
                        ),
                )
            })
    }

    fn render_secret_row(
        p: &ui::Palette,
        spec: SecretRowSpec,
        input: Option<&Entity<InputState>>,
        busy: bool,
        on_save: impl Fn(&mut View, &str, Option<Entity<InputState>>, &mut Context<View>) + 'static,
        on_clear: Option<ClearSecretCallback>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let input_clone = input.cloned();
        let btn_id = format!("bots-secret-save-{}", spec.id_prefix);
        let btn_sel = btn_id.clone();
        let clear_id = format!("bots-secret-clear-{}", spec.id_prefix);
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let value = input_clone
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                on_save(this, &value, input_clone.clone(), cx);
                            })),
                    )
                    .when(spec.configured, |d| {
                        d.when_some(on_clear, |dd, cb| {
                            dd.child(
                                ui::ghost(
                                    SharedString::from(clear_id),
                                    text::CLEAR_SECRET_BTN.get(),
                                )
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| cb(this, cx))),
                            )
                        })
                    }),
            )
    }

    pub(super) fn render_connectors_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.mode == super::Mode::Panel {
            return self.render_connectors_panel(bot, p, window, cx);
        }
        self.ensure_connector_inputs(bot, window, cx);
        let c = bot.connectors.clone();
        let bot_id = bot.id.clone();
        let bot_name = bot.name.clone();
        let busy = self.connectors.busy;

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
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.reload_connectors(bot_reload.clone(), cx);
                            })),
                    )
                    .child(
                        ui::secondary("bots-connectors-sync-all", text::SYNC_NOW_BTN.get())
                            .debug_selector(|| "bots-connectors-sync-all".into())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.trigger_sync_now(bot_sync.clone(), None, cx);
                            })),
                    ),
            );

        let tg_card = self.render_telegram_card(bot, &c, p, cx);
        let slack_card = self.render_slack_card(bot, &c, &bot_name, p, cx);
        let discord_card = self.render_discord_card(bot, &c, p, cx);
        let google_card = self.render_google_card(bot, &c, p, cx);
        let github_card = self.render_github_card(bot, &c, p, cx);
        let linear_card = self.render_linear_card(bot, &c, p, cx);
        let composio_card = self.render_composio_card(bot, &c, p, cx);
        let access_card = self.render_access_requests_card(&bot_id, p, cx);

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
            .child(tg_card)
            .child(slack_card)
            .child(discord_card)
            .child(google_card)
            .child(github_card)
            .child(linear_card)
            .child(composio_card)
    }

    pub(super) fn render_telegram_card(
        &self,
        bot: &BotConfig,
        c: &BotConnectorConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (tg_state, tg_status) = self.status_for(ExternalSource::Telegram, c.telegram.enabled);
        let (tg_tone, tg_label) = health_badge_spec(tg_state);
        let busy = self.connectors.busy;
        let entity = cx.entity();
        let bot_toggle = bot.id.clone();
        let bot_secret = bot.id.clone();
        let bot_clear = bot.id.clone();
        let bot_acts = bot.id.clone();
        let bot_save = bot.id.clone();
        let bot_test = bot.id.clone();
        let c_toggle = c.clone();
        let c_secret = c.clone();
        let c_clear = c.clone();
        let c_acts = c.clone();
        let c_save = c.clone();

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
                    .child(
                        ui::controls().child(ui::badge(p, tg_tone, tg_label)).child(
                            ui::switch(
                                p,
                                "bots-tg-enable",
                                c.telegram.enabled,
                                busy,
                                move |next, _, cx| {
                                    let bid = bot_toggle.clone();
                                    let base = c_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        let mut next_c =
                                            this.connectors.collect_connector_inputs(&base, cx);
                                        next_c.telegram.enabled = next;
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            )
                            .aria_label(format!(
                                "{} {}",
                                text::CONN_TELEGRAM.get(),
                                text::ENABLE_LABEL.get()
                            )),
                        ),
                    ),
            )
            .children(Self::render_connector_status_meta(tg_status, p))
            .child(Self::render_secret_row(
                p,
                SecretRowSpec {
                    id_prefix: "telegram-token",
                    label: text::TELEGRAM_TOKEN_LABEL.get(),
                    configured: c.telegram.secret_ref.is_some(),
                },
                self.connectors.tg_token.as_ref(),
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_secret, cx);
                    let sref =
                        next_c.telegram.secret_ref.clone().unwrap_or_else(|| {
                            connector_secret_ref(bot_secret.as_str(), "telegram")
                        });
                    next_c.telegram.secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_secret.clone(), sref, val, next_c, inp, cx);
                },
                c.telegram.secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_clear, cx);
                        next_c.telegram.secret_ref = None;
                        this.delete_connector_secret(bot_clear.clone(), sref.clone(), next_c, cx);
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
                cx,
            ))
            .child(
                div()
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
                            .child(ui::field_label(text::ALLOWED_CHATS_LABEL.get(), None, p))
                            .children(
                                self.connectors
                                    .tg_chats
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(200.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::ALLOWED_USERS_LABEL.get(), None, p))
                            .children(
                                self.connectors
                                    .tg_users
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    ),
            )
            .child(Self::render_acts_for_control(
                p,
                "tg",
                &c.telegram.acts_for,
                self.connectors.tg_member.as_ref(),
                busy,
                move |this, val, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_acts, cx);
                    next_c.telegram.acts_for = val;
                    this.save_connectors_config(bot_acts.clone(), next_c, cx);
                },
                cx,
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::ghost("bots-tg-test", text::TEST_CONNECTOR_BTN.get())
                            .debug_selector(|| "bots-tg-test".into())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.test_connector(bot_test.clone(), ExternalSource::Telegram, cx);
                            })),
                    )
                    .child(
                        ui::secondary("bots-tg-save", text::SAVE_CONNECTOR_BTN.get())
                            .debug_selector(|| "bots-tg-save".into())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let next_c = this.connectors.collect_connector_inputs(&c_save, cx);
                                this.save_connectors_config(bot_save.clone(), next_c, cx);
                            })),
                    ),
            )
    }

    pub(super) fn render_slack_card(
        &self,
        bot: &BotConfig,
        c: &BotConnectorConfig,
        bot_name: &str,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (sl_state, sl_status) = self.status_for(ExternalSource::Slack, c.slack.enabled);
        let (sl_tone, sl_label) = health_badge_spec(sl_state);
        let busy = self.connectors.busy;
        let entity = cx.entity();
        let entity_mirror = cx.entity();
        let bot_toggle = bot.id.clone();
        let bot_mirror = bot.id.clone();
        let bot_app = bot.id.clone();
        let bot_app_clear = bot.id.clone();
        let bot_token = bot.id.clone();
        let bot_token_clear = bot.id.clone();
        let bot_acts = bot.id.clone();
        let bot_save = bot.id.clone();
        let bot_test = bot.id.clone();
        let c_toggle = c.clone();
        let c_mirror = c.clone();
        let c_app = c.clone();
        let c_app_clear = c.clone();
        let c_bot = c.clone();
        let c_bot_clear = c.clone();
        let c_acts = c.clone();
        let c_save = c.clone();
        let bname = bot_name.to_owned();

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
                    .child(
                        ui::controls().child(ui::badge(p, sl_tone, sl_label)).child(
                            ui::switch(
                                p,
                                "bots-slack-enable",
                                c.slack.enabled,
                                busy,
                                move |next, _, cx| {
                                    let bid = bot_toggle.clone();
                                    let base = c_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        let mut next_c =
                                            this.connectors.collect_connector_inputs(&base, cx);
                                        next_c.slack.enabled = next;
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            )
                            .aria_label(format!(
                                "{} {}",
                                text::CONN_SLACK.get(),
                                text::ENABLE_LABEL.get()
                            )),
                        ),
                    ),
            )
            .children(Self::render_connector_status_meta(sl_status, p))
            .child(Self::render_secret_row(
                p,
                SecretRowSpec {
                    id_prefix: "slack-app-token",
                    label: text::SLACK_APP_TOKEN_LABEL.get(),
                    configured: c.slack.app_token_secret_ref.is_some(),
                },
                self.connectors.slack_app_token.as_ref(),
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_app, cx);
                    let sref = next_c
                        .slack
                        .app_token_secret_ref
                        .clone()
                        .unwrap_or_else(|| connector_secret_ref(bot_app.as_str(), "slack-app"));
                    next_c.slack.app_token_secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_app.clone(), sref, val, next_c, inp, cx);
                },
                c.slack.app_token_secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_app_clear, cx);
                        next_c.slack.app_token_secret_ref = None;
                        this.delete_connector_secret(
                            bot_app_clear.clone(),
                            sref.clone(),
                            next_c,
                            cx,
                        );
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
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
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_bot, cx);
                    let sref = next_c
                        .slack
                        .bot_token_secret_ref
                        .clone()
                        .unwrap_or_else(|| connector_secret_ref(bot_token.as_str(), "slack-bot"));
                    next_c.slack.bot_token_secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_token.clone(), sref, val, next_c, inp, cx);
                },
                c.slack.bot_token_secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_bot_clear, cx);
                        next_c.slack.bot_token_secret_ref = None;
                        this.delete_connector_secret(
                            bot_token_clear.clone(),
                            sref.clone(),
                            next_c,
                            cx,
                        );
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
                cx,
            ))
            .child(
                div()
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
                            .min_w(px(200.))
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(ui::field_label(text::SLACK_MIRROR_LABEL.get(), None, p))
                    .child(
                        ui::switch(
                            p,
                            "bots-slack-mirror",
                            c.slack.mirror_desktop,
                            busy,
                            move |next, _, cx| {
                                let bid = bot_mirror.clone();
                                let base = c_mirror.clone();
                                entity_mirror.update(cx, |this, cx| {
                                    let mut next_c =
                                        this.connectors.collect_connector_inputs(&base, cx);
                                    next_c.slack.mirror_desktop = next;
                                    this.save_connectors_config(bid, next_c, cx);
                                });
                            },
                        )
                        .aria_label(text::SLACK_MIRROR_LABEL.get()),
                    ),
            )
            .child(Self::render_acts_for_control(
                p,
                "slack",
                &c.slack.acts_for,
                self.connectors.slack_member.as_ref(),
                busy,
                move |this, val, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_acts, cx);
                    next_c.slack.acts_for = val;
                    this.save_connectors_config(bot_acts.clone(), next_c, cx);
                },
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        ui::ghost(
                            "bots-slack-copy-manifest",
                            text::SLACK_COPY_MANIFEST_BTN.get(),
                        )
                        .debug_selector(|| "bots-slack-copy-manifest".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_slack_manifest(&bname, cx);
                        })),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::ghost("bots-slack-test", text::TEST_CONNECTOR_BTN.get())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.test_connector(
                                            bot_test.clone(),
                                            ExternalSource::Slack,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ui::secondary("bots-slack-save", text::SAVE_CONNECTOR_BTN.get())
                                    .debug_selector(|| "bots-slack-save".into())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let next_c =
                                            this.connectors.collect_connector_inputs(&c_save, cx);
                                        this.save_connectors_config(bot_save.clone(), next_c, cx);
                                    })),
                            ),
                    ),
            )
    }

    pub(super) fn render_discord_card(
        &self,
        bot: &BotConfig,
        c: &BotConnectorConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (dc_state, dc_status) = self.status_for(ExternalSource::Discord, c.discord.enabled);
        let (dc_tone, dc_label) = health_badge_spec(dc_state);
        let busy = self.connectors.busy;
        let entity = cx.entity();
        let bot_toggle = bot.id.clone();
        let bot_secret = bot.id.clone();
        let bot_clear = bot.id.clone();
        let bot_acts = bot.id.clone();
        let bot_save = bot.id.clone();
        let bot_test = bot.id.clone();
        let c_toggle = c.clone();
        let c_secret = c.clone();
        let c_clear = c.clone();
        let c_acts = c.clone();
        let c_save = c.clone();

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
                    .child(
                        ui::controls().child(ui::badge(p, dc_tone, dc_label)).child(
                            ui::switch(
                                p,
                                "bots-discord-enable",
                                c.discord.enabled,
                                busy,
                                move |next, _, cx| {
                                    let bid = bot_toggle.clone();
                                    let base = c_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        let mut next_c =
                                            this.connectors.collect_connector_inputs(&base, cx);
                                        next_c.discord.enabled = next;
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            )
                            .aria_label(format!(
                                "{} {}",
                                text::CONN_DISCORD.get(),
                                text::ENABLE_LABEL.get()
                            )),
                        ),
                    ),
            )
            .children(Self::render_connector_status_meta(dc_status, p))
            .child(Self::render_secret_row(
                p,
                SecretRowSpec {
                    id_prefix: "discord-token",
                    label: text::DISCORD_TOKEN_LABEL.get(),
                    configured: c.discord.bot_token_secret_ref.is_some(),
                },
                self.connectors.discord_token.as_ref(),
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_secret, cx);
                    let sref = next_c
                        .discord
                        .bot_token_secret_ref
                        .clone()
                        .unwrap_or_else(|| connector_secret_ref(bot_secret.as_str(), "discord"));
                    next_c.discord.bot_token_secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_secret.clone(), sref, val, next_c, inp, cx);
                },
                c.discord.bot_token_secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_clear, cx);
                        next_c.discord.bot_token_secret_ref = None;
                        this.delete_connector_secret(bot_clear.clone(), sref.clone(), next_c, cx);
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(170.))
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
                            .min_w(px(170.))
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
                            .min_w(px(170.))
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
                self.connectors.discord_member.as_ref(),
                busy,
                move |this, val, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_acts, cx);
                    next_c.discord.acts_for = val;
                    this.save_connectors_config(bot_acts.clone(), next_c, cx);
                },
                cx,
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::ghost("bots-discord-test", text::TEST_CONNECTOR_BTN.get())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.test_connector(bot_test.clone(), ExternalSource::Discord, cx);
                            })),
                    )
                    .child(
                        ui::secondary("bots-discord-save", text::SAVE_CONNECTOR_BTN.get())
                            .debug_selector(|| "bots-discord-save".into())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let next_c = this.connectors.collect_connector_inputs(&c_save, cx);
                                this.save_connectors_config(bot_save.clone(), next_c, cx);
                            })),
                    ),
            )
    }

    pub(super) fn render_google_card(
        &self,
        bot: &BotConfig,
        c: &BotConnectorConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (gg_state, gg_status) = self.status_for(ExternalSource::Google, c.google.enabled);
        let (gg_tone, gg_label) = health_badge_spec(gg_state);
        let busy = self.connectors.busy;
        let entity = cx.entity();
        let bot_toggle = bot.id.clone();
        let bot_secret = bot.id.clone();
        let bot_clear = bot.id.clone();
        let bot_pick = bot.id.clone();
        let bot_oauth = bot.id.clone();
        let bot_save = bot.id.clone();
        let bot_test = bot.id.clone();
        let c_toggle = c.clone();
        let c_secret = c.clone();
        let c_clear = c.clone();
        let c_pick = c.clone();
        let c_save = c.clone();

        let services = [
            (
                "gmail",
                text::GOOGLE_SVC_GMAIL.get(),
                c.google.services.gmail,
            ),
            (
                "calendar",
                text::GOOGLE_SVC_CALENDAR.get(),
                c.google.services.calendar,
            ),
            (
                "drive",
                text::GOOGLE_SVC_DRIVE.get(),
                c.google.services.drive,
            ),
            ("docs", text::GOOGLE_SVC_DOCS.get(), c.google.services.docs),
            (
                "sheets",
                text::GOOGLE_SVC_SHEETS.get(),
                c.google.services.sheets,
            ),
            (
                "contacts",
                text::GOOGLE_SVC_CONTACTS.get(),
                c.google.services.contacts,
            ),
        ];
        let mut svc_pills = div().flex().flex_wrap().items_center().gap_1();
        for (key, label, active) in services {
            let bid = bot.id.clone();
            let base = c.clone();
            let sid = format!("bots-google-svc-{key}");
            let ssel = sid.clone();
            svc_pills = svc_pills.child(
                ui::segment(p, SharedString::from(sid), label, active)
                    .debug_selector(move || ssel.clone())
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut next_c = this.connectors.collect_connector_inputs(&base, cx);
                        match key {
                            "gmail" => next_c.google.services.gmail = !next_c.google.services.gmail,
                            "calendar" => {
                                next_c.google.services.calendar = !next_c.google.services.calendar;
                            }
                            "drive" => next_c.google.services.drive = !next_c.google.services.drive,
                            "docs" => next_c.google.services.docs = !next_c.google.services.docs,
                            "sheets" => {
                                next_c.google.services.sheets = !next_c.google.services.sheets;
                            }
                            "contacts" => {
                                next_c.google.services.contacts = !next_c.google.services.contacts;
                            }
                            _ => {}
                        }
                        this.save_connectors_config(bid.clone(), next_c, cx);
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
                    .child(
                        ui::controls()
                            .when(c.google.token_secret_ref.is_some(), |d| {
                                d.child(ui::badge(
                                    p,
                                    ui::BadgeTone::Success,
                                    text::GOOGLE_CONNECTED_BADGE.get(),
                                ))
                            })
                            .child(ui::badge(p, gg_tone, gg_label))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-google-enable",
                                    c.google.enabled,
                                    busy,
                                    move |next, _, cx| {
                                        let bid = bot_toggle.clone();
                                        let base = c_toggle.clone();
                                        entity.update(cx, |this, cx| {
                                            let mut next_c =
                                                this.connectors.collect_connector_inputs(&base, cx);
                                            next_c.google.enabled = next;
                                            this.save_connectors_config(bid, next_c, cx);
                                        });
                                    },
                                )
                                .aria_label(format!(
                                    "{} {}",
                                    text::CONN_GOOGLE.get(),
                                    text::ENABLE_LABEL.get()
                                )),
                            ),
                    ),
            )
            .children(Self::render_connector_status_meta(gg_status, p))
            .child(Self::render_secret_row(
                p,
                SecretRowSpec {
                    id_prefix: "google-oauth-json",
                    label: text::GOOGLE_OAUTH_JSON_LABEL.get(),
                    configured: c.google.oauth_client_secret_ref.is_some(),
                },
                self.connectors.google_oauth_json.as_ref(),
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_secret, cx);
                    let sref = next_c
                        .google
                        .oauth_client_secret_ref
                        .clone()
                        .unwrap_or_else(|| {
                            connector_secret_ref(bot_secret.as_str(), "google-oauth")
                        });
                    next_c.google.oauth_client_secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_secret.clone(), sref, val, next_c, inp, cx);
                },
                c.google.oauth_client_secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_clear, cx);
                        next_c.google.oauth_client_secret_ref = None;
                        this.delete_connector_secret(bot_clear.clone(), sref.clone(), next_c, cx);
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(ui::field_label(text::GOOGLE_SERVICES_LABEL.get(), None, p))
                    .child(svc_pills),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .w(px(140.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::POLL_INTERVAL_LABEL.get(), None, p))
                            .children(
                                self.connectors
                                    .google_poll
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::secondary(
                                    "bots-google-pick-json",
                                    text::GOOGLE_PICK_JSON_BTN.get(),
                                )
                                .disabled(busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let next_c =
                                            this.connectors.collect_connector_inputs(&c_pick, cx);
                                        this.pick_google_oauth_json(bot_pick.clone(), next_c, cx);
                                    },
                                )),
                            )
                            .child(
                                ui::secondary(
                                    "bots-google-start-oauth",
                                    text::GOOGLE_START_OAUTH_BTN.get(),
                                )
                                .debug_selector(|| "bots-google-start-oauth".into())
                                .disabled(busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.start_google_oauth(bot_oauth.clone(), cx);
                                    },
                                )),
                            )
                            .child(
                                ui::ghost("bots-google-test", text::TEST_CONNECTOR_BTN.get())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.test_connector(
                                            bot_test.clone(),
                                            ExternalSource::Google,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ui::secondary("bots-google-save", text::SAVE_CONNECTOR_BTN.get())
                                    .debug_selector(|| "bots-google-save".into())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let next_c =
                                            this.connectors.collect_connector_inputs(&c_save, cx);
                                        this.save_connectors_config(bot_save.clone(), next_c, cx);
                                    })),
                            ),
                    ),
            )
    }

    pub(super) fn render_github_card(
        &self,
        bot: &BotConfig,
        c: &BotConnectorConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (gh_state, gh_status) = self.status_for(ExternalSource::GitHub, c.github.enabled);
        let (gh_tone, gh_label) = health_badge_spec(gh_state);
        let is_default = self.connectors.default_tracker_bots.github.as_ref() == Some(&bot.id);
        let busy = self.connectors.busy;
        let entity = cx.entity();
        let entity_cli = cx.entity();
        let bot_toggle = bot.id.clone();
        let bot_cli = bot.id.clone();
        let bot_secret = bot.id.clone();
        let bot_clear = bot.id.clone();
        let bot_sync = bot.id.clone();
        let bot_save = bot.id.clone();
        let bot_default = bot.id.clone();
        let bot_test = bot.id.clone();
        let c_toggle = c.clone();
        let c_cli = c.clone();
        let c_secret = c.clone();
        let c_clear = c.clone();
        let c_save = c.clone();

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
                    .child(
                        ui::controls()
                            .when(is_default, |d| {
                                d.child(ui::badge(
                                    p,
                                    ui::BadgeTone::Success,
                                    text::DEFAULT_TRACKER_ACTIVE.get(),
                                ))
                            })
                            .child(ui::badge(p, gh_tone, gh_label))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-github-enable",
                                    c.github.enabled,
                                    busy,
                                    move |next, _, cx| {
                                        let bid = bot_toggle.clone();
                                        let base = c_toggle.clone();
                                        entity.update(cx, |this, cx| {
                                            let mut next_c =
                                                this.connectors.collect_connector_inputs(&base, cx);
                                            next_c.github.enabled = next;
                                            this.save_connectors_config(bid, next_c, cx);
                                        });
                                    },
                                )
                                .aria_label(format!(
                                    "{} {}",
                                    text::CONN_GITHUB.get(),
                                    text::ENABLE_LABEL.get()
                                )),
                            ),
                    ),
            )
            .children(Self::render_connector_status_meta(gh_status, p))
            .child(Self::render_secret_row(
                p,
                SecretRowSpec {
                    id_prefix: "github-token",
                    label: text::GITHUB_TOKEN_LABEL.get(),
                    configured: c.github.token_secret_ref.is_some(),
                },
                self.connectors.github_token.as_ref(),
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_secret, cx);
                    let sref = next_c
                        .github
                        .token_secret_ref
                        .clone()
                        .unwrap_or_else(|| connector_secret_ref(bot_secret.as_str(), "github"));
                    next_c.github.token_secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_secret.clone(), sref, val, next_c, inp, cx);
                },
                c.github.token_secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_clear, cx);
                        next_c.github.token_secret_ref = None;
                        this.delete_connector_secret(bot_clear.clone(), sref.clone(), next_c, cx);
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(ui::field_label(
                        text::GITHUB_USE_GH_CLI_LABEL.get(),
                        None,
                        p,
                    ))
                    .child(
                        ui::switch(
                            p,
                            "bots-github-gh-cli",
                            c.github.use_gh_cli,
                            busy,
                            move |next, _, cx| {
                                let bid = bot_cli.clone();
                                let base = c_cli.clone();
                                entity_cli.update(cx, |this, cx| {
                                    let mut next_c =
                                        this.connectors.collect_connector_inputs(&base, cx);
                                    next_c.github.use_gh_cli = next;
                                    this.save_connectors_config(bid, next_c, cx);
                                });
                            },
                        )
                        .aria_label(text::GITHUB_USE_GH_CLI_LABEL.get()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(220.))
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
                            .w(px(140.))
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
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        ui::ghost("bots-github-default", text::DEFAULT_TRACKER_LABEL.get())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let target = if is_default {
                                    None
                                } else {
                                    Some(bot_default.clone())
                                };
                                this.set_default_tracker_bot(ExternalSource::GitHub, target, cx);
                            })),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::ghost("bots-github-test", text::TEST_CONNECTOR_BTN.get())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.test_connector(
                                            bot_test.clone(),
                                            ExternalSource::GitHub,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ui::ghost("bots-github-sync", text::SYNC_NOW_BTN.get())
                                    .debug_selector(|| "bots-github-sync".into())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.trigger_sync_now(
                                            bot_sync.clone(),
                                            Some(ExternalSource::GitHub),
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ui::secondary("bots-github-save", text::SAVE_CONNECTOR_BTN.get())
                                    .debug_selector(|| "bots-github-save".into())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let next_c =
                                            this.connectors.collect_connector_inputs(&c_save, cx);
                                        this.save_connectors_config(bot_save.clone(), next_c, cx);
                                    })),
                            ),
                    ),
            )
    }

    pub(super) fn render_linear_card(
        &self,
        bot: &BotConfig,
        c: &BotConnectorConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (ln_state, ln_status) = self.status_for(ExternalSource::Linear, c.linear.enabled);
        let (ln_tone, ln_label) = health_badge_spec(ln_state);
        let is_default = self.connectors.default_tracker_bots.linear.as_ref() == Some(&bot.id);
        let busy = self.connectors.busy;
        let entity = cx.entity();
        let bot_toggle = bot.id.clone();
        let bot_secret = bot.id.clone();
        let bot_clear = bot.id.clone();
        let bot_sync = bot.id.clone();
        let bot_save = bot.id.clone();
        let bot_default = bot.id.clone();
        let bot_test = bot.id.clone();
        let c_toggle = c.clone();
        let c_secret = c.clone();
        let c_clear = c.clone();
        let c_save = c.clone();

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
                    .child(
                        ui::controls()
                            .when(is_default, |d| {
                                d.child(ui::badge(
                                    p,
                                    ui::BadgeTone::Success,
                                    text::DEFAULT_TRACKER_ACTIVE.get(),
                                ))
                            })
                            .child(ui::badge(p, ln_tone, ln_label))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-linear-enable",
                                    c.linear.enabled,
                                    busy,
                                    move |next, _, cx| {
                                        let bid = bot_toggle.clone();
                                        let base = c_toggle.clone();
                                        entity.update(cx, |this, cx| {
                                            let mut next_c =
                                                this.connectors.collect_connector_inputs(&base, cx);
                                            next_c.linear.enabled = next;
                                            this.save_connectors_config(bid, next_c, cx);
                                        });
                                    },
                                )
                                .aria_label(format!(
                                    "{} {}",
                                    text::CONN_LINEAR.get(),
                                    text::ENABLE_LABEL.get()
                                )),
                            ),
                    ),
            )
            .children(Self::render_connector_status_meta(ln_status, p))
            .child(Self::render_secret_row(
                p,
                SecretRowSpec {
                    id_prefix: "linear-api-key",
                    label: text::LINEAR_API_KEY_LABEL.get(),
                    configured: c.linear.api_key_secret_ref.is_some(),
                },
                self.connectors.linear_api_key.as_ref(),
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_secret, cx);
                    let sref = next_c
                        .linear
                        .api_key_secret_ref
                        .clone()
                        .unwrap_or_else(|| connector_secret_ref(bot_secret.as_str(), "linear"));
                    next_c.linear.api_key_secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_secret.clone(), sref, val, next_c, inp, cx);
                },
                c.linear.api_key_secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_clear, cx);
                        next_c.linear.api_key_secret_ref = None;
                        this.delete_connector_secret(bot_clear.clone(), sref.clone(), next_c, cx);
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(220.))
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
                            .w(px(140.))
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
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        ui::ghost("bots-linear-default", text::DEFAULT_TRACKER_LABEL.get())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let target = if is_default {
                                    None
                                } else {
                                    Some(bot_default.clone())
                                };
                                this.set_default_tracker_bot(ExternalSource::Linear, target, cx);
                            })),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::ghost("bots-linear-test", text::TEST_CONNECTOR_BTN.get())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.test_connector(
                                            bot_test.clone(),
                                            ExternalSource::Linear,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ui::ghost("bots-linear-sync", text::SYNC_NOW_BTN.get())
                                    .debug_selector(|| "bots-linear-sync".into())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.trigger_sync_now(
                                            bot_sync.clone(),
                                            Some(ExternalSource::Linear),
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ui::secondary("bots-linear-save", text::SAVE_CONNECTOR_BTN.get())
                                    .debug_selector(|| "bots-linear-save".into())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let next_c =
                                            this.connectors.collect_connector_inputs(&c_save, cx);
                                        this.save_connectors_config(bot_save.clone(), next_c, cx);
                                    })),
                            ),
                    ),
            )
    }

    pub(super) fn render_composio_card(
        &self,
        bot: &BotConfig,
        c: &BotConnectorConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (cp_state, cp_status) = self.status_for(ExternalSource::Composio, c.composio.enabled);
        let (cp_tone, cp_label) = health_badge_spec(cp_state);
        let busy = self.connectors.busy;
        let entity = cx.entity();
        let bot_toggle = bot.id.clone();
        let bot_secret = bot.id.clone();
        let bot_clear = bot.id.clone();
        let bot_test = bot.id.clone();
        let c_toggle = c.clone();
        let c_secret = c.clone();
        let c_clear = c.clone();

        let apps_heading = text::COMPOSIO_APPS_LABEL.format(&[
            ("selected", &c.composio.enabled_apps.len().to_string()),
            ("total", &COMPOSIO_CURATED_APPS.len().to_string()),
        ]);
        let mut apps_grid = div()
            .id("bots-composio-apps-grid")
            .debug_selector(|| "bots-composio-apps-grid".into())
            .flex()
            .flex_wrap()
            .gap_1p5();
        for app in COMPOSIO_CURATED_APPS {
            let selected = c.composio.enabled_apps.iter().any(|s| s == app.slug);
            let bot_app = bot.id.clone();
            let bot_connect = bot.id.clone();
            let c_app = c.clone();
            let slug = app.slug;
            let aid = format!("bots-composio-app-{slug}");
            let asel = aid.clone();
            apps_grid = apps_grid.child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .child(
                        ui::segment(p, SharedString::from(aid), app.name, selected)
                            .debug_selector(move || asel.clone())
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut next_c =
                                    this.connectors.collect_connector_inputs(&c_app, cx);
                                if selected {
                                    next_c.composio.enabled_apps.retain(|s| s != slug);
                                } else {
                                    next_c.composio.enabled_apps.push(slug.to_owned());
                                }
                                this.save_connectors_config(bot_app.clone(), next_c, cx);
                            })),
                    )
                    .when(selected && c.composio.api_key_secret_ref.is_some(), |d| {
                        d.child(
                            ui::ghost(
                                SharedString::from(format!("bots-composio-connect-{slug}")),
                                text::COMPOSIO_CONNECT_BTN.get(),
                            )
                            .disabled(busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.start_composio_connect(
                                        bot_connect.clone(),
                                        slug.to_owned(),
                                        cx,
                                    );
                                },
                            )),
                        )
                    }),
            );
        }

        let mut conns_box = div().flex().flex_col().gap_1();
        if !self.connectors.composio_connections.is_empty() {
            let heading = text::COMPOSIO_CONNECTIONS_HEADING
                .format(&[("n", &self.connectors.composio_connections.len().to_string())]);
            conns_box = conns_box.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(heading),
            );
            for (cidx, conn) in self.connectors.composio_connections.iter().enumerate() {
                let bot_disc = bot.id.clone();
                let cid = conn.id.clone();
                let acct = conn.account_label.as_deref().unwrap_or(&conn.app_id);
                conns_box = conns_box.child(
                    div()
                        .w_full()
                        .px_2p5()
                        .py_1()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.foreground)
                                .child(format!("{} — {} ({})", conn.app_id, acct, conn.status)),
                        )
                        .child(
                            ui::ghost(
                                SharedString::from(format!("bots-composio-disconnect-{cidx}")),
                                text::COMPOSIO_DISCONNECT_BTN.get(),
                            )
                            .disabled(busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.disconnect_composio_account(
                                        bot_disc.clone(),
                                        cid.clone(),
                                        cx,
                                    );
                                },
                            )),
                        ),
                );
            }
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
                    .child(
                        ui::controls().child(ui::badge(p, cp_tone, cp_label)).child(
                            ui::switch(
                                p,
                                "bots-composio-enable",
                                c.composio.enabled,
                                busy,
                                move |next, _, cx| {
                                    let bid = bot_toggle.clone();
                                    let base = c_toggle.clone();
                                    entity.update(cx, |this, cx| {
                                        let mut next_c =
                                            this.connectors.collect_connector_inputs(&base, cx);
                                        next_c.composio.enabled = next;
                                        this.save_connectors_config(bid, next_c, cx);
                                    });
                                },
                            )
                            .aria_label(format!(
                                "{} {}",
                                text::CONN_COMPOSIO.get(),
                                text::ENABLE_LABEL.get()
                            )),
                        ),
                    ),
            )
            .children(Self::render_connector_status_meta(cp_status, p))
            .child(Self::render_secret_row(
                p,
                SecretRowSpec {
                    id_prefix: "composio-api-key",
                    label: text::COMPOSIO_API_KEY_LABEL.get(),
                    configured: c.composio.api_key_secret_ref.is_some(),
                },
                self.connectors.composio_api_key.as_ref(),
                busy,
                move |this, val, inp, cx| {
                    let mut next_c = this.connectors.collect_connector_inputs(&c_secret, cx);
                    let sref = next_c
                        .composio
                        .api_key_secret_ref
                        .clone()
                        .unwrap_or_else(|| connector_secret_ref(bot_secret.as_str(), "composio"));
                    next_c.composio.api_key_secret_ref = Some(sref.clone());
                    this.save_connector_secret(bot_secret.clone(), sref, val, next_c, inp, cx);
                },
                c.composio.api_key_secret_ref.clone().map(|sref| {
                    Box::new(move |this: &mut View, cx: &mut Context<View>| {
                        let mut next_c = this.connectors.collect_connector_inputs(&c_clear, cx);
                        next_c.composio.api_key_secret_ref = None;
                        this.delete_connector_secret(bot_clear.clone(), sref.clone(), next_c, cx);
                    }) as Box<dyn Fn(&mut View, &mut Context<View>)>
                }),
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
            .child(conns_box)
            .child(
                div().flex().justify_end().child(
                    ui::ghost("bots-composio-test", text::TEST_CONNECTOR_BTN.get())
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.test_connector(bot_test.clone(), ExternalSource::Composio, cx);
                        })),
                ),
            )
    }

    fn render_access_requests_card(
        &self,
        bot_id: &BotId,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let busy = self.connectors.busy;
        let show_all = self.connectors.show_all_access_requests;
        let source_list: Vec<BotAccessRequest> = if show_all {
            self.all_access_requests.clone()
        } else {
            self.connectors
                .bot_access_requests
                .iter()
                .filter(|r| &r.bot_id == bot_id)
                .cloned()
                .collect()
        };
        let pending_count = source_list
            .iter()
            .filter(|r| r.status == BotAccessRequestStatus::Pending)
            .count();
        let access_heading =
            text::ACCESS_REQUESTS_HEADING.format(&[("n", &pending_count.to_string())]);

        let scope_pills = ui::segmented_control(
            p,
            [
                (false, "bots-access-scope-bot", text::ACCESS_SCOPE_BOT.get()),
                (true, "bots-access-scope-all", text::ACCESS_SCOPE_ALL.get()),
            ]
            .into_iter()
            .map(|(all_val, id, label)| {
                ui::segment(p, id, label, show_all == all_val).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.connectors.show_all_access_requests = all_val;
                        cx.notify();
                    },
                ))
            }),
        );

        let pairing_row = div()
            .flex()
            .items_center()
            .gap_1p5()
            .children(
                self.connectors
                    .pairing_code_input
                    .as_ref()
                    .map(|i| div().flex_1().min_w_0().child(Input::new(i).small())),
            )
            .child(
                ui::secondary("bots-access-redeem", text::REDEEM_PAIRING_BTN.get())
                    .debug_selector(|| "bots-access-redeem".into())
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let inp = this.connectors.pairing_code_input.clone();
                        let code = inp
                            .as_ref()
                            .map(|i| i.read(cx).value().to_string())
                            .unwrap_or_default();
                        this.redeem_pairing_code(&code, inp, cx);
                    })),
            );

        let mut access_card = ui::card(p)
            .id("bots-access-requests")
            .debug_selector(|| "bots-access-requests".into())
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
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(access_heading),
                    )
                    .child(scope_pills),
            )
            .child(pairing_row);

        if source_list.is_empty() {
            access_card = access_card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::ACCESS_REQUESTS_EMPTY.get()),
            );
        } else {
            for (ridx, req) in source_list.iter().enumerate() {
                let token_approve = req.token.clone();
                let token_decline = req.token.clone();
                let (st_tone, st_label) = match req.status {
                    BotAccessRequestStatus::Pending => {
                        (ui::BadgeTone::Warning, text::ACCESS_PENDING.get())
                    }
                    BotAccessRequestStatus::Approved => {
                        (ui::BadgeTone::Success, text::ACCESS_APPROVED.get())
                    }
                    BotAccessRequestStatus::Declined => {
                        (ui::BadgeTone::Muted, text::ACCESS_DECLINED.get())
                    }
                };
                access_card = access_card.child(
                    div()
                        .id(SharedString::from(format!("bots-access-req-{ridx}")))
                        .debug_selector(move || format!("bots-access-req-{ridx}"))
                        .w_full()
                        .p_2()
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
                                .flex_wrap()
                                .gap_1p5()
                                .when(show_all, |d| {
                                    d.child(ui::badge(
                                        p,
                                        ui::BadgeTone::Muted,
                                        req.bot_id.as_str().to_owned(),
                                    ))
                                })
                                .child(ui::badge(
                                    p,
                                    ui::BadgeTone::Neutral,
                                    Self::source_label(req.source),
                                ))
                                .child(ui::badge(p, st_tone, st_label))
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.foreground)
                                        .child(format!("{} ({})", req.sender_name, req.channel_id)),
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
                                        .disabled(busy)
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
                                        .disabled(busy)
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
        access_card
    }
}
