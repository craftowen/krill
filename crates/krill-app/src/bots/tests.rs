// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).

use std::sync::{Arc, Mutex};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px};
use krill_proto::SessionId;
use krill_proto::bots::{
    BOT_MCP_APPS, BUILTIN_BOT_TEMPLATES, BotAccessRequest, BotAccessRequestStatus, BotConfig,
    BotConnectorStatus, BotId, BotItem, BotItemEvent, BotItemEventKind, BotItemLink, BotItemStatus,
    BotMcpAuthKind, BotRpcRequest, BotRpcResponse, BotSuggestion, BotTemplateCategory,
    BotToolRelayStatus, BotToolServerStatus, COMPOSIO_CURATED_APPS, ConnectorHealthState,
    ExternalBotCandidate, ExternalBotSourceApp, ExternalSource, forget_memory_fact,
    generate_slack_socket_mode_manifest,
};

use super::create::{ModelChoice, filter_templates};
use super::items::ItemFilter;
use super::rpc::{
    BOT_RUNTIMES, BotRpcCall, format_csv_i64s, format_csv_strings, parse_csv_i64s,
    parse_csv_strings, runtime_models, sidebar_snapshot,
};
use super::{BadgeChanged, Close, DetailTab, OpenSession, Screen, View};

#[derive(Default)]
struct MockState {
    bots: Vec<BotConfig>,
    items: Vec<BotItem>,
    soul: String,
    memory: String,
    statuses: Vec<BotConnectorStatus>,
    access_requests: Vec<BotAccessRequest>,
    relay: BotToolRelayStatus,
    secrets: Vec<(String, String)>,
    requests: Vec<BotRpcRequest>,
    candidates: Vec<ExternalBotCandidate>,
    suggestions: Vec<BotSuggestion>,
}

fn sample_bot(id: &str, name: &str) -> BotConfig {
    let mut cfg =
        BotConfig::new(BotId::new(id).unwrap(), name, "claude", 1_700_000_000_000).unwrap();
    cfg.description = "Reviews pull requests automatically".to_owned();
    cfg.emoji = Some("🤖".to_owned());
    cfg.connectors.slack.enabled = true;
    cfg
}

fn sample_item(bot_id: &str, uid: &str, number: u64, status: BotItemStatus) -> BotItem {
    BotItem {
        uid: uid.to_owned(),
        number,
        owner_bot_id: BotId::new(bot_id).unwrap(),
        title: format!("Task #{number}"),
        description: Some("Detailed context for task".to_owned()),
        done_means: Some("Tests pass and PR merged".to_owned()),
        status,
        close_note: None,
        links: vec![BotItemLink::Pr(
            "https://github.com/acme/repo/pull/42".to_owned(),
        )],
        events: vec![BotItemEvent {
            id: 1,
            item_uid: uid.to_owned(),
            kind: BotItemEventKind::Commented,
            actor: krill_proto::bots::BotItemActor::Owner { device: None },
            text: "Initial comment".to_owned(),
            decisions: Vec::new(),
            created_at_ms: 1_700_000_000_000,
        }],
        snoozed_until_ms: None,
        version: 1,
        last_activity_at_ms: 1_700_000_000_000,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_000_000,
        closed_at_ms: None,
    }
}

fn make_mock_rpc(state: Arc<Mutex<MockState>>) -> BotRpcCall {
    Arc::new(move |req: &BotRpcRequest| {
        let mut st = state.lock().unwrap();
        st.requests.push(req.clone());
        match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(st.bots.clone())),
            BotRpcRequest::CreateBot(params) => {
                let id = format!("bot-{}", st.bots.len() + 1);
                let mut cfg = BotConfig::new(
                    BotId::new(&id).unwrap(),
                    &params.name,
                    &params.agent_id,
                    1_700_000_000_000,
                )
                .unwrap();
                cfg.description.clone_from(&params.description);
                cfg.emoji.clone_from(&params.emoji);
                cfg.model.clone_from(&params.model);
                cfg.workspace.clone_from(&params.workspace);
                cfg.template_id.clone_from(&params.template_id);
                if let Some(s) = &params.initial_soul {
                    st.soul.clone_from(s);
                }
                st.bots.push(cfg.clone());
                Ok(BotRpcResponse::Bot(Box::new(cfg)))
            }
            BotRpcRequest::UpdateBot(params) => {
                let bot = st
                    .bots
                    .iter_mut()
                    .find(|b| b.id == params.bot_id)
                    .ok_or_else(|| "bot not found".to_owned())?;
                if let Some(p) = params.paused {
                    bot.paused = p;
                }
                if let Some(tools) = &params.mcp_tools {
                    bot.mcp_tools.clone_from(tools);
                }
                Ok(BotRpcResponse::Bot(Box::new(bot.clone())))
            }
            BotRpcRequest::DeleteBot { bot_id, .. } => {
                st.bots.retain(|b| &b.id != bot_id);
                Ok(BotRpcResponse::Deleted {
                    bot_id: bot_id.clone(),
                })
            }
            BotRpcRequest::StartBotSession { bot_id, .. } => Ok(BotRpcResponse::SessionStarted {
                bot_id: bot_id.clone(),
                session_id: SessionId(77),
                pid: 4242,
            }),
            BotRpcRequest::GetSoul { .. } => Ok(BotRpcResponse::Text(st.soul.clone())),
            BotRpcRequest::SetSoul { content, .. } => {
                st.soul.clone_from(content);
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::GetMemory { .. } => Ok(BotRpcResponse::Text(st.memory.clone())),
            BotRpcRequest::UpdateMemory { content, .. } => {
                st.memory.clone_from(content);
                Ok(BotRpcResponse::MemoryUpdated {
                    content: st.memory.clone(),
                    restored_facts: Vec::new(),
                    removed_count: 0,
                })
            }
            BotRpcRequest::ForgetMemoryFact { fact, .. } => {
                let (next, removed) = forget_memory_fact(&st.memory, fact);
                st.memory = next.clone();
                Ok(BotRpcResponse::MemoryUpdated {
                    content: next,
                    restored_facts: Vec::new(),
                    removed_count: removed,
                })
            }
            BotRpcRequest::RunMemoryTidy { bot_id } => Ok(BotRpcResponse::MemoryTidyResult {
                bot_id: bot_id.clone(),
                summary: "Compacted 1 duplicate fact".to_owned(),
                changes: 1,
            }),
            BotRpcRequest::RunLookAround { bot_id } => Ok(BotRpcResponse::LookAroundResult {
                bot_id: bot_id.clone(),
                suggestions: st.suggestions.clone(),
            }),
            BotRpcRequest::ListItems {
                bot_id,
                include_closed,
            } => {
                let filtered = st
                    .items
                    .iter()
                    .filter(|i| bot_id.as_ref().is_none_or(|b| &i.owner_bot_id == b))
                    .filter(|i| *include_closed || i.status.is_open())
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::Items(filtered))
            }
            BotRpcRequest::CreateItem(params) => {
                let num = (st.items.len() as u64) + 1;
                let item = BotItem {
                    uid: format!("item-{num}"),
                    number: num,
                    owner_bot_id: params.bot_id.clone(),
                    title: params.title.clone(),
                    description: params.description.clone(),
                    done_means: params.done_means.clone(),
                    status: params.status.unwrap_or(BotItemStatus::Planned),
                    close_note: None,
                    links: params.links.clone(),
                    events: Vec::new(),
                    snoozed_until_ms: None,
                    version: 1,
                    last_activity_at_ms: 1_700_000_000_000,
                    created_at_ms: 1_700_000_000_000,
                    updated_at_ms: 1_700_000_000_000,
                    closed_at_ms: None,
                };
                st.items.push(item.clone());
                Ok(BotRpcResponse::Item(Box::new(item)))
            }
            BotRpcRequest::UpdateItem(params) => {
                let item = st
                    .items
                    .iter_mut()
                    .find(|i| i.uid == params.item_id)
                    .ok_or_else(|| "item not found".to_owned())?;
                if let Some(status) = params.status {
                    item.status = status;
                }
                params
                    .snoozed_until_ms
                    .clone()
                    .apply(&mut item.snoozed_until_ms);
                Ok(BotRpcResponse::Item(Box::new(item.clone())))
            }
            BotRpcRequest::AddItemComment { item_id, text, .. } => {
                let item = st
                    .items
                    .iter_mut()
                    .find(|i| i.uid == *item_id)
                    .ok_or_else(|| "item not found".to_owned())?;
                item.events.push(BotItemEvent {
                    id: (item.events.len() as u64) + 1,
                    item_uid: item.uid.clone(),
                    kind: BotItemEventKind::Commented,
                    actor: krill_proto::bots::BotItemActor::Owner { device: None },
                    text: text.clone(),
                    decisions: Vec::new(),
                    created_at_ms: 1_700_000_000_000,
                });
                Ok(BotRpcResponse::Item(Box::new(item.clone())))
            }
            BotRpcRequest::AddItemLink { item_id, link } => {
                let item = st
                    .items
                    .iter_mut()
                    .find(|i| i.uid == *item_id)
                    .ok_or_else(|| "item not found".to_owned())?;
                item.links.push(link.clone());
                Ok(BotRpcResponse::Item(Box::new(item.clone())))
            }
            BotRpcRequest::SetConnectorConfig { bot_id, connectors } => {
                let bot = st
                    .bots
                    .iter_mut()
                    .find(|b| &b.id == bot_id)
                    .ok_or_else(|| "bot not found".to_owned())?;
                bot.connectors = (**connectors).clone();
                Ok(BotRpcResponse::Bot(Box::new(bot.clone())))
            }
            BotRpcRequest::SetSecret { secret_ref, value } => {
                st.secrets.push((secret_ref.clone(), value.clone()));
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::ConnectorStatus { bot_id } => {
                let filtered = st
                    .statuses
                    .iter()
                    .filter(|s| bot_id.as_ref().is_none_or(|b| &s.bot_id == b))
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::ConnectorStatuses(filtered))
            }
            BotRpcRequest::TriggerSyncNow { bot_id, source } => Ok(BotRpcResponse::SyncTriggered {
                bot_id: bot_id.clone(),
                sources_synced: source.iter().copied().collect(),
            }),
            BotRpcRequest::ListAccessRequests { bot_id } => {
                let filtered = st
                    .access_requests
                    .iter()
                    .filter(|r| bot_id.as_ref().is_none_or(|b| &r.bot_id == b))
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::AccessRequests(filtered))
            }
            BotRpcRequest::DecideAccessRequest { token, approve } => {
                let req = st
                    .access_requests
                    .iter_mut()
                    .find(|r| r.token == *token)
                    .ok_or_else(|| "request not found".to_owned())?;
                req.status = if *approve {
                    BotAccessRequestStatus::Approved
                } else {
                    BotAccessRequestStatus::Declined
                };
                Ok(BotRpcResponse::AccessRequestDecided(req.clone()))
            }
            BotRpcRequest::StartGoogleOAuth { .. } => Ok(BotRpcResponse::GoogleOAuthStarted {
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth?state=test".to_owned(),
                state: "test".to_owned(),
            }),
            BotRpcRequest::ToolRelayStatus => Ok(BotRpcResponse::ToolRelayStatus(st.relay.clone())),
            BotRpcRequest::ImportExternalBots { dry_run, .. } => {
                let cands = st.candidates.clone();
                let imported = if *dry_run {
                    Vec::new()
                } else {
                    let b = sample_bot("imported-orca", "Imported Orca Bot");
                    st.bots.push(b.clone());
                    vec![b]
                };
                Ok(BotRpcResponse::ImportedExternalBots {
                    candidates: cands,
                    imported,
                })
            }
            _ => Ok(BotRpcResponse::Ok),
        }
    })
}

fn open_view(
    cx: &mut TestAppContext,
    state: Arc<Mutex<MockState>>,
) -> (Entity<View>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state);
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx));
    vcx.simulate_resize(gpui::size(px(1040.), px(720.)));
    vcx.run_until_parked();
    (view, vcx)
}

#[test]
fn catalog_helpers_cover_24_templates_6_categories_8_runtimes_and_csv() {
    assert_eq!(BUILTIN_BOT_TEMPLATES.len(), 24);
    assert_eq!(filter_templates(None, "").len(), 24);
    let mut sum = 0usize;
    for &cat in BotTemplateCategory::ALL {
        let count = filter_templates(Some(cat), "").len();
        assert!(count >= 2, "category {cat:?} should have templates");
        sum += count;
    }
    assert_eq!(sum, 24);
    assert_eq!(BOT_RUNTIMES.len(), 8);
    assert_eq!(BOT_MCP_APPS.len(), 13);
    assert_eq!(COMPOSIO_CURATED_APPS.len(), 24);

    assert_eq!(
        parse_csv_strings(" acme/a, acme/b\nacme/a , "),
        vec!["acme/a", "acme/b"]
    );
    assert_eq!(format_csv_strings(&["a".into(), "b".into()]), "a, b");
    assert_eq!(
        parse_csv_i64s("123, -100456, bad, 123"),
        vec![123, -100_456]
    );
    assert_eq!(format_csv_i64s(&[123, -100_456]), "123, -100456");
}

#[gpui::test]
fn create_view_filters_all_24_templates_and_creates_bot(cx: &mut TestAppContext) {
    let state = Arc::new(Mutex::new(MockState::default()));
    let (view, vcx) = open_view(cx, state.clone());

    assert!(vcx.debug_bounds("bots-empty").is_some());
    let new_btn = vcx.debug_bounds("bots-new-btn").unwrap();
    vcx.simulate_click(new_btn.center(), Modifiers::default());
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("bots-create-view").is_some());
    assert!(vcx.debug_bounds("bots-runtime-picker").is_some());
    assert!(vcx.debug_bounds("bots-model-picker").is_some());
    assert!(vcx.debug_bounds("bots-create-pick-folder").is_some());
    assert!(
        vcx.debug_bounds("bots-template-pr-review").is_none(),
        "templates stay folded until asked for"
    );
    let toggle = vcx.debug_bounds("bots-create-toggle-templates").unwrap();
    vcx.simulate_click(toggle.center(), Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-template-pr-review").is_some());

    // Filter to Releases category (4 templates)
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_template_category(Some(BotTemplateCategory::Releases), cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-template-pr-review").is_none());

    // Select all categories, pick PR review template, choose Codex runtime, create
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_template_category(None, cx);
            v.select_template(&BUILTIN_BOT_TEMPLATES[0], Some(window), cx);
            v.set_create_model(ModelChoice::Preset("opus"), cx);
            v.set_create_runtime("codex", cx);
            assert_eq!(
                v.create.model,
                ModelChoice::Default,
                "a Claude preset must not follow the bot to Codex"
            );
            v.set_create_model(ModelChoice::Preset("gpt-5.4"), cx);
            v.submit_create_bot(cx);
        });
    });
    vcx.run_until_parked();

    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert_eq!(v.screen(), Screen::Detail);
        assert_eq!(v.bots().len(), 1);
        assert_eq!(v.bots()[0].agent_id, "codex");
        assert_eq!(v.bots()[0].model.as_deref(), Some("gpt-5.4"));
        assert_eq!(v.bots()[0].template_id.as_deref(), Some("pr-review"));
    });
}

#[gpui::test]
fn items_inbox_filters_status_snooze_comments_and_open_chat(cx: &mut TestAppContext) {
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "PR Bot"));
    mock.items
        .push(sample_item("bot-1", "item-1", 1, BotItemStatus::NeedsYou));
    mock.items
        .push(sample_item("bot-1", "item-2", 2, BotItemStatus::Working));
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());

    let opened_session = Arc::new(Mutex::new(None));
    let opened_capture = opened_session.clone();
    let badge_seen = Arc::new(Mutex::new(0usize));
    let badge_capture = badge_seen.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &OpenSession, _| {
            *opened_capture.lock().unwrap() = Some(ev.session_id);
        })
        .detach();
        cx.subscribe(&view, move |_, ev: &BadgeChanged, _| {
            *badge_capture.lock().unwrap() = ev.attention_count;
        })
        .detach();
    });

    assert!(vcx.debug_bounds("bots-item-card-item-1").is_some());
    assert!(vcx.debug_bounds("bots-item-card-item-2").is_some());

    // Filter NeedsYou -> only item-1 shown
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_item_filter(ItemFilter::NeedsYou, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-item-card-item-1").is_some());
    assert!(vcx.debug_bounds("bots-item-card-item-2").is_none());

    // Mark item-1 Done -> badge drops to 0; add comment; create item; open chat
    let bot_id = BotId::new("bot-1").unwrap();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.update_item_status(bot_id.clone(), "item-1".into(), BotItemStatus::Done, cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(*badge_seen.lock().unwrap(), 0);

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_item_filter(ItemFilter::All, cx);
            v.snooze_item(bot_id.clone(), "item-2".into(), Some(9_999_999_999_999), cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        state.lock().unwrap().items[1].snoozed_until_ms,
        Some(9_999_999_999_999)
    );

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.submit_item_comment("item-2".into(), "Looks great", None, cx);
        });
    });
    vcx.run_until_parked();
    assert!(
        state.lock().unwrap().items[1]
            .events
            .iter()
            .any(|e| e.text == "Looks great")
    );

    let chat_btn = vcx.debug_bounds("bots-open-chat").unwrap();
    vcx.simulate_click(chat_btn.center(), Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(*opened_session.lock().unwrap(), Some(SessionId(77)));
}

#[gpui::test]
fn soul_memory_guard_blocks_silent_deletion_and_allows_forget_tidy_look_around(
    cx: &mut TestAppContext,
) {
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "Memory Bot"));
    mock.soul = "# Memory Bot\n\nReviews carefully.\n".to_owned();
    mock.memory =
        "# Memory\n\n- 2026-11-20 Always check tests\n- 2026-11-19 Prefer small PRs\n".to_owned();
    mock.suggestions.push(BotSuggestion {
        id: "sug-1".to_owned(),
        bot_id: BotId::new("bot-1").unwrap(),
        title: "Review flaky test".to_owned(),
        why: "Failed twice on CI".to_owned(),
        ask: "Want me to investigate?".to_owned(),
        link: Some("https://github.com/acme/repo/issues/9".to_owned()),
        source: Some(ExternalSource::GitHub),
        dismissed: false,
        created_at_ms: 1_700_000_000_000,
    });
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());
    let bot_id = BotId::new("bot-1").unwrap();

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::SoulMemory, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-tab-soul-memory-body").is_some());

    // Attempt to save MEMORY.md with a line silently deleted -> blocked by MemoryDeletionViolation!
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.save_memory(
                bot_id.clone(),
                "# Memory\n\n- 2026-11-20 Always check tests\n".to_owned(),
                cx,
            );
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bots-memory-deletion-violation").is_some(),
        "silent deletion must display MemoryDeletionViolation banner"
    );

    // Explicit Forget removes the fact cleanly via ForgetMemoryFact
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.forget_memory_fact(bot_id.clone(), "Prefer small PRs".to_owned(), cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-memory-deletion-violation").is_none());
    assert!(!state.lock().unwrap().memory.contains("Prefer small PRs"));

    // Run Memory Tidy-up & Run Look Around
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.run_memory_tidy(bot_id.clone(), cx));
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.run_look_around(bot_id.clone(), cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-suggestion-0").is_some());
}

#[gpui::test]
fn connectors_slack_manifest_composio_access_and_tools_relay_and_escape(cx: &mut TestAppContext) {
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "Ops Bot"));
    mock.statuses.push(BotConnectorStatus {
        bot_id: BotId::new("bot-1").unwrap(),
        source: ExternalSource::Slack,
        enabled: true,
        connected: true,
        state: ConnectorHealthState::Connected,
        account_label: Some("ops-ws".to_owned()),
        last_sync_ms: None,
        last_error: None,
        error_category: None,
    });
    mock.access_requests.push(BotAccessRequest {
        token: "tok-approve-me".to_owned(),
        bot_id: BotId::new("bot-1").unwrap(),
        source: ExternalSource::Telegram,
        sender_id: "u-99".to_owned(),
        sender_name: "Alice".to_owned(),
        channel_id: "chat-1".to_owned(),
        status: BotAccessRequestStatus::Pending,
        created_at: 1_700_000_000_000,
        decided_at: None,
    });
    mock.relay = BotToolRelayStatus {
        running: true,
        listen_addr: Some("127.0.0.1:49152".to_owned()),
        endpoint_url: Some("http://127.0.0.1:49152/bot-tools".to_owned()),
        connected_tools: vec![BotToolServerStatus {
            id: "sentry".to_owned(),
            name: "Sentry".to_owned(),
            endpoint_url: "https://mcp.sentry.dev/mcp".to_owned(),
            connected: true,
            tool_count: 5,
            last_error: None,
        }],
        last_error: None,
    };
    mock.candidates.push(ExternalBotCandidate {
        app: ExternalBotSourceApp::Orca,
        agent: "claude".to_owned(),
        name: "Migrated Bot".to_owned(),
        path: "/tmp/orca/bots/migrated".to_owned(),
        soul_preview: Some("Helps with triage".to_owned()),
    });
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());

    let closed = Arc::new(Mutex::new(false));
    let closed_cap = closed.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, _: &Close, _| {
            *closed_cap.lock().unwrap() = true;
        })
        .detach();
    });

    // Tab 3: Connectors & Access
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::Connectors, cx));
    });
    vcx.run_until_parked();
    for sel in [
        "bots-conn-telegram",
        "bots-conn-slack",
        "bots-conn-discord",
        "bots-conn-google",
        "bots-conn-github",
        "bots-conn-linear",
        "bots-conn-composio",
        "bots-access-req-0",
    ] {
        assert!(vcx.debug_bounds(sel).is_some(), "missing {sel}");
    }

    // Copy Slack Socket Mode manifest YAML
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.copy_slack_manifest("Ops Bot", cx));
    });
    vcx.run_until_parked();
    let expected_yaml = generate_slack_socket_mode_manifest("Ops Bot");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()),
        Some(expected_yaml)
    );

    // Approve access request
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.decide_access_request("tok-approve-me".to_owned(), true, cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        state.lock().unwrap().access_requests[0].status,
        BotAccessRequestStatus::Approved
    );

    // Tab 4: Tools & Sharing (all 13 MCP apps + ToolRelay status)
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::ToolsSharing, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-tool-relay-card").is_some());
    for sel in [
        "bots-mcp-app-vercel",
        "bots-mcp-app-sentry",
        "bots-mcp-app-datadog",
        "bots-mcp-app-posthog",
        "bots-mcp-app-cloudflare",
        "bots-mcp-app-supabase",
        "bots-mcp-app-neon",
        "bots-mcp-app-netlify",
        "bots-mcp-app-gitlab",
        "bots-mcp-app-context7",
        "bots-mcp-app-atlassian",
        "bots-mcp-app-notion",
        "bots-mcp-app-stripe",
    ] {
        assert!(vcx.debug_bounds(sel).is_some(), "missing MCP app {sel}");
    }
    assert_eq!(BOT_MCP_APPS[9].auth_kind, BotMcpAuthKind::None);

    // Import screen + External scan + Escape back to Detail + Escape emits Close
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.open_import(cx);
            v.run_external_import(true, cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-import-view").is_some());

    vcx.update(|_, cx| view.update(cx, View::handle_escape));
    vcx.run_until_parked();
    assert_eq!(view.read_with(vcx, |v, _| v.screen()), Screen::Detail);
    assert!(!*closed.lock().unwrap());

    vcx.update(|_, cx| view.update(cx, View::handle_escape));
    vcx.run_until_parked();
    assert!(*closed.lock().unwrap());
}

#[test]
fn sidebar_snapshot_counts_attention_per_bot_and_offers_models_per_runtime() {
    let bots = vec![sample_bot("bot-a", "A"), sample_bot("bot-b", "B")];
    let items = vec![
        sample_item("bot-a", "i1", 1, BotItemStatus::NeedsYou),
        sample_item("bot-a", "i2", 2, BotItemStatus::Working),
        sample_item("bot-gone", "i3", 3, BotItemStatus::NeedsYou),
    ];
    let snap = sidebar_snapshot(&bots, &items, &[]);
    let names: Vec<_> = snap
        .bots
        .iter()
        .map(|b| (b.name.as_str(), b.attention))
        .collect();
    assert_eq!(names, vec![("A", 1), ("B", 0)]);
    assert_eq!(
        snap.attention, 2,
        "the badge total keeps items of bots not listed"
    );

    for rt in BOT_RUNTIMES {
        let models = runtime_models(rt.id);
        assert!(
            models
                .iter()
                .all(|(id, label)| !id.is_empty() && !label.is_empty())
        );
    }
    assert!(runtime_models("claude").iter().any(|(id, _)| *id == "opus"));
    assert!(runtime_models("pi").is_empty());
}

#[gpui::test]
fn create_form_submits_with_cmd_enter_and_custom_model(cx: &mut TestAppContext) {
    let state = Arc::new(Mutex::new(MockState::default()));
    let (view, vcx) = open_view(cx, state.clone());
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.open_create(cx);
            v.set_create_model(ModelChoice::Custom, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let v = view.read(cx);
        let name = v.create.name_input.clone().unwrap();
        let model = v.create.model_input.clone().unwrap();
        name.update(cx, |s, cx| s.set_value("Release bot", window, cx));
        model.update(cx, |s, cx| s.set_value("claude-opus-5-5", window, cx));
        name.update(cx, |input, cx| input.focus(window, cx));
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("cmd-enter");
    vcx.run_until_parked();

    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert_eq!(v.screen(), Screen::Detail);
        assert_eq!(v.bots().len(), 1);
        assert_eq!(v.bots()[0].name, "Release bot");
        assert_eq!(v.bots()[0].model.as_deref(), Some("claude-opus-5-5"));
    });

    // The next New bot starts empty instead of offering "Release bot" again.
    vcx.update(|_, cx| view.update(cx, View::open_create));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        let name = v
            .create
            .name_input
            .as_ref()
            .unwrap()
            .read(cx)
            .value()
            .to_string();
        assert_eq!(name, "");
        assert_eq!(v.create.model, ModelChoice::Default);
    });
}

#[gpui::test]
fn create_form_rejects_empty_custom_model_and_missing_folder(cx: &mut TestAppContext) {
    // One existing bot, so Escape from the in-modal form would go back to it.
    let state = Arc::new(Mutex::new(MockState {
        bots: vec![sample_bot("bot-a", "A")],
        ..MockState::default()
    }));
    let (view, vcx) = open_view(cx, state.clone());
    vcx.update(|_, cx| view.update(cx, View::open_create_alone));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let v = view.read(cx);
        let name = v.create.name_input.clone().unwrap();
        let ws = v.create.workspace_input.clone().unwrap();
        name.update(cx, |s, cx| s.set_value("Bot", window, cx));
        ws.update(cx, |s, cx| {
            s.set_value("/definitely/not/a/krill/dir", window, cx);
        });
        view.update(cx, View::submit_create_bot);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let v = view.read(cx);
        assert!(
            v.error.is_some(),
            "a missing folder is refused, not silently replaced"
        );
        assert_eq!(v.bots().len(), 1);
        let ws = v.create.workspace_input.clone().unwrap();
        ws.update(cx, |s, cx| s.set_value("", window, cx));
        view.update(cx, |v, cx| {
            v.set_create_model(ModelChoice::Custom, cx);
            v.submit_create_bot(cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(
            v.error.is_some(),
            "Custom with no id is refused, not sent as Default"
        );
        assert_eq!(v.bots().len(), 1);
    });
    assert!(
        state
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|r| !matches!(r, BotRpcRequest::CreateBot(_))),
        "neither attempt reached the daemon"
    );

    // Opened alone (sidebar "+"), Escape closes the modal rather than showing a bot.
    let closed = Arc::new(Mutex::new(false));
    let closed_capture = closed.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, _: &Close, _| {
            *closed_capture.lock().unwrap() = true;
        })
        .detach();
        view.update(cx, View::handle_escape);
    });
    assert!(*closed.lock().unwrap());
}
