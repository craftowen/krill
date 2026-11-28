// Adapted from Orca:
// - `src/renderer/src/components/provider-accounts/AntigravityChatSignInFlowPanel.tsx`
// - `src/renderer/src/components/settings/accounts/providers/AntigravityAgyAccountRows.tsx`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Execution-host `agy acp --login` sign-in flow lifecycle, async poller, and inline panel UI.

use std::time::Duration;

use gpui::{
    AppContext as _, Context, InteractiveElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::usage::{
    AntigravityAccountOp, AntigravitySignInIntent, AntigravitySignInPhase, AntigravitySignInState,
    agy_login_answer, agy_login_link,
};

use super::super::{
    SettingsView, accept_account_mutation_reply, is_daemon_state_dir, layout,
    notify_external_change, state_dir, text,
};

const SIGN_IN_POLL_INTERVAL: Duration = Duration::from_millis(300);
const OTHER_WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(1_000);
const MAX_CONSECUTIVE_POLL_ERRORS: u8 = 3;

pub(super) enum AntigravityRpcTarget {
    Local,
    Remote(Box<krill_hosts::HostTarget>),
    Disconnected,
}

impl AntigravityRpcTarget {
    pub(super) fn from_settings(view: &SettingsView) -> Self {
        if view.account_host.is_remote() {
            view.selected_account_target()
                .map_or(Self::Disconnected, |target| Self::Remote(Box::new(target)))
        } else {
            Self::Local
        }
    }

    pub(super) fn resolve(self) -> Result<krill_hosts::HostTarget, String> {
        match self {
            Self::Remote(target) => Ok(*target),
            Self::Disconnected => Err(text::REMOTE_HOST_DISCONNECTED.get().to_owned()),
            Self::Local => {
                let paths = krill_proto::daemon::Paths::dev()
                    .map_err(|e| crate::errors::user_io_error(&e))?;
                Ok(krill_hosts::HostTarget::local(paths))
            }
        }
    }
}

pub(super) fn fire_and_forget_cancel_thread(target: krill_hosts::HostTarget, flow_id: String) {
    std::thread::Builder::new()
        .name("agy-signin-drop-cancel".into())
        .spawn(move || {
            crate::accounts::antigravity_accounts_target(
                &target,
                AntigravityAccountOp::SignInCancel { flow_id },
            )
            .ok();
        })
        .ok();
}

impl SettingsView {
    /// Start an in-app `agy acp --login` flow on the selected execution host (local or remote).
    pub(super) fn start_antigravity_sign_in(
        &mut self,
        intent: AntigravitySignInIntent,
        cx: &mut Context<Self>,
    ) {
        if self.antigravity.sign_in_busy || self.antigravity.busy {
            return;
        }
        let is_remote = self.account_host.is_remote();
        let host = self.account_host.clone();
        let rpc_target = AntigravityRpcTarget::from_settings(self);
        let dir = state_dir(cx);
        let daemon = is_remote || is_daemon_state_dir(dir.as_deref());
        if !daemon {
            self.antigravity.sign_in_alert = Some(text::ACCOUNTS_DAEMON_REQUIRED.get().into());
            cx.notify();
            return;
        }

        self.antigravity.sign_in_busy = true;
        self.antigravity.sign_in_alert = None;
        self.antigravity.sign_in_refusal = None;
        self.antigravity.sign_in_copied_at = None;
        self.antigravity.sign_in_opened_url = None;
        self.antigravity.sign_in_cancel_target = None;
        self.antigravity.code_input = None;
        self.antigravity.code_input_focused_for = None;
        let req_gen = self.antigravity.sign_in_generation.wrapping_add(1);
        self.antigravity.sign_in_generation = req_gen;
        self.antigravity.sign_in_poll_gen = self.antigravity.sign_in_poll_gen.wrapping_add(1);
        self.antigravity.sign_in_polling = false;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let mut outcome = Some(
                cx.background_executor()
                    .spawn(async move {
                        let target = rpc_target.resolve()?;
                        let next_state = crate::accounts::antigravity_accounts_target(
                            &target,
                            AntigravityAccountOp::SignInStart { intent },
                        )?;
                        Ok::<_, String>((target, next_state))
                    })
                    .await,
            );

            let mut orphan_cancel: Option<(krill_hosts::HostTarget, String)> = None;
            this.update(cx, |this, cx| {
                let Some(outcome) = outcome.take() else {
                    return;
                };
                let is_current = accept_account_mutation_reply(
                    &this.account_host,
                    this.antigravity.sign_in_generation,
                    &host,
                    req_gen,
                );
                if this.antigravity.sign_in_generation == req_gen {
                    this.antigravity.sign_in_busy = false;
                }
                if is_current {
                    match outcome {
                        Ok((target, next_state)) => {
                            let flow_id =
                                next_state.sign_in.as_ref().and_then(|s| s.flow_id.clone());
                            this.antigravity.sign_in_cancel_target =
                                flow_id.is_some().then_some(target);
                            this.antigravity.sign_in_flow_id = flow_id;
                            this.antigravity.state = Some(next_state);
                            this.antigravity.error = None;
                            this.antigravity.sign_in_alert = None;
                            this.handle_antigravity_sign_in_update(cx);
                            this.ensure_antigravity_sign_in_poller(cx);
                        }
                        Err(err) => {
                            this.antigravity.sign_in_alert = Some(err.into());
                        }
                    }
                    cx.notify();
                } else if let Ok((target, next_state)) = outcome
                    && let Some(flow_id) = next_state.sign_in.and_then(|s| s.flow_id)
                {
                    orphan_cancel = Some((target, flow_id));
                }
            })
            .ok();

            if let Some(Ok((target, next_state))) = outcome.take()
                && let Some(flow_id) = next_state.sign_in.and_then(|s| s.flow_id)
            {
                orphan_cancel = Some((target, flow_id));
            }
            if let Some((target, flow_id)) = orphan_cancel {
                cx.background_executor()
                    .spawn(async move {
                        crate::accounts::antigravity_accounts_target(
                            &target,
                            AntigravityAccountOp::SignInCancel { flow_id },
                        )
                        .ok();
                    })
                    .detach();
            }
        })
        .detach();
    }

    /// Submit the pasted authorization code or callback URL to the running `agy acp --login` flow.
    pub(in crate::settings) fn submit_antigravity_sign_in_code(&mut self, cx: &mut Context<Self>) {
        if self.antigravity.sign_in_busy {
            return;
        }
        let Some(flow_id) = self.antigravity.sign_in_flow_id.clone() else {
            return;
        };
        let pasted = self
            .antigravity
            .code_input
            .as_ref()
            .map(|(input, _)| input.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let Some(answer) = agy_login_answer(&pasted) else {
            self.antigravity.sign_in_refusal = Some(text::AGY_SIGN_IN_BAD_ANSWER.get().into());
            cx.notify();
            return;
        };

        let host = self.account_host.clone();
        let rpc_target = AntigravityRpcTarget::from_settings(self);
        let req_gen = self.antigravity.sign_in_generation.wrapping_add(1);
        self.antigravity.sign_in_generation = req_gen;
        self.antigravity.sign_in_busy = true;
        self.antigravity.sign_in_refusal = None;
        self.antigravity.sign_in_alert = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let target = rpc_target.resolve()?;
                    crate::accounts::antigravity_accounts_target(
                        &target,
                        AntigravityAccountOp::SignInComplete { flow_id, answer },
                    )
                })
                .await;

            this.update(cx, |this, cx| {
                let is_current = accept_account_mutation_reply(
                    &this.account_host,
                    this.antigravity.sign_in_generation,
                    &host,
                    req_gen,
                );
                if this.antigravity.sign_in_generation == req_gen {
                    this.antigravity.sign_in_busy = false;
                }
                if is_current {
                    match outcome {
                        Ok(next_state) => {
                            this.antigravity.state = Some(next_state);
                            this.antigravity.sign_in_refusal = None;
                            this.handle_antigravity_sign_in_update(cx);
                            this.ensure_antigravity_sign_in_poller(cx);
                        }
                        Err(err) => {
                            this.antigravity.sign_in_refusal = Some(err.into());
                        }
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Cancel the running `agy acp --login` flow owned by this Settings view.
    pub(in crate::settings) fn cancel_antigravity_sign_in(&mut self, cx: &mut Context<Self>) {
        // If `SignInStart` is still in flight (`flow_id == None`), invalidate its generation so
        // the completion callback immediately sends `SignInCancel` for the newly minted flow.
        if self.antigravity.sign_in_busy && self.antigravity.sign_in_flow_id.is_none() {
            self.antigravity.sign_in_generation =
                self.antigravity.sign_in_generation.wrapping_add(1);
            self.antigravity.sign_in_busy = false;
            cx.notify();
            return;
        }
        if self.antigravity.sign_in_busy {
            return;
        }
        let Some(flow_id) = self.antigravity.sign_in_flow_id.clone() else {
            self.antigravity.clear_active_flow_ui();
            cx.notify();
            return;
        };
        let is_remote = self.account_host.is_remote();
        let host = self.account_host.clone();
        let cached_target = self.antigravity.sign_in_cancel_target.clone();
        let rpc_target = AntigravityRpcTarget::from_settings(self);
        let dir = state_dir(cx);
        let daemon = is_remote || is_daemon_state_dir(dir.as_deref());
        if !daemon {
            self.antigravity.clear_active_flow_ui();
            if let Some(state) = self.antigravity.state.as_mut() {
                state.sign_in = None;
            }
            cx.notify();
            return;
        }
        let req_gen = self.antigravity.sign_in_generation.wrapping_add(1);
        self.antigravity.sign_in_generation = req_gen;
        self.antigravity.sign_in_busy = true;
        self.antigravity.sign_in_alert = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let target = match cached_target {
                        Some(target) => target,
                        None => rpc_target.resolve()?,
                    };
                    crate::accounts::antigravity_accounts_target(
                        &target,
                        AntigravityAccountOp::SignInCancel { flow_id },
                    )
                })
                .await;

            this.update(cx, |this, cx| {
                let is_current = accept_account_mutation_reply(
                    &this.account_host,
                    this.antigravity.sign_in_generation,
                    &host,
                    req_gen,
                );
                if this.antigravity.sign_in_generation == req_gen {
                    this.antigravity.sign_in_busy = false;
                }
                if is_current {
                    match outcome {
                        Ok(next_state) => {
                            this.antigravity.clear_active_flow_ui();
                            this.antigravity.state = Some(next_state);
                            this.antigravity.sign_in_alert = None;
                        }
                        Err(err) => {
                            // Keep `sign_in_flow_id` on transient cancel failure so the user
                            // can retry instead of stranding the daemon flow for 5 minutes.
                            this.antigravity.sign_in_alert = Some(err.into());
                            this.ensure_antigravity_sign_in_poller(cx);
                        }
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Fire-and-forget cancellation when Settings is hidden or switches to another host so
    /// `krilld` never keeps an orphaned `agy acp --login` flow locked out for 5 minutes.
    pub(in crate::settings) fn abandon_antigravity_sign_in(&mut self, cx: &mut Context<Self>) {
        self.antigravity.sign_in_generation = self.antigravity.sign_in_generation.wrapping_add(1);
        self.antigravity.sign_in_poll_gen = self.antigravity.sign_in_poll_gen.wrapping_add(1);
        self.antigravity.sign_in_busy = false;
        self.antigravity.sign_in_polling = false;
        let flow_id = self.antigravity.sign_in_flow_id.take();
        let cached_target = self.antigravity.sign_in_cancel_target.take();
        self.antigravity.clear_active_flow_ui();
        let Some(flow_id) = flow_id else {
            return;
        };
        let is_remote = self.account_host.is_remote();
        let rpc_target = AntigravityRpcTarget::from_settings(self);
        let dir = state_dir(cx);
        let daemon = cached_target.is_some() || is_remote || is_daemon_state_dir(dir.as_deref());
        if !daemon {
            return;
        }
        cx.background_executor()
            .spawn(async move {
                let target = match cached_target {
                    Some(target) => Ok(target),
                    None => rpc_target.resolve(),
                };
                if let Ok(target) = target {
                    crate::accounts::antigravity_accounts_target(
                        &target,
                        AntigravityAccountOp::SignInCancel { flow_id },
                    )
                    .ok();
                }
            })
            .detach();
    }

    fn copy_antigravity_sign_in_link(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self
            .antigravity
            .own_running_flow()
            .and_then(|flow| flow.url.as_deref())
            .and_then(agy_login_link)
            .map(str::to_owned)
        else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
        let stamp = std::time::Instant::now();
        self.antigravity.sign_in_copied_at = Some(stamp);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |this, cx| {
                if this.antigravity.sign_in_copied_at == Some(stamp) {
                    this.antigravity.sign_in_copied_at = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn handle_antigravity_sign_in_update(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self
            .antigravity
            .state
            .as_ref()
            .and_then(|s| s.sign_in.clone())
        else {
            if self.antigravity.sign_in_flow_id.is_some() {
                self.antigravity.clear_active_flow_ui();
            }
            return;
        };
        if flow.flow_id != self.antigravity.sign_in_flow_id {
            return;
        }
        match flow.phase {
            AntigravitySignInPhase::Starting | AntigravitySignInPhase::Verifying => {}
            AntigravitySignInPhase::Waiting => {
                if let Some(url) = flow.url.as_deref().and_then(agy_login_link)
                    && self.antigravity.sign_in_opened_url.as_deref() != Some(url)
                {
                    self.antigravity.sign_in_opened_url = Some(url.to_owned());
                    cx.open_url(url);
                }
            }
            AntigravitySignInPhase::SignedIn => {
                self.antigravity.clear_active_flow_ui();
                self.antigravity.sign_in_alert = None;
                self.refresh_accounts_async(cx);
                cx.defer(notify_external_change);
            }
            AntigravitySignInPhase::Failed => {
                let err = flow
                    .error
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| text::AGY_SIGN_IN_FAILED.get().to_owned());
                self.antigravity.clear_active_flow_ui();
                self.antigravity.sign_in_alert = Some(err.into());
            }
            AntigravitySignInPhase::Expired => {
                self.antigravity.clear_active_flow_ui();
                self.antigravity.sign_in_alert = Some(text::AGY_SIGN_IN_EXPIRED.get().into());
            }
            AntigravitySignInPhase::Cancelled => {
                self.antigravity.clear_active_flow_ui();
                self.antigravity.sign_in_alert = None;
            }
        }
    }

    pub(super) fn ensure_antigravity_sign_in_poller(&mut self, cx: &mut Context<Self>) {
        let should_poll =
            self.antigravity.has_own_running_sign_in() || self.antigravity.other_window_running();
        if self.antigravity.sign_in_polling || !should_poll {
            return;
        }
        let is_remote = self.account_host.is_remote();
        let dir = state_dir(cx);
        if !is_remote && !is_daemon_state_dir(dir.as_deref()) {
            return;
        }
        self.antigravity.sign_in_poll_gen = self.antigravity.sign_in_poll_gen.wrapping_add(1);
        let my_poll_gen = self.antigravity.sign_in_poll_gen;
        self.antigravity.sign_in_polling = true;
        let host = self.account_host.clone();
        cx.spawn(async move |this, cx| {
            let mut consecutive_errors: u8 = 0;
            loop {
                let poll_info = this
                    .read_with(cx, |this, _| {
                        if this.account_host != host
                            || this.antigravity.sign_in_poll_gen != my_poll_gen
                        {
                            return None;
                        }
                        let own = this.antigravity.has_own_running_sign_in();
                        let other = this.antigravity.other_window_running();
                        if !own && !other {
                            return None;
                        }
                        let expected_flow_id = if own {
                            this.antigravity.sign_in_flow_id.clone()
                        } else {
                            None
                        };
                        let rpc_target = AntigravityRpcTarget::from_settings(this);
                        let interval = if own {
                            SIGN_IN_POLL_INTERVAL
                        } else {
                            OTHER_WINDOW_POLL_INTERVAL
                        };
                        Some((expected_flow_id, rpc_target, interval))
                    })
                    .ok()
                    .flatten();
                let Some((expected_flow_id, rpc_target, interval)) = poll_info else {
                    this.update(cx, |this, _| {
                        if this.antigravity.sign_in_poll_gen == my_poll_gen {
                            this.antigravity.sign_in_polling = false;
                        }
                    })
                    .ok();
                    break;
                };
                cx.background_executor().timer(interval).await;

                // Re-check generation after sleeping before issuing IPC.
                let still_current = this
                    .read_with(cx, |this, _| {
                        this.account_host == host
                            && this.antigravity.sign_in_poll_gen == my_poll_gen
                            && (this.antigravity.has_own_running_sign_in()
                                || this.antigravity.other_window_running())
                    })
                    .unwrap_or(false);
                if !still_current {
                    this.update(cx, |this, _| {
                        if this.antigravity.sign_in_poll_gen == my_poll_gen {
                            this.antigravity.sign_in_polling = false;
                        }
                    })
                    .ok();
                    break;
                }

                let poll_flow_id = expected_flow_id.clone();
                let outcome = cx
                    .background_executor()
                    .spawn(async move {
                        let target = rpc_target.resolve()?;
                        crate::accounts::antigravity_accounts_target(
                            &target,
                            AntigravityAccountOp::SignInStatus {
                                flow_id: poll_flow_id,
                            },
                        )
                    })
                    .await;

                let keep_polling = this
                    .update(cx, |this, cx| {
                        if this.account_host != host
                            || this.antigravity.sign_in_poll_gen != my_poll_gen
                            || (expected_flow_id.is_some()
                                && this.antigravity.sign_in_flow_id != expected_flow_id)
                        {
                            if this.antigravity.sign_in_poll_gen == my_poll_gen {
                                this.antigravity.sign_in_polling = false;
                            }
                            return false;
                        }
                        match outcome {
                            Ok(next_state) => {
                                consecutive_errors = 0;
                                this.antigravity.state = Some(next_state);
                                this.handle_antigravity_sign_in_update(cx);
                                cx.notify();
                                let still_running = this.antigravity.has_own_running_sign_in()
                                    || this.antigravity.other_window_running();
                                if !still_running
                                    && this.antigravity.sign_in_poll_gen == my_poll_gen
                                {
                                    this.antigravity.sign_in_polling = false;
                                }
                                still_running
                            }
                            Err(err) => {
                                consecutive_errors = consecutive_errors.saturating_add(1);
                                if consecutive_errors < MAX_CONSECUTIVE_POLL_ERRORS {
                                    return true;
                                }
                                if this.antigravity.sign_in_poll_gen == my_poll_gen {
                                    this.antigravity.sign_in_polling = false;
                                }
                                // Keep `sign_in_flow_id` intact so the user can still submit or
                                // explicitly cancel once the daemon responds again.
                                this.antigravity.sign_in_alert = Some(err.into());
                                cx.notify();
                                false
                            }
                        }
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn ensure_antigravity_code_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<gpui_component::input::InputState> {
        if let Some((input, _)) = self.antigravity.code_input.as_ref() {
            crate::i18n::sync_input_placeholder(
                input,
                text::AGY_SIGN_IN_PASTE_CODE.get(),
                window,
                cx,
            );
            return input.clone();
        }
        let input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::AGY_SIGN_IN_PASTE_CODE.get())
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &gpui_component::input::InputEvent, window, cx| match event {
                gpui_component::input::InputEvent::Change => {
                    this.antigravity.sign_in_refusal = None;
                    // Always notify so pasting into an empty box immediately enables Continue.
                    cx.notify();
                }
                gpui_component::input::InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } if !crate::jump::entity_input_is_composing(input, window, cx) => {
                    this.submit_antigravity_sign_in_code(cx);
                }
                _ => {}
            },
        );
        self.antigravity.code_input = Some((input.clone(), sub));
        input
    }

    pub(super) fn render_antigravity_sign_in_panel(
        &self,
        flow: &AntigravitySignInState,
        code_input: Option<gpui::Entity<gpui_component::input::InputState>>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let sign_in_busy = self.antigravity.sign_in_busy;
        let sign_in_alert = self.antigravity.sign_in_alert.clone();
        let sign_in_refusal = self.antigravity.sign_in_refusal.clone();
        let sign_in_copied = self.antigravity.sign_in_copied_at.is_some();
        let validated_url = flow.url.as_deref().and_then(agy_login_link);

        let verifying = flow.phase == AntigravitySignInPhase::Verifying;
        let form_busy = verifying || sign_in_busy;
        let status_line = match flow.phase {
            AntigravitySignInPhase::Waiting => text::AGY_SIGN_IN_FINISH_IN_BROWSER.get(),
            AntigravitySignInPhase::Verifying => text::AGY_SIGN_IN_VERIFYING.get(),
            AntigravitySignInPhase::Starting
            | AntigravitySignInPhase::SignedIn
            | AntigravitySignInPhase::Failed
            | AntigravitySignInPhase::Cancelled
            | AntigravitySignInPhase::Expired => text::AGY_SIGN_IN_STARTING.get(),
        };

        let mut panel = div()
            .id("admin-agy-row-signing-in")
            .debug_selector(|| "admin-agy-row-signing-in".into())
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2p5()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
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
                                    .child(text::AGY_SIGN_IN_GOOGLE.get()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(Icon::new(IconName::LoaderCircle).xsmall())
                                    .child(status_line),
                            ),
                    )
                    .child(
                        Button::new("admin-agy-sign-in-cancel")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip(text::AGY_SIGN_IN_CANCEL.get())
                            .accessibility_label(text::AGY_SIGN_IN_CANCEL.get())
                            .disabled(sign_in_busy)
                            .debug_selector(|| "admin-agy-sign-in-cancel".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_antigravity_sign_in(cx);
                            })),
                    ),
            );

        if let Some(alert) = sign_in_alert {
            panel = panel.child(
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
        }

        if let Some(input) = code_input {
            let answer_empty = input.read(cx).value().trim().is_empty();
            panel = panel.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            gpui_component::input::Input::new(&input)
                                .small()
                                .disabled(form_busy),
                        ),
                    )
                    .child(
                        layout::secondary(
                            "admin-agy-sign-in-continue",
                            text::AGY_SIGN_IN_CONTINUE.get(),
                        )
                        .xsmall()
                        .loading(form_busy)
                        .disabled(form_busy || answer_empty)
                        .debug_selector(|| "admin-agy-sign-in-continue".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_antigravity_sign_in_code(cx);
                        })),
                    ),
            );
            if let Some(refusal) = sign_in_refusal {
                panel = panel.child(
                    div()
                        .id("admin-agy-sign-in-refusal")
                        .debug_selector(|| "admin-agy-sign-in-refusal".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(refusal),
                );
            }
            if validated_url.is_some() {
                let copy_label = text::AGY_SIGN_IN_COPY_LINK.get();
                let active_label = if sign_in_copied {
                    text::AGY_SIGN_IN_COPIED.get()
                } else {
                    copy_label
                };
                panel = panel.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1p5()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::AGY_SIGN_IN_PAGE_DIDNT_OPEN.get())
                        .child(
                            div()
                                .id("admin-agy-sign-in-copy-link")
                                .debug_selector(|| "admin-agy-sign-in-copy-link".into())
                                .relative()
                                .whitespace_nowrap()
                                .cursor_pointer()
                                .text_color(p.foreground)
                                .underline()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.copy_antigravity_sign_in_link(cx);
                                }))
                                // Reserve the wider label width so toggling to "Copied" causes
                                // zero layout shift (STYLEGUIDE §7.3).
                                .child(div().invisible().child(copy_label))
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .whitespace_nowrap()
                                        .child(active_label),
                                ),
                        ),
                );
            }
        }
        panel
    }
}
