// Adapted from Orca:
// - `src/renderer/src/components/settings/accounts/providers/AntigravityAccountsSection.tsx`
// - `src/renderer/src/components/settings/accounts/providers/AntigravityTerminalAccountRows.tsx`
// - `src/renderer/src/components/settings/accounts/providers/use-antigravity-account-run.ts`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 1:1 GPUI port of Orca's Antigravity (`agy`) terminal account management section.

use gpui::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::usage::{
    AntigravityAccountOp, AntigravityAccountState, AntigravityStoredUsage, ProviderKind,
    UsageRequest,
};

use super::{
    SettingsView, accept_account_mutation_reply, agent_install_command, is_daemon_state_dir,
    layout, notify_external_change, provider_connection_badge, queue_terminal_command_on,
    set_status_provider_visible, state_dir, status_provider_visible, text,
};

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
        let remote_target = is_remote.then(|| self.selected_account_target());
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
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let target = if let Some(remote) = remote_target {
                        remote.ok_or_else(|| {
                            (text::REMOTE_HOST_DISCONNECTED.get().to_owned(), None)
                        })?
                    } else {
                        let paths = krill_proto::daemon::Paths::dev()
                            .map_err(|e| (crate::errors::user_io_error(&e), None))?;
                        krill_hosts::HostTarget::local(paths)
                    };

                    match action_clone {
                        AntigravityAction::List => {
                            let state = crate::accounts::antigravity_accounts_target(
                                &target,
                                AntigravityAccountOp::List,
                            )
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
                        Ok((next_state, reading)) => {
                            this.antigravity.state = Some(next_state);
                            this.antigravity.error = None;
                            this.antigravity.usage = reading;
                            this.antigravity.usage_error = None;
                            if !matches!(action, AntigravityAction::List) {
                                this.refresh_accounts_async(cx);
                                cx.defer(notify_external_change);
                            }
                        }
                        Err((err, observed)) => {
                            if let Some(obs) = observed {
                                this.antigravity.state = Some(obs);
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
    /// `AntigravityAccountsSection.tsx` + `AntigravityTerminalAccountRows.tsx`.
    pub(super) fn render_antigravity_section(
        &self,
        provider_installed: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let provider = ProviderKind::Antigravity;
        let state = self.antigravity.state.as_ref();
        let busy = self.antigravity.busy;
        let show_in_bar = status_provider_visible(provider, cx);
        let subtitle = if cfg!(windows) && !self.account_host.is_remote() {
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
        // (No `+ Add account` button, matching Orca `AntigravityAccountsSection.tsx:378-399`).
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
                .disabled(busy)
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

        let rows = self.render_antigravity_rows(cx);

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

    fn render_antigravity_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        let p = crate::ui::palette(cx);
        let state = self.antigravity.state.as_ref();
        let busy = self.antigravity.busy;
        let now_secs = crate::accounts::display::now();
        let changed = state.is_some_and(AntigravityAccountState::native_account_changed);
        let changed_text = if cfg!(windows) && !self.account_host.is_remote() {
            text::AGY_WINDOWS_CHANGED.get()
        } else {
            text::AGY_CHANGED.get()
        };

        let mut rows: Vec<gpui::Stateful<gpui::Div>> = Vec::new();

        // 1. Error or Loading row (`AntigravityTerminalAccountRows.tsx:502-526`)
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

        // 2. Signed-out OR Current unsaved login row (`AntigravityTerminalAccountRows.tsx:527-579`)
        if let Some(state) = state {
            if state.current_account.is_none() {
                let mut detail_col = div().flex().flex_col().gap_0p5().child(
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
                rows.push(
                    div()
                        .id("admin-agy-row-signed-out")
                        .debug_selector(|| "admin-agy-row-signed-out".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .px_3()
                        .py_2p5()
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(text::AGY_NOT_SIGNED_IN.get()),
                        )
                        .child(detail_col),
                );
            } else if let Some(curr) = state.current_account.as_ref()
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
                        .disabled(busy)
                        .debug_selector(|| "admin-agy-current-usage".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.run_antigravity_action(AntigravityAction::Usage, cx);
                        })),
                );
                if !state.vault_locked {
                    action_row = action_row.child(
                        layout::secondary("admin-agy-save-current", text::AGY_SAVE_SHORT.get())
                            .xsmall()
                            .disabled(busy)
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

            // 3. Saved account rows (`AntigravityTerminalAccountRows.tsx:580-620`)
            for account in &state.accounts {
                let active = state.active_account_id.as_deref() == Some(account.id.as_str());
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

                let label_block = div()
                    .id(SharedString::from(select_id.clone()))
                    .debug_selector(move || select_id.clone())
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .min_w_0()
                    .flex_1()
                    .when(!busy, gpui::Styled::cursor_pointer)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.antigravity.busy {
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
                        .disabled(busy)
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
                    .disabled(busy)
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
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2p5()
                        .when(!busy, |d| d.hover(|s| s.bg(p.accent.opacity(0.08))))
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

            // 4. Add-account footer help row (`AntigravityTerminalAccountRows.tsx:621-637`)
            if state.current_account.is_some() {
                rows.push(
                    div()
                        .id("admin-agy-add-account-help")
                        .debug_selector(|| "admin-agy-add-account-help".into())
                        .w_full()
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

        rows
    }
}
