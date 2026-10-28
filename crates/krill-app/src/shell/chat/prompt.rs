//! Multiline input uses the existing terminal paste contract, then a separate Enter.
use super::{Context, Duration, SessionId, Shell, TextareaState, Window};

pub(super) struct MultilinePrompt {
    pub pane: super::super::PaneId,
    pub session: SessionId,
    pub input: gpui::Entity<TextareaState>,
    pub text: String,
    pub bytes: Vec<u8>,
}

impl Shell {
    pub(super) fn start_multiline_chat_prompt(
        &mut self,
        prompt: MultilinePrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let MultilinePrompt {
            pane,
            session,
            input,
            text,
            bytes,
        } = prompt;
        let host = self.layout.host(pane).clone();
        let target = self.live.get(&pane).and_then(|live| live.target.clone());
        let epoch = self.chat.logs.get(&session).map_or(0, |log| log.epoch);
        if !self.send_to_chat_pane(pane, session, &bytes, cx) {
            self.chat.prompt_failed.insert(session);
            cx.notify();
            return;
        }
        self.chat.prompt_failed.remove(&session);
        let task = cx.spawn_in(window, async move |this, cx| {
            // Input ACK settles the PTY write, not the provider's ingestion or
            // acceptance. Never infer delivery from a fixed elapsed delay.
            let mut submitted = false;
            for attempt in 0..250 {
                cx.background_executor()
                    .timer(Duration::from_millis(20))
                    .await;
                let finished = this
                    .update_in(cx, |this, window, cx| {
                        let same_origin = this.is_session_chat_view(session, cx)
                            && !this.chat.prompt_review.contains(&session)
                            && this.layout.host(pane) == &host
                            && this.layout.session(pane) == Some(session)
                            && this.chat.logs.get(&session).map_or(0, |log| log.epoch) == epoch
                            && this.live.get(&pane).is_some_and(|live| {
                                live.target == target
                                    && live.pane.is_linked()
                                    && !live.pane.account_switching()
                                    && !live.suspended
                            })
                            && target
                                .as_ref()
                                .is_some_and(krill_app::hosts::HostTarget::is_current);
                        if !same_origin || attempt == 249 {
                            this.chat.prompt_tasks.remove(&session);
                            this.chat.prompt_review.insert(session);
                            cx.notify();
                            return true;
                        }
                        if this
                            .live
                            .get(&pane)
                            .is_none_or(|live| live.pane.has_pending_input())
                        {
                            return false;
                        }
                        if submitted {
                            this.chat.prompt_tasks.remove(&session);
                            Self::clear_sent_chat_draft(&input, &text, window, cx);
                            this.chat.prompt_failed.remove(&session);
                            cx.notify();
                            return true;
                        }
                        let task = this.chat.prompt_tasks.remove(&session);
                        let sent = this.send_to_chat_pane(pane, session, b"\r", cx);
                        if sent {
                            if let Some(task) = task {
                                this.chat.prompt_tasks.insert(session, task);
                            }
                            submitted = true;
                            return false;
                        }
                        this.chat.prompt_review.insert(session);
                        cx.notify();
                        true
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        });
        self.chat.prompt_tasks.insert(session, task);
        cx.notify();
    }
}
