//! Sidebar Bots section, Orca parity: the fold strip, chats with their workers, collapse all,
//! Bot options, search results and the row's line.

use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, VisualTestContext, px};
use krill_app::sidebar;
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
) -> krill_app::bots::SidebarBot {
    let mut cfg = krill_proto::bots::BotConfig::new(
        BotId::new(id).unwrap(),
        name,
        "claude",
        1_700_000_000_000,
    )
    .unwrap();
    cfg.avatar = Some("B1".to_owned());
    desc.clone_into(&mut cfg.description);
    let mut row = krill_app::bots::rpc::sidebar_snapshot(&[cfg], &[], &[], &sessions)
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
            Some(krill_app::bots::sidebar_view::RowLine::Replying(Some(
                "Claude Chat".to_owned()
            )))
        );
        assert_eq!(
            facts.chats[0].workers.len(),
            1,
            "Home Chat started the worker"
        );
    });
    // A chat starts folded; its chevron shows the worker it started.
    assert!(cx.debug_bounds("bot-worker-d1").is_none());
    click(cx, "bot-chat-fold-21");
    let worker = cx.debug_bounds("bot-worker-d1").expect("worker line");
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
            v.sort = krill_app::bots::sidebar_view::BotSidebarSort::Name;
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
) -> krill_app::bots::BotRpcCall {
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
                krill_app::bots::group_panel::GroupChatScope::AllTheirWork,
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
            s.bot_ui.delete_group = Some(group);
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
                sb.mark_session_unread(&krill_app::hosts::HostId::Local, SessionId(10), cx);
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
