//! Verified Codex 0.164 direct /model argument semantics. Unknown layouts fail closed.
use krill_proto::screen::{Flags, Screen};

pub(super) fn row(screen: &Screen, y: usize) -> String {
    screen
        .rows
        .get(y)
        .into_iter()
        .flatten()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

pub(super) fn verified(screen: &Screen) -> bool {
    screen
        .rows
        .iter()
        .enumerate()
        .any(|(y, _)| row(screen, y).trim() == ">_ OpenAI Codex (v0.164.0)")
}

pub(super) fn composer(screen: &Screen, command: Option<&str>) -> bool {
    if !screen.cursor.visible {
        return false;
    }
    let y = usize::from(screen.cursor.line);
    let Some(cells) = screen.rows.get(y) else {
        return false;
    };
    if cells
        .first()
        .is_none_or(|c| c.ch != '›' || !c.flags.contains(Flags::BOLD))
        || cells.get(1).is_none_or(|c| c.ch != ' ')
        || !cells.iter().all(|c| c.zerowidth.is_empty())
        || y == 0
        || !row(screen, y - 1).trim().is_empty()
        || !row(screen, y + 1).trim().is_empty()
        || !row(screen, y + 2).contains(" · ")
    {
        return false;
    }
    match command {
        Some(command) => {
            row(screen, y) == format!("› {command}")
                && usize::from(screen.cursor.column) == command.len() + 2
                && cells.iter().skip(2).all(|c| !c.flags.contains(Flags::DIM))
        }
        None => {
            screen.cursor.column == 2
                && cells
                    .iter()
                    .skip(2)
                    .all(|c| c.ch.is_whitespace() || c.flags.contains(Flags::DIM))
        }
    }
}

#[cfg(test)]
pub(super) fn confirmations(screen: &Screen, id: &str) -> usize {
    let prefix = format!("• Model changed to {id} ");
    let mut count = 0;
    let mut line = String::new();
    for cells in screen.rows.iter().take(usize::from(screen.cursor.line)) {
        let wrapped = cells
            .last()
            .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
        let text: String = cells.iter().map(|c| c.ch).collect();
        line.push_str(if wrapped { &text } else { text.trim_end() });
        if !wrapped {
            count +=
                usize::from(line.starts_with(&prefix) && line.ends_with(" for this session only"));
            line.clear();
        }
    }
    count
}

pub(super) fn fresh_text(tap: &crate::session::Tap, from: u64) -> String {
    let crate::session::Read::Bytes(bytes) = tap.read(from, 64 * 1024) else {
        return String::new();
    };
    plain(&bytes)
}

fn plain(bytes: &[u8]) -> String {
    crate::term::printed_text(bytes)
}

pub(super) fn contains_phrase(output: &str, phrase: &str) -> bool {
    // Native renderers may position each word with CSI column moves instead of printing spaces.
    // The current screen still supplies exact target/layout authority; this only checks fresh text.
    output
        .split_whitespace()
        .collect::<String>()
        .contains(&phrase.split_whitespace().collect::<String>())
}

pub(super) fn fresh_confirmation(tap: &crate::session::Tap, from: u64, id: &str) -> bool {
    let output = fresh_text(tap, from);
    contains_phrase(&output, &format!("Model changed to {id} "))
        && contains_phrase(&output, "for this session only")
}

pub(super) fn latest_confirmation(screen: &Screen, id: &str) -> bool {
    let mut last = String::new();
    let mut line = String::new();
    for cells in screen.rows.iter().take(usize::from(screen.cursor.line)) {
        let wrapped = cells
            .last()
            .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
        let text: String = cells.iter().map(|c| c.ch).collect();
        line.push_str(if wrapped { &text } else { text.trim_end() });
        if !wrapped {
            if !line.trim().is_empty() {
                last.clone_from(&line);
            }
            line.clear();
        }
    }
    last.starts_with(&format!("• Model changed to {id} "))
        && last.ends_with(" for this session only")
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::screen::{Cell, Cursor};
    #[test]
    fn cursor_positioned_words_are_fresh_confirmation_text() {
        let printed = plain(b"\x1b[2GSet\x1b[6Gmodel\x1b[12Gto\x1b[15GOpus\x1b[20G5.5");
        assert!(contains_phrase(&printed, "Set model to"));
        assert!(!contains_phrase(&printed, "Model changed to"));
        assert!(!contains_phrase("unrelated output", "Set model to"));
    }
    fn screen(draft: &str) -> Screen {
        let lines = [
            ">_ OpenAI Codex (v0.164.0)",
            "",
            "› ",
            "",
            "  Actual model default · /workspace",
        ];
        let mut screen = Screen {
            cols: 80,
            lines: 5,
            rows: lines
                .iter()
                .map(|s| {
                    s.chars()
                        .map(|ch| Cell {
                            ch,
                            ..Cell::default()
                        })
                        .collect()
                })
                .collect(),
            cursor: Cursor {
                line: 2,
                column: 2,
                visible: true,
            },
            ..Screen::default()
        };
        screen.rows[2][0].flags = Flags::BOLD;
        screen.rows[2].extend(draft.chars().map(|ch| Cell {
            ch,
            ..Cell::default()
        }));
        screen
    }
    #[test]
    fn draft_cursor_styles_and_known_version_are_required() {
        let mut s = screen("Ask Codex to do anything");
        assert!(!composer(&s, None));
        for c in s.rows[2].iter_mut().skip(2) {
            c.flags = Flags::DIM;
        }
        assert!(composer(&s, None));
        assert!(verified(&s));
        s.cursor.column = 3;
        assert!(!composer(&s, None));
        let mut draft = screen("/model actual-model");
        draft.cursor.column = 21;
        assert!(!composer(&draft, None));
        assert!(composer(&draft, Some("/model actual-model")));
        assert!(!composer(&draft, Some("/model forged")));
        draft.rows[0].clear();
        assert!(!verified(&draft));
    }
    #[test]
    fn acknowledgment_must_match_exact_id_and_session_only_semantics() {
        let mut s = screen("");
        s.rows.insert(
            0,
            "• Model changed to actual-model medium for this session only"
                .chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect(),
        );
        assert_eq!(confirmations(&s, "actual-model"), 1);
        assert_eq!(confirmations(&s, "actual"), 0);
        assert_eq!(confirmations(&s, "forged"), 0);
    }

    #[test]
    fn repeated_model_confirmation_does_not_depend_on_viewport_counts() {
        let output =
            plain(b"\x1b[32mModel changed to actual-model medium for this session only\x1b[0m");
        assert!(output.contains("Model changed to actual-model "));
        // An old identical acknowledgment may leave the viewport as this new one enters.
        // The backend uses this command's raw seq plus the actual live model footer instead of count growth.
        let stale = plain(b"\x1b[2Jmodel selector closed");
        assert!(!stale.contains("Model changed to actual-model "));
        let ack_a = "• Model changed to model-a medium for this session only";
        let ack_b = "• Model changed to model-b medium for this session only";
        let mut before = screen("");
        before.rows[0] = ack_a
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        before.rows[1] = ack_b
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        let mut after = before.clone();
        after.rows.swap(0, 1);
        assert_eq!(
            confirmations(&before, "model-a"),
            confirmations(&after, "model-a")
        );
        assert!(!latest_confirmation(&before, "model-a"));
        assert!(latest_confirmation(&after, "model-a"));
    }
}
