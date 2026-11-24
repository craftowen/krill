use super::*;
use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext};

fn projects(sidebar: &Sidebar) -> Vec<PathBuf> {
    sidebar
        .rows
        .iter()
        .filter_map(|row| project_key(row).map(|(_, path)| path.clone()))
        .collect()
}

fn drag_header(view: &Entity<Sidebar>, cx: &mut VisualTestContext, from: &str, to: &str) {
    view.update(cx, |s, cx| s.scroll_to_row(0, cx));
    cx.run_until_parked();
    let bounds = |path: &str, cx: &mut VisualTestContext| {
        let (kind, index) = view.read_with(cx, |s, _| {
            s.rows
                .iter()
                .enumerate()
                .find_map(|(i, row)| match row {
                    Row::Repository(p) if p == Path::new(path) => Some(("repo", i)),
                    Row::Folder(p) if p == Path::new(path) => Some(("folder", i)),
                    _ => None,
                })
                .unwrap()
        });
        let selector: &'static str = Box::leak(format!("{kind}-header-{index}").into_boxed_str());
        cx.debug_bounds(selector).unwrap()
    };
    // Avoid fold and action buttons: drag the title region of each header.
    let from = bounds(from, cx).center();
    let to = bounds(to, cx).center();
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn project_headers_drag_in_both_directions_and_restore_order(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-project-drag-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sidebar.bin");
    let snapshot = Snapshot {
        sessions: vec![Session {
            id: SessionId(1),
            status: AgentStatus::UNKNOWN,
            worktree: None,
            cwd: Some("/p/folder".into()),
            account: None,
        }],
        trees: BTreeMap::from([
            ("/p/a".into(), vec!["/p/a".into()]),
            ("/p/b".into(), vec!["/p/b".into()]),
            ("/p/c".into(), vec!["/p/c".into()]),
        ]),
    };
    let (view, cx) = cx.add_window_view({
        let file = file.clone();
        move |_, _| Sidebar::new_loaded(Some(file))
    });
    view.update(cx, |s, cx| {
        s.snapshot = snapshot.clone();
        s.collapsed
            .extend(["/p/a", "/p/b", "/p/c"].map(PathBuf::from));
        s.rebuild(cx);
    });
    cx.run_until_parked();
    drag_header(&view, cx, "/p/a", "/p/c");
    assert_eq!(
        view.read_with(cx, |s, _| projects(s)),
        ["/p/b", "/p/c", "/p/a", "/p/folder"].map(PathBuf::from)
    );
    drag_header(&view, cx, "/p/a", "/p/b");
    assert_eq!(
        view.read_with(cx, |s, _| projects(s)),
        ["/p/a", "/p/b", "/p/c", "/p/folder"].map(PathBuf::from)
    );
    drag_header(&view, cx, "/p/folder", "/p/a");
    view.update(cx, |s, cx| {
        assert_eq!(
            projects(s),
            ["/p/folder", "/p/a", "/p/b", "/p/c"].map(PathBuf::from)
        );
        assert!(matches!(&s.rows[1], Row::Session(session, _) if session.id == SessionId(1)));
        assert!(
            s.collapsed.contains(Path::new("/p/a")),
            "drag must not fold/unfold the source"
        );
        // Repeated daemon snapshots must retain the saved order.
        s.snapshot = snapshot.clone();
        s.rebuild(cx);
    });
    cx.run_until_parked();
    let mut restored = Sidebar::new_loaded(Some(file));
    restored.snapshot = snapshot;
    assert_eq!(
        restored.project_order,
        ["/p/folder", "/p/a", "/p/b", "/p/c"].map(PathBuf::from)
    );
    assert_eq!(
        view.read_with(cx, |s, _| projects(s)),
        restored.project_order
    );
    let rows = restored.local_rows();
    assert!(matches!(&rows[0], Row::Folder(p) if p == Path::new("/p/folder")));
    fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn remote_projects_reorder_as_complete_merged_blocks(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    view.update(cx, |s, cx| {
        s.snapshot.trees = BTreeMap::from([
            ("/p/a".into(), vec!["/p/a".into()]),
            ("/p/b".into(), vec!["/p/b".into()]),
        ]);
        s.collapsed
            .extend(["/p/a", "/p/b", "/remote/c"].map(PathBuf::from));
        s.set_remote_snapshot_for_test(
            "studio",
            HostStatus::Connected,
            Snapshot {
                sessions: vec![],
                trees: BTreeMap::from([
                    ("/remote/a".into(), vec!["/remote/a".into()]),
                    ("/remote/c".into(), vec!["/remote/c".into()]),
                ]),
            },
            cx,
        );
    });
    cx.run_until_parked();
    drag_header(&view, cx, "/remote/c", "/p/a");
    view.update(cx, |s, cx| {
        assert_eq!(
            projects(s),
            ["/remote/c", "/p/a", "/p/b"].map(PathBuf::from)
        );
        let index = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Repository(p) if p == Path::new("/p/a")))
            .unwrap();
        assert_eq!(
            s.merged_projects.get(&index).unwrap(),
            &vec![(HostId::Remote("studio".into()), PathBuf::from("/remote/a"))]
        );
        // Expanding a reordered project must reveal both PCs' worktrees together.
        s.fold(Path::new("/p/a"), Some(true), cx);
        let index = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Repository(p) if p == Path::new("/p/a")))
            .unwrap();
        let next = s
            .rows
            .iter()
            .enumerate()
            .skip(index + 1)
            .find_map(|(i, row)| project_key(row).map(|_| i))
            .unwrap();
        let children: Vec<_> = s.rows[index + 1..next]
            .iter()
            .zip(&s.row_hosts[index + 1..next])
            .filter_map(|(row, host)| match row {
                Row::Worktree(path) => Some((host.clone(), path.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            children,
            vec![
                (HostId::Local, PathBuf::from("/p/a")),
                (HostId::Remote("studio".into()), PathBuf::from("/remote/a"))
            ]
        );
        let order = s.project_order.clone();
        s.move_project(Path::new("/p/a"), Path::new("/p/a"), cx);
        s.move_project(Path::new("/missing"), Path::new("/p/a"), cx);
        assert_eq!(s.project_order, order);
    });
    drag_header(&view, cx, "/remote/c", "/p/b");
    assert_eq!(
        view.read_with(cx, |s, _| projects(s)),
        ["/p/a", "/p/b", "/remote/c"].map(PathBuf::from)
    );
}
