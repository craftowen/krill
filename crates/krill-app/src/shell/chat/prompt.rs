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
    pub host: krill_app::hosts::HostId,
    pub target: Option<krill_app::hosts::HostTarget>,
    pub epoch: u64,
}

#[derive(Clone, Copy)]
struct FinishPrompt<'a> {
    key: &'a ChatKey,
    pending_id: u64,
    confirmed: bool,
}

impl Origin {
    pub(super) fn key(&self) -> ChatKey {
        (self.host.clone(), self.session)
    }
}

impl Shell {
    pub(super) fn chat_prompt_current(&self, origin: &Origin, cx: &Context<Self>) -> bool {
        let Origin {
            pane,
            session,
            host,
            target,
            epoch,
        } = origin;
        self.is_pane_chat_view(*pane, cx)
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
                .is_some_and(krill_app::hosts::HostTarget::is_current)
    }

    pub(super) fn chat_prompt_delivery(
        &self,
        origin: &Origin,
        receipt: Receipt,
        cx: &Context<Self>,
    ) -> Delivery {
        if !self.chat_prompt_current(origin, cx) {
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

    fn chat_prompt_confirmed(&mut self, origin: &Origin, pending_id: u64) -> bool {
        #[cfg(test)]
        if let Some(confirmed) = self.chat.prompt_confirmed {
            return confirmed;
        }
        let key = origin.key();
        self.reconcile_pending_prompts_on_poll(&key);
        if !self.has_pending_prompt(&key, pending_id) {
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
        if finish.confirmed {
            self.chat.prompt_failed.remove(finish.key);
            self.confirm_pending_prompt(finish.key, finish.pending_id);
            cx.notify();
        } else {
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

    pub(super) fn start_chat_prompt(
        &mut self,
        prompt: ChatPrompt,
        window: &mut Window,
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
        if !self.send_to_chat_pane(pane, session, &bytes, cx) {
            self.transition_pending_to_review_required(
                super::PendingReview {
                    key: &key,
                    pending_id,
                    immediate_send_failure: true,
                },
                Some(window),
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
                Some(window),
                cx,
            );
            return;
        };
        self.chat.prompt_failed.remove(&key);
        let task_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            // Queue acceptance can precede the actual Windows pipe write by an
            // arbitrary interval. Start settling only after the Applied ACK.
            let mut applied = false;
            for _ in 0..RECEIPT_POLLS {
                let Ok(delivery) = this.update_in(cx, |this, _, cx| {
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
                this.update_in(cx, |this, window, cx| {
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            confirmed: false,
                        },
                        Some(window),
                        cx,
                    );
                })
                .ok();
                return;
            }
            cx.background_executor()
                .timer(krill_proto::input::PASTE_SUBMIT_DELAY)
                .await;
            let enter = this
                .update_in(cx, |this, _, cx| {
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
                this.update_in(cx, |this, window, cx| {
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            confirmed: false,
                        },
                        Some(window),
                        cx,
                    );
                })
                .ok();
                return;
            };
            let mut applied = false;
            for _ in 0..RECEIPT_POLLS {
                let Ok(delivery) = this.update_in(cx, |this, _, cx| {
                    this.chat_prompt_delivery(&origin, enter, cx)
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
            if applied && kind == PromptKind::OpensTerminal {
                this.update_in(cx, |this, window, cx| {
                    let current = this.chat_prompt_current(&origin, cx);
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            confirmed: true,
                        },
                        Some(window),
                        cx,
                    );
                    if current {
                        this.switch_pane_to_terminal(pane, window, cx);
                    }
                })
                .ok();
                return;
            }
            if applied && kind == PromptKind::LocalSlash {
                this.update_in(cx, |this, window, cx| {
                    this.finish_chat_prompt(
                        FinishPrompt {
                            key: &task_key,
                            pending_id,
                            confirmed: true,
                        },
                        Some(window),
                        cx,
                    );
                })
                .ok();
                return;
            }
            if applied {
                enum TurnCheck {
                    Pending,
                    Invalid,
                    Confirmed,
                    AgyQueued { send_immediately_hint: bool },
                }
                for step in 0..TURN_POLLS {
                    let Ok(check) = this.update_in(cx, |this, _, cx| {
                        if !this.chat_prompt_current(&origin, cx) {
                            return TurnCheck::Invalid;
                        }
                        if this.chat_prompt_confirmed(&origin, pending_id) {
                            return TurnCheck::Confirmed;
                        }
                        if is_agy
                            && let Some(qm) = this.chat_agy_queued_match(
                                &origin,
                                &submitted_payload,
                                baseline_queued,
                            )
                        {
                            return TurnCheck::AgyQueued {
                                send_immediately_hint: qm.send_immediately_hint,
                            };
                        }
                        if step % 5 == 0 {
                            this.poll_session_chat(pane, session, cx);
                        }
                        TurnCheck::Pending
                    }) else {
                        return;
                    };
                    match check {
                        TurnCheck::Invalid => break,
                        TurnCheck::Confirmed => {
                            this.update_in(cx, |this, window, cx| {
                                this.finish_chat_prompt(
                                    FinishPrompt {
                                        key: &task_key,
                                        pending_id,
                                        confirmed: true,
                                    },
                                    Some(window),
                                    cx,
                                );
                            })
                            .ok();
                            return;
                        }
                        TurnCheck::AgyQueued {
                            send_immediately_hint,
                        } => {
                            this.update_in(cx, |this, _, cx| {
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
                        TurnCheck::Pending => {}
                    }
                    cx.background_executor().timer(POLL).await;
                }
            }
            // Applied only proves a PTY write, never provider consumption. Keep
            // the draft and require review rather than replaying or adding Enter.
            this.update_in(cx, |this, window, cx| {
                this.finish_chat_prompt(
                    FinishPrompt {
                        key: &task_key,
                        pending_id,
                        confirmed: false,
                    },
                    Some(window),
                    cx,
                );
            })
            .ok();
        });
        self.chat.prompt_tasks.insert(key, task);
        cx.notify();
    }
}
