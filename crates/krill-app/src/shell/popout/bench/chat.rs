//! Synthetic transcript rendered through the production Chat view in a native window.
//! Run with an isolated `KRILL_STATE_DIR` and --popout-bench=chat.

use super::*;
use krill_app::i18n::catalog::chat_preview as preview;
use krill_proto::agent_history::{ChatItem, ChatItemKind, ChatPollSnapshot, ChatQuestionOption};

struct Transcript;

impl sidebar::Source for Transcript {
    fn fetch(&self) -> io::Result<sidebar::Snapshot> {
        Ok(sidebar::Snapshot {
            sessions: vec![sidebar::Session {
                id: SessionId(0),
                status: AgentStatus {
                    state: AgentState::Done,
                    agent: "codex".into(),
                    ..AgentStatus::UNKNOWN
                },
                worktree: None,
                cwd: None,
                account: None,
            }],
            ..Default::default()
        })
    }
}

fn item(kind: ChatItemKind, text: &str) -> ChatItem {
    ChatItem {
        kind,
        text: text.into(),
        ..Default::default()
    }
}

pub(super) fn install(shell: &mut Shell, window: &mut gpui::Window, cx: &mut Context<Shell>) {
    shell.link = Link::Down;
    shell.local_generation = shell.local_generation.wrapping_add(1);
    shell.accounts_poll = None;
    shell.usage_refresh = None;
    shell.orch_poll = None;
    screen(shell, cx);
    let session = shell.active_session_id().unwrap_or(SessionId(0));
    shell.sidebar.update(cx, |sidebar, cx| {
        sidebar.connect_source(Arc::new(Transcript), cx);
    });
    let empty = std::env::var_os("KRILL_CHAT_PREVIEW_EMPTY").is_some();
    let items = if empty {
        Vec::new()
    } else {
        vec![
            item(ChatItemKind::User, preview::USER.get()),
            item(ChatItemKind::Reasoning, preview::REASONING.get()),
            ChatItem {
                tool_name: "Read".into(),
                title: "Inspect chat view".into(),
                tool_id: "preview-read".into(),
                ..item(ChatItemKind::ToolCall, "crates/krill-app/src/shell/chat.rs")
            },
            ChatItem {
                tool_id: "preview-read".into(),
                ..item(ChatItemKind::ToolResult, preview::TOOL_RESULT.get())
            },
            item(ChatItemKind::Assistant, preview::ASSISTANT.get()),
            ChatItem {
                title: preview::QUESTION_TITLE.get().into(),
                tool_id: "preview-question".into(),
                options: vec![
                    ChatQuestionOption {
                        label: preview::REVIEW.get().into(),
                        description: preview::REVIEW_DESC.get().into(),
                    },
                    ChatQuestionOption {
                        label: preview::KEYS.get().into(),
                        description: preview::KEYS_DESC.get().into(),
                    },
                ],
                ..item(ChatItemKind::Question, preview::QUESTION.get())
            },
            ChatItem {
                title: "Permission request".into(),
                tool_id: "preview-permission".into(),
                ..item(
                    ChatItemKind::Permission,
                    "Synthetic request: review a local file. This preview has no input transport.",
                )
            },
        ]
    };
    if let Some(pane) = shell.layout.focused() {
        shell.layout.set_chat_view(pane, true);
    }
    shell.apply_chat_poll_for_host(
        session,
        krill_app::hosts::HostId::Local,
        ChatPollSnapshot {
            agent: "codex".into(),
            session_id: "synthetic-chat-preview".into(),
            transcript_path: "/synthetic/chat-preview.jsonl".into(),
            bound: true,
            items,
            ..Default::default()
        },
    );
    if let Ok(width) = std::env::var("KRILL_BENCH_WIDTH")
        && let Ok(width) = width.parse::<f32>()
    {
        let height = std::env::var("KRILL_BENCH_HEIGHT")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(800.);
        window.resize(gpui::size(
            px(width.clamp(640., 1600.)),
            px(height.clamp(480., 1200.)),
        ));
    }
    cx.notify();
}
