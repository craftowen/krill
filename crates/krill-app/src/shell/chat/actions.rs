//! Chat controls are adapters for a visible terminal prompt, not an agent RPC/ACK.
use super::*;
use krill_app::hosts::HostId;

#[derive(Clone)]
pub(super) struct RequestIdentity {
    pane: super::super::PaneId,
    host: HostId,
    pub(super) session: SessionId,
    epoch: u64,
    pub(super) key: String,
    index: usize,
    pub(super) item: ChatItem,
}

pub(super) enum Submission {
    Sending,
    Awaiting,
    Confirmed,
    Failed,
    /// Some keys may have reached the TUI. Replaying a toggle could undo it.
    ReviewRequired,
}

impl Submission {
    pub(super) fn blocks_input(&self) -> bool {
        matches!(
            self,
            Self::Sending | Self::Awaiting | Self::Confirmed | Self::ReviewRequired
        )
    }

    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Sending => ctext::REQUEST_SENDING.get(),
            Self::Awaiting => ctext::REQUEST_QUEUED.get(),
            Self::Confirmed => ctext::REQUEST_CONFIRMED.get(),
            Self::Failed => ctext::REQUEST_FAILED.get(),
            Self::ReviewRequired => ctext::REQUEST_REVIEW.get(),
        }
    }
}

/// Require a recognizable prompt and the card's actual content. Generic approval
/// hints alone are insufficient: historical cards must never type into a shell.
fn screen_matches(item: &ChatItem, rows: &[Vec<Cell>]) -> bool {
    let screen = normalized(
        &rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let text = normalized(&item.text);
    let title = normalized(&item.title);
    match item.kind {
        ChatItemKind::Question => {
            ((!text.is_empty() && screen.contains(&text))
                || (!title.is_empty() && screen.contains(&title)))
                && !item.options.is_empty()
                && question_option_rows(
                    rows,
                    &item
                        .options
                        .iter()
                        .map(|opt| opt.label.clone())
                        .collect::<Vec<_>>(),
                )
                .is_some_and(|matched| matched.iter().all(Option::is_some))
                && detect_question_cursor_index(
                    rows,
                    &item
                        .options
                        .iter()
                        .map(|opt| opt.label.clone())
                        .collect::<Vec<_>>(),
                )
                .is_some()
        }
        ChatItemKind::Permission => {
            text.chars().count() >= 8
                && screen.contains(&text)
                && ["(y/n)", "[y/n]"]
                    .iter()
                    .any(|hint| screen.to_lowercase().contains(hint))
        }
        _ => false,
    }
}

impl Shell {
    pub(super) fn chat_request_identity(
        &self,
        pane: super::super::PaneId,
        session: SessionId,
        index: usize,
        item: &ChatItem,
    ) -> RequestIdentity {
        RequestIdentity {
            pane,
            host: self.layout.host(pane).clone(),
            session,
            epoch: self.chat.logs.get(&session).map_or(0, |log| log.epoch),
            key: format!("{}:{index}", item.tool_id),
            index,
            item: item.clone(),
        }
    }

    fn chat_request_binding_matches(&self, request: &RequestIdentity, cx: &gpui::App) -> bool {
        if !self.is_pane_chat_view(request.pane, cx)
            || self.layout.session(request.pane) != Some(request.session)
            || self.layout.host(request.pane) != &request.host
            || self.layout.file(request.pane).is_some()
        {
            return false;
        }
        let Some(log) = self.chat.logs.get(&request.session) else {
            return false;
        };
        if !log.bound
            || log.epoch != request.epoch
            || log.host.as_ref() != Some(&request.host)
            || log.binding.0.is_empty()
            || log.binding.1.is_empty()
        {
            return false;
        }
        let Some(index) = request.index.checked_sub(log.base_index) else {
            return false;
        };
        if log
            .items
            .get(index)
            .is_none_or(|item| item.as_ref() != &request.item)
            || log.answered.contains(&request.key)
        {
            return false;
        }
        // Only the most recent unresolved request, with no later conversation,
        // can be backed by the current live prompt.
        !log.items.iter().skip(index + 1).any(|item| {
            matches!(
                item.kind,
                ChatItemKind::Question
                    | ChatItemKind::Permission
                    | ChatItemKind::User
                    | ChatItemKind::Assistant
                    | ChatItemKind::ToolResult
            )
        })
    }

    pub(super) fn chat_request_is_current(
        &self,
        request: &RequestIdentity,
        cx: &gpui::App,
    ) -> bool {
        !self.paste_queue.has_pending_for(request.pane)
            && self
                .sidebar
                .read(cx)
                .session_on(&request.host, request.session)
                .is_some_and(|session| {
                    matches!(
                        session.status.state,
                        krill_proto::control::AgentState::Waiting
                            | krill_proto::control::AgentState::Blocked
                    )
                })
            && self.chat_request_binding_matches(request, cx)
            && self.live.get(&request.pane).is_some_and(|live| {
                live.target
                    .as_ref()
                    .is_some_and(|target| target.host() == &request.host && target.is_current())
                    && live.pane.session() == Some(request.session)
                    && live.pane.is_linked()
                    && !live.suspended
                    && !live.pane.is_cold()
                    && !live.pane.is_closed()
                    && !live.pane.account_switching()
                    && screen_matches(&request.item, &live.pane.screen().rows)
                    && (!request.item.multi_select
                        || checked_options(
                            &live.pane.screen().rows,
                            &request
                                .item
                                .options
                                .iter()
                                .map(|opt| opt.label.clone())
                                .collect::<Vec<_>>(),
                        )
                        .is_some())
            })
    }

    fn set_chat_submission(&mut self, request: &RequestIdentity, state: Submission) {
        if let Some(log) = self.chat.logs.get_mut(&request.session)
            && log.epoch == request.epoch
        {
            log.submissions.insert(request.key.clone(), state);
            log.revision = log.revision.wrapping_add(1);
        }
    }

    pub(super) fn answer_chat_question(
        &mut self,
        request: RequestIdentity,
        selected: Vec<usize>,
        multi: bool,
        cx: &mut Context<Self>,
    ) {
        if selected.is_empty()
            || selected
                .iter()
                .any(|&index| index >= request.item.options.len())
            || request.item.multi_select != multi
        {
            return;
        }
        if !self.chat_request_is_current(&request, cx)
            || self.chat.logs.get(&request.session).is_some_and(|log| {
                log.submissions
                    .get(&request.key)
                    .is_some_and(Submission::blocks_input)
            })
        {
            return;
        }
        if let Some(log) = self.chat.logs.get_mut(&request.session) {
            log.multi_selected
                .insert(request.key.clone(), selected.iter().copied().collect());
        }
        let labels: Vec<_> = request
            .item
            .options
            .iter()
            .map(|opt| opt.label.clone())
            .collect();
        let Some(mut cursor) = self
            .live
            .get(&request.pane)
            .and_then(|live| detect_question_cursor_index(&live.pane.screen().rows, &labels))
        else {
            return;
        };
        let mut steps = Vec::new();
        if multi {
            // Do not assume an empty checked set. Only automate a multiselect
            // when the live TUI exposes every checkbox state explicitly.
            let Some(checked) = self
                .live
                .get(&request.pane)
                .and_then(|live| checked_options(&live.pane.screen().rows, &labels))
            else {
                return;
            };
            let desired: BTreeSet<_> = selected.into_iter().collect();
            for &target in checked.symmetric_difference(&desired) {
                step_cursor(&mut steps, &mut cursor, target);
                steps.push(b" ".to_vec());
            }
        } else {
            step_cursor(&mut steps, &mut cursor, selected[0]);
        }
        steps.push(b"\r".to_vec());
        self.start_chat_request(request, steps, cx);
    }

    pub(super) fn approve_chat_permission(
        &mut self,
        request: RequestIdentity,
        yes: bool,
        cx: &mut Context<Self>,
    ) {
        if request.item.kind != ChatItemKind::Permission {
            return;
        }
        self.start_chat_request(
            request,
            vec![if yes {
                b"y\r".to_vec()
            } else {
                b"n\r".to_vec()
            }],
            cx,
        );
    }

    fn start_chat_request(
        &mut self,
        request: RequestIdentity,
        steps: Vec<Vec<u8>>,
        cx: &mut Context<Self>,
    ) {
        if !self.chat_request_is_current(&request, cx)
            || self.chat.logs.get(&request.session).is_some_and(|log| {
                log.submissions
                    .get(&request.key)
                    .is_some_and(Submission::blocks_input)
            })
        {
            return;
        }
        self.set_chat_submission(&request, Submission::Sending);
        let session = request.session;
        let task = cx.spawn(async move |this, cx| {
            for (sent, bytes) in steps.into_iter().enumerate() {
                if sent > 0 {
                    cx.background_executor()
                        .timer(Duration::from_millis(60))
                        .await;
                }
                let ok = this
                    .update(cx, |this, cx| {
                        // Recheck identity for every key; cursor movement itself need
                        // not preserve the old cursor position, only the same prompt.
                        this.chat_request_is_current(&request, cx)
                            && this.send_to_chat_pane(request.pane, request.session, &bytes, cx)
                    })
                    .unwrap_or(false);
                if !ok {
                    this.update(cx, |this, cx| {
                        this.set_chat_submission(
                            &request,
                            if sent > 0 {
                                Submission::ReviewRequired
                            } else {
                                Submission::Failed
                            },
                        );
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.set_chat_submission(&request, Submission::Awaiting);
                cx.notify();
            })
            .ok();
            // Queue delivery has no agent acceptance ACK. Bound the waiting
            // state and use the terminal for ambiguous retries instead of
            // replaying an approval that may already have been accepted.
            cx.background_executor().timer(Duration::from_secs(5)).await;
            this.update(cx, |this, cx| {
                if this.chat.logs.get(&session).is_some_and(|log| {
                    log.epoch == request.epoch
                        && matches!(
                            log.submissions.get(&request.key),
                            Some(Submission::Awaiting)
                        )
                }) {
                    this.set_chat_submission(&request, Submission::ReviewRequired);
                    cx.notify();
                }
            })
            .ok();
        });
        self.chat.request_tasks.insert(session, task);
        cx.notify();
    }
}

pub(super) fn checked_options(rows: &[Vec<Cell>], labels: &[String]) -> Option<BTreeSet<usize>> {
    let matched = question_option_rows(rows, labels)?;
    let mut checked = BTreeSet::new();
    for (index, row) in matched.into_iter().enumerate() {
        if row?.checked? {
            checked.insert(index);
        }
    }
    Some(checked)
}
