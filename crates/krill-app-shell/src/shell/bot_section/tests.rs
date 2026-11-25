//! Sidebar Bots section, Orca parity: the fold strip, chats with their workers, collapse all,
//! Bot options, search results and the row's line.

use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, VisualTestContext, px};
use krill_app_core::sidebar;
use krill_proto::SessionId;
use krill_proto::bots::{BotId, BotSessionSummary};
use krill_proto::control::{AgentState, AgentStatus};

use crate::shell::Shell;
use crate::shell::tests::{open, state_path};

struct Memory(sidebar::Snapshot);
impl sidebar::Source for Memory {
    fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
        Ok(self.0.clone())
    }
}

fn bot(
    id: &str,
    name: &str,
    desc: &str,
    sessions: Vec<BotSessionSummary>,
) -> krill_bots_ui::bots::SidebarBot {
    let mut cfg = krill_proto::bots::BotConfig::new(
        BotId::new(id).unwrap(),
        name,
        "claude",
        1_700_000_000_000,
    )
    .unwrap();
    cfg.avatar = Some("B1".to_owned());
    desc.clone_into(&mut cfg.description);
    let mut row = krill_bots_ui::bots::rpc::sidebar_snapshot(&[cfg], &[], &[], &sessions)
        .bots
        .remove(0);
    row.sessions = sessions;
    row
}

fn chat(bot: &str, sid: u64, label: &str) -> BotSessionSummary {
    BotSessionSummary {
        session_id: SessionId(sid),
        bot_id: BotId::new(bot).unwrap(),
        link_id: None,
        pid: 1,
        live: true,
        working: false,
        label: label.to_owned(),
        created_at_ms: 1_700_000_000_000,
    }
}

fn session(
    id: u64,
    state: AgentState,
    title: Option<&str>,
    worktree: Option<&str>,
) -> sidebar::Session {
    sidebar::Session {
        id: SessionId(id),
        status: AgentStatus {
            state,
            agent: "claude".into(),
            title: title.map(str::to_owned),
            ..AgentStatus::UNKNOWN
        },
        worktree: worktree.map(std::path::PathBuf::from),
        cwd: None,
        account: None,
    }
}

fn worker_store(
    coordinator: u64,
    worker: u64,
    cwd: &str,
) -> krill_proto::orchestration::OrchestrationStore {
    use krill_proto::orchestration::{
        Dispatch, DispatchStatus, OrchestrationStore, Run, RunStatus,
    };
    OrchestrationStore {
        runs: vec![Run {
            id: "r1".into(),
            title: "release".into(),
            coordinator_session_id: Some(coordinator),
            status: RunStatus::Active,
            created_at_ms: 0,
            updated_at_ms: 0,
        }],
        dispatches: vec![Dispatch {
            id: "d1".into(),
            run_id: "r1".into(),
            task_id: None,
            worker_name: "release".into(),
            agent: "claude".into(),
            session_id: Some(worker),
            depth: 1,
            cwd: cwd.into(),
            prompt: String::new(),
            status: DispatchStatus::Active,
            summary: None,
            archived_output: None,
            started_at_ms: 0,
            ended_at_ms: None,
            release_state: krill_proto::orchestration::DispatchReleaseState::default(),
            session_origin: krill_proto::orchestration::DispatchSessionOrigin::default(),
            model: None,
            effort: None,
            retry_of: None,
            request_id: None,
        }],
        ..OrchestrationStore::default()
    }
}

fn setup<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
) -> (gpui::Entity<Shell>, &'a mut VisualTestContext) {
    let (view, cx) = open(cx, state_path(name));
    cx.simulate_resize(gpui::size(px(1400.), px(1000.)));
    let wt = "/repo/krill/.worktrees/release";
    let snapshot = sidebar::Snapshot {
        sessions: vec![
            session(21, AgentState::Done, Some("Fixed the search"), None),
            session(22, AgentState::Waiting, None, None),
            session(31, AgentState::Working, None, Some(wt)),
            session(41, AgentState::Done, None, None),
        ],
        trees: std::collections::BTreeMap::from([(
            std::path::PathBuf::from("/repo/krill"),
            vec![std::path::PathBuf::from(wt)],
        )]),
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.orch_snapshot = Arc::new(worker_store(21, 31, wt));
            s.sidebar
                .update(cx, |b, cx| b.connect_source(Arc::new(Memory(snapshot)), cx));
            let krill = bot(
                "krill",
                "Krill bot",
                "Ships releases",
                vec![chat("krill", 21, ""), chat("krill", 22, "Claude Chat")],
            );
            let mut paused = bot(
                "zed",
                "Alpha shop",
                "Finds prices",
                vec![chat("zed", 41, "")],
            );
            paused.paused = true;
            s.apply_sidebar_bots(0, vec![krill, paused], cx);
        });
    });
    cx.run_until_parked();
    (view, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).expect(selector).center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn bot_chats_fold_their_workers_and_rows_say_what_needs_you(cx: &mut TestAppContext) {
    let (view, cx) = setup(cx, "bot-sidebar-orca-tree");
    // Home Chat, then the other chat, at Orca's 24 px, indented under the picture.
    let row = cx.debug_bounds("bot-row-krill").unwrap();
    let home = cx.debug_bounds("bot-chat-21").unwrap();
    let side = cx.debug_bounds("bot-chat-22").unwrap();
    assert_eq!(home.size.height, px(24.));
    assert!(home.top() < side.top() && home.left() > row.left());
    // A chat asking the owner speaks for the bot: Claude Chat waits on a question.
    cx.update(|_, cx| {
        let s = view.read(cx);
        let facts = s.bot_facts(&s.sidebar_bots[0], None, cx);
        assert_eq!(
            facts.line,
            Some(krill_bots_ui::bots::sidebar_view::RowLine::Replying(Some(
                "Claude Chat".to_owned()
            )))
        );
        assert_eq!(
            facts.chats[0].workers.len(),
            1,
            "Home Chat started the worker"
        );
    });
    // A chat with workers starts OPEN; clicking its chevron folds it, clicking again unfolds it.
    let worker = cx
        .debug_bounds("bot-worker-d1")
        .expect("worker line visible by default");
    assert!(worker.top() > home.top() && worker.left() > home.left());
    click(cx, "bot-chat-fold-21");
    assert!(cx.debug_bounds("bot-worker-d1").is_none());
    click(cx, "bot-chat-fold-21");
    let worker = cx
        .debug_bounds("bot-worker-d1")
        .expect("worker line re-shown");
    assert!(worker.top() > home.top() && worker.left() > home.left());
    // The bot's fold strip hides its chats; collapse all folds every bot.
    click(cx, "bot-fold-krill");
    assert!(cx.debug_bounds("bot-chat-21").is_none());
    click(cx, "bot-fold-krill");
    assert!(cx.debug_bounds("bot-chat-21").is_some());
    cx.update(|_, cx| view.update(cx, Shell::toggle_fold_all_bots));
    cx.run_until_parked();
    assert!(cx.debug_bounds("bot-chat-21").is_none());
    assert!(cx.debug_bounds("bot-chat-41").is_none());
}

#[gpui::test]
fn bot_options_sort_and_hide_and_search_lists_matches(cx: &mut TestAppContext) {
    let (view, cx) = setup(cx, "bot-sidebar-orca-options");
    let top = |cx: &mut VisualTestContext, id: &'static str| cx.debug_bounds(id).map(|b| b.top());
    // Manual keeps krilld's order; Name puts Alpha first.
    assert!(top(cx, "bot-row-krill") < top(cx, "bot-row-zed"));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut v = s.bot_view(cx);
            v.sort = krill_bots_ui::bots::sidebar_view::BotSidebarSort::Name;
            s.set_bot_view(v, cx);
        });
    });
    cx.run_until_parked();
    assert!(top(cx, "bot-row-zed") < top(cx, "bot-row-krill"));
    // Hide paused bots, with the options badge counting it.
    assert!(cx.debug_bounds("bots-options-badge").is_none());
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut v = s.bot_view(cx);
            v.hide_paused = true;
            s.set_bot_view(v, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("bot-row-zed").is_none());
    assert!(cx.debug_bounds("bots-options-badge").is_some());
    // Search: a hidden paused bot matching by name is counted, with "Show", which lists it.
    click(cx, "bots-section-search");
    cx.simulate_input("alpha");
    cx.run_until_parked();
    assert!(cx.debug_bounds("bot-search-result-zed").is_none());
    assert!(cx.debug_bounds("bots-search-paused").is_some());
    click(cx, "bots-search-show-paused");
    assert!(cx.debug_bounds("bot-search-result-zed").is_some());
    assert!(
        cx.debug_bounds("bot-row-krill").is_none(),
        "results replace the list"
    );
    click(cx, "bot-search-result-zed");
    assert!(cx.debug_bounds("bots-search-results").is_none());
}

fn group_rpc(
    recorded: Arc<std::sync::Mutex<Vec<String>>>,
    room_view: krill_proto::bots::BotGroupRoomView,
    items: Vec<krill_proto::bots::BotItem>,
) -> krill_bots_ui::bots::BotRpcCall {
    use krill_proto::bots::{BotGroupThread, BotGroupView, BotRpcRequest, BotRpcResponse};
    let base_view = room_view.clone();
    Arc::new(move |req| match req {
        BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![])),
        BotRpcRequest::ListGroupSummaries => Ok(BotRpcResponse::GroupSummaries(vec![])),
        BotRpcRequest::ReadGroupRoom { .. } => Ok(BotRpcResponse::GroupRoom(room_view.clone())),
        BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(items.clone())),
        BotRpcRequest::CreateGroup { name, bot_ids, .. } => {
            recorded
                .lock()
                .expect("record lock")
                .push(format!("create:{name}:{}", bot_ids.len()));
            let created = krill_proto::bots::BotGroupChat {
                id: "grp-new".to_owned(),
                name: name.clone(),
                bot_ids: bot_ids.clone(),
                description: String::new(),
                created_at_ms: 1_700_000_000_100,
                updated_at_ms: 1_700_000_000_100,
            };
            Ok(BotRpcResponse::Group(created))
        }
        BotRpcRequest::NewGroupThread { group_id, title } => {
            recorded
                .lock()
                .expect("record lock")
                .push(format!("thread:{group_id}:{title:?}"));
            let mut g = base_view.group.clone();
            g.id.clone_from(group_id);
            Ok(BotRpcResponse::GroupView(BotGroupView {
                group: g,
                threads: vec![BotGroupThread {
                    thread_id: "thr-new".to_owned(),
                    group_id: group_id.clone(),
                    title: title.clone().unwrap_or_else(|| "Follow-up".to_owned()),
                    created_at_ms: 1_700_000_000_200,
                    updated_at_ms: 1_700_000_000_200,
                }],
                messages: Vec::new(),
                active_run: false,
            }))
        }
        BotRpcRequest::SetGroupReplyAudience { group_id, audience } => {
            recorded
                .lock()
                .expect("record lock")
                .push(format!("audience:{group_id}:{audience:?}"));
            Ok(BotRpcResponse::GroupReplyAudienceUpdated {
                group_id: group_id.clone(),
                audience: audience.clone(),
            })
        }
        BotRpcRequest::SendGroupMessageWithAudience {
            group_id,
            text,
            audience,
            ..
        } => {
            recorded
                .lock()
                .expect("record lock")
                .push(format!("send:{group_id}:{text}:{audience:?}"));
            Ok(BotRpcResponse::GroupMessagePosted {
                message: krill_proto::bots::BotGroupMessage {
                    message_id: "m-sent".to_owned(),
                    group_id: group_id.clone(),
                    thread_id: "thr-main".to_owned(),
                    sender_id: "owner".to_owned(),
                    sender_name: "You".to_owned(),
                    body: text.clone(),
                    mentions: Vec::new(),
                    created_at_ms: 1_700_000_000_300,
                    in_progress: false,
                },
                dispatched_bots: Vec::new(),
            })
        }
        BotRpcRequest::StopGroupMember {
            group_id, bot_id, ..
        } => {
            recorded
                .lock()
                .expect("record lock")
                .push(format!("stop:{group_id}:{bot_id:?}"));
            Ok(BotRpcResponse::Ok)
        }
        BotRpcRequest::DeleteGroup { group_id } => {
            recorded
                .lock()
                .expect("record lock")
                .push(format!("delete:{group_id}"));
            Ok(BotRpcResponse::Ok)
        }
        _ => Ok(BotRpcResponse::Ok),
    })
}

#[gpui::test]
fn group_sidebar_row_create_dialog_search_and_open_page(cx: &mut TestAppContext) {
    use krill_proto::bots::{
        BotGroupChat, BotGroupMessage, BotGroupReplyAudience, BotGroupRoomView, BotGroupSummary,
        BotGroupThread,
    };

    let (view, cx) = setup(cx, "bot-group-create-and-search");
    let group = BotGroupChat {
        id: "grp-1".to_owned(),
        name: "Release Crew".to_owned(),
        bot_ids: vec![BotId::new("krill").unwrap(), BotId::new("zed").unwrap()],
        description: String::new(),
        created_at_ms: 1_700_000_000_010,
        updated_at_ms: 1_700_000_000_010,
    };
    let summary = BotGroupSummary {
        group: group.clone(),
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: vec![BotGroupThread {
            thread_id: "thr-main".to_owned(),
            group_id: "grp-1".to_owned(),
            title: "Main thread".to_owned(),
            created_at_ms: 1_700_000_000_010,
            updated_at_ms: 1_700_000_000_010,
        }],
        latest_message: Some(BotGroupMessage {
            message_id: "m0".to_owned(),
            group_id: "grp-1".to_owned(),
            thread_id: "thr-main".to_owned(),
            sender_id: "krill".to_owned(),
            sender_name: "Krill bot".to_owned(),
            body: "Standing by".to_owned(),
            mentions: Vec::new(),
            created_at_ms: 1_700_000_000_020,
            in_progress: false,
        }),
        active_turns: Vec::new(),
    };
    let room_view = BotGroupRoomView {
        group,
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: summary.threads.clone(),
        active_thread_id: Some("thr-main".to_owned()),
        messages: summary.latest_message.iter().cloned().collect(),
        has_earlier: false,
        active_turns: Vec::new(),
    };
    let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rpc = group_rpc(recorded.clone(), room_view, Vec::new());

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let bots = s.sidebar_bots.clone();
            s.bot_rpc_override = Some(rpc);
            s.apply_sidebar_state(0, bots, vec![summary], cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("bot-group-row-grp-1").is_some());

    // Group search hit opens the Bot Group Page & BotGroupItemsPanel.
    click(cx, "bots-section-search");
    cx.simulate_input("Release");
    cx.run_until_parked();
    assert!(cx.debug_bounds("bot-group-search-result-grp-1").is_some());
    click(cx, "bot-group-search-result-grp-1");
    assert!(cx.debug_bounds("bot-group-page").is_some());
    assert!(cx.debug_bounds("group-composer-card").is_some());
    assert!(cx.debug_bounds("group-reply-picker").is_some());
    assert!(cx.debug_bounds("bot-group-panel").is_some());
    assert!(cx.debug_bounds("bot-group-panel-header").is_some());
    assert!(cx.debug_bounds("bot-group-panel-members-btn").is_some());
    assert!(cx.debug_bounds("bot-group-panel-filter").is_some());

    // Open BotGroupMembersDialog (`New group`), pick 2 bots, accept suggested name, and submit.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| s.open_new_group_dialog(window, cx));
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("bot-group-members-dialog").is_some());
    click(cx, "group-member-pick-krill");
    click(cx, "group-member-pick-zed");
    assert!(cx.debug_bounds("group-name-suggestion").is_some());
    click(cx, "group-name-suggestion");
    click(cx, "group-dialog-submit");
    assert!(cx.debug_bounds("bot-group-members-dialog").is_none());

    let log = recorded.lock().expect("log lock");
    assert!(
        log.iter().any(|e| e == "create:Krill bot & Alpha shop:2"),
        "expected CreateGroup RPC in log, got {log:?}"
    );
}

#[gpui::test]
fn group_page_turns_peer_handoff_reply_picker_and_panel_filters(cx: &mut TestAppContext) {
    use krill_proto::bots::{
        BotGroupChat, BotGroupMessage, BotGroupReplyAudience, BotGroupRoomView, BotGroupSummary,
        BotGroupThread, BotGroupTurnInfo, BotItem, BotItemEvent, BotItemEventKind, BotItemLink,
        BotItemStatus,
    };

    let (view, cx) = setup(cx, "bot-group-page-features");
    let krill_id = BotId::new("krill").unwrap();
    let zed_id = BotId::new("zed").unwrap();
    let group = BotGroupChat {
        id: "grp-release".to_owned(),
        name: "Ops Team".to_owned(),
        bot_ids: vec![krill_id.clone(), zed_id.clone()],
        description: String::new(),
        created_at_ms: 1_700_000_000_010,
        updated_at_ms: 1_700_000_000_050,
    };
    let summary = BotGroupSummary {
        group: group.clone(),
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: vec![BotGroupThread {
            thread_id: "thr-main".to_owned(),
            group_id: "grp-release".to_owned(),
            title: "Main thread".to_owned(),
            created_at_ms: 1_700_000_000_010,
            updated_at_ms: 1_700_000_000_050,
        }],
        latest_message: None,
        active_turns: vec![BotGroupTurnInfo {
            group_id: "grp-release".to_owned(),
            thread_id: "thr-main".to_owned(),
            bot_id: zed_id.clone(),
            session_id: SessionId(41),
            message_id: "m3".to_owned(),
            started_at_ms: 1_700_000_000_045,
        }],
    };
    let room_view = BotGroupRoomView {
        group: group.clone(),
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: summary.threads.clone(),
        active_thread_id: Some("thr-main".to_owned()),
        messages: vec![
            BotGroupMessage {
                message_id: "m1".to_owned(),
                group_id: "grp-release".to_owned(),
                thread_id: "thr-main".to_owned(),
                sender_id: "owner".to_owned(),
                sender_name: "You".to_owned(),
                body: "Verify deploy @krill".to_owned(),
                mentions: vec![krill_id.clone()],
                created_at_ms: 1_700_000_000_020,
                in_progress: false,
            },
            BotGroupMessage {
                message_id: "m2".to_owned(),
                group_id: "grp-release".to_owned(),
                thread_id: "thr-main".to_owned(),
                sender_id: "krill".to_owned(),
                sender_name: "Krill bot".to_owned(),
                body: "Passed unit checks; @zed please deploy.".to_owned(),
                mentions: vec![zed_id.clone()],
                created_at_ms: 1_700_000_000_030,
                in_progress: false,
            },
            BotGroupMessage {
                message_id: "m3".to_owned(),
                group_id: "grp-release".to_owned(),
                thread_id: "thr-main".to_owned(),
                sender_id: "zed".to_owned(),
                sender_name: "Alpha shop".to_owned(),
                body: "Deploying package…".to_owned(),
                mentions: Vec::new(),
                created_at_ms: 1_700_000_000_045,
                in_progress: true,
            },
        ],
        has_earlier: true,
        active_turns: summary.active_turns.clone(),
    };
    let group_item = BotItem {
        uid: "item-group-1".to_owned(),
        number: 1,
        owner_bot_id: krill_id,
        title: "Release notes check".to_owned(),
        description: Some("Tracked from grp-release".to_owned()),
        done_means: None,
        status: BotItemStatus::NeedsYou,
        close_note: None,
        links: vec![BotItemLink::Url("krill://group/grp-release".to_owned())],
        events: vec![BotItemEvent {
            id: 1,
            item_uid: "item-group-1".to_owned(),
            actor: krill_proto::bots::BotItemActor::Owner { device: None },
            kind: BotItemEventKind::Made,
            text: "Created in grp-release".to_owned(),
            decisions: Vec::new(),
            created_at_ms: 1_700_000_000_025,
        }],
        snoozed_until_ms: None,
        version: 1,
        last_activity_at_ms: 1_700_000_000_025,
        created_at_ms: 1_700_000_000_025,
        updated_at_ms: 1_700_000_000_025,
        closed_at_ms: None,
        waiting_reason: None,
        waiting_since_ms: None,
    };
    let external_item = BotItem {
        uid: "item-other-2".to_owned(),
        number: 2,
        owner_bot_id: zed_id,
        title: "Price watch scan".to_owned(),
        description: Some("Separate schedule".to_owned()),
        done_means: None,
        status: BotItemStatus::Working,
        close_note: None,
        links: Vec::new(),
        events: Vec::new(),
        snoozed_until_ms: None,
        version: 1,
        last_activity_at_ms: 1_700_000_000_020,
        created_at_ms: 1_700_000_000_020,
        updated_at_ms: 1_700_000_000_020,
        closed_at_ms: None,
        waiting_reason: None,
        waiting_since_ms: None,
    };

    let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rpc = group_rpc(recorded.clone(), room_view, vec![group_item, external_item]);

    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let bots = s.sidebar_bots.clone();
            s.bot_rpc_override = Some(rpc);
            s.apply_sidebar_state(0, bots, vec![summary], cx);
            s.open_bot_group("grp-release", None, Some(window), cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("bot-group-page").is_some());
    assert!(cx.debug_bounds("group-load-earlier").is_some());
    assert!(cx.debug_bounds("group-peer-request").is_some());
    assert!(cx.debug_bounds("grp-turn-krill").is_some());
    assert!(cx.debug_bounds("grp-turn-zed").is_some());
    assert!(cx.debug_bounds("grp-turn-stop").is_some());
    assert!(cx.debug_bounds("group-composer-stop").is_some());

    // Clicking Reply on krill inserts `@krill `, and typing `@kr` keeps mention popup reachable.
    click(cx, "grp-turn-reply");
    assert!(cx.debug_bounds("group-composer-send").is_some());
    click(cx, "group-composer-send");

    // Scope starts at `ThisGroup` (`item-group-1` only) and expands on `AllTheirWork`.
    assert!(cx.debug_bounds("bot-group-items-list").is_some());
    cx.update(|_, cx| {
        let panel_view = view.update(cx, Shell::bot_panel_view).unwrap();
        assert_eq!(panel_view.read(cx).group_filter_counts().0[0].1, 1);
        panel_view.update(cx, |v, cx| {
            v.set_group_chat_scope(
                krill_bots_ui::bots::group_panel::GroupChatScope::AllTheirWork,
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let panel_view = view.update(cx, Shell::bot_panel_view).unwrap();
        let total: usize = panel_view
            .read(cx)
            .group_filter_counts()
            .0
            .iter()
            .map(|(_, c)| *c)
            .sum();
        assert_eq!(total, 2, "AllTheirWork shows both member bots' items");
    });

    // Delete confirmation modal sends `DeleteGroup`.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_ui.delete_group = Some((krill_app_core::hosts::HostId::Local, group));
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("bot-group-delete-dialog").is_some());
    click(cx, "bot-group-delete-confirm");
    assert!(cx.debug_bounds("bot-group-delete-dialog").is_none());

    let log = recorded.lock().expect("log lock");
    assert!(log.iter().any(|e| e.starts_with("send:grp-release:")));
    assert!(log.iter().any(|e| e == "delete:grp-release"));
}

#[test]
fn bot_fold_mark_priority_matches_orca() {
    use super::rows::bot_fold_mark;
    // Unfolded: only the bot's own work shows a spinner on the fold strip.
    assert_eq!(bot_fold_mark(false, false, None), None);
    assert_eq!(bot_fold_mark(false, false, Some(AgentState::Waiting)), None);
    assert_eq!(bot_fold_mark(false, false, Some(AgentState::Working)), None);
    assert_eq!(bot_fold_mark(false, true, None), Some(AgentState::Working));
    assert_eq!(
        bot_fold_mark(false, true, Some(AgentState::Waiting)),
        Some(AgentState::Working)
    );
    // Folded: waiting worker outranks bot working, which outranks folded working worker.
    assert_eq!(
        bot_fold_mark(true, true, Some(AgentState::Waiting)),
        Some(AgentState::Waiting)
    );
    assert_eq!(
        bot_fold_mark(true, true, Some(AgentState::Working)),
        Some(AgentState::Working)
    );
    assert_eq!(
        bot_fold_mark(true, false, Some(AgentState::Working)),
        Some(AgentState::Working)
    );
    assert_eq!(bot_fold_mark(true, false, None), None);
}

#[gpui::test]
fn working_bot_row_shows_one_spinner_at_row_end_and_aligns_with_group_row(cx: &mut TestAppContext) {
    use krill_proto::bots::{
        BotGroupChat, BotGroupReplyAudience, BotGroupSummary, BotGroupTurnInfo,
    };

    let (view, cx) = open(cx, state_path("bot-sidebar-one-spinner"));
    cx.simulate_resize(gpui::size(px(1400.), px(1000.)));
    let wt = "/repo/krill/.worktrees/release";
    let snapshot = sidebar::Snapshot {
        sessions: vec![
            session(21, AgentState::Working, None, None),
            session(31, AgentState::Working, None, Some(wt)),
            session(41, AgentState::Done, None, None),
            session(51, AgentState::Working, None, Some(wt)),
        ],
        trees: std::collections::BTreeMap::from([(
            std::path::PathBuf::from("/repo/krill"),
            vec![std::path::PathBuf::from(wt)],
        )]),
    };
    let group = BotGroupChat {
        id: "grp-work".to_owned(),
        name: "Release Crew".to_owned(),
        bot_ids: vec![BotId::new("scout").unwrap(), BotId::new("idle").unwrap()],
        description: String::new(),
        created_at_ms: 1_700_000_000_010,
        updated_at_ms: 1_700_000_000_010,
    };
    let group_summary = BotGroupSummary {
        group,
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: Vec::new(),
        latest_message: None,
        active_turns: vec![BotGroupTurnInfo {
            group_id: "grp-work".to_owned(),
            thread_id: "thr-main".to_owned(),
            bot_id: BotId::new("scout").unwrap(),
            session_id: SessionId(21),
            message_id: "m1".to_owned(),
            started_at_ms: 1_700_000_000_020,
        }],
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut store = worker_store(21, 31, wt);
            // Idle bot's folded worker (session 51) is working.
            store.runs.push(krill_proto::orchestration::Run {
                id: "r2".into(),
                title: "check".into(),
                coordinator_session_id: Some(41),
                status: krill_proto::orchestration::RunStatus::Active,
                created_at_ms: 0,
                updated_at_ms: 0,
            });
            store.dispatches.push(krill_proto::orchestration::Dispatch {
                id: "d2".into(),
                run_id: "r2".into(),
                task_id: None,
                worker_name: "check".into(),
                agent: "claude".into(),
                session_id: Some(51),
                depth: 1,
                cwd: wt.into(),
                prompt: String::new(),
                status: krill_proto::orchestration::DispatchStatus::Active,
                summary: None,
                archived_output: None,
                started_at_ms: 0,
                ended_at_ms: None,
                release_state: krill_proto::orchestration::DispatchReleaseState::default(),
                session_origin: krill_proto::orchestration::DispatchSessionOrigin::default(),
                model: None,
                effort: None,
                retry_of: None,
                request_id: None,
            });
            s.orch_snapshot = Arc::new(store);
            s.sidebar
                .update(cx, |b, cx| b.connect_source(Arc::new(Memory(snapshot)), cx));
            let mut scout_chat = chat("scout", 21, "");
            scout_chat.working = true;
            let scout = bot("scout", "Scout", "Checks nightly", vec![scout_chat]);
            let idle = bot(
                "idle",
                "Idle",
                "Has working worker",
                vec![chat("idle", 41, "")],
            );
            let bare = bot("bare", "Bare", "No chats yet", vec![]);
            s.apply_sidebar_state(0, vec![scout, idle, bare], vec![group_summary], cx);
        });
    });
    cx.run_until_parked();

    // Unfolded working bot: mark sits on its fold strip after the row's left/avatar side.
    let scout_row = cx.debug_bounds("bot-row-scout").expect("scout row");
    let scout_mark = cx
        .debug_bounds("bot-fold-mark-scout")
        .expect("scout fold mark");
    assert!(scout_mark.left() > scout_row.center().x);
    // Idle bot unfolded: no working mark on its fold strip; folded: worker's spinner shows once.
    assert!(cx.debug_bounds("bot-fold-mark-idle").is_none());
    click(cx, "bot-fold-idle");
    assert!(cx.debug_bounds("bot-fold-mark-idle").is_some());
    // Bare bot with no chats has no fold toggle at all.
    assert!(cx.debug_bounds("bot-fold-bare").is_none());
    // Folded working bot with working workers still shows exactly one fold mark (no duplicate).
    click(cx, "bot-fold-scout");
    let scout_folded_mark = cx
        .debug_bounds("bot-fold-mark-scout")
        .expect("folded scout mark");
    // Working group row (no fold control) shows trailing working mark aligned in the same column.
    let group_mark = cx
        .debug_bounds("bot-row-working-mark-group-grp-work")
        .expect("group working mark");
    let delta = (f32::from(scout_folded_mark.left()) - f32::from(group_mark.left())).abs();
    assert!(
        delta <= 2.0,
        "trailing working mark should align with fold row mark: fold={} group={}",
        scout_folded_mark.left(),
        group_mark.left()
    );
}

#[gpui::test]
fn collapsed_bot_preview_stays_next_to_bots_label(cx: &mut TestAppContext) {
    let (view, cx) = setup(cx, "bot-sidebar-collapsed-left");
    // Leave only one bot so centering vs left-alignment is unmistakable.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let one = s.sidebar_bots[0].clone();
            s.apply_sidebar_bots(0, vec![one], cx);
            s.bot_ui.collapsed = true;
            cx.notify();
        });
    });
    cx.run_until_parked();

    let title = cx.debug_bounds("bots-section-title").expect("Bots label");
    let preview = cx
        .debug_bounds("bots-collapsed-preview")
        .expect("collapsed preview container");
    let face = cx
        .debug_bounds("bots-collapsed-krill")
        .expect("collapsed bot face");
    assert!(
        preview.size.width > px(80.),
        "preview fills remaining header width"
    );
    assert!(
        face.left() >= title.right(),
        "collapsed bot sits to the right of the Bots label"
    );
    assert!(
        face.left() - title.right() <= px(16.),
        "collapsed bot stays flush next to the Bots label, not centered: title_right={} face_left={} preview_width={}",
        title.right(),
        face.left(),
        preview.size.width
    );
}

#[gpui::test]
fn bot_sidebar_times_match_projects_size_and_folded_avatars_show_blue_unread_dot(
    cx: &mut TestAppContext,
) {
    use krill_proto::bots::{
        BotGroupChat, BotGroupMessage, BotGroupReplyAudience, BotGroupSummary,
    };

    let (view, cx) = setup(cx, "bot-sidebar-time-and-unread-dot");
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    let activity_ms = now_ms.saturating_sub(60_000);
    let group_summary = BotGroupSummary {
        group: BotGroupChat {
            id: "grp-time".into(),
            name: "Ops Team".into(),
            bot_ids: vec![BotId::new("krill").unwrap()],
            description: String::new(),
            created_at_ms: now_ms.saturating_sub(120_000),
            updated_at_ms: activity_ms,
        },
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: Vec::new(),
        latest_message: Some(BotGroupMessage {
            message_id: "m-time-1".into(),
            group_id: "grp-time".into(),
            thread_id: "thr-main".into(),
            sender_id: "owner".into(),
            sender_name: "Owner".into(),
            body: "Checking status".into(),
            mentions: Vec::new(),
            created_at_ms: activity_ms,
            in_progress: false,
        }),
        active_turns: Vec::new(),
    };
    let mut sess = session(
        10,
        AgentState::Done,
        Some("Checked nightly"),
        Some("/repo/krill"),
    );
    sess.status.since_unix_ns = activity_ms.saturating_mul(1_000_000);
    let snapshot = sidebar::Snapshot {
        sessions: vec![sess],
        trees: std::collections::BTreeMap::new(),
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.sidebar.update(cx, |sb, cx| {
                sb.connect_source(Arc::new(Memory(snapshot)), cx);
            });
            let mut c = chat("krill", 10, "");
            c.created_at_ms = activity_ms;
            let mut b = bot("krill", "Krill", "Watches builds", vec![c]);
            b.updated_at_ms = activity_ms;
            s.apply_sidebar_state(0, vec![b], vec![group_summary], cx);
            // Mark session 10 unread via Sidebar unread state.
            s.sidebar.update(cx, |sb, cx| {
                sb.mark_session_unread(&krill_app_core::hosts::HostId::Local, SessionId(10), cx);
            });
            cx.notify();
        });
    });
    cx.run_until_parked();

    // Commit 8 (096a87bc): bot row, group row, and chat row times render at TEXT_2XS (<= 14px tall).
    let bot_age = cx.debug_bounds("bot-row-age-krill").expect("bot row age");
    let group_age = cx
        .debug_bounds("bot-group-row-age-grp-time")
        .expect("group row age");
    let chat_age = cx.debug_bounds("bot-chat-age-10").expect("chat row age");
    assert!(
        bot_age.size.height <= px(14.),
        "bot row age should use TEXT_2XS height, got {:?}",
        bot_age.size.height
    );
    assert!(
        group_age.size.height <= px(14.),
        "group row age should use TEXT_2XS height, got {:?}",
        group_age.size.height
    );
    assert!(
        chat_age.size.height <= px(14.),
        "bot chat row age should use TEXT_2XS height, got {:?}",
        chat_age.size.height
    );

    // Commit 9 (490ec49b): collapsing Bots shows the blue unread alert dot on the unread avatar.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_ui.collapsed = true;
            cx.notify();
        });
    });
    cx.run_until_parked();
    let dot = cx
        .debug_bounds("bots-collapsed-unread-krill")
        .expect("folded bot avatar must display unread dot");
    assert_eq!(dot.size.width, px(10.), "6px dot + 2px ring on each side");
    assert_eq!(dot.size.height, px(10.), "6px dot + 2px ring on each side");
}

#[gpui::test]
fn remote_host_bot_chat_workers_render_open_by_default_and_fold_cleanly(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("bot-sidebar-remote-workers"));
    cx.simulate_resize(gpui::size(px(1400.), px(1000.)));
    let remote_host = krill_app_core::hosts::HostId::Remote("studio".into());
    let wt = "/remote/krill/.worktrees/fix-ui";
    let remote_snapshot = sidebar::Snapshot {
        sessions: vec![
            session(21, AgentState::Done, Some("Checked remote PR"), None),
            session(31, AgentState::Working, None, Some(wt)),
        ],
        trees: std::collections::BTreeMap::from([(
            std::path::PathBuf::from("/remote/krill"),
            vec![std::path::PathBuf::from(wt)],
        )]),
    };
    let store = Arc::new(worker_store(21, 31, wt));
    let remote_bot = bot(
        "remote-krill",
        "Remote Krill",
        "Runs on studio",
        vec![chat("remote-krill", 21, "")],
    );
    let remote_bot_meta = remote_bot.session_meta();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.launch_host = remote_host.clone();
            s.remote_orch_snapshots
                .insert(remote_host.clone(), store.clone());
            s.sidebar.update(cx, |sb, cx| {
                sb.set_remote_snapshot_for_test(
                    "studio",
                    krill_app_core::hosts::HostStatus::Connected,
                    remote_snapshot,
                    cx,
                );
                sb.set_bot_sessions(
                    remote_host.clone(),
                    std::collections::BTreeMap::from([(SessionId(21), remote_bot_meta)]),
                    cx,
                );
                let rows_before = sb.row_count();
                sb.set_remote_orchestration(remote_host.clone(), store, cx);
                assert!(
                    sb.row_count() < rows_before,
                    "remote bot worker worktree should be hidden from Projects ({} -> {})",
                    rows_before,
                    sb.row_count()
                );
            });
            s.apply_sidebar_bots(0, vec![remote_bot], cx);
        });
    });
    cx.run_until_parked();

    let home = cx.debug_bounds("bot-chat-21").expect("remote bot chat");
    let worker = cx
        .debug_bounds("bot-worker-d1")
        .expect("remote worker visible immediately by default");
    assert!(worker.top() > home.top() && worker.left() > home.left());

    cx.update(|_, cx| {
        let s = view.read(cx);
        let facts = s.bot_facts(&s.sidebar_bots[0], None, cx);
        assert_eq!(facts.chats[0].workers.len(), 1);
        assert_eq!(facts.chats[0].workers[0].title, "fix-ui");
        assert_eq!(facts.chats[0].workers[0].repo.as_deref(), Some("krill"));
        assert!(facts.chats[0].workers[0].live);
    });

    click(cx, "bot-chat-fold-21");
    assert!(cx.debug_bounds("bot-worker-d1").is_none());
    click(cx, "bot-chat-fold-21");
    assert!(cx.debug_bounds("bot-worker-d1").is_some());
}

#[gpui::test]
fn bot_workers_dedup_by_session_keep_resumed_live_and_avoid_repo_root_duplicate_title(
    cx: &mut TestAppContext,
) {
    use krill_proto::orchestration::{
        Dispatch, DispatchStatus, OrchestrationStore, Run, RunStatus,
    };

    let (view, cx) = open(cx, state_path("bot-sidebar-worker-facts"));
    cx.simulate_resize(gpui::size(px(1400.), px(1000.)));
    let repo_root = "/repo/krill";
    let child_wt = "/repo/krill/.worktrees/feature-a";
    let snapshot = sidebar::Snapshot {
        sessions: vec![
            session(21, AgentState::Done, None, None),
            // Resumed worker after previous dispatch succeeded: still actively Working in repo root
            // with a bare agent title ("claude"), so title must fall back to worker_name, not "krill".
            session(31, AgentState::Working, Some("claude"), Some(repo_root)),
            // Worker in repo root with a meaningful session title.
            session(
                32,
                AgentState::Done,
                Some("Refactor auth flow"),
                Some(repo_root),
            ),
            // Worker in distinct child worktree: keeps worktree directory basename.
            session(
                33,
                AgentState::Working,
                Some("Ignored for child wt"),
                Some(child_wt),
            ),
        ],
        trees: std::collections::BTreeMap::from([(
            std::path::PathBuf::from(repo_root),
            vec![
                std::path::PathBuf::from(repo_root),
                std::path::PathBuf::from(child_wt),
            ],
        )]),
    };
    let make_dispatch =
        |id: &str, worker_name: &str, sid: u64, cwd: &str, status, at_ms| Dispatch {
            id: id.into(),
            run_id: "r1".into(),
            task_id: None,
            worker_name: worker_name.into(),
            agent: "claude".into(),
            session_id: Some(sid),
            depth: 1,
            cwd: cwd.into(),
            prompt: String::new(),
            status,
            summary: None,
            archived_output: None,
            started_at_ms: at_ms,
            ended_at_ms: (status != DispatchStatus::Active).then_some(at_ms),
            release_state: krill_proto::orchestration::DispatchReleaseState::default(),
            session_origin: krill_proto::orchestration::DispatchSessionOrigin::default(),
            model: None,
            effort: None,
            retry_of: None,
            request_id: None,
        };
    let store = OrchestrationStore {
        runs: vec![Run {
            id: "r1".into(),
            title: "run".into(),
            coordinator_session_id: Some(21),
            status: RunStatus::Active,
            created_at_ms: 0,
            updated_at_ms: 0,
        }],
        dispatches: vec![
            // Two dispatches sharing session 31 (older succeeded + newer succeeded while session is Working).
            make_dispatch(
                "d-old",
                "old-name",
                31,
                repo_root,
                DispatchStatus::Succeeded,
                1000,
            ),
            make_dispatch(
                "d-new",
                "fix-login",
                31,
                repo_root,
                DispatchStatus::Succeeded,
                2000,
            ),
            make_dispatch(
                "d-titled",
                "auth-worker",
                32,
                repo_root,
                DispatchStatus::Succeeded,
                1500,
            ),
            make_dispatch(
                "d-child",
                "child-worker",
                33,
                child_wt,
                DispatchStatus::Active,
                1800,
            ),
        ],
        ..OrchestrationStore::default()
    };

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.orch_snapshot = Arc::new(store);
            s.sidebar
                .update(cx, |b, cx| b.connect_source(Arc::new(Memory(snapshot)), cx));
            let krill = bot("krill", "Krill bot", "Ships", vec![chat("krill", 21, "")]);
            s.apply_sidebar_bots(0, vec![krill], cx);
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        let s = view.read(cx);
        let facts = s.bot_facts(&s.sidebar_bots[0], None, cx);
        let workers = &facts.chats[0].workers;
        assert_eq!(workers.len(), 3, "duplicate session 31 must be deduplicated");

        let w31 = workers
            .iter()
            .find(|w| w.session == Some(SessionId(31)))
            .expect("session 31");
        assert_eq!(w31.key, "d-new", "newest dispatch for session 31 wins");
        assert!(
            w31.live,
            "resumed Working session must be live even after DispatchStatus::Succeeded"
        );
        assert_eq!(
            w31.title, "fix-login",
            "repo-root worker with bare agent title must fall back to worker_name, not repo basename"
        );
        assert_eq!(w31.repo.as_deref(), Some("krill"));

        let w32 = workers
            .iter()
            .find(|w| w.session == Some(SessionId(32)))
            .expect("session 32");
        assert!(!w32.live);
        assert_eq!(
            w32.title, "Refactor auth flow",
            "repo-root worker prefers meaningful session title"
        );
        assert_eq!(w32.repo.as_deref(), Some("krill"));

        let w33 = workers
            .iter()
            .find(|w| w.session == Some(SessionId(33)))
            .expect("session 33");
        assert!(w33.live);
        assert_eq!(
            w33.title, "feature-a",
            "distinct child worktree keeps worktree directory name"
        );
        assert_eq!(w33.repo.as_deref(), Some("krill"));
    });
}

#[gpui::test]
fn bot_panel_chats_tab_shows_orch_and_linked_dispatch_workers_open_by_default(
    cx: &mut TestAppContext,
) {
    use krill_proto::bots::{
        BotConfig, BotItem, BotItemActor, BotItemEvent, BotItemEventKind, BotItemStatus,
        BotRpcRequest, BotRpcResponse,
    };
    use krill_proto::orchestration::{Dispatch, DispatchStatus, Run, RunStatus};

    let (view, cx) = open(cx, state_path("bot-panel-chats-workers"));
    cx.simulate_resize(gpui::size(px(1400.), px(1000.)));
    let wt1 = "/repo/krill/.worktrees/release";
    let wt2 = "/repo/krill/.worktrees/linked-fix";
    let snapshot = sidebar::Snapshot {
        sessions: vec![
            session(21, AgentState::Done, Some("Home"), None),
            session(31, AgentState::Working, Some("Release build"), Some(wt1)),
            session(32, AgentState::Done, Some("Linked dispatch fix"), Some(wt2)),
        ],
        trees: std::collections::BTreeMap::from([(
            std::path::PathBuf::from("/repo/krill"),
            vec![std::path::PathBuf::from(wt1), std::path::PathBuf::from(wt2)],
        )]),
    };
    let krill_id = BotId::new("krill").unwrap();
    let cfg = BotConfig::new(krill_id.clone(), "Krill bot", "claude", 1_700_000_000_000).unwrap();
    let item = BotItem {
        uid: "item-1".to_owned(),
        number: 1,
        owner_bot_id: krill_id.clone(),
        title: "Track linked dispatch".to_owned(),
        description: None,
        done_means: None,
        status: BotItemStatus::Working,
        close_note: None,
        links: Vec::new(),
        events: vec![BotItemEvent {
            id: 1,
            item_uid: "item-1".to_owned(),
            actor: BotItemActor::Bot {
                bot_id: krill_id,
                session_id: Some(SessionId(21)),
            },
            kind: BotItemEventKind::Linked,
            text: "dispatch:disp-7182".to_owned(),
            decisions: Vec::new(),
            created_at_ms: 1_700_000_050_000,
        }],
        snoozed_until_ms: None,
        version: 1,
        last_activity_at_ms: 1_700_000_050_000,
        created_at_ms: 1_700_000_050_000,
        updated_at_ms: 1_700_000_050_000,
        closed_at_ms: None,
        waiting_reason: None,
        waiting_since_ms: None,
    };
    let chats = vec![chat("krill", 21, "")];
    let listed = chats.clone();
    let rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| {
        Ok(match req {
            BotRpcRequest::ListBots => BotRpcResponse::Bots(vec![cfg.clone()]),
            BotRpcRequest::ListItems { .. } => BotRpcResponse::Items(vec![item.clone()]),
            BotRpcRequest::ListBotSessions { .. } => BotRpcResponse::BotSessions(listed.clone()),
            BotRpcRequest::ConnectorStatus { .. } => BotRpcResponse::ConnectorStatuses(Vec::new()),
            BotRpcRequest::ListAccessRequests { .. } => BotRpcResponse::AccessRequests(Vec::new()),
            BotRpcRequest::ToolRelayStatus => {
                BotRpcResponse::ToolRelayStatus(krill_proto::bots::BotToolRelayStatus::default())
            }
            _ => BotRpcResponse::Ok,
        })
    });
    let mut store = worker_store(21, 31, wt1);
    store.runs.push(Run {
        id: "r-linked".into(),
        title: "linked".into(),
        coordinator_session_id: None,
        status: RunStatus::Active,
        created_at_ms: 0,
        updated_at_ms: 0,
    });
    store.dispatches.push(Dispatch {
        id: "disp-7182".into(),
        run_id: "r-linked".into(),
        task_id: None,
        worker_name: "linked-fix".into(),
        agent: "claude".into(),
        session_id: Some(32),
        depth: 1,
        cwd: wt2.into(),
        prompt: String::new(),
        status: DispatchStatus::Succeeded,
        summary: None,
        archived_output: None,
        started_at_ms: 1_700_000_040_000,
        ended_at_ms: Some(1_700_000_045_000),
        release_state: krill_proto::orchestration::DispatchReleaseState::default(),
        session_origin: krill_proto::orchestration::DispatchSessionOrigin::default(),
        model: None,
        effort: None,
        retry_of: None,
        request_id: None,
    });

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = Some(rpc);
            s.orch_snapshot = Arc::new(store);
            s.sidebar
                .update(cx, |b, cx| b.connect_source(Arc::new(Memory(snapshot)), cx));
            let krill = bot("krill", "Krill bot", "Ships releases", chats);
            s.apply_sidebar_bots(0, vec![krill], cx);
        });
    });
    cx.run_until_parked();

    // Open the bot's Home Chat so the right-hand BotPanel appears, then switch to Chats tab.
    click(cx, "bot-row-krill");
    assert!(cx.debug_bounds("bot-panel").is_some());
    click(cx, "bot-panel-tab-chats");
    assert!(cx.debug_bounds("bot-panel-chat-21").is_some());
    assert!(
        cx.debug_bounds("bot-chat-worker-31").is_some(),
        "orchestration worker must appear open by default in Chats tab"
    );
    assert!(
        cx.debug_bounds("bot-chat-worker-32").is_some(),
        "dispatch:disp-7182 item-linked worker must resolve via orch_snapshot and appear open by default"
    );

    // Clicking the panel's fold button collapses/expands the worker list.
    click(cx, "bot-panel-chat-fold-21");
    assert!(cx.debug_bounds("bot-chat-worker-31").is_none());
    assert!(cx.debug_bounds("bot-chat-worker-32").is_none());
    click(cx, "bot-panel-chat-fold-21");
    assert!(cx.debug_bounds("bot-chat-worker-31").is_some());
    assert!(cx.debug_bounds("bot-chat-worker-32").is_some());

    // Clicking a worker opens it as a regular session (NOT a bot chat session).
    click(cx, "bot-chat-worker-31");
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(
            s.focused_session(),
            Some((krill_app_core::hosts::HostId::Local, SessionId(31)))
        );
        assert!(
            s.sidebar
                .read(cx)
                .bot_session_meta_on(&krill_app_core::hosts::HostId::Local, SessionId(31))
                .is_none(),
            "opening a worker must not register it as a bot chat session"
        );
    });
}

#[test]
fn sidebar_snapshot_drops_stale_pre_creation_chats_from_reused_bot_id() {
    let mut cfg_new = krill_proto::bots::BotConfig::new(
        BotId::new("bot-bot-5").unwrap(),
        "크릴 에러 수정봇",
        "agy",
        1_795_438_259_549,
    )
    .unwrap();
    cfg_new.slug = krill_proto::bots::BotSlug::new("bot-5").unwrap();
    cfg_new.folder = "bot-5".to_owned();

    let mut cfg_other = krill_proto::bots::BotConfig::new(
        BotId::new("bot-bot-4").unwrap(),
        "리서치봇",
        "agy",
        1_795_324_030_205,
    )
    .unwrap();
    cfg_other.slug = krill_proto::bots::BotSlug::new("bot-4").unwrap();
    cfg_other.folder = "bot-4".to_owned();

    let sessions = vec![
        BotSessionSummary {
            session_id: SessionId(41764),
            bot_id: cfg_new.id.clone(),
            link_id: None,
            pid: 36865,
            live: true,
            working: false,
            label: "리서치봇".to_owned(),
            created_at_ms: 1_795_340_556_317, // 27h before cfg_new was created
        },
        BotSessionSummary {
            session_id: SessionId(41801),
            bot_id: cfg_new.id.clone(),
            link_id: None,
            pid: 78853,
            live: true,
            working: false,
            label: "크릴 에러 수정봇".to_owned(),
            created_at_ms: 1_795_438_259_692,
        },
        BotSessionSummary {
            session_id: SessionId(41774),
            bot_id: cfg_other.id.clone(),
            link_id: None,
            pid: 6994,
            live: true,
            working: false,
            label: "리서치봇".to_owned(),
            created_at_ms: 1_795_388_014_183,
        },
    ];

    let snap = krill_bots_ui::bots::rpc::sidebar_snapshot(
        &[cfg_other.clone(), cfg_new.clone()],
        &[],
        &[],
        &sessions,
    );
    let err_bot = snap.bots.iter().find(|b| b.id == cfg_new.id).unwrap();
    assert_eq!(err_bot.sessions.len(), 1);
    assert_eq!(err_bot.sessions[0].session_id, SessionId(41801));

    let other_bot = snap.bots.iter().find(|b| b.id == cfg_other.id).unwrap();
    assert_eq!(other_bot.sessions.len(), 1);
    assert_eq!(other_bot.sessions[0].session_id, SessionId(41774));

    // Also verify `fetch_sidebar_state` excludes the stale session from `sessions` metadata map.
    let bots_for_rpc = vec![cfg_other, cfg_new];
    let sessions_for_rpc = sessions;
    let rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| match req {
        krill_proto::bots::BotRpcRequest::ListBots => Ok(krill_proto::bots::BotRpcResponse::Bots(
            bots_for_rpc.clone(),
        )),
        krill_proto::bots::BotRpcRequest::ListBotSessions { .. } => Ok(
            krill_proto::bots::BotRpcResponse::BotSessions(sessions_for_rpc.clone()),
        ),
        krill_proto::bots::BotRpcRequest::ListItems { .. } => {
            Ok(krill_proto::bots::BotRpcResponse::Items(Vec::new()))
        }
        krill_proto::bots::BotRpcRequest::ListAccessRequests { .. } => {
            Ok(krill_proto::bots::BotRpcResponse::AccessRequests(Vec::new()))
        }
        krill_proto::bots::BotRpcRequest::ListGroupSummaries => {
            Ok(krill_proto::bots::BotRpcResponse::GroupSummaries(Vec::new()))
        }
        _ => Ok(krill_proto::bots::BotRpcResponse::Ok),
    });
    let state = krill_bots_ui::bots::fetch_sidebar_state(&rpc, &std::collections::HashMap::new())
        .expect("sidebar state");
    assert!(!state.sessions.contains_key(&SessionId(41764)));
    assert!(state.sessions.contains_key(&SessionId(41801)));
    assert!(state.sessions.contains_key(&SessionId(41774)));
}

#[gpui::test]
fn opening_new_bot_from_existing_bot_tab_switches_right_panel_and_folder(cx: &mut TestAppContext) {
    use krill_proto::bots::{BotConfig, BotRpcRequest, BotRpcResponse, BotSlug};
    use krill_proto::os::ffi::OsStrExt as _;

    let (view, cx) = open(cx, state_path("bot-panel-new-bot-switch"));
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));

    let mut cfg_krill = BotConfig::new(
        BotId::new("bot-krill").unwrap(),
        "크릴",
        "agy",
        1_795_312_744_127,
    )
    .unwrap();
    cfg_krill.slug = BotSlug::new("bot-3").unwrap();
    cfg_krill.folder = "bot-3".to_owned();

    let mut cfg_fixer = BotConfig::new(
        BotId::new("bot-fixer").unwrap(),
        "크릴 에러 수정봇",
        "agy",
        1_795_438_259_549,
    )
    .unwrap();
    cfg_fixer.slug = BotSlug::new("bot-5").unwrap();
    cfg_fixer.folder = "bot-5".to_owned();

    let bots_rpc = vec![cfg_krill.clone(), cfg_fixer.clone()];
    let rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| match req {
        BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(bots_rpc.clone())),
        BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(Vec::new())),
        BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(Vec::new())),
        BotRpcRequest::ConnectorStatus { .. } => Ok(BotRpcResponse::ConnectorStatuses(Vec::new())),
        BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(Vec::new())),
        BotRpcRequest::ListSuggestions { .. } => Ok(BotRpcResponse::Suggestions(Vec::new())),
        BotRpcRequest::GetMemoryHistory { .. } => Ok(BotRpcResponse::MemoryHistory(Vec::new())),
        BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(Vec::new())),
        BotRpcRequest::GetSoul { .. } | BotRpcRequest::GetMemory { .. } => {
            Ok(BotRpcResponse::Text(String::new()))
        }
        _ => Ok(BotRpcResponse::Ok),
    });

    let sess_krill = BotSessionSummary {
        session_id: SessionId(41788),
        bot_id: cfg_krill.id.clone(),
        link_id: None,
        pid: 100,
        live: true,
        working: false,
        label: "크릴".to_owned(),
        created_at_ms: 1_795_409_209_582,
    };
    let sess_fixer = BotSessionSummary {
        session_id: SessionId(41801),
        bot_id: cfg_fixer.id.clone(),
        link_id: None,
        pid: 200,
        live: true,
        working: false,
        label: "크릴 에러 수정봇".to_owned(),
        created_at_ms: 1_795_438_259_692,
    };

    let sb_bots = krill_bots_ui::bots::rpc::sidebar_snapshot(
        &[cfg_krill.clone(), cfg_fixer.clone()],
        &[],
        &[],
        &[sess_krill.clone(), sess_fixer.clone()],
    )
    .bots;
    assert_eq!(
        sb_bots[0].id, cfg_krill.id,
        "크릴 precedes 크릴 에러 수정봇"
    );
    assert_eq!(sb_bots[1].id, cfg_fixer.id);

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = Some(rpc);
            s.panel_collapsed = false;
            s.apply_sidebar_bots(0, sb_bots, cx);
            // Open 크릴's tab first with its folder `bot-3` as `launch.cwd`.
            s.open_bot_session(&cfg_krill.id, SessionId(41788), cx);
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.focused_bot().map(|b| &b.id), Some(&cfg_krill.id));
        let panel = s.bot_ui.panel.view.clone().expect("panel view");
        assert_eq!(
            panel.read(cx).selected_bot().map(|b| &b.id),
            Some(&cfg_krill.id)
        );
    });

    // Now open the newly created bot's session (not yet known to sidebar `folder_on`),
    // while `크릴`'s tab (`cwd = .../bots/bot-3`) is active.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.open_bot_session(&cfg_fixer.id, SessionId(41801), cx);
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        let s = view.read(cx);
        let pane = s.layout.focused().expect("focused pane");
        assert_eq!(s.layout.session(pane), Some(SessionId(41801)));
        assert_eq!(
            s.focused_bot().map(|b| &b.id),
            Some(&cfg_fixer.id),
            "right panel target must switch to the new bot, not stay on 크릴"
        );
        let expected_folder = s.bot_folder(&s.sidebar_bots[1]).expect("bot-5 folder");
        let actual_cwd = {
            use krill_proto::os::ffi::OsStringExt as _;
            std::path::PathBuf::from(std::ffi::OsString::from_vec(s.launch_for(pane).cwd))
        };
        assert_eq!(
            actual_cwd, expected_folder,
            "new bot tab must not keep the previous bot's inherited cwd"
        );
        let panel = s.bot_ui.panel.view.clone().expect("panel view");
        assert_eq!(
            panel.read(cx).selected_bot().map(|b| &b.id),
            Some(&cfg_fixer.id),
            "right panel view must select the new bot"
        );
    });

    // Even if a pane's `launch.cwd` explicitly points to another bot's folder (`bot-3`) while
    // running `bot-fixer`'s session (`41801`), session ownership still wins over `cwd`.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let pane = s.layout.focused().expect("focused pane");
            let krill_folder = s.bot_folder(&s.sidebar_bots[0]).expect("bot-3 folder");
            let mut launch = s.launch_for(pane);
            launch.cwd = krill_folder.as_os_str().as_bytes().to_vec();
            s.layout.set_launch(pane, launch);
            assert_eq!(s.bot_for_pane(pane).map(|b| &b.id), Some(&cfg_fixer.id));
            cx.notify();
        });
    });
}

#[gpui::test]
fn remote_bots_load_and_open_while_launch_host_is_local_with_zero_local_bots(
    cx: &mut TestAppContext,
) {
    use krill_app_core::hosts::{HostId, HostStatus};
    use krill_proto::bots::{BotConfig, BotRpcRequest, BotRpcResponse};

    let (view, cx) = open(cx, state_path("remote-bots-load-with-empty-local"));
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));

    let remote_host = HostId::Remote("iroh:macmini".to_owned());
    let mut remote_cfg = BotConfig::new(
        BotId::new("bot-shop").unwrap(),
        "쇼핑봇",
        "claude",
        1_700_000_000_000,
    )
    .unwrap();
    remote_cfg.avatar = Some("B1".to_owned());
    let remote_session = BotSessionSummary {
        session_id: SessionId(901),
        bot_id: remote_cfg.id.clone(),
        link_id: None,
        pid: 42,
        live: true,
        working: false,
        label: "쇼핑봇".to_owned(),
        created_at_ms: 1_700_000_001_000,
    };

    let local_rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(|req| match req {
        BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(Vec::new())),
        BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(Vec::new())),
        BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(Vec::new())),
        BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(Vec::new())),
        BotRpcRequest::ListGroupSummaries => Ok(BotRpcResponse::GroupSummaries(Vec::new())),
        _ => Ok(BotRpcResponse::Ok),
    });

    let remote_bots_rpc = vec![remote_cfg.clone()];
    let remote_sessions_rpc = vec![remote_session];
    let remote_rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| match req {
        BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(remote_bots_rpc.clone())),
        BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(Vec::new())),
        BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(Vec::new())),
        BotRpcRequest::ListBotSessions { .. } => {
            Ok(BotRpcResponse::BotSessions(remote_sessions_rpc.clone()))
        }
        BotRpcRequest::ListGroupSummaries => Ok(BotRpcResponse::GroupSummaries(Vec::new())),
        BotRpcRequest::ConnectorStatus { .. } => Ok(BotRpcResponse::ConnectorStatuses(Vec::new())),
        BotRpcRequest::ListSuggestions { .. } => Ok(BotRpcResponse::Suggestions(Vec::new())),
        BotRpcRequest::GetMemoryHistory { .. } => Ok(BotRpcResponse::MemoryHistory(Vec::new())),
        BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(Vec::new())),
        BotRpcRequest::GetSoul { .. } | BotRpcRequest::GetMemory { .. } => {
            Ok(BotRpcResponse::Text(String::new()))
        }
        _ => Ok(BotRpcResponse::Ok),
    });

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.link = crate::shell::Link::Up;
            s.launch_host = HostId::Local;
            s.panel_collapsed = false;
            s.bot_rpc_overrides.insert(HostId::Local, local_rpc);
            s.bot_rpc_overrides.insert(remote_host.clone(), remote_rpc);
            s.sidebar.update(cx, |sb, cx| {
                sb.set_remote_snapshot_for_test(
                    "iroh:macmini",
                    HostStatus::Connected,
                    sidebar::Snapshot::default(),
                    cx,
                );
            });
            s.refresh_bots_badge(cx);
        });
    });
    cx.run_until_parked();

    // Must render the remote bot instead of the empty onboarding card while launch_host == Local.
    assert!(
        cx.debug_bounds("bots-empty-card").is_none(),
        "must not show empty onboarding card when a connected remote host has bots"
    );
    assert!(
        cx.debug_bounds("bot-row-bot-shop").is_some(),
        "remote bot row must appear in sidebar even while launch_host is Local"
    );
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.launch_host, HostId::Local);
        assert_eq!(s.sidebar_bots.len(), 1);
        assert_eq!(s.sidebar_bots[0].host, remote_host);
        assert_eq!(s.sidebar_bots[0].name, "쇼핑봇");
    });

    // Clicking the remote bot row opens its session on the remote host and shows its right panel.
    click(cx, "bot-row-bot-shop");
    assert!(
        cx.debug_bounds("bot-panel").is_some(),
        "right-hand BotPanel must appear for remote bot even while launch_host is Local"
    );
    cx.update(|_, cx| {
        let s = view.read(cx);
        let pane = s.layout.focused().expect("focused pane");
        assert_eq!(s.layout.host(pane), &remote_host);
        assert_eq!(s.layout.session(pane), Some(SessionId(901)));
        assert_eq!(
            s.focused_bot().map(|b| (&b.host, &b.id)),
            Some((&remote_host, &remote_cfg.id))
        );
        let panel = s.bot_ui.panel.view.clone().expect("remote panel view");
        assert_eq!(panel.read(cx).host(), &remote_host);
        assert_eq!(
            panel.read(cx).selected_bot().map(|b| &b.id),
            Some(&remote_cfg.id)
        );
    });
}

#[gpui::test]
fn transient_remote_error_preserves_bots_and_unsupported_host_is_cached(cx: &mut TestAppContext) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use krill_app_core::hosts::{HostId, HostStatus};
    use krill_proto::bots::{BotConfig, BotRpcRequest, BotRpcResponse};

    let (view, cx) = open(
        cx,
        state_path("remote-bots-error-preserve-and-unsupported-cache"),
    );
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));

    let good_host = HostId::Remote("iroh:macmini".to_owned());
    let old_host = HostId::Remote("iroh:old-mini".to_owned());
    let fail_good = Arc::new(AtomicBool::new(false));
    let fail_good_bg = Arc::clone(&fail_good);
    let old_calls = Arc::new(AtomicUsize::new(0));
    let old_calls_bg = Arc::clone(&old_calls);

    let cfg = BotConfig::new(BotId::new("bot-shop").unwrap(), "쇼핑봇", "claude", 1_000).unwrap();
    let good_rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| {
        if fail_good_bg.load(Ordering::Acquire) {
            return Err("transient stream reset".to_owned());
        }
        match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![cfg.clone()])),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(Vec::new())),
            BotRpcRequest::ListAccessRequests { .. } => {
                Ok(BotRpcResponse::AccessRequests(Vec::new()))
            }
            BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(Vec::new())),
            BotRpcRequest::ListGroupSummaries => Ok(BotRpcResponse::GroupSummaries(Vec::new())),
            _ => Ok(BotRpcResponse::Ok),
        }
    });
    let old_rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |_| {
        old_calls_bg.fetch_add(1, Ordering::AcqRel);
        Err("krilld does not support FEAT_BOTS (krilld 0.3.6)".to_owned())
    });

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_overrides.insert(good_host.clone(), good_rpc);
            s.bot_rpc_overrides.insert(old_host.clone(), old_rpc);
            s.sidebar.update(cx, |sb, cx| {
                sb.set_remote_snapshot_for_test(
                    "iroh:macmini",
                    HostStatus::Connected,
                    sidebar::Snapshot::default(),
                    cx,
                );
                sb.set_remote_snapshot_for_test(
                    "iroh:old-mini",
                    HostStatus::Connected,
                    sidebar::Snapshot::default(),
                    cx,
                );
            });
            s.refresh_bots_badge(cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("bot-row-bot-shop").is_some());
    assert_eq!(old_calls.load(Ordering::Acquire), 1);

    // Flip macmini into transient failure and force another refresh:
    // - macmini's previously loaded bot must be preserved (never wiped)
    // - old-mini (cached unsupported) must NOT be polled again
    fail_good.store(true, Ordering::Release);
    cx.update(|_, cx| {
        view.update(cx, Shell::refresh_bots_badge);
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("bot-row-bot-shop").is_some(),
        "transient RPC error must preserve last-known remote bots"
    );
    assert_eq!(
        old_calls.load(Ordering::Acquire),
        1,
        "host lacking FEAT_BOTS must stay cached and skip subsequent polls"
    );
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.sidebar_bots.len(), 1);
        assert_eq!(s.sidebar_bots[0].host, good_host);
        assert!(s.bot_ui.hosts[&good_host].consecutive_failures >= 1);
        assert!(s.bot_ui.hosts[&old_host].unsupported.is_some());
    });
}

#[gpui::test]
fn same_bot_id_across_hosts_coexists_and_badge_update_is_host_scoped(cx: &mut TestAppContext) {
    use krill_app_core::hosts::{HostId, HostStatus};

    let (view, cx) = open(cx, state_path("multi-host-same-bot-id-isolation"));
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));

    let remote_host = HostId::Remote("iroh:macmini".to_owned());
    let local_bot = bot(
        "shared",
        "Local Shared",
        "Local copy",
        vec![chat("shared", 10, "Local")],
    );
    let mut remote_bot = bot(
        "shared",
        "Remote Shared",
        "Remote copy",
        vec![chat("shared", 20, "Remote")],
    );
    remote_bot.host = remote_host.clone();

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.sidebar.update(cx, |sb, cx| {
                sb.set_remote_snapshot_for_test(
                    "iroh:macmini",
                    HostStatus::Connected,
                    sidebar::Snapshot::default(),
                    cx,
                );
            });
            s.apply_sidebar_bots_for_host(&HostId::Local, 1, vec![local_bot], cx);
            s.apply_sidebar_bots_for_host(&remote_host, 2, vec![remote_bot], cx);
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.sidebar_bots.len(), 2);
        assert_eq!(s.bots_attention_count, 3);
    });

    // Updating Local's bots (e.g. via BadgeChanged from the modal) must NOT wipe Remote's bots.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.apply_sidebar_bots_for_host(&HostId::Local, 0, Vec::new(), cx);
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.sidebar_bots.len(), 1);
        assert_eq!(s.sidebar_bots[0].host, remote_host);
        assert_eq!(s.sidebar_bots[0].name, "Remote Shared");
        assert_eq!(s.bots_attention_count, 2);
    });
}

#[gpui::test]
fn domain_a_parity_collapsed_edges_up_next_chat_controls_and_chat_search(cx: &mut TestAppContext) {
    let (view, cx) = setup(cx, "bot-sidebar-domain-a-parity");

    // 1. Avatar constant is 32px, worker shows right-aligned age, and BotUpNextCount renders & clicks.
    assert!((super::rows::AVATAR - 32.).abs() < f32::EPSILON);
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_ui.up_next_unseen.insert(
                (
                    krill_app_core::hosts::HostId::Local,
                    BotId::new("krill").unwrap(),
                ),
                3,
            );
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("bot-row-up-next-krill").is_some(),
        "BotUpNextCount badge must render on active bot row"
    );
    // Hovering a chat row reveals its `⋯` BotChatMoreButton in place of the trailing age.
    let home_chat = cx.debug_bounds("bot-chat-21").expect("Home Chat row");
    assert!(
        cx.debug_bounds("bot-chat-more-21").is_none(),
        "BotChatMoreButton stays hidden until row hover"
    );
    cx.simulate_mouse_move(home_chat.center(), None, Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("bot-chat-more-21").is_some(),
        "BotChatMoreButton must become visible on chat row hover"
    );

    // 2. Search matches chat titles & bodies and keeps result cards inside the scroll list (#46830).
    click(cx, "bots-section-search");
    cx.simulate_input("Claude");
    cx.run_until_parked();
    let list_bounds = cx
        .debug_bounds("bots-search-results")
        .expect("search results list");
    let hit = cx
        .debug_bounds("bot-search-result-krill")
        .expect("chat title hit resolves to owning bot");
    assert!(
        hit.left() >= list_bounds.left() && hit.right() <= list_bounds.right(),
        "search result focus border must stay inside list bounds: hit={hit:?} list={list_bounds:?}"
    );
    // Close search before testing collapsed preview.
    click(cx, "bots-section-search");

    // 3. Collapsed preview virtualized track + left/right overflow edges (#47622, #47884).
    let many: Vec<_> = (0..10)
        .map(|i| {
            bot(
                &format!("bot-{i}"),
                &format!("Bot {i}"),
                "Automation worker",
                Vec::new(),
            )
        })
        .collect();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.apply_sidebar_bots(0, many, cx);
            s.bot_ui.collapsed = true;
            s.bot_ui.preview_offset = 0;
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("bots-collapsed-edge-left").is_none(),
        "at offset 0 there are no hidden bots on the left"
    );
    assert!(
        cx.debug_bounds("bots-collapsed-edge-right").is_some(),
        "10 bots must overflow and show the right BotPreviewEdge"
    );
    // Paging right advances preview_offset and reveals the left BotPreviewEdge.
    click(cx, "bots-collapsed-edge-right");
    assert!(
        cx.debug_bounds("bots-collapsed-edge-left").is_some(),
        "after paging right, left BotPreviewEdge must appear"
    );
    click(cx, "bots-collapsed-edge-left");
    assert!(
        cx.debug_bounds("bots-collapsed-edge-left").is_none(),
        "paging back left returns to offset 0"
    );
}

#[gpui::test]
fn s2_07_08_11_15_bot_section_errors_delete_cleanup_offline_dialogs_and_up_next(
    cx: &mut TestAppContext,
) {
    use krill_app_core::hosts::HostId;
    use krill_proto::bots::{BotGroupChat, BotGroupSummary, BotRpcRequest, BotRpcResponse};

    let (view, cx) = setup(cx, "bot-section-wave2a-fixes");
    let krill_id = BotId::new("krill").unwrap();
    let zed_id = BotId::new("zed").unwrap();

    // S2-07: RPC errors across pause/delete/stop-chat/rename/delete-group all set notice + notify.
    let err_rpc: krill_bots_ui::bots::BotRpcCall =
        Arc::new(|_req| Err("simulated rpc failure".to_owned()));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = Some(err_rpc.clone());
            s.set_bot_paused_on(&HostId::Local, krill_id.clone(), true, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |s, _| s.notice.clone()).as_deref(),
        Some("simulated rpc failure")
    );

    for action in ["stop_chat", "rename", "delete_group", "delete_bot"] {
        cx.update(|_, cx| {
            view.update(cx, |s, cx| {
                s.notice = None;
                match action {
                    "stop_chat" => {
                        s.stop_bot_chat_on(&HostId::Local, krill_id.clone(), SessionId(21), cx);
                    }
                    "rename" => {
                        s.rename_bot_on(&HostId::Local, krill_id.clone(), "Renamed".into(), cx);
                    }
                    "delete_group" => {
                        s.confirm_delete_group_on(&HostId::Local, "grp-1".into(), cx);
                    }
                    "delete_bot" => {
                        s.delete_bot_keep_folder_on(&HostId::Local, zed_id.clone(), cx);
                    }
                    _ => {}
                }
            });
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |s, _| s.notice.clone()).as_deref(),
            Some("simulated rpc failure"),
            "S2-07 action `{action}` must surface RPC error via notice"
        );
    }

    // S2-08: delete_bot_keep_folder_on clears panel state and closes open tabs for the deleted bot.
    let ok_rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(|req| match req {
        BotRpcRequest::DeleteBot { bot_id, .. } => Ok(BotRpcResponse::Deleted {
            bot_id: bot_id.clone(),
        }),
        _ => Ok(BotRpcResponse::Ok),
    });
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = Some(ok_rpc.clone());
            s.open_bot_session_on(&HostId::Local, &krill_id, SessionId(21), cx);
            s.bot_ui.panel.shown = Some((
                HostId::Local,
                krill_id.clone(),
                krill_bots_ui::bots::DetailTab::Items,
            ));
            s.bot_ui.panel.settings = Some(krill_bots_ui::bots::DetailTab::Settings);
            s.bot_ui.panel.pending_draft = Some((HostId::Local, krill_id.clone(), "draft".into()));
            s.bot_ui.panel.stale = false;
            s.delete_bot_keep_folder_on(&HostId::Local, krill_id.clone(), cx);
        });
    });
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert!(s.bot_ui.panel.shown.is_none());
        assert!(s.bot_ui.panel.settings.is_none());
        assert!(s.bot_ui.panel.pending_draft.is_none());
        assert!(s.bot_ui.panel.stale);
        let open_21 = (0..s.layout.tabs().len())
            .flat_map(|i| s.layout.tab_panes(i))
            .any(|p| s.layout.session(p) == Some(SessionId(21)));
        assert!(!open_21, "deleted bot's open session tab must be closed");
    });

    // S2-11: offline remote launch_host must not drop Templates or NewBot when Local is available.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = None;
            s.bot_rpc_overrides.clear();
            s.bot_rpc_overrides.insert(HostId::Local, ok_rpc.clone());
            s.launch_host = HostId::Remote("offline-host".into());
            assert!(
                s.bot_rpc().is_none(),
                "offline remote launch_host has no direct bot_rpc"
            );
            s.request_bot_templates(cx);
            s.open_pending_bot_dialogs(window, cx);
            assert!(
                s.bot_ui.templates.is_some(),
                "Templates dialog must open even when launch_host is offline"
            );
            s.bot_ui.templates = None;
            s.request_new_bot(None, None, cx);
            s.open_pending_bot_dialogs(window, cx);
            assert!(
                s.bot_ui.new_bot.is_some(),
                "NewBot dialog must open using Local when remote launch_host is offline"
            );
            s.bot_ui.new_bot = None;
            s.launch_host = HostId::Local;
        });
    });

    // S2-15: up_next_unseen refreshes when a paused bot unpauses and clears after up_next_seen_ms advances.
    let seen_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let seen_ms_rpc = seen_ms.clone();
    let up_next_rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| match req {
        BotRpcRequest::GetBotUpcoming { .. } => {
            Ok(BotRpcResponse::Upcoming(krill_proto::bots::BotUpcoming {
                up_next_seen_ms: seen_ms_rpc.load(std::sync::atomic::Ordering::SeqCst),
                ..Default::default()
            }))
        }
        BotRpcRequest::ListSuggestions { bot_id, .. } => {
            let bid = bot_id.clone().unwrap_or_else(|| BotId::new("zed").unwrap());
            Ok(BotRpcResponse::Suggestions(vec![
                krill_proto::bots::BotSuggestion {
                    id: "sug-1".into(),
                    bot_id: bid,
                    title: "Triage alert".into(),
                    why: String::new(),
                    ask: String::new(),
                    link: None,
                    source: None,
                    dismissed: false,
                    created_at_ms: 500,
                },
            ]))
        }
        _ => Ok(BotRpcResponse::Ok),
    });
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_overrides.clear();
            s.bot_rpc_override = Some(up_next_rpc.clone());
            let mut paused_bot = bot("zed", "Alpha shop", "Finds prices", vec![]);
            paused_bot.paused = true;
            s.apply_sidebar_bots(0, vec![paused_bot], cx);
            s.sync_bot_sidebar_ephemeral(window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |s, _| s
            .bot_ui
            .up_next_unseen
            .get(&(HostId::Local, zed_id.clone()))
            .copied()),
        None,
        "paused bot must have no up_next_unseen count"
    );
    // Unpause the same bot (same (HostId, BotId) list, only paused changed!).
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let unpaused_bot = bot("zed", "Alpha shop", "Finds prices", vec![]);
            s.apply_sidebar_bots(0, vec![unpaused_bot], cx);
            s.sync_bot_sidebar_ephemeral(window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |s, _| s
            .bot_ui
            .up_next_unseen
            .get(&(HostId::Local, zed_id.clone()))
            .copied()),
        Some(1),
        "unpausing bot must refresh up_next_unseen"
    );
    // Simulate MarkUpNextSeen advancing up_next_seen_ms + BadgeChanged refreshing counts.
    seen_ms.store(1000, std::sync::atomic::Ordering::SeqCst);
    cx.update(|_, cx| {
        view.update(cx, Shell::refresh_bot_up_next);
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |s, _| s
            .bot_ui
            .up_next_unseen
            .get(&(HostId::Local, zed_id.clone()))
            .copied()),
        None,
        "refresh_bot_up_next must clear stale badge once seen_ms passes suggestion timestamp"
    );

    // S2-09: poll_group_room retains only active sessions in RoomState::member_items.
    let grp = BotGroupChat {
        id: "grp-prune".into(),
        name: "Ops".into(),
        bot_ids: vec![zed_id.clone()],
        description: String::new(),
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let room_rpc: krill_bots_ui::bots::BotRpcCall = Arc::new({
        let grp = grp.clone();
        move |req| match req {
            BotRpcRequest::ReadGroupRoom { .. } => Ok(BotRpcResponse::GroupRoom(
                krill_proto::bots::BotGroupRoomView {
                    group: grp.clone(),
                    reply_audience: krill_proto::bots::BotGroupReplyAudience::All,
                    threads: Vec::new(),
                    active_thread_id: None,
                    messages: Vec::new(),
                    has_earlier: false,
                    active_turns: Vec::new(),
                },
            )),
            _ => Ok(BotRpcResponse::Ok),
        }
    });
    let room_key = crate::shell::bot_group_chat::GroupRoomKey {
        host: HostId::Local,
        group_id: "grp-prune".into(),
        thread_id: None,
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = Some(room_rpc.clone());
            s.apply_sidebar_state(
                0,
                vec![bot(
                    "zed",
                    "Alpha shop",
                    "Finds prices",
                    vec![chat("zed", 41, "")],
                )],
                vec![BotGroupSummary {
                    group: grp.clone(),
                    reply_audience: krill_proto::bots::BotGroupReplyAudience::All,
                    threads: Vec::new(),
                    latest_message: None,
                    active_turns: Vec::new(),
                }],
                cx,
            );
            let room = s
                .bot_ui
                .group_chat
                .rooms
                .entry(room_key.clone())
                .or_default();
            room.member_items.insert(
                SessionId(999),
                vec![krill_proto::agent_history::ChatItem::default()],
            );
            s.poll_group_room(room_key.clone(), cx);
        });
    });
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |s, _| s
            .bot_ui
            .group_chat
            .rooms
            .get(&room_key)
            .is_some_and(|r| r.member_items.is_empty())),
        "S2-09: completed sessions must be pruned from RoomState::member_items"
    );
}

#[gpui::test]
fn offline_remote_host_returns_none_for_bot_rpc_and_blocks_group_approval(cx: &mut TestAppContext) {
    use crate::shell::bot_group_chat::{GroupRoomKey, RoomState};
    use krill_app_core::hosts::HostId;
    use krill_proto::bots::BotRpcResponse;

    let (view, cx) = open(cx, state_path("bot-ub01-offline-remote"));
    let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let local_calls_ref = Arc::clone(&local_calls);
    let offline_remote = HostId::Remote("iroh:offline-box".into());
    let key = GroupRoomKey {
        host: offline_remote.clone(),
        group_id: "grp-remote".into(),
        thread_id: None,
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = Some(Arc::new(move |_| {
                local_calls_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(BotRpcResponse::Ok)
            }));
            assert!(s.bot_rpc_on(&HostId::Local).is_some());
            assert!(
                s.bot_rpc_on(&offline_remote).is_none(),
                "offline remote host must not fall back to local bot_rpc_override"
            );
            assert!(matches!(
                s.resolved_target_for_host(&offline_remote),
                crate::shell::bot_section::ResolvedHostTarget::Offline
            ));
            s.bot_ui
                .group_chat
                .rooms
                .insert(key.clone(), RoomState::default());
            s.send_group_approval_input(key.clone(), SessionId(99), "y\r", cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(local_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    view.read_with(cx, |s, _| {
        assert!(
            s.bot_ui.group_chat.rooms[&key].action_error.is_some(),
            "offline remote approval input must surface disconnect error instead of hitting local daemon"
        );
    });
}

#[gpui::test]
fn close_panes_for_group_closes_background_split_without_touching_active_tab_or_other_host(
    cx: &mut TestAppContext,
) {
    use crate::shell::bot_group_chat::{GroupRoomKey, RoomState};
    use krill_app_core::hosts::HostId;
    use krill_app_core::layout::{Axis, GroupPaneTarget};

    let (view, cx) = open(cx, state_path("bot-ub02-close-panes-group"));
    let remote = HostId::Remote("iroh:studio".into());
    let (p_active, p_bg_regular, p_bg_group, p_remote_group) = view.update(cx, |s, cx| {
        // Tab 0 (active): regular pane.
        let p_active = s.layout.focused().unwrap();
        s.layout.set_session(p_active, SessionId(1));

        // Tab 1 (background): split with regular pane + local group "grp-x".
        s.layout.new_tab();
        let p_bg_regular = s.layout.focused().unwrap();
        s.layout.set_session(p_bg_regular, SessionId(2));
        s.layout.split(Axis::Horizontal);
        let p_bg_group = s.layout.focused().unwrap();
        s.layout.set_host(p_bg_group, HostId::Local);
        s.layout.set_group_pane(
            p_bg_group,
            Some(GroupPaneTarget {
                group_id: "grp-x".into(),
                thread_id: None,
            }),
        );

        // Tab 2 (background): remote group with the same id "grp-x".
        s.layout.new_tab();
        let p_remote_group = s.layout.focused().unwrap();
        s.layout.set_host(p_remote_group, remote.clone());
        s.layout.set_group_pane(
            p_remote_group,
            Some(GroupPaneTarget {
                group_id: "grp-x".into(),
                thread_id: None,
            }),
        );

        // Return focus to Tab 0's regular pane before closing the local group.
        s.layout.activate(0);
        s.layout.focus(p_active);

        s.bot_ui.group_chat.rooms.insert(
            GroupRoomKey {
                host: HostId::Local,
                group_id: "grp-x".into(),
                thread_id: None,
            },
            RoomState::default(),
        );
        s.bot_ui.group_chat.rooms.insert(
            GroupRoomKey {
                host: remote.clone(),
                group_id: "grp-x".into(),
                thread_id: None,
            },
            RoomState::default(),
        );

        s.close_panes_for_group(&HostId::Local, "grp-x", cx);
        (p_active, p_bg_regular, p_bg_group, p_remote_group)
    });
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert!(
            s.layout.pane_exists(p_active),
            "active tab pane must stay open"
        );
        assert_eq!(s.layout.focused(), Some(p_active));
        assert!(s.layout.pane_exists(p_bg_regular));
        assert!(
            !s.layout.pane_exists(p_bg_group),
            "background split group pane on Local must be closed"
        );
        assert!(
            s.layout.pane_exists(p_remote_group),
            "same-id group pane on remote host must stay open"
        );
        assert!(
            !s.bot_ui.group_chat.rooms.contains_key(&GroupRoomKey {
                host: HostId::Local,
                group_id: "grp-x".into(),
                thread_id: None,
            }),
            "Local room state must be pruned"
        );
        assert!(
            s.bot_ui.group_chat.rooms.contains_key(&GroupRoomKey {
                host: remote,
                group_id: "grp-x".into(),
                thread_id: None,
            }),
            "Remote room state must remain"
        );
    });
}

#[gpui::test]
fn group_room_uses_active_thread_prunes_member_items_and_discards_stale_polls(
    cx: &mut TestAppContext,
) {
    use crate::shell::bot_group_chat::{GroupRoomKey, RoomState};
    use krill_app_core::hosts::HostId;
    use krill_proto::agent_history::{ChatItem, ChatItemKind};
    use krill_proto::bots::{
        BotGroupChat, BotGroupMessage, BotGroupReplyAudience, BotGroupRoomView, BotRpcRequest,
        BotRpcResponse,
    };

    let (view, cx) = setup(cx, "bot-ub11-ub12-group-poll");
    let seen_threads = Arc::new(std::sync::Mutex::new(Vec::<(String, Option<String>)>::new()));
    let seen_ref = Arc::clone(&seen_threads);
    let key = GroupRoomKey {
        host: HostId::Local,
        group_id: "grp-sync".into(),
        thread_id: None,
    };
    let room_view = BotGroupRoomView {
        group: BotGroupChat {
            id: "grp-sync".into(),
            name: "Sync Crew".into(),
            bot_ids: vec![BotId::new("krill").unwrap()],
            description: String::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        },
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: Vec::new(),
        active_thread_id: Some("thr-resolved".into()),
        messages: vec![BotGroupMessage {
            message_id: "m1".into(),
            group_id: "grp-sync".into(),
            thread_id: "thr-resolved".into(),
            sender_id: "owner".into(),
            sender_name: "You".into(),
            body: "hi".into(),
            mentions: Vec::new(),
            created_at_ms: 10,
            in_progress: false,
        }],
        has_earlier: true,
        active_turns: Vec::new(),
    };
    let room_for_rpc = room_view.clone();
    let rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| match req {
        BotRpcRequest::ReadGroupRoom {
            thread_id,
            before_message_id,
            ..
        } => {
            let tag = if before_message_id.is_some() {
                "earlier"
            } else {
                "poll"
            };
            seen_ref
                .lock()
                .unwrap()
                .push((tag.to_owned(), thread_id.clone()));
            Ok(BotRpcResponse::GroupRoom(room_for_rpc.clone()))
        }
        BotRpcRequest::SendGroupMessageWithAudience { thread_id, .. } => {
            seen_ref
                .lock()
                .unwrap()
                .push(("send".to_owned(), thread_id.clone()));
            Ok(BotRpcResponse::Ok)
        }
        BotRpcRequest::StopGroupMember { thread_id, .. } => {
            seen_ref
                .lock()
                .unwrap()
                .push(("stop".to_owned(), thread_id.clone()));
            Ok(BotRpcResponse::Ok)
        }
        BotRpcRequest::SetGroupReplyAudience { group_id, audience } => {
            Ok(BotRpcResponse::GroupReplyAudienceUpdated {
                group_id: group_id.clone(),
                audience: audience.clone(),
            })
        }
        _ => Ok(BotRpcResponse::Ok),
    });

    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.bot_rpc_override = Some(rpc);
            let mut state = RoomState {
                room: Some(room_view),
                draft: "follow up".into(),
                ..Default::default()
            };
            // Seed a stale member_items entry for an exited/finished session (41 = Done in setup).
            state.member_items.insert(
                SessionId(41),
                vec![ChatItem {
                    kind: ChatItemKind::Question,
                    text: "old prompt".into(),
                    ..Default::default()
                }],
            );
            s.bot_ui.group_chat.rooms.insert(key.clone(), state);

            // Calling poll while loading sets repoll_needed rather than silently dropping.
            s.bot_ui.group_chat.rooms.get_mut(&key).unwrap().loading = true;
            s.poll_group_room(key.clone(), cx);
            assert!(s.bot_ui.group_chat.rooms[&key].repoll_needed);
            s.bot_ui.group_chat.rooms.get_mut(&key).unwrap().loading = false;

            // Poll, load earlier, send, and stop all forward resolved `active_thread_id`.
            s.poll_group_room(key.clone(), cx);
            s.load_earlier_group_messages(key.clone(), cx);
            s.send_group_composer_message(key.clone(), window, cx);
            s.stop_group_turn(key.clone(), None, cx);
        });
    });
    cx.run_until_parked();

    let log = seen_threads.lock().unwrap().clone();
    for tag in ["poll", "earlier", "send", "stop"] {
        assert!(
            log.iter()
                .any(|(t, tid)| t == tag && tid.as_deref() == Some("thr-resolved")),
            "expected {tag} to forward active_thread_id thr-resolved, got {log:?}"
        );
    }

    // Finished turn's stale member_items must be pruned after poll applies snapshot.
    view.read_with(cx, |s, _| {
        assert!(
            s.bot_ui.group_chat.rooms[&key].member_items.is_empty(),
            "stale member_items must be pruned when active_turns is empty"
        );
    });

    // Optimistic reply_audience bumps poll_gen so an older in-flight poll cannot revert it.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.poll_group_room(key.clone(), cx);
            s.set_group_reply_audience(key.clone(), BotGroupReplyAudience::All, cx);
        });
    });
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert_eq!(
            s.bot_ui.group_chat.rooms[&key].reply_audience,
            BotGroupReplyAudience::All
        );
    });
}

#[gpui::test]
fn inline_bot_and_group_rename_stop_click_propagation_and_cancel_on_escape(
    cx: &mut TestAppContext,
) {
    use krill_app_core::hosts::HostId;
    use krill_proto::bots::{BotGroupChat, BotGroupReplyAudience, BotGroupSummary};

    let (view, cx) = setup(cx, "bot-ub14-inline-rename");
    let group_summary = BotGroupSummary {
        group: BotGroupChat {
            id: "grp-ren".into(),
            name: "Ops".into(),
            bot_ids: vec![BotId::new("krill").unwrap()],
            description: String::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        },
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: Vec::new(),
        latest_message: None,
        active_turns: Vec::new(),
    };
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let bots = s.sidebar_bots.clone();
            s.apply_sidebar_state(0, bots, vec![group_summary], cx);
            let id = BotId::new("krill").unwrap();
            s.start_bot_rename_on(&HostId::Local, &id, window, cx);
        });
    });
    cx.run_until_parked();

    let tabs_before = view.read_with(cx, |s, _| s.layout.tabs().len());
    // Clicking inside the bot rename input must not bubble to row on_click (open_bot_home).
    click(cx, "bot-row-rename-krill");
    view.read_with(cx, |s, _| {
        assert!(s.bot_ui.renaming.is_some());
        assert_eq!(s.layout.tabs().len(), tabs_before);
    });
    // Pressing Escape cancels bot rename.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |s, _| assert!(s.bot_ui.renaming.is_none()));

    // Same for group inline rename.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.start_group_rename_on(&HostId::Local, "grp-ren", window, cx);
        });
    });
    cx.run_until_parked();
    click(cx, "bot-group-row-rename-grp-ren");
    view.read_with(cx, |s, _| {
        assert!(s.bot_ui.renaming_group.is_some());
        assert_eq!(s.layout.tabs().len(), tabs_before);
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |s, _| assert!(s.bot_ui.renaming_group.is_none()));
}

#[gpui::test]
fn inline_bot_and_group_rename_guard_ime_escape_and_modified_enter(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    use krill_app_core::hosts::HostId;
    use krill_proto::bots::{BotGroupChat, BotGroupReplyAudience, BotGroupSummary};

    let (view, cx) = setup(cx, "bot-r3a02-r3a05-inline-rename");
    let group_summary = BotGroupSummary {
        group: BotGroupChat {
            id: "grp-ime".into(),
            name: "Ops".into(),
            bot_ids: vec![BotId::new("krill").unwrap()],
            description: String::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        },
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: Vec::new(),
        latest_message: None,
        active_turns: Vec::new(),
    };
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let bots = s.sidebar_bots.clone();
            s.apply_sidebar_state(0, bots, vec![group_summary], cx);
            let id = BotId::new("krill").unwrap();
            s.start_bot_rename_on(&HostId::Local, &id, window, cx);
        });
    });
    cx.run_until_parked();

    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let input = s.bot_ui.renaming.as_ref().unwrap().2.clone();
            input.update(cx, |st, cx| {
                st.focus(window, cx);
                st.set_value("Renamed".to_owned(), window, cx);
                st.replace_and_mark_text_in_range(None, "한", None, window, cx);
            });
        });
    });
    cx.run_until_parked();

    // First Escape cancels IME preedit only, keeping inline bot rename open.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert!(
            s.bot_ui.renaming.is_some(),
            "Escape during IME composition must not cancel bot inline rename"
        );
    });

    // Modified Enter (Cmd+Shift+Enter / Shift+Enter) must not commit bot rename.
    for key in ["secondary-shift-enter", "shift-enter", "secondary-enter"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        view.read_with(cx, |s, _| {
            assert!(
                s.bot_ui.renaming.is_some(),
                "{key} must not commit bot inline rename"
            );
        });
    }

    // Second Escape (no IME composition) cancels bot rename.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |s, _| assert!(s.bot_ui.renaming.is_none()));

    // Same for group inline rename.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.start_group_rename_on(&HostId::Local, "grp-ime", window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let input = s.bot_ui.renaming_group.as_ref().unwrap().2.clone();
            input.update(cx, |st, cx| {
                st.focus(window, cx);
                st.set_value("RenamedGrp".to_owned(), window, cx);
                st.replace_and_mark_text_in_range(None, "한", None, window, cx);
            });
        });
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert!(
            s.bot_ui.renaming_group.is_some(),
            "Escape during IME composition must not cancel group inline rename"
        );
    });
    for key in ["secondary-shift-enter", "shift-enter", "secondary-enter"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        view.read_with(cx, |s, _| {
            assert!(
                s.bot_ui.renaming_group.is_some(),
                "{key} must not commit group inline rename"
            );
        });
    }
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |s, _| assert!(s.bot_ui.renaming_group.is_none()));
}

#[gpui::test]
fn bot_attention_facts_and_lifecycle_panel_reset_dedup_and_group_rename_rollback(
    cx: &mut TestAppContext,
) {
    use crate::shell::bot_panel::HomeChrome;
    use krill_app_core::hosts::HostId;
    use krill_bots_ui::bots::DetailTab;
    use krill_proto::bots::{
        BotConfig, BotGroupChat, BotGroupReplyAudience, BotGroupSummary, BotRpcRequest,
        BotRpcResponse,
    };

    let (view, cx) = setup(cx, "bot-ub13-ub16-ub43-ub44-ub45");
    let group_summary = BotGroupSummary {
        group: BotGroupChat {
            id: "grp-rb".into(),
            name: "Original Name".into(),
            bot_ids: vec![BotId::new("krill").unwrap()],
            description: String::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        },
        reply_audience: BotGroupReplyAudience::Mentions,
        threads: Vec::new(),
        latest_message: None,
        active_turns: Vec::new(),
    };
    let rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(|req| match req {
        BotRpcRequest::StartBotSession { bot_id, .. } => Ok(BotRpcResponse::SessionStarted {
            bot_id: bot_id.clone(),
            session_id: SessionId(21), // Already in krill's sessions!
            pid: 123,
        }),
        BotRpcRequest::UpdateGroup { .. } => Ok(BotRpcResponse::Error("rename refused".into())),
        _ => Ok(BotRpcResponse::Ok),
    });

    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            // UB-16: bot with attention > 0 and idle sessions must have needs_you == true.
            let mut attn_bot = bot(
                "attn",
                "Attn Bot",
                "Needs review",
                vec![chat("attn", 41, "")],
            );
            attn_bot.attention = 2;
            let facts = s.bot_facts(&attn_bot, None, cx);
            assert!(facts.needs_you);

            let bots = s.sidebar_bots.clone();
            s.bot_rpc_override = Some(rpc);
            s.apply_sidebar_state(0, bots, vec![group_summary], cx);

            // UB-43: SessionStarted with existing session_id must not duplicate row.sessions.
            let krill_id = BotId::new("krill").unwrap();
            s.start_bot_session_on(&HostId::Local, krill_id.clone(), None, cx);

            // UB-44: rename_group_rpc_on rolls back optimistic name on Err.
            s.rename_group_rpc_on(&HostId::Local, "grp-rb", "Optimistic Name", cx);
            assert_eq!(s.sidebar_groups[0].group.name, "Optimistic Name");

            // UB-13 & UB-45: delete_bot_keep_folder_on clears panel shown/settings/pending_draft/home_chrome.
            let zed_id = BotId::new("zed").unwrap();
            s.bot_ui.panel.shown = Some((HostId::Local, zed_id.clone(), DetailTab::SoulMemory));
            s.bot_ui.panel.settings = Some(DetailTab::SoulMemory);
            s.bot_ui.panel.pending_draft = Some((HostId::Local, zed_id.clone(), "draft".into()));
            s.bot_ui.panel.home_chrome = HomeChrome {
                bot_id: Some(zed_id.clone()),
                quiet_line: Some(("Running".into(), None, true)),
                ..Default::default()
            };
            s.bot_ui.panel.stale = false;
            s.delete_bot_keep_folder_on(&HostId::Local, zed_id, cx);
            assert!(s.bot_ui.panel.shown.is_none());
            assert!(s.bot_ui.panel.settings.is_none());
            assert!(s.bot_ui.panel.pending_draft.is_none());
            assert!(s.bot_ui.panel.home_chrome.bot_id.is_none());
            assert!(s.bot_ui.panel.stale);

            // UB-13: same-host Created event clears panel shown/settings/pending_draft and marks stale.
            s.bot_ui.panel.shown = Some((HostId::Local, krill_id.clone(), DetailTab::Items));
            s.bot_ui.panel.settings = Some(DetailTab::Settings);
            s.bot_ui.panel.pending_draft = Some((HostId::Local, krill_id, "old".into()));
            s.bot_ui.panel.stale = false;
            let created_cfg =
                BotConfig::new(BotId::new("fresh").unwrap(), "Fresh", "claude", 1).unwrap();
            s.on_new_bot_event(
                &krill_bots_ui::bots::new_bot::Event::Created {
                    bot: Box::new(created_cfg),
                    warning: None,
                    host: HostId::Local,
                },
                window,
                cx,
            );
            assert!(s.bot_ui.panel.shown.is_none());
            assert!(s.bot_ui.panel.settings.is_none());
            assert!(s.bot_ui.panel.pending_draft.is_none());
            assert!(s.bot_ui.panel.stale);
        });
    });
    cx.run_until_parked();

    view.read_with(cx, |s, _| {
        let krill_row = s
            .sidebar_bots
            .iter()
            .find(|b| b.id.as_str() == "krill")
            .unwrap();
        assert_eq!(
            krill_row
                .sessions
                .iter()
                .filter(|x| x.session_id == SessionId(21))
                .count(),
            1,
            "duplicate session_id must not be inserted into row.sessions"
        );
        assert_eq!(
            s.sidebar_groups[0].group.name, "Original Name",
            "failed group rename must roll back optimistic name"
        );
    });
}
