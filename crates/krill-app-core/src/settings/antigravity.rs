// Adapted from Orca:
// - `src/renderer/src/components/settings/accounts/providers/AntigravityAccountsSection.tsx`
// - `src/renderer/src/components/settings/accounts/providers/AntigravityAgyAccountRows.tsx`
// - `src/renderer/src/components/provider-accounts/AntigravityChatSignInFlowPanel.tsx`
// - `src/renderer/src/components/settings/accounts/providers/use-antigravity-account-run.ts`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 1:1 GPUI port of Orca's Antigravity (`agy`) account management & in-app sign-in section.

use gpui::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::usage::{
    AntigravityAccountOp, AntigravityAccountState, AntigravitySignInIntent, AntigravitySignInPhase,
    AntigravitySignInState, AntigravityStoredUsage, ProviderKind, UsageRequest, agy_login_link,
};

use super::{
    SettingsView, accept_account_mutation_reply, agent_install_command, is_daemon_state_dir,
    layout, notify_external_change, provider_connection_badge, queue_terminal_command_on,
    set_status_provider_visible, state_dir, status_provider_visible, text,
};

#[path = "antigravity_sign_in.rs"]
mod sign_in;

const AGY_SIGN_IN_HELP_URL: &str = "https://antigravity.google/docs/cli/install/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AntigravityAction {
    List,
    AddCurrent,
    Select(String),
    Remove(String),
    ResetLockedVault,
    Usage,
}

#[derive(Default)]
pub(super) struct AntigravityPanelState {
    pub state: Option<AntigravityAccountState>,
    pub error: Option<SharedString>,
    pub usage: Option<AntigravityStoredUsage>,
    pub usage_error: Option<SharedString>,
    pub busy: bool,
    pub generation: u64,
    pub queued: Option<AntigravityAction>,
    /// Active `agy acp --login` capability ID owned by this Settings view.
    pub sign_in_flow_id: Option<String>,
    /// Target host where `sign_in_flow_id` is running (used for cancel on Drop/host switch).
    pub sign_in_cancel_target: Option<krill_hosts::HostTarget>,
    /// In-flight `SignInStart` / `SignInComplete` / `SignInCancel` RPC.
    pub sign_in_busy: bool,
    /// True while an async poller loop is running for the current host.
    pub sign_in_polling: bool,
    /// Generation counter for sign-in operations (drops stale replies after host switch/cancel).
    pub sign_in_generation: u64,
    /// Generation counter for active poller loop (prevents exiting pollers clobbering newer ones).
    pub sign_in_poll_gen: u64,
    /// Last Google OAuth URL automatically opened for the current flow (prevents double-open).
    pub sign_in_opened_url: Option<String>,
    /// Clipboard copy confirmation timestamp (clears after 2s without clobbering newer copies).
    pub sign_in_copied_at: Option<std::time::Instant>,
    /// Inline input validation refusal while pasting a code/callback URL.
    pub sign_in_refusal: Option<SharedString>,
    /// Surface alert from starting/running/failing a sign-in flow.
    pub sign_in_alert: Option<SharedString>,
    /// Inline code/callback-URL paste input and its subscription.
    pub code_input: Option<(
        gpui::Entity<gpui_component::input::InputState>,
        gpui::Subscription,
    )>,
    /// Tracks which `flow_id` already received initial focus on `code_input`.
    pub code_input_focused_for: Option<String>,
}

impl Drop for AntigravityPanelState {
    fn drop(&mut self) {
        if let (Some(flow_id), Some(target)) = (
            self.sign_in_flow_id.take(),
            self.sign_in_cancel_target.take(),
        ) {
            sign_in::fire_and_forget_cancel_thread(target, flow_id);
        }
    }
}

impl AntigravityPanelState {
    pub(super) fn reset_for_host_switch(&mut self) {
        self.state = None;
        self.error = None;
        self.usage = None;
        self.usage_error = None;
        self.busy = false;
        self.queued = None;
        self.generation = self.generation.wrapping_add(1);
        self.sign_in_flow_id = None;
        self.sign_in_cancel_target = None;
        self.sign_in_busy = false;
        self.sign_in_polling = false;
        self.sign_in_generation = self.sign_in_generation.wrapping_add(1);
        self.sign_in_poll_gen = self.sign_in_poll_gen.wrapping_add(1);
        self.sign_in_opened_url = None;
        self.sign_in_copied_at = None;
        self.sign_in_refusal = None;
        self.sign_in_alert = None;
        self.code_input = None;
        self.code_input_focused_for = None;
    }

    pub(super) fn own_running_flow(&self) -> Option<&AntigravitySignInState> {
        let flow = self.state.as_ref()?.sign_in.as_ref()?;
        (flow.phase.is_running() && flow.flow_id.is_some() && flow.flow_id == self.sign_in_flow_id)
            .then_some(flow)
    }

    pub(super) fn has_own_running_sign_in(&self) -> bool {
        self.own_running_flow().is_some()
    }

    pub(super) fn other_window_running(&self) -> bool {
        self.state
            .as_ref()
            .and_then(|s| s.sign_in.as_ref())
            .is_some_and(|flow| {
                flow.phase.is_running()
                    && (flow.flow_id.is_none() || flow.flow_id != self.sign_in_flow_id)
            })
    }

    pub(super) fn clear_active_flow_ui(&mut self) {
        self.sign_in_flow_id = None;
        self.sign_in_cancel_target = None;
        self.sign_in_opened_url = None;
        self.sign_in_copied_at = None;
        self.sign_in_refusal = None;
        self.code_input = None;
        self.code_input_focused_for = None;
    }
}

pub(super) fn format_usage_reading(usage: &AntigravityStoredUsage) -> String {
    let session = usage
        .usage
        .five_hour
        .as_ref()
        .map_or_else(|| "—".to_owned(), |w| format!("{}%", w.percent));
    let weekly = usage
        .usage
        .seven_day
        .as_ref()
        .map_or_else(|| "—".to_owned(), |w| format!("{}%", w.percent));
    text::AGY_USAGE_READING.format(&[("session", &session), ("weekly", &weekly)])
}

pub(super) fn format_time_ago(updated_at_secs: u64, now_secs: u64) -> String {
    let delta = now_secs.saturating_sub(updated_at_secs);
    if delta < 60 {
        text::AGY_TIME_JUST_NOW.get().to_owned()
    } else if delta < 3600 {
        text::AGY_TIME_MINUTES_AGO.format(&[("n", &(delta / 60).to_string())])
    } else if delta < 86_400 {
        text::AGY_TIME_HOURS_AGO.format(&[("n", &(delta / 3600).to_string())])
    } else {
        text::AGY_TIME_DAYS_AGO.format(&[("n", &(delta / 86_400).to_string())])
    }
}

impl SettingsView {
    /// Serialized Antigravity account action runner (1:1 `use-antigravity-account-run.ts`).
    pub(super) fn run_antigravity_action(
        &mut self,
        action: AntigravityAction,
        cx: &mut Context<Self>,
    ) {
        if self.antigravity.busy {
            self.antigravity.queued = Some(action);
            return;
        }
        let is_remote = self.account_host.is_remote();
        let host = self.account_host.clone();
        let rpc_target = sign_in::AntigravityRpcTarget::from_settings(self);
        let dir = state_dir(cx);
        let daemon = is_remote || is_daemon_state_dir(dir.as_deref());
        if !daemon {
            self.antigravity.state = Some(AntigravityAccountState::default());
            self.antigravity.error = None;
            cx.notify();
            return;
        }

        self.antigravity.busy = true;
        self.antigravity.error = None;
        self.antigravity.usage = None;
        self.antigravity.usage_error = None;
        let req_gen = self.antigravity.generation.wrapping_add(1);
        self.antigravity.generation = req_gen;
        cx.notify();

        let action_clone = action.clone();
        let existing_flow_id = self.antigravity.sign_in_flow_id.clone();
        let existing_flow_id_reply = existing_flow_id.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let target = rpc_target.resolve().map_err(|e| (e, None))?;

                    match action_clone {
                        AntigravityAction::List => {
                            let op = if let Some(flow_id) = existing_flow_id {
                                AntigravityAccountOp::SignInStatus {
                                    flow_id: Some(flow_id),
                                }
                            } else {
                                AntigravityAccountOp::List
                            };
                            let state = crate::accounts::antigravity_accounts_target(&target, op)
                                .map_err(|e| (e, None))?;
                            Ok::<_, (String, Option<AntigravityAccountState>)>((state, None))
                        }
                        AntigravityAction::AddCurrent => {
                            match crate::accounts::antigravity_accounts_target(
                                &target,
                                AntigravityAccountOp::AddCurrent,
                            ) {
                                Ok(state) => Ok((state, None)),
                                Err(err) => {
                                    let observed = crate::accounts::antigravity_accounts_target(
                                        &target,
                                        AntigravityAccountOp::List,
                                    )
                                    .ok();
                                    Err((err, observed))
                                }
                            }
                        }
                        AntigravityAction::Select(account_id) => {
                            match crate::accounts::antigravity_accounts_target(
                                &target,
                                AntigravityAccountOp::Select { account_id },
                            ) {
                                Ok(state) => Ok((state, None)),
                                Err(err) => {
                                    let observed = crate::accounts::antigravity_accounts_target(
                                        &target,
                                        AntigravityAccountOp::List,
                                    )
                                    .ok();
                                    Err((err, observed))
                                }
                            }
                        }
                        AntigravityAction::Remove(account_id) => {
                            match crate::accounts::antigravity_accounts_target(
                                &target,
                                AntigravityAccountOp::Remove { account_id },
                            ) {
                                Ok(state) => Ok((state, None)),
                                Err(err) => {
                                    let observed = crate::accounts::antigravity_accounts_target(
                                        &target,
                                        AntigravityAccountOp::List,
                                    )
                                    .ok();
                                    Err((err, observed))
                                }
                            }
                        }
                        AntigravityAction::ResetLockedVault => {
                            match crate::accounts::antigravity_accounts_target(
                                &target,
                                AntigravityAccountOp::ResetLockedVault,
                            ) {
                                Ok(state) => Ok((state, None)),
                                Err(err) => {
                                    let observed = crate::accounts::antigravity_accounts_target(
                                        &target,
                                        AntigravityAccountOp::List,
                                    )
                                    .ok();
                                    Err((err, observed))
                                }
                            }
                        }
                        AntigravityAction::Usage => {
                            // Mirrors Orca `AntigravityAccountsSection.tsx:158-189`:
                            // List before -> RefreshUsage -> List after -> reject if native login changed.
                            let before = crate::accounts::antigravity_accounts_target(
                                &target,
                                AntigravityAccountOp::List,
                            )
                            .map_err(|e| (e, None))?;
                            let snapshot = crate::accounts::request_target(
                                &target,
                                &UsageRequest::RefreshUsage,
                            )
                            .map_err(|e| (e, Some(before.clone())))?;
                            let after = crate::accounts::antigravity_accounts_target(
                                &target,
                                AntigravityAccountOp::List,
                            )
                            .map_err(|e| (e, Some(before.clone())))?;
                            let same = before.active_account_id == after.active_account_id
                                && before.current_account == after.current_account;
                            if !same {
                                return Err((text::AGY_STALE_USAGE.get().to_owned(), Some(after)));
                            }
                            let reading = after
                                .active_account_id
                                .as_deref()
                                .and_then(|id| after.accounts.iter().find(|a| a.id == id))
                                .and_then(|a| a.usage.clone())
                                .or_else(|| {
                                    snapshot
                                        .accounts
                                        .iter()
                                        .find(|a| {
                                            a.provider == ProviderKind::Antigravity && a.active
                                        })
                                        .filter(|a| !a.usage.is_empty())
                                        .map(|a| AntigravityStoredUsage {
                                            usage: a.usage.clone(),
                                            meters: snapshot
                                                .meters_for(&a.id)
                                                .map(|m| m.meters.clone())
                                                .unwrap_or_default(),
                                            updated_at: crate::accounts::display::now(),
                                        })
                                });
                            Ok((after, reading))
                        }
                    }
                })
                .await;

            this.update(cx, |this, cx| {
                let is_current = accept_account_mutation_reply(
                    &this.account_host,
                    this.antigravity.generation,
                    &host,
                    req_gen,
                );
                if this.antigravity.generation == req_gen {
                    this.antigravity.busy = false;
                }
                if is_current {
                    match outcome {
                        Ok((mut next_state, reading)) => {
                            // Never let an unauthenticated `List` reply started before `SignInStart`
                            // clobber the active sign-in flow with a redacted `sign_in`.
                            if this.antigravity.sign_in_flow_id != existing_flow_id_reply
                                && let Some(prev_sign_in) = this
                                    .antigravity
                                    .state
                                    .as_ref()
                                    .and_then(|s| s.sign_in.clone())
                            {
                                next_state.sign_in = Some(prev_sign_in);
                            }
                            this.antigravity.state = Some(next_state);
                            this.antigravity.error = None;
                            this.antigravity.usage = reading;
                            this.antigravity.usage_error = None;
                            if existing_flow_id_reply.is_some()
                                && this.antigravity.sign_in_flow_id == existing_flow_id_reply
                            {
                                this.handle_antigravity_sign_in_update(cx);
                            }
                            this.ensure_antigravity_sign_in_poller(cx);
                            if !matches!(action, AntigravityAction::List) {
                                this.refresh_accounts_async(cx);
                                cx.defer(notify_external_change);
                            }
                        }
                        Err((err, observed)) => {
                            if let Some(mut obs) = observed {
                                if this.antigravity.sign_in_flow_id != existing_flow_id_reply
                                    && let Some(prev_sign_in) = this
                                        .antigravity
                                        .state
                                        .as_ref()
                                        .and_then(|s| s.sign_in.clone())
                                {
                                    obs.sign_in = Some(prev_sign_in);
                                }
                                this.antigravity.state = Some(obs);
                                this.ensure_antigravity_sign_in_poller(cx);
                            }
                            this.antigravity.error = Some(err.into());
                        }
                    }
                }
                let next_queued = this.antigravity.queued.take();
                cx.notify();
                if let Some(queued) = next_queued
                    && is_current
                {
                    this.run_antigravity_action(queued, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Renders the Antigravity (`agy`) provider section 1:1 with Orca's
    /// `AntigravityAccountsSection.tsx` + `AntigravityAgyAccountRows.tsx`.
    pub(super) fn render_antigravity_section(
        &mut self,
        provider_installed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let provider = ProviderKind::Antigravity;
        let state = self.antigravity.state.as_ref();
        let busy = self.antigravity.busy;
        let refresh_disabled =
            busy || self.antigravity.sign_in_busy || self.antigravity.has_own_running_sign_in();
        let show_in_bar = status_provider_visible(provider, cx);
        let is_windows_host = (cfg!(windows) && !self.account_host.is_remote())
            || state.is_some_and(|s| s.sign_in_supported && !s.restores_selection_at_launch);
        let subtitle = if is_windows_host {
            text::AGY_TERMINAL_DESC_WINDOWS.get()
        } else {
            text::PROVIDER_AGY_DESC.get()
        };

        let active_conn = if !provider_installed {
            None
        } else if state.and_then(|s| s.current_account.as_ref()).is_some() {
            Some(krill_proto::usage::ConnectionStatus::Connected)
        } else if state.is_some() {
            Some(krill_proto::usage::ConnectionStatus::NeedsLogin)
        } else {
            Some(krill_proto::usage::ConnectionStatus::Checking)
        };
        let (status_tone, status_label) = if provider_installed {
            provider_connection_badge(active_conn)
        } else {
            (
                crate::ui::BadgeTone::Muted,
                text::CONNECTION_NOT_INSTALLED.get(),
            )
        };

        // Section header: icon + title + status badge + status-bar toggle + Refresh accounts button
        // (`+ Add account` lives inside the card footer row, matching Orca `AntigravityAgyAccountRows.tsx`).
        let mut header = div()
            .id("admin-accounts-subheader-agy")
            .debug_selector(|| "admin-accounts-subheader-agy".into())
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .id("admin-provider-title-agy")
                    .tooltip(move |window, cx| {
                        crate::ui::tooltip_view("admin-provider-title-agy", subtitle, window, cx)
                    })
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(crate::ui::icons::provider_icon(
                        provider,
                        crate::ui::ICON_MD,
                        p.foreground,
                    ))
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(provider.display_name()),
                    )
                    .child(
                        crate::ui::badge(p, status_tone, status_label)
                            .id("admin-provider-status-agy")
                            .debug_selector(|| "admin-provider-status-agy".into()),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::PROVIDER_SHOW_IN_STATUS_BAR.get()),
            )
            .child(
                layout::switch(
                    p,
                    "admin-provider-status-bar-agy",
                    show_in_bar,
                    false,
                    move |on, _, cx| {
                        set_status_provider_visible(provider, on, cx);
                    },
                )
                .aria_label(format!(
                    "{} · {}",
                    provider.display_name(),
                    text::PROVIDER_SHOW_IN_STATUS_BAR.get()
                )),
            );

        if !provider_installed && let Some(install_cmd) = agent_install_command(provider.as_str()) {
            let install_host = self.account_host.clone();
            header = header.child(
                layout::secondary(
                    "admin-install-provider-agy",
                    text::INSTALL_IN_TERMINAL.get(),
                )
                .xsmall()
                .icon(IconName::SquareTerminal)
                .debug_selector(|| "admin-install-provider-agy".into())
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(install_cmd.clone()));
                    queue_terminal_command_on(install_host.clone(), install_cmd.clone(), cx);
                }),
            );
        }

        header = header.child(
            Button::new("admin-agy-refresh-accounts")
                .xsmall()
                .ghost()
                .icon(if busy {
                    IconName::LoaderCircle
                } else {
                    IconName::RefreshCw
                })
                .tooltip(text::AGY_REFRESH_ACCOUNTS.get())
                .accessibility_label(text::AGY_REFRESH_ACCOUNTS.get())
                .disabled(refresh_disabled)
                .debug_selector(|| "admin-agy-refresh-accounts".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.run_antigravity_action(AntigravityAction::List, cx);
                })),
        );

        let desc_line = div()
            .id("admin-agy-description")
            .debug_selector(|| "admin-agy-description".into())
            .text_size(crate::ui::TEXT_XS)
            .text_color(p.muted_foreground)
            .child(subtitle);

        let rows = self.render_antigravity_rows(provider_installed, window, cx);

        div().child(
            div()
                .id("admin-accounts-section-agy")
                .debug_selector(|| "admin-accounts-section-agy".into())
                .flex()
                .flex_col()
                .gap_1()
                .child(header)
                .child(desc_line)
                .child(crate::ui::group(p, rows)),
        )
    }

    fn render_antigravity_rows(
        &mut self,
        provider_installed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        let p = crate::ui::palette(cx);
        let own_flow = self.antigravity.own_running_flow().cloned();
        let other_window = self.antigravity.other_window_running();
        let sign_in_busy = self.antigravity.sign_in_busy;
        let sign_in_alert = self.antigravity.sign_in_alert.clone();

        // Prepare input lazily when the flow reaches Waiting (with validated URL) or Verifying.
        let code_input = if let Some(flow) = own_flow.as_ref()
            && (flow.url.as_deref().and_then(agy_login_link).is_some()
                || flow.phase == AntigravitySignInPhase::Verifying)
        {
            let input = self.ensure_antigravity_code_input(window, cx);
            if flow.phase == AntigravitySignInPhase::Waiting
                && flow.flow_id.is_some()
                && self.antigravity.code_input_focused_for != flow.flow_id
            {
                self.antigravity
                    .code_input_focused_for
                    .clone_from(&flow.flow_id);
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            Some(input)
        } else {
            None
        };

        let state = self.antigravity.state.as_ref();
        let busy = self.antigravity.busy;
        let mutation_disabled = busy || sign_in_busy || own_flow.is_some() || other_window;
        let now_secs = crate::accounts::display::now();
        let changed = state.is_some_and(AntigravityAccountState::native_account_changed);
        let launches_use = state
            .and_then(AntigravityAccountState::launches_use_account)
            .map(str::to_owned);
        let is_windows_host = (cfg!(windows) && !self.account_host.is_remote())
            || state.is_some_and(|s| s.sign_in_supported && !s.restores_selection_at_launch);
        let changed_text = if is_windows_host {
            text::AGY_WINDOWS_CHANGED.get()
        } else {
            text::AGY_CHANGED.get()
        };

        let mut rows: Vec<gpui::Stateful<gpui::Div>> = Vec::new();

        // 1. Error or Loading row (`AntigravityAgyAccountRows.tsx:608-632`)
        if let Some(err) = self.antigravity.error.clone() {
            let err_detail = div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.destructive)
                .child(Icon::new(IconName::CircleAlert).xsmall())
                .child(err);
            let mut row = div()
                .id("admin-agy-row-error")
                .debug_selector(|| "admin-agy-row-error".into())
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_3()
                .py_2p5();
            if state.is_some() {
                row = row.child(err_detail);
            } else {
                row = row
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(text::AGY_LOAD_FAILED.get()),
                            )
                            .child(err_detail),
                    )
                    .child(
                        layout::secondary("admin-agy-retry", text::AGY_RETRY.get())
                            .xsmall()
                            .disabled(busy)
                            .debug_selector(|| "admin-agy-retry".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.run_antigravity_action(AntigravityAction::List, cx);
                            })),
                    );
            }
            rows.push(row);
        } else if state.is_none() {
            rows.push(
                div()
                    .id("admin-agy-row-loading")
                    .debug_selector(|| "admin-agy-row-loading".into())
                    .w_full()
                    .px_3()
                    .py_2p5()
                    .text_size(crate::ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::AGY_LOADING.get()),
            );
        }

        // 1b. Locked legacy vault warning + one-click recovery row (keeps host login visible below)
        if state.is_some_and(|s| s.vault_locked) {
            rows.push(
                div()
                    .id("admin-agy-row-vault-locked")
                    .debug_selector(|| "admin-agy-row-vault-locked".into())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2p5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(p.warning)
                                    .child(Icon::new(IconName::CircleAlert).xsmall())
                                    .child(text::AGY_VAULT_LOCKED_TITLE.get()),
                            )
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::AGY_VAULT_LOCKED_DESC.get()),
                            ),
                    )
                    .child(
                        layout::secondary("admin-agy-recover-vault", text::AGY_VAULT_RECOVER.get())
                            .xsmall()
                            .disabled(busy)
                            .debug_selector(|| "admin-agy-recover-vault".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.run_antigravity_action(
                                    AntigravityAction::ResetLockedVault,
                                    cx,
                                );
                            })),
                    ),
            );
        }

        // 2. Active in-app sign-in flow panel OR signed-out row OR unsaved current login row
        if let Some(flow) = own_flow.as_ref() {
            rows.push(self.render_antigravity_sign_in_panel(flow, code_input, cx));
        }

        if let Some(state) = state {
            if own_flow.is_none() && state.current_account.is_none() {
                let mut detail_col = div().flex().flex_col().gap_0p5();
                if let Some(alert) = sign_in_alert.clone() {
                    detail_col = detail_col.child(
                        div()
                            .id("admin-agy-sign-in-error")
                            .debug_selector(|| "admin-agy-sign-in-error".into())
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.destructive)
                            .child(Icon::new(IconName::CircleAlert).xsmall())
                            .child(alert),
                    );
                } else if other_window {
                    detail_col = detail_col.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(Icon::new(IconName::LoaderCircle).xsmall())
                            .child(text::AGY_SIGN_IN_OTHER_WINDOW.get()),
                    );
                }
                detail_col = detail_col.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1p5()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::AGY_RUN_AGY.get())
                        .child("·")
                        .child(
                            div()
                                .id("admin-agy-sign-in-help")
                                .debug_selector(|| "admin-agy-sign-in-help".into())
                                .cursor_pointer()
                                .text_color(p.foreground)
                                .underline()
                                .on_click(|_, _, cx| cx.open_url(AGY_SIGN_IN_HELP_URL))
                                .child(text::AGY_SIGN_IN_HELP.get()),
                        ),
                );
                if changed {
                    detail_col = detail_col.child(
                        div()
                            .id("admin-agy-changed-warning")
                            .debug_selector(|| "admin-agy-changed-warning".into())
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.warning)
                            .child(changed_text),
                    );
                }
                let mut row = div()
                    .id("admin-agy-row-signed-out")
                    .debug_selector(|| "admin-agy-row-signed-out".into())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2p5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(text::AGY_NOT_SIGNED_IN.get()),
                            )
                            .child(detail_col),
                    );
                if provider_installed && !state.vault_locked && state.sign_in_supported {
                    row = row.child(
                        div().flex_none().child(
                            layout::primary(
                                "admin-agy-sign-in-google",
                                text::AGY_SIGN_IN_GOOGLE.get(),
                            )
                            .xsmall()
                            .loading(sign_in_busy)
                            .disabled(busy || sign_in_busy || other_window)
                            .debug_selector(|| "admin-agy-sign-in-google".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_antigravity_sign_in(AntigravitySignInIntent::SignIn, cx);
                            })),
                        ),
                    );
                }
                rows.push(row);
            } else if own_flow.is_none()
                && let Some(curr) = state.current_account.as_ref()
                && state.active_account_id.is_none()
            {
                let title = curr
                    .email
                    .clone()
                    .unwrap_or_else(|| text::AGY_CURRENT_LOGIN.get().to_owned());
                let detail_node = if let Some(uerr) = self.antigravity.usage_error.clone() {
                    div()
                        .id("admin-agy-current-usage-error")
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(uerr)
                } else if changed {
                    div()
                        .id("admin-agy-changed-warning")
                        .debug_selector(|| "admin-agy-changed-warning".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.warning)
                        .child(changed_text)
                } else {
                    div()
                        .id("admin-agy-current-not-saved")
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(if curr.email.is_some() {
                            text::AGY_CURRENT_NOT_SAVED.get()
                        } else {
                            text::AGY_NO_EMAIL_NOT_SAVED.get()
                        })
                };
                let usage_label = self.antigravity.usage.as_ref().map_or_else(
                    || text::AGY_USAGE_REFRESH.get().to_owned(),
                    format_usage_reading,
                );
                let mut action_row = div().flex().items_center().gap_1p5().flex_none().child(
                    layout::ghost("admin-agy-current-usage", usage_label)
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .disabled(mutation_disabled)
                        .debug_selector(|| "admin-agy-current-usage".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.run_antigravity_action(AntigravityAction::Usage, cx);
                        })),
                );
                if !state.vault_locked {
                    action_row = action_row.child(
                        layout::secondary("admin-agy-save-current", text::AGY_SAVE_SHORT.get())
                            .xsmall()
                            .disabled(mutation_disabled)
                            .debug_selector(|| "admin-agy-save-current".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.run_antigravity_action(AntigravityAction::AddCurrent, cx);
                            })),
                    );
                }
                rows.push(
                    div()
                        .id("admin-agy-row-current")
                        .debug_selector(|| "admin-agy-row-current".into())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2p5()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(crate::ui::TEXT_SM)
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child(title),
                                        )
                                        .child(
                                            crate::ui::badge(
                                                p,
                                                crate::ui::BadgeTone::Success,
                                                text::AGY_ACTIVE.get(),
                                            )
                                            .id("admin-agy-current-active-badge"),
                                        ),
                                )
                                .child(detail_node),
                        )
                        .child(action_row),
                );
            }

            // 2b. Diverged saved accounts warning when the host does not auto-restore at launch
            // (Windows or N-1 Unix daemon where selected != active and both are saved accounts).
            if own_flow.is_none()
                && changed
                && launches_use.is_none()
                && state.active_account_id.is_some()
            {
                rows.push(
                    div()
                        .id("admin-agy-changed-warning")
                        .debug_selector(|| "admin-agy-changed-warning".into())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_3()
                        .py_2()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.warning)
                        .child(Icon::new(IconName::CircleAlert).xsmall())
                        .child(changed_text),
                );
            }

            // 3. Saved account rows (`AntigravityAgyAccountRows.tsx:705-736`)
            for account in &state.accounts {
                let active = state.active_account_id.as_deref() == Some(account.id.as_str());
                let is_launches_use = launches_use.as_deref() == Some(account.id.as_str());
                let title = account
                    .email
                    .clone()
                    .unwrap_or_else(|| text::AGY_SAVED_ACCOUNT.get().to_owned());
                let effective_usage = if active {
                    self.antigravity.usage.as_ref().or(account.usage.as_ref())
                } else {
                    account.usage.as_ref()
                };
                let reading_detail = if active && self.antigravity.usage_error.is_some() {
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(self.antigravity.usage_error.clone().unwrap_or_default())
                } else if is_launches_use {
                    let launches_id = format!("admin-agy-launches-use-{}", account.id);
                    div().child(
                        div()
                            .id(SharedString::from(launches_id.clone()))
                            .debug_selector(move || launches_id.clone())
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::AGY_LAUNCHES_USE.get()),
                    )
                } else if launches_use.is_some() && active {
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::AGY_LOGIN_NOW.get())
                } else if let Some(u) = effective_usage {
                    let ago = format_time_ago(u.updated_at, now_secs);
                    let line = if active {
                        text::AGY_READ_AT.format(&[("time", &ago)])
                    } else {
                        let reading = format_usage_reading(u);
                        text::AGY_LAST_READING.format(&[("reading", &reading), ("time", &ago)])
                    };
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(line)
                } else {
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::AGY_NO_READING.get())
                };

                let row_id = format!("admin-agy-account-{}", account.id);
                let select_id = format!("admin-agy-select-{}", account.id);
                let select_acc_id = account.id.clone();
                let remove_acc_id = account.id.clone();
                let remove_id = format!("admin-agy-remove-{}", account.id);
                let usage_id = format!("admin-agy-usage-{}", account.id);
                let badge_id = format!("admin-agy-active-{}", account.id);

                let select_disabled = mutation_disabled;
                let label_block = div()
                    .id(SharedString::from(select_id.clone()))
                    .debug_selector(move || select_id.clone())
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .min_w_0()
                    .flex_1()
                    .when(!select_disabled, gpui::Styled::cursor_pointer)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.antigravity.busy
                            && !this.antigravity.sign_in_busy
                            && !this.antigravity.has_own_running_sign_in()
                            && !this.antigravity.other_window_running()
                        {
                            this.run_antigravity_action(
                                AntigravityAction::Select(select_acc_id.clone()),
                                cx,
                            );
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(title),
                            )
                            .when(active, |d| {
                                d.child(
                                    crate::ui::badge(
                                        p,
                                        crate::ui::BadgeTone::Success,
                                        text::AGY_ACTIVE.get(),
                                    )
                                    .id(SharedString::from(badge_id.clone()))
                                    .debug_selector(move || badge_id.clone()),
                                )
                            }),
                    )
                    .child(reading_detail);

                let actions = if active {
                    let usage_label = effective_usage.map_or_else(
                        || text::AGY_USAGE_REFRESH.get().to_owned(),
                        format_usage_reading,
                    );
                    layout::ghost(gpui::ElementId::Name(usage_id.clone().into()), usage_label)
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .disabled(select_disabled)
                        .debug_selector(move || usage_id.clone())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.run_antigravity_action(AntigravityAction::Usage, cx);
                        }))
                } else {
                    layout::ghost(
                        gpui::ElementId::Name(remove_id.clone().into()),
                        text::AGY_REMOVE.get(),
                    )
                    .xsmall()
                    .icon(IconName::Delete)
                    .disabled(select_disabled)
                    .debug_selector(move || remove_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_antigravity_action(
                            AntigravityAction::Remove(remove_acc_id.clone()),
                            cx,
                        );
                    }))
                };

                rows.push(
                    div()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2p5()
                        .when(!select_disabled, |d| {
                            d.hover(|s| s.bg(p.accent.opacity(0.08)))
                        })
                        .child(label_block)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .flex_none()
                                .child(actions),
                        ),
                );
            }

            // 4. Add-account footer row (`AntigravityAgyAccountRows.tsx:737-778`).
            // Hidden while the legacy vault is locked so we never tell the user to `/logout`
            // before recovering their vault.
            if state.current_account.is_some() && own_flow.is_none() && !state.vault_locked {
                if provider_installed && state.sign_in_supported {
                    let left_detail = if let Some(alert) = sign_in_alert {
                        div().min_w_0().flex_1().child(
                            div()
                                .id("admin-agy-sign-in-error")
                                .debug_selector(|| "admin-agy-sign-in-error".into())
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.destructive)
                                .child(Icon::new(IconName::CircleAlert).xsmall())
                                .child(alert),
                        )
                    } else if other_window {
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .flex_1()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(Icon::new(IconName::LoaderCircle).xsmall())
                            .child(text::AGY_SIGN_IN_OTHER_WINDOW.get())
                    } else {
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::AGY_ADD_ACCOUNT_IN_APP.get())
                    };
                    rows.push(
                        div()
                            .id("admin-agy-add-account-help")
                            .debug_selector(|| "admin-agy-add-account-help".into())
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .px_3()
                            .py_2p5()
                            .child(left_detail)
                            .child(
                                div().flex_none().child(
                                    layout::secondary(
                                        "admin-agy-add-account",
                                        text::ACCOUNTS_ADD.get(),
                                    )
                                    .xsmall()
                                    .icon(IconName::Plus)
                                    .loading(sign_in_busy)
                                    .disabled(busy || sign_in_busy || other_window)
                                    .debug_selector(|| "admin-agy-add-account".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.start_antigravity_sign_in(
                                                AntigravitySignInIntent::AddAccount,
                                                cx,
                                            );
                                        },
                                    )),
                                ),
                            ),
                    );
                } else {
                    rows.push(
                        div()
                            .id("admin-agy-add-account-help")
                            .debug_selector(|| "admin-agy-add-account-help".into())
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2p5()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(Icon::new(IconName::Plus).xsmall())
                            .child(text::AGY_ADD_ACCOUNT_HELP.get()),
                    );
                }
            }
        }

        rows
    }
}
