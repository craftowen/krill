//! Keep a screen search hit attached to matching cells as output moves its row.
use super::{Cell, Flags, Screen, SearchState};
use krill_proto::screen::Revision;
use krill_proto::search::{Hit, Source};

impl SearchState {
    pub(super) fn refresh_screen_hit(&mut self, screen: &Screen, revision: Option<Revision>) {
        if !self.track_screen_hit {
            return;
        }
        self.highlight = None;
        let Some(result) = self.result.as_mut() else {
            self.track_screen_hit = false;
            return;
        };
        let Some(hit) = result
            .hit
            .as_mut()
            .filter(|hit| hit.source == Source::Screen)
        else {
            self.track_screen_hit = false;
            return;
        };
        if let Some(line) = locate(screen, hit) {
            if let Some(row) = screen.rows.get(usize::from(line))
                && !same_text(trim(row), trim(&hit.cells))
            {
                // Keep newly observed surrounding text for a subsequent row movement.
                hit.cells.clone_from(row);
            }
            hit.row = u32::from(line);
            self.highlight = Some(crate::terminal::Highlight {
                line,
                column: hit.column,
                cols: hit.cols,
            });
        } else if revision.is_some_and(|revision| revision >= result.snapshot.revision) {
            // Once the matching grid has arrived, missing text expires the active selection.
            // A future identical output row is a new occurrence, not this selected match.
            self.track_screen_hit = false;
        }
        // A search can beat its corresponding grid onto the socket. Keep the anchor
        // until that grid arrives, with no highlight on the unrelated older frame.
    }
}

fn locate(screen: &Screen, hit: &Hit) -> Option<u16> {
    if hit.cols == 0 {
        return None;
    }
    let start = usize::from(hit.column);
    let end = start.checked_add(usize::from(hit.cols))?;
    let expected = hit.cells.get(start..end)?;
    let matches = |row: &[Cell]| {
        row.get(start..end)
            .is_some_and(|cells| same_text(cells, expected))
    };
    // Unrelated text/style changes elsewhere on the same row do not lose the match.
    if let Ok(line) = usize::try_from(hit.row)
        && screen.rows.get(line).is_some_and(|row| matches(row))
    {
        return u16::try_from(line).ok();
    }
    // On row movement, require the whole textual row, not just another occurrence
    // of the query at the same column in an unrelated output line.
    screen.rows.iter().enumerate().find_map(|(line, row)| {
        (matches(row) && same_text(trim(row), trim(&hit.cells)))
            .then(|| u16::try_from(line).ok())
            .flatten()
    })
}

fn trim(row: &[Cell]) -> &[Cell] {
    let end = row
        .iter()
        .rposition(|cell| {
            cell.ch != ' '
                || !cell.zerowidth.is_empty()
                || cell.flags.contains(Flags::WIDE)
                || cell.flags.contains(Flags::SPACER)
        })
        .map_or(0, |index| index + 1);
    row.get(..end).unwrap_or_default()
}

fn same_text(a: &[Cell], b: &[Cell]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.ch == b.ch
                && a.zerowidth == b.zerowidth
                && [Flags::WIDE, Flags::SPACER, Flags::HIDDEN]
                    .into_iter()
                    .all(|flag| a.flags.contains(flag) == b.flags.contains(flag))
        })
}
