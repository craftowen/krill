//! Variable-height rows retain their host-scoped identity and scroll anchor across snapshots.
use gpui::{FocusHandle, ListAlignment, ListOffset, ListState, ScrollStrategy, SharedString, px};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::{HostId, Row};

pub(super) struct CardList {
    pub state: ListState,
    pub pending_reveal: Rc<Cell<Option<usize>>>,
    rows: Vec<(HostId, Row, usize)>,
    editor: Option<usize>,
}

impl Default for CardList {
    fn default() -> Self {
        Self {
            state: ListState::new(0, ListAlignment::Top, px(super::ROW_HEIGHT * 2.)),
            pending_reveal: Rc::new(Cell::new(None)),
            rows: Vec::new(),
            editor: None,
        }
    }
}

impl CardList {
    /// A committed account handoff keeps the same measured row and scroll anchor.
    pub(super) fn rebind_session(
        &mut self,
        host: &HostId,
        from: krill_proto::SessionId,
        to: krill_proto::SessionId,
    ) {
        for (row_host, row, _) in &mut self.rows {
            if row_host == host
                && let Row::Session(session, _) = row
                && session.id == from
            {
                session.id = to;
            }
        }
    }

    pub fn update(&mut self, rows: Vec<(HostId, Row, usize)>) {
        if rows == self.rows {
            return;
        }
        let indices: BTreeMap<_, _> = rows
            .iter()
            .enumerate()
            .map(|(i, (host, row, _))| (row_id(host, row), i))
            .collect();
        let pending = self
            .pending_reveal
            .get()
            .and_then(|row| self.rows.iter().find(|(_, _, i)| *i == row));
        self.pending_reveal.set(pending.and_then(|(host, row, _)| {
            rows.iter()
                .find(|(h, r, _)| h == host && same_item(row, r))
                .map(|(_, _, i)| *i)
        }));
        let old_offset = self.state.logical_scroll_top();
        if let Some(index) = self.editor.take() {
            self.state.splice_focusable(index..index + 1, [None]);
        }
        let mut offset = old_offset;
        // Prefer the top row, then its next surviving neighbour, then the previous one.
        // Identical paths/session IDs on other hosts must never acquire the anchor.
        let anchor = self
            .rows
            .iter()
            .enumerate()
            .skip(old_offset.item_ix)
            .chain(self.rows.iter().enumerate().take(old_offset.item_ix).rev())
            .find_map(|(old_index, (host, row, _))| {
                indices
                    .get(&row_id(host, row))
                    .copied()
                    .map(|index| (old_index, index))
            });
        if let Some((old_index, index)) = anchor {
            offset.item_ix = index;
            if old_index != old_offset.item_ix {
                offset.offset_in_item = px(0.);
            }
        } else {
            offset.item_ix = old_offset.item_ix.min(rows.len().saturating_sub(1));
            offset.offset_in_item = px(0.);
        }
        let prefix = self
            .rows
            .iter()
            .zip(&rows)
            .take_while(|((a, r, _), (b, s, _))| a == b && same_item(r, s))
            .count();
        let suffix = self
            .rows
            .iter()
            .skip(prefix)
            .rev()
            .zip(rows.iter().skip(prefix).rev())
            .take_while(|((a, r, _), (b, s, _))| a == b && same_item(r, s))
            .count();
        if prefix + suffix != self.rows.len() || self.rows.len() != rows.len() {
            self.state.splice(
                prefix..self.rows.len() - suffix,
                rows.len() - prefix - suffix,
            );
        }
        // Metadata, wrapping and expansion can change heights without changing row identity.
        // Keep GPUI's measured size hints rather than discarding every off-screen measurement.
        self.state.remeasure_items(0..rows.len());
        self.rows = rows;
        self.state.scroll_to(offset);
    }

    pub fn row_index(&self, index: usize) -> Option<usize> {
        self.rows.get(index).map(|(_, _, row)| *row)
    }

    pub fn contains_row(&self, row: usize) -> bool {
        self.rows
            .binary_search_by_key(&row, |(_, _, index)| *index)
            .is_ok()
    }

    pub fn invalidate_heights(&self) {
        self.state.remeasure_items(0..self.rows.len());
    }

    /// Keep the focused inline input mounted when its card scrolls off-screen.
    /// Adapted from the preserved concurrent sidebar fix (run-51 concurrent-sidebar.patch).
    pub fn focus_editor(&mut self, editing: Option<(&std::path::Path, FocusHandle)>) {
        let next = editing.and_then(|(path, focus)| {
            self.rows
                .iter()
                .position(|(_, row, _)| matches!(row, Row::Worktree(p) if p == path))
                .map(|index| (index, focus))
        });
        if self.editor == next.as_ref().map(|(index, _)| *index) {
            return;
        }
        let offset = self.state.logical_scroll_top();
        if let Some(index) = self.editor.take() {
            self.state.splice_focusable(index..index + 1, [None]);
        }
        if let Some((index, focus)) = next {
            self.state.splice_focusable(index..index + 1, [Some(focus)]);
            self.editor = Some(index);
        }
        self.state.scroll_to(offset);
    }

    pub fn reveal(&self, row: usize, strategy: ScrollStrategy) {
        if self.rows.is_empty() {
            return;
        }
        let index = self
            .rows
            .partition_point(|(_, _, i)| *i <= row)
            .saturating_sub(1);
        self.pending_reveal.set(Some(self.rows[index].2));
        if strategy == ScrollStrategy::Top || self.state.bounds_for_item(index).is_none() {
            self.state.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: px(0.),
            });
        } else {
            self.state.scroll_to_reveal_item(index);
        }
    }
}

/// Stable element identity excludes mutable labels/status and includes the host and row kind.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Path Debug escapes distinguish non-UTF8 names in stable IDs; Display is lossy"
)]
pub(super) fn row_id(host: &HostId, row: &Row) -> SharedString {
    let identity = match row {
        Row::HostHeader { host, .. } => format!("host/{host:?}"),
        Row::GroupHeader { key, .. } => format!("group/{key:?}"),
        Row::Repository(path) => format!("repo/{path:?}"),
        Row::PendingWorktree { id, .. } => format!("pending/{id}"),
        Row::Worktree(path) => format!("tree/{path:?}"),
        Row::Folder(path) => format!("folder/{path:?}"),
        Row::HiddenWorktrees(path, ..) => format!("hidden/{path:?}"),
        Row::Session(session, nested) => format!("session/{}/{nested}", session.id.0),
        Row::NeedsYou => "needs-you".into(),
        Row::Loose => "loose".into(),
    };
    format!("{host:?}/{identity}").into()
}

pub(super) fn same_item(a: &Row, b: &Row) -> bool {
    match (a, b) {
        (Row::HostHeader { host: a, .. }, Row::HostHeader { host: b, .. }) => a == b,
        (Row::GroupHeader { key: a, .. }, Row::GroupHeader { key: b, .. }) => a == b,
        (Row::PendingWorktree { id: a, .. }, Row::PendingWorktree { id: b, .. }) => a == b,
        (Row::HiddenWorktrees(a, ..), Row::HiddenWorktrees(b, ..))
        | (Row::Repository(a), Row::Repository(b))
        | (Row::Worktree(a), Row::Worktree(b))
        | (Row::Folder(a), Row::Folder(b)) => a == b,
        (Row::Session(a, nested_a), Row::Session(b, nested_b)) => {
            a.id == b.id && nested_a == nested_b
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::{CardLayout, Sidebar, Snapshot};
    use gpui::{
        AppContext as _, Context, Entity, Render, StyleRefinement, TestAppContext, Window, div,
        prelude::*,
    };
    use std::collections::BTreeMap;

    #[test]
    fn changed_cards_keep_the_host_scoped_scroll_anchor_and_nested_session_target() {
        let mut list = CardList::default();
        let card = |host, path: &str, row| (host, Row::Worktree(path.into()), row);
        list.update(vec![
            card(HostId::Local, "/same", 0),
            card(HostId::Remote("pc".into()), "/same", 4),
        ]);
        list.state.scroll_to(ListOffset {
            item_ix: 1,
            offset_in_item: px(17.),
        });
        list.update(vec![
            card(HostId::Local, "/new", 0),
            card(HostId::Local, "/same", 1),
            card(HostId::Remote("pc".into()), "/same", 5),
        ]);
        assert_eq!(list.state.logical_scroll_top().item_ix, 2);
        assert_eq!(list.state.logical_scroll_top().offset_in_item, px(17.));
        list.reveal(7, ScrollStrategy::Top);
        assert_eq!(list.state.logical_scroll_top().item_ix, 2);
        assert_eq!(list.state.logical_scroll_top().offset_in_item, px(0.));
        list.update(Vec::new());
        assert_eq!(list.row_index(0), None);
    }

    #[gpui::test]
    fn detailed_cards_are_virtualized_at_every_row_count(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
        cx.simulate_resize(gpui::size(px(360.), px(480.)));
        for count in [98, 99, 100, 500] {
            view.update(cx, |s, cx| {
                s.snapshot = Snapshot {
                    sessions: Vec::new(),
                    trees: BTreeMap::from([(
                        "/repo".into(),
                        (0..count)
                            .map(|i| format!("/repo/tree-{i:03}").into())
                            .collect(),
                    )]),
                };
                s.options.card_layout = CardLayout::Detailed;
                s.rendered_rows = 0;
                s.rebuild(cx);
                s.scroll_to_row(0, cx);
            });
            cx.run_until_parked();
            let (last, rendered) = view.read_with(cx, |s, _| (s.rows.len() - 1, s.rendered_rows));
            assert_eq!(last, count);
            assert!(
                rendered < count,
                "off-screen cards must not be eagerly constructed: {rendered}/{count}"
            );
            let selector: &'static str = format!("worktree-card-{last}").leak();
            assert!(cx.debug_bounds(selector).is_none());
            let height = cx
                .debug_bounds("worktree-card-1")
                .expect("first card")
                .size
                .height;
            assert!(
                height > px(32.),
                "detailed layout must not become a fixed-height row"
            );
            view.update(cx, |s, cx| s.scroll_to_row(last, cx));
            cx.run_until_parked();
            assert!(
                cx.debug_bounds(selector).is_some(),
                "keyboard navigation must reveal the last card"
            );
        }
    }

    #[gpui::test]
    fn nearest_navigation_reveals_unmeasured_cards_and_sessions_inside_a_tall_card(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                trees: BTreeMap::from([("/repo".into(), vec!["/repo/a".into()])]),
                sessions: (1..=80)
                    .map(|id| crate::sidebar::Session {
                        id: krill_proto::SessionId(id),
                        status: krill_proto::control::AgentStatus::UNKNOWN,
                        worktree: Some("/repo/a".into()),
                        cwd: Some("/repo/a".into()),
                        account: None,
                    })
                    .collect(),
            };
            s.expanded_cards.insert("/repo/a".into());
            s.rebuild(cx);
        });
        cx.run_until_parked();
        let last = view.read_with(cx, |s, _| s.rows.len() - 1);
        assert!(last < 99);
        for row in [last, 2] {
            view.update(cx, |s, cx| {
                s.cursor = Some(row - 1);
                s.step(1, cx);
            });
            cx.run_until_parked();
            let bounds = cx
                .debug_bounds(format!("sidebar-session-{row}").leak())
                .unwrap();
            let height = cx.update(|window, _| window.viewport_size().height);
            assert!(
                bounds.top() >= px(0.) && bounds.bottom() <= height,
                "selected session must be visible: {bounds:?}, viewport {height:?}"
            );
            assert!(view.read_with(cx, |s, _| s.card_list.pending_reveal.get().is_none()));
        }
        view.update(cx, |s, cx| {
            s.snapshot.sessions.clear();
            s.snapshot.trees = BTreeMap::from([(
                "/repo".into(),
                (0..90).map(|i| format!("/repo/{i:03}").into()).collect(),
            )]);
            s.rebuild(cx);
        });
        cx.run_until_parked();
        view.update(cx, |s, cx| {
            s.cursor = Some(89);
            s.step(1, cx);
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("worktree-card-90").unwrap();
        let height = cx.update(|window, _| window.viewport_size().height);
        assert!(bounds.top() >= px(0.) && bounds.bottom() <= height);
    }

    #[gpui::test]
    fn scrolling_an_inline_editor_offscreen_preserves_its_input_and_enter_action(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
        cx.update(|window, cx| {
            view.update(cx, |s, cx| {
                s.snapshot.trees.insert(
                    "/repo".into(),
                    (0..90).map(|i| format!("/repo/{i:03}").into()).collect(),
                );
                s.rebuild(cx);
                s.rename("/repo/000".into(), window, cx);
            });
        });
        cx.run_until_parked();
        view.update(cx, |s, cx| s.scroll_to_row(90, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("스크롤 후에도 편집");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        view.read_with(cx, |s, _| {
            assert!(s.editing.is_none());
            assert_eq!(
                s.prefs.get(std::path::Path::new("/repo/000")).unwrap().name,
                "스크롤 후에도 편집"
            );
        });
    }

    #[gpui::test]
    fn unrelated_parent_updates_reuse_the_sidebar_but_sidebar_edits_invalidate_it(
        cx: &mut TestAppContext,
    ) {
        struct Parent {
            sidebar: Entity<Sidebar>,
            ticks: usize,
        }
        impl Render for Parent {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(
                    self.sidebar
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                )
            }
        }
        cx.update(gpui_component::init);
        let (parent, cx) = cx.add_window_view(|_, cx| Parent {
            sidebar: cx.new(|_| Sidebar::new_loaded(None)),
            ticks: 0,
        });
        cx.run_until_parked();
        let sidebar = parent.read_with(cx, |p, _| p.sidebar.clone());
        let before = sidebar.read_with(cx, |s, _| s.renders);
        for _ in 0..10 {
            parent.update(cx, |p, cx| {
                p.ticks += 1;
                cx.notify();
            });
            cx.run_until_parked();
        }
        assert_eq!(sidebar.read_with(cx, |s, _| s.renders), before);
        sidebar.update(cx, |s, cx| {
            s.set_title(krill_proto::SessionId(1), "Changed title", cx);
        });
        cx.run_until_parked();
        assert!(sidebar.read_with(cx, |s, _| s.renders) > before);
    }

    #[gpui::test]
    fn working_spinners_tick_in_the_parent_overlay_without_rerendering_the_sidebar(
        cx: &mut TestAppContext,
    ) {
        struct Parent {
            sidebar: Entity<Sidebar>,
            renders: usize,
        }
        impl Render for Parent {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                self.renders += 1;
                div()
                    .size_full()
                    .child(
                        self.sidebar
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    )
                    .child(self.sidebar.read(cx).spinner_overlay())
            }
        }
        cx.update(gpui_component::init);
        let (parent, cx) = cx.add_window_view(|_, cx| Parent {
            sidebar: cx.new(|_| Sidebar::new_loaded(None)),
            renders: 0,
        });
        let sidebar = parent.read_with(cx, |p, _| p.sidebar.clone());
        sidebar.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                trees: BTreeMap::from([("/repo".into(), vec!["/repo/a".into()])]),
                sessions: vec![crate::sidebar::Session {
                    id: krill_proto::SessionId(1),
                    status: krill_proto::control::AgentStatus {
                        state: krill_proto::control::AgentState::Working,
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: Some("/repo/a".into()),
                    cwd: Some("/repo/a".into()),
                    account: None,
                }],
            };
            s.expanded_cards.insert("/repo/a".into());
            s.rebuild(cx);
        });
        cx.run_until_parked();
        assert!(sidebar.read_with(cx, |s, _| !s.spinner_slots.is_empty()));
        let sidebar_before = sidebar.read_with(cx, |s, _| s.renders);
        let parent_before = parent.read_with(cx, |p, _| p.renders);
        for _ in 0..12 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
            cx.run_until_parked();
        }
        assert!(parent.read_with(cx, |p, _| p.renders) > parent_before + 5);
        assert_eq!(sidebar.read_with(cx, |s, _| s.renders), sidebar_before);
        assert!(sidebar.read_with(cx, |s, _| !s.spinner_slots.is_empty()));
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::sidebar::{CardLayout, CloseSession, Session, Sidebar, Snapshot, SortBy};
    use gpui::TestAppContext;
    use krill_proto::{
        SessionId,
        control::{AgentState, AgentStatus},
    };
    use std::{
        cell::Cell,
        collections::BTreeMap,
        path::{Path, PathBuf},
        rc::Rc,
    };

    fn session(id: u64, path: &str) -> Session {
        Session {
            id: SessionId(id),
            status: AgentStatus {
                agent: "pi".into(),
                state: AgentState::Done,
                title: Some(format!("Task {id}")),
                ..AgentStatus::UNKNOWN
            },
            worktree: Some(path.into()),
            cwd: Some(path.into()),
            account: None,
        }
    }

    #[test]
    fn stable_ids_exclude_mutable_status_but_include_host_and_session_occurrence() {
        let mut item = session(1, "/same");
        let original = row_id(&HostId::Local, &Row::Session(item.clone(), true));
        item.status.title = Some("Renamed".into());
        item.status.state = AgentState::Working;
        assert_eq!(
            original,
            row_id(&HostId::Local, &Row::Session(item.clone(), true))
        );
        assert_ne!(
            original,
            row_id(
                &HostId::Remote("pc".into()),
                &Row::Session(item.clone(), true)
            )
        );
        assert_ne!(original, row_id(&HostId::Local, &Row::Session(item, false)));
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_paths_with_the_same_lossy_display_keep_distinct_ids() {
        use std::os::unix::ffi::OsStringExt as _;
        let path = |last| PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', b'r', last]));
        let first = path(0xff);
        let second = path(0xfe);
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(
            row_id(&HostId::Local, &Row::Worktree(first)),
            row_id(&HostId::Local, &Row::Worktree(second))
        );
    }

    #[test]
    fn account_replacement_keeps_the_same_scroll_anchor_and_pixel_offset() {
        let mut list = CardList::default();
        let row = |id, index| {
            (
                HostId::Local,
                Row::Session(session(id, "/tree"), true),
                index,
            )
        };
        list.update(vec![row(10, 0), row(20, 1), row(30, 2)]);
        list.state.scroll_to(ListOffset {
            item_ix: 1,
            offset_in_item: px(11.),
        });
        list.rebind_session(&HostId::Local, SessionId(20), SessionId(90));
        list.update(vec![row(10, 0), row(90, 1), row(30, 2)]);
        assert_eq!(list.state.logical_scroll_top().item_ix, 1);
        assert_eq!(list.state.logical_scroll_top().offset_in_item, px(11.));
    }

    #[test]
    fn removing_the_anchor_keeps_its_next_surviving_neighbour() {
        let mut list = CardList::default();
        let row = |path: &str, i| (HostId::Local, Row::Worktree(path.into()), i);
        list.update(vec![row("/a", 0), row("/b", 1), row("/c", 2)]);
        list.state.scroll_to(ListOffset {
            item_ix: 1,
            offset_in_item: px(10.),
        });
        list.update(vec![row("/new", 0), row("/a", 1), row("/c", 2)]);
        assert_eq!(list.state.logical_scroll_top().item_ix, 2);
        assert_eq!(list.state.logical_scroll_top().offset_in_item, px(0.));
    }

    #[gpui::test]
    fn one_expanded_card_virtualizes_individual_sessions_and_scroll_never_closes_them(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
        cx.simulate_resize(gpui::size(px(360.), px(440.)));
        let snapshot = Snapshot {
            sessions: (1..=250).map(|id| session(id, "/repo/main")).collect(),
            trees: BTreeMap::from([("/repo".into(), vec!["/repo/main".into()])]),
        };
        let closed = Rc::new(Cell::new(0));
        let out = closed.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&view, move |_, _: &CloseSession, _| out.set(out.get() + 1))
        });
        for layout in [CardLayout::Detailed, CardLayout::Compact] {
            view.update(cx, |s, cx| {
                s.snapshot = snapshot.clone();
                s.options.card_layout = layout;
                s.expanded_cards.insert("/repo/main".into());
                s.cursor = None;
                s.rendered_rows = 0;
                s.rebuild(cx);
                s.scroll_to_row(0, cx);
            });
            cx.run_until_parked();
            let built = view.read_with(cx, |s, _| s.rendered_rows);
            assert!(built < 64, "{layout:?}: constructed {built} rows of 252");
            assert!(cx.debug_bounds("sidebar-session-251").is_none());
            let mut maximum = built;
            for target in [70, 140, 251, 10, 251] {
                view.update(cx, |s, cx| {
                    s.rendered_rows = 0;
                    s.scroll_to_row(target, cx);
                });
                cx.run_until_parked();
                maximum = maximum.max(view.read_with(cx, |s, _| s.rendered_rows));
                let selector: &'static str = format!("sidebar-session-{target}").leak();
                let bounds = cx
                    .debug_bounds(selector)
                    .expect("off-screen session revealed");
                let list = cx.debug_bounds("sidebar-viewport").expect("list");
                assert!(
                    bounds.origin.y >= list.origin.y
                        && bounds.bottom_left().y <= list.bottom_left().y
                );
                assert_eq!(view.read_with(cx, |s, _| s.snapshot.clone()), snapshot);
            }
            assert!(
                maximum < 64,
                "{layout:?}: max rows constructed {maximum}/252"
            );
            println!("sidebar expanded {layout:?}: logical_rows=252 max_constructed={maximum}");
        }
        assert_eq!(closed.get(), 0);
    }

    #[gpui::test]
    fn inserting_rows_keeps_the_actual_surface_anchor_and_selected_identity(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
        cx.simulate_resize(gpui::size(px(360.), px(460.)));
        let target = PathBuf::from("/repo/tree-040");
        view.update(cx, |s, cx| {
            s.options.sort_by = SortBy::Name;
            s.snapshot.trees.insert(
                "/repo".into(),
                (0..80)
                    .map(|i| format!("/repo/tree-{i:03}").into())
                    .collect(),
            );
            s.rebuild(cx);
            let index = s
                .rows
                .iter()
                .position(|r| *r == Row::Worktree(target.clone()))
                .unwrap();
            s.cursor = Some(index);
            s.scroll_to_row(index, cx);
        });
        cx.run_until_parked();
        let before = cx
            .debug_bounds("worktree-card-41")
            .expect("anchor card")
            .origin
            .y;
        view.update(cx, |s, cx| {
            s.snapshot
                .trees
                .get_mut(Path::new("/repo"))
                .unwrap()
                .insert(0, "/repo/tree-000a".into());
            s.rebuild(cx);
            assert_eq!(s.cursor_row(), Some(&Row::Worktree(target.clone())));
        });
        cx.run_until_parked();
        let after = cx
            .debug_bounds("worktree-card-42")
            .expect("same anchor after insert")
            .origin
            .y;
        assert_eq!(
            after, before,
            "inserting above the viewport must preserve the card's screen position"
        );
    }

    #[gpui::test]
    fn wip_decoration_is_absent_while_status_grouping_and_store_remain(cx: &mut TestAppContext) {
        use crate::sidebar::{GroupBy, GroupKey};
        use krill_proto::orchestration::WorkspaceStatus;
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
        let target = PathBuf::from("/repo/aaa");
        for count in [1, 150] {
            for layout in [CardLayout::Detailed, CardLayout::Compact] {
                view.update(cx, |s, cx| {
                    s.options.card_layout = layout;
                    s.options.group_by = GroupBy::Repo;
                    let mut trees = vec![target.clone()];
                    trees.extend((1..count).map(|i| format!("/repo/tree-{i:03}").into()));
                    s.snapshot.trees = BTreeMap::from([("/repo".into(), trees)]);
                    s.disk
                        .store
                        .statuses
                        .insert(target.display().to_string(), "in-progress".into());
                    s.worktree_statuses
                        .insert(target.clone(), WorkspaceStatus::InProgress);
                    s.rebuild(cx);
                    s.scroll_to_row(0, cx);
                });
                cx.run_until_parked();
                assert!(cx.debug_bounds("worktree-card-1").is_some());
                assert!(cx.debug_bounds("checkpoint-badge-1").is_none());
                view.update(cx, |s, cx| {
                    assert_eq!(
                        s.disk.store.statuses.get("/repo/aaa").map(String::as_str),
                        Some("in-progress")
                    );
                    assert_eq!(
                        s.worktree_statuses.get(&target),
                        Some(&WorkspaceStatus::InProgress)
                    );
                    s.options.group_by = GroupBy::WorkspaceStatus;
                    s.rebuild(cx);
                    assert!(s.rows.iter().any(|r| matches!(
                        r,
                        Row::GroupHeader {
                            key: GroupKey::WorkspaceStatus(WorkspaceStatus::InProgress),
                            ..
                        }
                    )));
                });
            }
        }
    }

    #[gpui::test]
    fn lineage_and_session_indents_stay_scoped_to_their_host(cx: &mut TestAppContext) {
        use crate::hosts::HostStatus;
        use crate::sidebar::RemoteHost;
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
        cx.simulate_resize(gpui::size(px(360.), px(1000.)));
        let remote = HostId::Remote("pc".into());
        let paths: Vec<PathBuf> = ["/repo/parent", "/repo/child", "/repo/grandchild"]
            .map(PathBuf::from)
            .into();
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: paths
                    .iter()
                    .enumerate()
                    .map(|(i, p)| session(u64::try_from(i).unwrap() + 1, p.to_str().unwrap()))
                    .collect(),
                trees: BTreeMap::from([("/repo".into(), paths.clone())]),
            };
            s.remote_hosts = vec![RemoteHost {
                id: "pc".into(),
                label: "Remote".into(),
                status: HostStatus::Connected,
                snapshot: s.snapshot.clone(),
                meta: BTreeMap::new(),
                source: None,
                error: None,
            }];
            s.parents.insert(paths[1].clone(), paths[0].clone());
            s.parents.insert(paths[2].clone(), paths[1].clone());
            s.collapsed_lineage.insert(paths[0].clone());
            assert!(
                s.remote_rows(&s.remote_hosts[0], &s.collapsed)
                    .iter()
                    .any(|r| r == &Row::Worktree(paths[2].clone())),
                "a local collapsed lineage must not hide remote same-path worktrees"
            );
            s.collapsed_lineage.clear();
            s.expanded_cards.extend(paths.iter().cloned());
            s.rebuild(cx);
        });
        for layout in [CardLayout::Detailed, CardLayout::Compact] {
            view.update(cx, |s, cx| s.set_card_layout(layout, cx));
            cx.run_until_parked();
            let mut positions = Vec::new();
            for host in [&HostId::Local, &remote] {
                let mut host_positions = Vec::new();
                for path in &paths {
                    let index = view.read_with(cx, |s, _| {
                        s.rows
                            .iter()
                            .enumerate()
                            .position(|(i, r)| {
                                s.row_host(i) == host && r == &Row::Worktree(path.clone())
                            })
                            .unwrap()
                    });
                    let card = cx
                        .debug_bounds(format!("worktree-card-{index}").leak())
                        .unwrap();
                    let glyph = cx
                        .debug_bounds(format!("session-state-{}", index + 1).leak())
                        .unwrap();
                    assert!(glyph.origin.x > card.origin.x + px(super::super::CHILD_INDENT));
                    host_positions.push((card.origin.x, glyph.origin.x));
                }
                positions.push(host_positions);
            }
            for pair in positions[0].windows(2) {
                assert_eq!(pair[1].0 - pair[0].0, px(14.));
                let glyph_step = pair[1].1 - pair[0].1;
                // The first descendant also gains the existing 1px heavier lineage border.
                assert!(glyph_step >= px(14.) && glyph_step <= px(15.));
            }
            assert!(
                positions[1].windows(2).all(|pair| pair[0] == pair[1]),
                "remote paths must not inherit the local lineage depth"
            );
        }
    }

    #[test]
    fn live_workspace_sessions_include_detached_and_nested_cwd_but_exclude_other_hosts_and_exits() {
        use crate::{hosts::HostStatus, sidebar::RemoteHost};
        let mut s = Sidebar::new_loaded(None);
        let mut nested = session(4, "/repo/main");
        nested.worktree = None;
        nested.cwd = Some("/repo/main/src".into());
        let mut exited = session(3, "/repo/main");
        exited.status.state = AgentState::Exited;
        exited.status.source = Some(krill_proto::control::Source::Process);
        s.snapshot = Snapshot {
            sessions: vec![
                session(1, "/repo/main"),
                session(2, "/repo/other"),
                exited,
                nested,
            ],
            trees: BTreeMap::from([(
                "/repo".into(),
                vec!["/repo/main".into(), "/repo/other".into()],
            )]),
        };
        s.remote_hosts = vec![RemoteHost {
            id: "pc".into(),
            label: "Remote".into(),
            status: HostStatus::Connected,
            snapshot: Snapshot {
                sessions: vec![session(1, "/repo/main")],
                trees: s.snapshot.trees.clone(),
            },
            meta: BTreeMap::new(),
            source: None,
            error: None,
        }];
        assert_eq!(
            s.live_sessions_in_workspace(&HostId::Local, Path::new("/repo/main")),
            vec![SessionId(1), SessionId(4)]
        );
        let remote = HostId::Remote("pc".into());
        assert_eq!(
            s.live_sessions_in_workspace(&remote, Path::new("/repo/main")),
            vec![SessionId(1)]
        );
        s.remote_hosts[0].status = HostStatus::Disconnected;
        assert!(
            s.live_sessions_in_workspace(&remote, Path::new("/repo/main"))
                .is_empty()
        );
    }
}
