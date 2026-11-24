use super::*;
use crate::hosts::HostStatus;
use crate::sidebar::{Meta, Row, Session};
use gpui::{AppContext as _, TestAppContext};
use krill_proto::control::AgentStatus;
use std::path::PathBuf;

fn snapshot(folder: bool) -> Snapshot {
    Snapshot {
        sessions: [1, 2, 3, 4, 5]
            .into_iter()
            .map(|id| Session {
                id: SessionId(id),
                status: AgentStatus::UNKNOWN,
                worktree: (!folder).then(|| "/repo/tree".into()),
                cwd: Some("/repo/tree".into()),
                account: None,
            })
            .collect(),
        trees: if folder {
            BTreeMap::new()
        } else {
            BTreeMap::from([(PathBuf::from("/repo"), vec![PathBuf::from("/repo/tree")])])
        },
    }
}

fn rows(sidebar: &Sidebar, host: &HostId) -> Vec<SessionId> {
    sidebar
        .rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| match row {
            Row::Session(session, _) if sidebar.row_host(i) == host => Some(session.id),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn session_order_projects_real_rows_for_worktrees_folders_and_separate_hosts(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    let remote = HostId::Remote("studio".into());
    for folder in [false, true] {
        view.update(cx, |s, cx| {
            s.apply(Ok((snapshot(folder), Meta::new())), BTreeMap::new(), cx);
            s.set_remote_snapshot_for_test("studio", HostStatus::Connected, snapshot(folder), cx);
            s.set_session_order(
                vec![
                    (HostId::Local, SessionId(3)),
                    (HostId::Local, SessionId(1)),
                    (HostId::Local, SessionId(2)),
                    (remote.clone(), SessionId(2)),
                    (remote.clone(), SessionId(3)),
                    (remote.clone(), SessionId(1)),
                ],
                cx,
            );
            assert_eq!(
                rows(s, &HostId::Local),
                vec![
                    SessionId(3),
                    SessionId(1),
                    SessionId(2),
                    SessionId(4),
                    SessionId(5)
                ]
            );
            assert_eq!(
                rows(s, &remote),
                vec![
                    SessionId(2),
                    SessionId(3),
                    SessionId(1),
                    SessionId(4),
                    SessionId(5)
                ]
            );
            // A fresh poll or disconnected host retains its tab projection.
            s.apply(Ok((snapshot(folder), Meta::new())), BTreeMap::new(), cx);
            s.set_remote_snapshot_for_test(
                "studio",
                HostStatus::Disconnected,
                snapshot(folder),
                cx,
            );
            s.set_remote_snapshot_for_test("studio", HostStatus::Connected, snapshot(folder), cx);
            assert_eq!(
                rows(s, &HostId::Local)[..3],
                [SessionId(3), SessionId(1), SessionId(2)]
            );
            assert_eq!(
                rows(s, &remote)[..3],
                [SessionId(2), SessionId(3), SessionId(1)]
            );
        });
    }
}

#[gpui::test]
fn session_order_repeating_projection_does_not_notify(cx: &mut TestAppContext) {
    use std::cell::Cell;
    use std::rc::Rc;
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    let order = vec![(HostId::Local, SessionId(3)), (HostId::Local, SessionId(1))];
    view.update(cx, |s, cx| s.set_session_order(order.clone(), cx));
    cx.run_until_parked();
    let notifications = Rc::new(Cell::new(0));
    let count = notifications.clone();
    let _observer = cx.update(|cx| cx.observe(&view, move |_, _| count.set(count.get() + 1)));
    view.update(cx, |s, cx| s.set_session_order(order.clone(), cx));
    cx.run_until_parked();
    assert_eq!(notifications.get(), 0);
}
