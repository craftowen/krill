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
