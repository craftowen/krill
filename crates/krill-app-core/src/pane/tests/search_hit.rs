use super::*;
use krill_proto::search::{
    Direction, Hit, SearchRequest, SearchResult, Snapshot, Source as SearchSource,
};

fn result(pane: &mut Pane<Vec<u8>>, revision: u64, line: u32, cells: Vec<Cell>) -> SearchResult {
    pane.set_searchable(true);
    let query: String = cells
        .iter()
        .skip(2)
        .take(3)
        .filter(|cell| !cell.flags.contains(Flags::SPACER))
        .flat_map(|cell| std::iter::once(cell.ch).chain(cell.zerowidth.iter().copied()))
        .collect();
    assert!(pane.search(&query, Direction::Next));
    let frames = sent(out(pane));
    let request = SearchRequest::decode(frames[0].0, &frames[0].1).unwrap();
    SearchResult {
        req: request.req,
        snapshot: Snapshot {
            revision: Revision(revision),
            seq: Seq(revision),
            ..Snapshot::default()
        },
        total: 1,
        hit: Some(Hit {
            index: 0,
            source: SearchSource::Screen,
            row: line,
            total_rows: 3,
            column: 2,
            cols: 3,
            cells,
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    }
}

#[test]
fn delayed_screen_hit_follows_the_matching_row_instead_of_old_coordinates() {
    let mut pane = linked();
    update(&mut pane, 1, snapshot(["header", "a hit", "tail"]));
    let response = result(&mut pane, 1, 1, row("a hit"));
    update(
        &mut pane,
        2,
        vec![
            Op::Scroll(1),
            Op::Line {
                line: 2,
                cells: row("new"),
            },
        ],
    );
    let shown = pane.shown.rows.clone();
    assert!(feed(&mut pane, &response.encode().unwrap()).redraw);
    assert_eq!(pane.shown.rows, shown, "search must retain live output");
    assert_eq!(
        pane.highlight().unwrap().line,
        0,
        "old row now contains tail, not hit"
    );
    assert!(sent(out(&mut pane)).is_empty());
}

#[test]
fn active_screen_hit_moves_with_output_and_expires_when_overwritten() {
    let mut pane = linked();
    update(&mut pane, 1, snapshot(["header", "a hit", "tail"]));
    let response = result(&mut pane, 1, 1, row("a hit"));
    feed(&mut pane, &response.encode().unwrap());
    assert_eq!(pane.highlight().unwrap().line, 1);
    update(
        &mut pane,
        2,
        vec![
            Op::Scroll(1),
            Op::Line {
                line: 2,
                cells: row("new"),
            },
        ],
    );
    assert_eq!(pane.highlight().unwrap().line, 0);
    update(
        &mut pane,
        3,
        vec![Op::Line {
            line: 0,
            cells: row("gone"),
        }],
    );
    assert!(
        pane.highlight().is_none(),
        "must not highlight unrelated output"
    );
    update(
        &mut pane,
        4,
        vec![Op::Line {
            line: 0,
            cells: row("a hit"),
        }],
    );
    assert!(
        pane.highlight().is_none(),
        "a later identical row must not revive an expired selection"
    );
    assert_eq!(pane.search.query, "hit");
    assert!(pane.is_linked());
}

#[test]
fn ahead_of_grid_search_waits_without_highlighting_the_previous_frame() {
    for action in ["wait", "input", "scroll"] {
        let mut pane = linked();
        update(&mut pane, 1, snapshot(["header", "older", "tail"]));
        let response = result(&mut pane, 2, 1, row("a hit"));
        feed(&mut pane, &response.encode().unwrap());
        assert!(
            pane.highlight().is_none(),
            "search frame arrived before the matching grid"
        );
        match action {
            "input" => {
                pane.input(b"x", Now::get());
            }
            "scroll" => {
                pane.scroll(1);
            }
            _ => {}
        }
        update(
            &mut pane,
            2,
            vec![Op::Line {
                line: 1,
                cells: row("a hit"),
            }],
        );
        assert_eq!(pane.highlight().is_some(), action == "wait", "{action}");
        if action == "wait" {
            assert_eq!(pane.highlight().unwrap().line, 1);
            assert!(
                sent(out(&mut pane)).is_empty(),
                "wait for the already-sent grid, not another search"
            );
        }
    }
}

#[test]
fn screen_hit_retains_style_changes_and_new_surrounding_text_when_it_moves() {
    let mut pane = linked();
    update(&mut pane, 1, snapshot(["header", "a hit", "tail"]));
    let response = result(&mut pane, 1, 1, row("a hit"));
    feed(&mut pane, &response.encode().unwrap());
    let mut changed = row("a hit end");
    changed[2].fg = krill_proto::screen::Color::Indexed(1);
    changed[3].flags = Flags::BOLD;
    update(
        &mut pane,
        2,
        vec![Op::Line {
            line: 1,
            cells: changed,
        }],
    );
    assert_eq!(pane.highlight().unwrap().line, 1);
    update(
        &mut pane,
        3,
        vec![
            Op::Scroll(1),
            Op::Line {
                line: 2,
                cells: row("new"),
            },
        ],
    );
    assert_eq!(pane.highlight().unwrap().line, 0);
}

#[test]
fn screen_hit_compares_cell_width_and_combining_characters() {
    let mut cells = row("a ");
    cells.push(Cell {
        ch: '한',
        flags: Flags::WIDE,
        ..Cell::default()
    });
    cells.push(Cell {
        flags: Flags::SPACER,
        ..Cell::default()
    });
    cells.push(Cell {
        ch: 'e',
        zerowidth: vec!['\u{301}'],
        ..Cell::default()
    });
    for change_width in [false, true] {
        let mut pane = linked();
        update(&mut pane, 1, snapshot(["header", "", "tail"]));
        update(
            &mut pane,
            2,
            vec![Op::Line {
                line: 1,
                cells: cells.clone(),
            }],
        );
        let response = result(&mut pane, 2, 1, cells.clone());
        feed(&mut pane, &response.encode().unwrap());
        assert_eq!(pane.highlight().unwrap().cols, 3);
        let mut changed = cells.clone();
        if change_width {
            changed[2].flags = Flags::default();
        } else {
            changed[4].zerowidth = vec!['\u{300}'];
        }
        update(
            &mut pane,
            3,
            vec![Op::Line {
                line: 1,
                cells: changed,
            }],
        );
        assert!(pane.highlight().is_none());
    }
}
