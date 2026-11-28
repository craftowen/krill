//! Paste submission waits for PTY receipts, then for the provider's user turn.
use super::{ChatKey, Context, SessionId, Shell, Window};
use krill_proto::input::{Delivery, Receipt};
use std::time::Duration;

pub(super) const POLL: Duration = Duration::from_millis(20);
pub(super) const RECEIPT_POLLS: usize = 500;
const TURN_POLLS: usize = 750;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptKind {
    Regular,
    OpensTerminal,
    LocalSlash,
}

pub(super) struct ChatPrompt {
    pub pane: super::super::PaneId,
    pub session: SessionId,
    pub pending_id: u64,
    pub submitted_payload: String,
    pub bytes: Vec<u8>,
    pub kind: PromptKind,
    pub steer: bool,
    pub is_agy: bool,
}

pub(super) struct Origin {
    pub pane: super::super::PaneId,
    pub session: SessionId,
    pub host: krill_app_core::hosts::HostId,
    pub target: Option<krill_app_core::hosts::HostTarget>,
    pub epoch: u64,
    pub initial_status: Option<(krill_proto::control::AgentState, u64)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FinishOutcome {
    Confirmed,
    Detached,
    ReviewRequired,
}

#[derive(Clone, Copy)]
struct FinishPrompt<'a> {
    key: &'a ChatKey,
    pending_id: u64,
    outcome: FinishOutcome,
}

impl Origin {
    pub(super) fn key(&self) -> ChatKey {
        (self.host.clone(), self.session)
    }
}

pub(super) fn update_with_optional_window(
    this: &gpui::WeakEntity<Shell>,
    win: Option<gpui::AnyWindowHandle>,
    cx: &mut gpui::AsyncApp,
    f: impl FnOnce(&mut Shell, Option<&mut Window>, &mut Context<Shell>),
) {
    let mut f = Some(f);
    if let Some(handle) = win {
        let ran = handle
            .update(cx, |_, window, cx| {
                if let Some(f) = f.take() {
                    this.update(cx, |this, cx| f(this, Some(window), cx)).ok();
                }
            })
            .is_ok();
        if ran {
            return;
        }
    }
    if let Some(f) = f.take() {
        this.update(cx, |this, cx| f(this, None, cx)).ok();
    }
}

impl Shell {
    pub(super) fn chat_prompt_transport_current(&self, origin: &Origin) -> bool {
        let Origin {
            pane,
            session,
            host,
            target,
            epoch,
            ..
        } = origin;
        self.layout.pane_exists(*pane)
            && self.layout.host(*pane) == host
            && self.layout.session(*pane) == Some(*session)
            && self.chat.logs.get(&origin.key()).map_or(0, |log| log.epoch) == *epoch
            && self.live.get(pane).is_some_and(|live| {
                &live.target == target
                    && live.pane.session() == Some(*session)
                    && live.pane.is_linked()
                    && !live.pane.is_closed()
                    && !live.pane.is_cold()
                    && !live.pane.account_switching()
                    && !live.suspended
            })
            && target
                .as_ref()
                .is_some_and(krill_app_core::hosts::HostTarget::is_current)
    }

    pub(super) fn chat_prompt_current(&self, origin: &Origin, cx: &gpui::App) -> bool {
        self.is_pane_chat_view(origin.pane, cx) && self.chat_prompt_transport_current(origin)
    }

    pub(super) fn chat_prompt_view_detached(&self, origin: &Origin, cx: &gpui::App) -> bool {
        if !self.layout.pane_exists(origin.pane) {
            return true;
        }
        self.layout.host(origin.pane) == &origin.host
            && self.layout.session(origin.pane) == Some(origin.session)
            && (!self.is_pane_chat_view(origin.pane, cx)
                || !self.layout.visible().contains(&origin.pane))
    }

    pub(super) fn chat_prompt_turn_started(
        &self,
        key: &ChatKey,
        initial_status: Option<(krill_proto::control::AgentState, u64)>,
        cx: &gpui::App,
    ) -> bool {
        use krill_proto::control::AgentState;
        let Some((state, since)) = self.session_status(key, cx) else {
            return false;
        };
        match initial_status {
            Some((AgentState::Working, prev_since)) => {
                state == AgentState::Working && since != prev_since
            }
            Some((_, prev_since)) => {
                state == AgentState::Working
                    || (since > prev_since
                        && matches!(state, AgentState::Done | AgentState::Waiting))
            }
            None => state == AgentState::Working,
        }
    }

    pub(super) fn chat_prompt_delivery(
        &self,
        origin: &Origin,
        receipt: Receipt,
        _cx: &gpui::App,
    ) -> Delivery {
        if !self.chat_prompt_transport_current(origin) {
            return Delivery::Uncertain;
        }
        #[cfg(test)]
        if self.chat.send_input.is_some() {
            return self.chat.prompt_delivery.unwrap_or(Delivery::Applied);
        }
        self.live
            .get(&origin.pane)
            .map_or(Delivery::Uncertain, |live| {
                live.pane.input_delivery(receipt)
            })
    }

    pub(super) fn chat_agy_queued_match(
        &self,
        origin: &Origin,
        submitted_payload: &str,
        baseline_matches: usize,
    ) -> Option<krill_proto::screen::agy::QueuedMatch> {
        #[cfg(test)]
        if self.chat.prompt_confirmed == Some(false) {
            return None;
        }
        let live = self.live.get(&origin.pane)?;
        krill_proto::screen::agy::queued_submission_confirmed(
            live.pane.screen(),
            submitted_payload,
            baseline_matches,
        )
    }

    fn chat_prompt_confirmed(&mut self, origin: &Origin, pending_id: u64, cx: &gpui::App) -> bool {
        #[cfg(test)]
        if let Some(confirmed) = self.chat.prompt_confirmed {
            return confirmed;
        }
        let key = origin.key();
        self.reconcile_pending_prompts_on_poll(&key);
        if !self.has_pending_prompt(&key, pending_id) {
            return true;
        }
        if self.chat_prompt_turn_started(&key, origin.initial_status, cx) {
            return true;
        }
        #[cfg(test)]
        if self.chat.send_input.is_some() && self.chat.poll_call.is_none() {
            return true;
        }
        false
    }

    fn finish_chat_prompt(
        &mut self,
        finish: FinishPrompt<'_>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.chat.prompt_tasks.remove(finish.key);
        match finish.outcome {
            FinishOutcome::Confirmed => {
                self.chat.prompt_failed.remove(finish.key);
                self.confirm_pending_prompt(finish.key, finish.pending_id);
                cx.notify();
            }
            FinishOutcome::Detached => {
                self.confirm_pending_prompt(finish.key, finish.pending_id);
                cx.notify();
            }
            FinishOutcome::ReviewRequired => {
                self.transition_pending_to_review_required(
                    super::PendingReview {
                        key: finish.key,
                        pending_id: finish.pending_id,
                        immediate_send_failure: false,
                    },
                    window,
                    cx,
                );
            }
        }
    }

    pub(super) fn start_chat_prompt(
        &mut self,
        prompt: ChatPrompt,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let ChatPrompt {
            pane,
            session,
            pending_id,
            submitted_payload,
            bytes,
            kind,
            steer,
            is_agy,
        } = prompt;
        let key = (self.layout.host(pane).clone(), session);
        let log = self.chat.logs.get(&key);
        let origin = Origin {
            pane,
            session,
            host: key.0.clone(),
            target: self.live.get(&pane).and_then(|live| live.target.clone()),
            epoch: log.map_or(0, |log| log.epoch),
            initial_status: self.session_status(&key, cx),
        };
        let baseline_queued = if is_agy {
            self.live.get(&pane).map_or(0, |live| {
                krill_proto::screen::agy::count_queued_matches(
                    live.pane.screen(),
                    &submitted_payload,
                )
                .count
            })
        } else {
            0
        };
        let win = window.as_deref().map(Window::window_handle).or(self.window);
        if !self.send_to_chat_pane(pane, session, &bytes, cx) {
            self.transition_pending_to_review_required(
                super::PendingReview {
                    key: &key,
                    pending_id,
                    immediate_send_failure: true,
                },
                window,
                cx,
            );
            return;
        }
        let Some(receipt) = self.live.get(&pane).map(|live| live.pane.input_receipt()) else {
            self.transition_pending_to_review_required(
                super::PendingReview {
                    key: &key,
                    pending_id,
                    immediate_send_failure: true,
                },
                window,
                cx,
            );
            return;
        };
        self.chat.prompt_failed.remove(&key);
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            // Queue acceptance can precede the actual Windows pipe write by an
            // arbitrary interval. Start settling only after the Applied ACK.
            let mut applied = false;
            for _ in 0..RECEIPT_POLLS {
                let Ok(delivery) = this.update(cx, |this, cx| {
                    this.chat_prompt_delivery(&origin, receipt, cx)
                }) else {
                    return;
                };
                match delivery {
                    Delivery::Applied => {
                        applied = true;
                        break;
                    }
                    Delivery::Uncertain => break,
                    Delivery::Pending => cx.background_executor().timer(POLL).await,
                }
            }
            if !applied {
                update_with_optional_window(&this, win, cx, |this, window, cx| {
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            outcome: FinishOutcome::ReviewRequired,
                        },
                        window,
                        cx,
                    );
                });
                return;
            }
            cx.background_executor()
                .timer(krill_proto::input::PASTE_SUBMIT_DELAY)
                .await;
            let enter = this
                .update(cx, |this, cx| {
                    if this.chat_prompt_delivery(&origin, receipt, cx) != Delivery::Applied
                        || !this.send_chat_prompt_input(pane, session, b"\r", cx)
                    {
                        return None;
                    }
                    this.live.get(&pane).map(|live| live.pane.input_receipt())
                })
                .ok()
                .flatten();
            let Some(enter) = enter else {
                update_with_optional_window(&this, win, cx, |this, window, cx| {
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            outcome: FinishOutcome::ReviewRequired,
                        },
                        window,
                        cx,
                    );
                });
                return;
            };
            let mut applied = false;
            for _ in 0..RECEIPT_POLLS {
                let Ok(delivery) =
                    this.update(cx, |this, cx| this.chat_prompt_delivery(&origin, enter, cx))
                else {
                    return;
                };
                match delivery {
                    Delivery::Applied => {
                        applied = true;
                        break;
                    }
                    Delivery::Uncertain => break,
                    Delivery::Pending => cx.background_executor().timer(POLL).await,
                }
            }
            if applied && kind == PromptKind::OpensTerminal {
                update_with_optional_window(&this, win, cx, |this, window, cx| {
                    let current = this.chat_prompt_current(&origin, cx);
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            outcome: FinishOutcome::Confirmed,
                        },
                        None,
                        cx,
                    );
                    if current {
                        this.switch_pane_to_terminal_opt(pane, &task_key, window, cx);
                    }
                });
                return;
            }
            if applied && kind == PromptKind::LocalSlash {
                update_with_optional_window(&this, win, cx, |this, window, cx| {
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            outcome: FinishOutcome::Confirmed,
                        },
                        window,
                        cx,
                    );
                });
                return;
            }
            if applied {
                enum WatchStep {
                    Confirmed,
                    AgyQueued { send_immediately_hint: bool },
                    Detached,
                    Failed,
                    Wait,
                }
                for step in 0..TURN_POLLS {
                    let Ok(watch) = this.update(cx, |this, cx| {
                        if this.chat_prompt_confirmed(&origin, pending_id, cx) {
                            return WatchStep::Confirmed;
                        }
                        if is_agy
                            && this.chat_prompt_transport_current(&origin)
                            && let Some(qm) = this.chat_agy_queued_match(
                                &origin,
                                &submitted_payload,
                                baseline_queued,
                            )
                        {
                            return WatchStep::AgyQueued {
                                send_immediately_hint: qm.send_immediately_hint,
                            };
                        }
                        if this.chat_prompt_view_detached(&origin, cx) {
                            return WatchStep::Detached;
                        }
                        if !this.chat_prompt_transport_current(&origin) {
                            return WatchStep::Failed;
                        }
                        if step % 5 == 0 {
                            this.poll_session_chat(pane, session, cx);
                        }
                        WatchStep::Wait
                    }) else {
                        return;
                    };
                    match watch {
                        WatchStep::Confirmed => {
                            update_with_optional_window(&this, win, cx, |this, window, cx| {
                                this.finish_chat_prompt(
                                    FinishPrompt {
                                        key: &task_key,
                                        pending_id,
                                        outcome: FinishOutcome::Confirmed,
                                    },
                                    window,
                                    cx,
                                );
                            });
                            return;
                        }
                        WatchStep::AgyQueued {
                            send_immediately_hint,
                        } => {
                            this.update(cx, |this, cx| {
                                if steer
                                    && send_immediately_hint
                                    && this.chat_prompt_current(&origin, cx)
                                {
                                    let _ = this.send_chat_prompt_input(
                                        pane,
                                        session,
                                        krill_proto::screen::agy::SEND_IMMEDIATELY_KEY,
                                        cx,
                                    );
                                }
                                this.mark_pending_prompt_queued(&task_key, pending_id, cx);
                            })
                            .ok();
                            return;
                        }
                        WatchStep::Detached => {
                            update_with_optional_window(&this, win, cx, |this, window, cx| {
                                this.finish_chat_prompt(
                                    FinishPrompt {
                                        key: &task_key,
                                        pending_id,
                                        outcome: FinishOutcome::Detached,
                                    },
                                    window,
                                    cx,
                                );
                            });
                            return;
                        }
                        WatchStep::Failed => break,
                        WatchStep::Wait => cx.background_executor().timer(POLL).await,
                    }
                }
                // Mid-turn steer on Claude/Codex: PTY delivery was already `Applied`, but JSONL
                // often does not flush until the running tool call finishes (>15s). While the
                // session remains `Working` on the same live transport, mark the pending steer
                // `Queued` awaiting transcript echo instead of falsely escalating to ReviewRequired.
                if steer {
                    let stayed_working = this
                        .update(cx, |this, cx| {
                            if this.chat_prompt_transport_current(&origin)
                                && this.session_agent_state(&task_key.0, task_key.1, cx)
                                    == krill_proto::control::AgentState::Working
                            {
                                this.mark_pending_prompt_queued(&task_key, pending_id, cx);
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if stayed_working {
                        return;
                    }
                }
            }
            // Applied only proves a PTY write, never provider consumption. Keep
            // the draft and require review rather than replaying or adding Enter.
            update_with_optional_window(&this, win, cx, |this, window, cx| {
                this.finish_chat_prompt(
                    FinishPrompt {
                        key: &task_key,
                        pending_id,
                        outcome: FinishOutcome::ReviewRequired,
                    },
                    window,
                    cx,
                );
            });
        });
        self.chat.prompt_tasks.insert(key, task);
        cx.notify();
    }
}
