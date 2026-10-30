//! Paste submission waits for PTY receipts, then for the provider's user turn.
use super::{ChatKey, Context, SessionId, Shell, TextareaState, Window};
use krill_proto::input::{Delivery, Receipt};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(20);
const RECEIPT_POLLS: usize = 500;
const TURN_POLLS: usize = 750;

pub(super) struct ChatPrompt {
    pub pane: super::super::PaneId,
    pub session: SessionId,
    pub input: gpui::Entity<TextareaState>,
    pub text: String,
    pub bytes: Vec<u8>,
}

struct Origin {
    pane: super::super::PaneId,
    session: SessionId,
    host: krill_app::hosts::HostId,
    target: Option<krill_app::hosts::HostTarget>,
    epoch: u64,
    item: usize,
}

impl Origin {
    fn key(&self) -> ChatKey {
        (self.host.clone(), self.session)
    }
}

impl Shell {
    fn chat_prompt_current(&self, origin: &Origin, cx: &Context<Self>) -> bool {
        let Origin {
            pane,
            session,
            host,
            target,
            epoch,
            ..
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

    fn chat_prompt_delivery(
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

    fn chat_prompt_confirmed(&self, origin: &Origin, text: &str) -> bool {
        #[cfg(test)]
        if self.chat.send_input.is_some() {
            return self.chat.prompt_confirmed.unwrap_or(true);
        }
        self.chat.logs.get(&origin.key()).is_some_and(|log| {
            log.bound
                && log.epoch == origin.epoch
                && log.items.iter().enumerate().any(|(i, item)| {
                    log.base_index.saturating_add(i) >= origin.item
                        && item.kind == krill_proto::agent_history::ChatItemKind::User
                        && super::normalized(&item.text) == super::normalized(text)
                })
        })
    }

    fn finish_chat_prompt(&mut self, key: &ChatKey, confirmed: bool, cx: &mut Context<Self>) {
        self.chat.prompt_tasks.remove(key);
        if confirmed {
            self.chat.prompt_failed.remove(key);
        } else {
            self.chat.prompt_review.insert(key.clone());
        }
        cx.notify();
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
            input,
            text,
            bytes,
        } = prompt;
        let key = (self.layout.host(pane).clone(), session);
        let log = self.chat.logs.get(&key);
        let origin = Origin {
            pane,
            session,
            host: key.0.clone(),
            target: self.live.get(&pane).and_then(|live| live.target.clone()),
            epoch: log.map_or(0, |log| log.epoch),
            item: log.map_or(0, |log| log.base_index.saturating_add(log.items.len())),
        };
        if !self.send_to_chat_pane(pane, session, &bytes, cx) {
            self.chat.prompt_failed.insert(key);
            cx.notify();
            return;
        }
        let Some(receipt) = self.live.get(&pane).map(|live| live.pane.input_receipt()) else {
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
                this.update_in(cx, |this, _, cx| {
                    this.finish_chat_prompt(&task_key, false, cx);
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
                this.update_in(cx, |this, _, cx| {
                    this.finish_chat_prompt(&task_key, false, cx);
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
            if applied {
                for _ in 0..TURN_POLLS {
                    let Ok((current, confirmed)) = this.update_in(cx, |this, _, cx| {
                        (
                            this.chat_prompt_current(&origin, cx),
                            this.chat_prompt_confirmed(&origin, &text),
                        )
                    }) else {
                        return;
                    };
                    if !current {
                        break;
                    }
                    if confirmed {
                        this.update_in(cx, |this, window, cx| {
                            Self::clear_sent_chat_draft(&input, &text, window, cx);
                            this.finish_chat_prompt(&task_key, true, cx);
                        })
                        .ok();
                        return;
                    }
                    cx.background_executor().timer(POLL).await;
                }
            }
            // Applied only proves a PTY write, never provider consumption. Keep
            // the draft and require review rather than replaying or adding Enter.
            this.update_in(cx, |this, _, cx| {
                this.finish_chat_prompt(&task_key, false, cx);
            })
            .ok();
        });
        self.chat.prompt_tasks.insert(key, task);
        cx.notify();
    }
}
