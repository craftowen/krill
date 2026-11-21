//! Paste submission waits for PTY receipts, then for the provider's user turn.
use super::{ChatKey, Context, SessionId, Shell, Window};
use krill_proto::input::{Delivery, Receipt};
use std::time::Duration;

pub(super) const POLL: Duration = Duration::from_millis(20);
pub(super) const RECEIPT_POLLS: usize = 500;
const TURN_POLLS: usize = 750;

pub(super) struct ChatPrompt {
    pub pane: super::super::PaneId,
    pub session: SessionId,
    pub pending_id: u64,
    pub bytes: Vec<u8>,
    pub opens_terminal: bool,
    pub local_slash: bool,
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
            bytes,
            opens_terminal,
            local_slash,
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
            if applied && opens_terminal {
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
            if applied && local_slash {
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
                for step in 0..TURN_POLLS {
                    let Ok((current, confirmed)) = this.update_in(cx, |this, _, cx| {
                        let current = this.chat_prompt_current(&origin, cx);
                        let confirmed = current && this.chat_prompt_confirmed(&origin, pending_id);
                        if current && !confirmed && step % 5 == 0 {
                            this.poll_session_chat(pane, session, cx);
                        }
                        (current, confirmed)
                    }) else {
                        return;
                    };
                    if !current {
                        break;
                    }
                    if confirmed {
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
