use super::*;
use krill_proto::search::{
    Direction, Hit, SearchRequest, SearchResult, Snapshot, Source as SearchSource,
};

#[test]
fn late_search_results_do_not_undo_input_or_scroll() {
    for action in ["input", "scroll"] {
        for source in [SearchSource::Screen, SearchSource::Cold { page: 0 }] {
            let mut pane = linked();
            update(&mut pane, 1, snapshot(["live-a", "live hit", "live-c"]));
            pane.scroll(3);
            answer(&mut pane, 4, &[]);
            assert_eq!(pane.scroll_offset(), 3);
            pane.set_searchable(true);
            assert!(pane.search("hit", Direction::Next));
            let frames = sent(out(&mut pane));
            let request = SearchRequest::decode(frames[0].0, &frames[0].1).unwrap();
            let late = SearchResult {
                req: request.req,
                snapshot: Snapshot::default(),
                total: 1,
                hit: Some(Hit {
                    index: 0,
                    source,
                    row: 1,
                    total_rows: 3,
                    column: 5,
                    cols: 3,
                    cells: row("live hit"),
                }),
                wrapped: false,
                stale: false,
                incomplete: false,
                approximate: false,
            };
            match action {
                "input" => {
                    assert!(pane.input(b"typed\r", Now::get()).is_none());
                    let frames = sent(out(&mut pane));
                    assert_eq!(frames.len(), 1);
                    assert_eq!(
                        Input::decode(frames[0].0, &frames[0].1).unwrap().bytes,
                        b"typed\r"
                    );
                    assert_eq!(pane.scroll_offset(), 0);
                }
                "scroll" => {
                    assert!(pane.scroll(-1));
                    assert_eq!(pane.scroll_offset(), 2);
                }
                _ => unreachable!(),
            }
            let shown = Rc::clone(&pane.shown);
            let offset = pane.scroll_offset();
            let event = feed(&mut pane, &late.encode().unwrap());
            assert!(
                !event.redraw,
                "late {source:?} search took over after {action}"
            );
            assert_eq!(pane.scroll_offset(), offset);
            assert!(Rc::ptr_eq(&shown, &pane.shown));
            assert!(pane.highlight().is_none());
            assert!(
                sent(out(&mut pane)).is_empty(),
                "old search must not fetch more history"
            );
            assert_eq!(
                pane.search.query, "hit",
                "manual actions retain the search query"
            );
            assert!(pane.is_linked());

            // Explicitly finding again still works; another old reply cannot consume it.
            assert!(pane.search("hit", Direction::Next));
            let frames = sent(out(&mut pane));
            let current = SearchRequest::decode(frames[0].0, &frames[0].1).unwrap();
            assert_ne!(current.req, request.req);
            assert!(!feed(&mut pane, &late.encode().unwrap()).redraw);
            assert_eq!(pane.search.pending, Some(current.req));
            let mut fresh = late;
            fresh.req = current.req;
            fresh.hit.as_mut().unwrap().source = SearchSource::Screen;
            assert!(feed(&mut pane, &fresh.encode().unwrap()).redraw);
            assert_eq!(pane.highlight().unwrap().line, 1);
            assert_eq!(pane.search_result().unwrap().req, current.req);
        }
    }
}
