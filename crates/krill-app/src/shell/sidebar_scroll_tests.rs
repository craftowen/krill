use super::tests::{open, state_path};
use super::*;
use gpui::TestAppContext;
use krill_app::sidebar::CardLayout;
use krill_proto::{SessionId, control::AgentStatus};
use std::path::Path;

#[gpui::test]
fn sidebar_clicks_keep_scroll_with_persisted_tab_order(cx: &mut TestAppContext) {
    struct Memory;
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(sidebar::Snapshot {
                sessions: (1..=81)
                    .map(|id| {
                        let path = if id == 81 {
                            "/repo/other"
                        } else {
                            "/repo/tree"
                        };
                        sidebar::Session {
                            id: SessionId(id),
                            status: AgentStatus {
                                title: Some(format!("Task {id}")),
                                ..AgentStatus::UNKNOWN
                            },
                            worktree: Some(path.into()),
                            cwd: Some(path.into()),
                            account: None,
                        }
                    })
                    .collect(),
                trees: [(
                    "/repo".into(),
                    vec!["/repo/other".into(), "/repo/tree".into()],
                )]
                .into(),
            })
        }
    }
    let (view, cx) = open(cx, state_path("sidebar-click-scroll"));
    cx.simulate_resize(gpui::size(px(1100.), px(650.)));
    let sidebar = view.read_with(cx, |s, _| s.sidebar.clone());
    sidebar.update(cx, |s, cx| s.connect_source(Arc::new(Memory), cx));
    cx.run_until_parked();

    for layout in [CardLayout::Detailed, CardLayout::Compact] {
        view.update(cx, |s, cx| {
            s.sidebar.update(cx, |s, cx| s.set_card_layout(layout, cx));
            s.open_session(SessionId(80), cx);
            s.open_session(SessionId(81), cx);
        });
        cx.run_until_parked();
        let index = sidebar.read_with(cx, |s, _| {
            s.visible_workspaces()
                .into_iter()
                .find(|(_, _, path)| path == Path::new("/repo/tree"))
                .unwrap()
                .0
        });
        sidebar.update(cx, |s, cx| s.scroll_to_row(index, cx));
        cx.run_until_parked();
        let card_selector: &'static str = format!("worktree-card-{index}").leak();
        let before = cx.debug_bounds(card_selector).expect("visible worktree");
        let last_session: &'static str = format!("sidebar-session-{}", index + 80).leak();
        assert!(cx.debug_bounds(last_session).is_none());
        // Restore the worktree MRU without moving the card; persisted tab order pins it first.
        cx.simulate_click(
            before.origin + point(px(45.), px(12.)),
            Modifiers::default(),
        );
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |s, _| s.focused_local_session()),
            Some(SessionId(80))
        );
        assert_eq!(
            cx.debug_bounds(card_selector).unwrap().origin,
            before.origin,
            "{layout:?}"
        );
        assert!(cx.update(|window, cx| view.read(cx).focus.is_focused(window)));

        let session_selector: &'static str = format!("sidebar-session-{}", index + 2).leak();
        let session_before = cx.debug_bounds(session_selector).expect("visible session");
        cx.simulate_click(session_before.center(), Modifiers::default());
        cx.run_until_parked();
        assert_ne!(
            view.read_with(cx, |s, _| s.focused_local_session()),
            Some(SessionId(80))
        );
        assert_eq!(
            cx.debug_bounds(session_selector).unwrap().origin,
            session_before.origin,
            "{layout:?}"
        );

        // An explicit reveal after selection must still work: the scroll guard is scoped.
        view.update(cx, |s, cx| s.open_session(SessionId(79), cx));
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |s, _| s.focused_local_session()),
            Some(SessionId(79))
        );
        let selected_row = sidebar.read_with(cx, |s, _| {
            let index = s
                .visible_workspaces()
                .into_iter()
                .find(|(_, _, path)| path == Path::new("/repo/tree"))
                .unwrap()
                .0;
            let position = s
                .live_sessions_in_workspace(
                    &krill_app::hosts::HostId::Local,
                    Path::new("/repo/tree"),
                )
                .iter()
                .position(|id| *id == SessionId(79))
                .unwrap();
            index + position + 1
        });
        let revealed: &'static str = format!("sidebar-session-{selected_row}").leak();
        assert!(
            cx.debug_bounds(revealed).is_some(),
            "explicit navigation reveals the selected session"
        );
    }
}
