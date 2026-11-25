//! Live terminal screen detection for Antigravity (`agy`) mid-turn queued prompts.
//!
//! When Antigravity is busy mid-turn, pressing Enter after pasting a follow-up prompt
//! empties the `> ` composer immediately and renders the submitted prompt in a `▸ `
//! queue strip above the composer border (`▸ <text> (alt+m to send immediately)`),
//! without writing `USER_EXPLICIT` to `transcript_full.jsonl` until the running step
//! or turn completes. Both the desktop UI and the daemon inspect the live PTY screen
//! through this module to confirm queued acceptance without weakening the fail-closed
//! `PROMPT_REVIEW` safeguard when a draft remains unsubmitted in the composer.

use super::{Cell, Color, Flags, Screen};

/// Trailing hint Antigravity prints on the newest mid-turn queued item until `alt+m` is pressed.
pub const SEND_IMMEDIATELY_HINT: &str = "(alt+m to send immediately)";

/// Escape sequence for `Alt+M`, which promotes a queued Antigravity message to immediate steer.
pub const SEND_IMMEDIATELY_KEY: &[u8] = b"\x1bm";

/// Maximum interior rows between top and bottom composer borders when scanning for an active
/// composer (allows multiline drafts to be recognized as non-empty).
const MAX_COMPOSER_INTERIOR_ROWS: usize = 24;

/// Maximum blank rows skipped between the `▸ ` queued strip and the composer's top border.
const MAX_BLANK_GAP_ABOVE_BORDER: usize = 2;

/// Summary of matching Antigravity mid-turn queued items above the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueuedMatch {
    /// Number of contiguous `▸ ` queued rows directly above the composer matching the payload.
    pub count: usize,
    /// True if at least one matching queued row currently displays `(alt+m to send immediately)`.
    pub send_immediately_hint: bool,
}

fn row_text(cells: &[Cell]) -> String {
    let mut out = String::with_capacity(cells.len());
    for cell in cells {
        if cell.flags.contains(Flags::SPACER) {
            continue;
        }
        out.push(cell.ch);
        for &zw in &cell.zerowidth {
            out.push(zw);
        }
    }
    out.trim_end().to_owned()
}

fn is_border_row(cells: &[Cell]) -> bool {
    let text = row_text(cells);
    text.chars().count() >= 20 && text.chars().all(|c| c == '─')
}

/// True when a single composer interior row shows `> ` with no user-entered draft text
/// (only trailing whitespace or a dimmed/muted placeholder).
fn is_empty_composer_row(cells: &[Cell]) -> bool {
    let mut visible = cells.iter().filter(|c| !c.flags.contains(Flags::SPACER));
    let Some(first) = visible.next() else {
        return false;
    };
    if first.ch != '>' || !first.zerowidth.is_empty() {
        return false;
    }
    let prompt_fg = first.fg;
    if let Some(second) = visible.next()
        && (!second.ch.is_whitespace() || !second.zerowidth.is_empty())
    {
        return false;
    }
    let tail: Vec<&Cell> = visible.collect();
    if tail
        .iter()
        .all(|c| c.ch.is_whitespace() && c.zerowidth.is_empty())
    {
        return true;
    }
    let first_non_ws = tail
        .iter()
        .find(|c| !c.ch.is_whitespace() || !c.zerowidth.is_empty())
        .map(|c| c.ch);
    let has_muted_tail = tail.get(1..).is_some_and(|rest| {
        rest.iter()
            .any(|c| !c.ch.is_whitespace() || !c.zerowidth.is_empty())
    });
    !matches!(first_non_ws, Some('/' | '!' | '@'))
        && tail.iter().enumerate().all(|(idx, c)| {
            (c.ch.is_whitespace() && c.zerowidth.is_empty())
                || c.flags.contains(Flags::DIM)
                || (c.fg != Color::Default && c.fg != prompt_fg)
                || (idx == 0 && has_muted_tail && c.flags.contains(Flags::INVERSE))
        })
}

/// Locates Antigravity's interactive composer box (`───` / `> ...` / `───`) near the bottom
/// of the screen, returning `(top_border_row, composer_is_empty)`.
fn find_composer_box(screen: &Screen) -> Option<(usize, bool)> {
    let rows = &screen.rows;
    if rows.len() < 3 {
        return None;
    }
    let mut bottom = rows.len();
    while bottom > 2 {
        bottom = bottom.saturating_sub(1);
        if !rows.get(bottom).is_some_and(|r| is_border_row(r)) {
            continue;
        }
        let min_top = bottom.saturating_sub(MAX_COMPOSER_INTERIOR_ROWS.saturating_add(1));
        for top in (min_top..bottom.saturating_sub(1)).rev() {
            if !rows.get(top).is_some_and(|r| is_border_row(r)) {
                continue;
            }
            let interior_idx = top.saturating_add(1);
            let Some(interior) = rows.get(interior_idx) else {
                break;
            };
            let first_interior = row_text(interior);
            if first_interior.starts_with('>') {
                let single_row = bottom == top.saturating_add(2);
                let empty = single_row && is_empty_composer_row(interior);
                return Some((top, empty));
            }
            break;
        }
    }
    None
}

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_send_immediately_hint(text: &str) -> (&str, bool) {
    let trimmed = text.trim_end();
    if let Some(split_at) = trimmed.len().checked_sub(SEND_IMMEDIATELY_HINT.len())
        && let (Some(head), Some(tail)) = (trimmed.get(..split_at), trimmed.get(split_at..))
        && tail.eq_ignore_ascii_case(SEND_IMMEDIATELY_HINT)
    {
        return (head.trim_end(), true);
    }
    (trimmed, false)
}

fn queued_row_matches(row: &str, submitted_payload: &str) -> Option<bool> {
    let after_bullet = row.trim_start().strip_prefix('▸')?;
    let (without_hint, has_hint) = strip_send_immediately_hint(after_bullet.trim_start());
    let actual = normalize_ws(without_hint);
    if actual.is_empty() {
        return None;
    }
    let first_line = submitted_payload
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(normalize_ws)
        .unwrap_or_default();
    if first_line.is_empty() {
        return None;
    }
    let full_norm = normalize_ws(submitted_payload);
    if actual == first_line || actual == full_norm {
        return Some(has_hint);
    }
    let truncated_prefix = actual
        .strip_suffix("...")
        .or_else(|| actual.strip_suffix('…'))
        .map(str::trim_end);
    if let Some(prefix) = truncated_prefix
        && !prefix.is_empty()
    {
        let min_chars = if first_line.chars().count() >= 4 {
            3
        } else {
            1
        };
        if prefix.chars().count() >= min_chars
            && (first_line.starts_with(prefix) || full_norm.starts_with(prefix))
        {
            return Some(has_hint);
        }
    }
    None
}

fn count_matching_rows_above(
    screen: &Screen,
    top_border: usize,
    submitted_payload: &str,
) -> QueuedMatch {
    let mut y = top_border;
    let mut skipped_blanks = 0usize;
    let mut result = QueuedMatch::default();
    let mut saw_bullet = false;
    while y > 0 {
        y = y.saturating_sub(1);
        let Some(row) = screen.rows.get(y) else {
            break;
        };
        let text = row_text(row);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            if saw_bullet || skipped_blanks >= MAX_BLANK_GAP_ABOVE_BORDER {
                break;
            }
            skipped_blanks = skipped_blanks.saturating_add(1);
            continue;
        }
        if trimmed.starts_with('⎿') {
            continue;
        }
        if !trimmed.starts_with('▸') {
            break;
        }
        saw_bullet = true;
        if let Some(has_hint) = queued_row_matches(trimmed, submitted_payload) {
            result.count = result.count.saturating_add(1);
            result.send_immediately_hint |= has_hint;
        }
    }
    result
}

/// Returns `true` when plain screen text shows an Antigravity (`agy`) unauthenticated login
/// or OAuth authentication TUI where sending Enter or a prompt could launch a browser OAuth flow.
#[must_use]
pub fn auth_login_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("welcome to the antigravity cli. you are currently not signed in")
        || lower.contains("you are not logged into antigravity")
        || lower.contains("click here to authenticate")
        || lower.contains("log in with google")
        || lower.contains("if you aren't automatically redirected, paste the authorization code")
        || lower.contains("enter the redirect url (if redirect failed)")
    {
        return true;
    }
    if lower.contains("select login method")
        && (lower.contains("google oauth") || lower.contains("google cloud project"))
    {
        return true;
    }
    if lower.contains("your browser should open automatically")
        && (lower.contains("accounts.google.com/o/oauth2/auth")
            || lower.contains("antigravity.google")
            || lower.contains("authorization code")
            || lower.contains("click here to authenticate"))
    {
        return true;
    }
    if lower.contains("accounts.google.com/o/oauth2/auth")
        && (lower.contains("antigravity.google")
            || lower.contains("oauth-callback")
            || lower.contains("authorization code..."))
    {
        return true;
    }
    false
}

/// Returns `true` when the live PTY screen is showing an Antigravity (`agy`) login or OAuth TUI.
/// Automated prompt, steer, bot, schedule, and auto-resume senders must refuse to inject input
/// (especially `\r` / Enter) whenever this returns `true`.
#[must_use]
pub fn auth_login_screen(screen: &Screen) -> bool {
    let mut text = String::new();
    for (idx, row) in screen.rows.iter().enumerate() {
        if idx > 0 {
            text.push('\n');
        }
        text.push_str(&row_text(row));
    }
    auth_login_text(&text)
}

/// Returns `true` when Antigravity's interactive composer box (`───` / `> ...` / `───`)
/// is visible and empty (blank or placeholder-only, with no user draft).
#[must_use]
pub fn empty_composer_ready(screen: &Screen) -> bool {
    if auth_login_screen(screen) {
        return false;
    }
    find_composer_box(screen).is_some_and(|(_, empty)| empty)
}

/// Counts how many contiguous `▸ ` queued rows directly above Antigravity's composer currently
/// match `submitted_payload`. Call this before pasting to establish a baseline when the same
/// prompt might already be queued.
#[must_use]
pub fn count_queued_matches(screen: &Screen, submitted_payload: &str) -> QueuedMatch {
    let Some((top_border, _)) = find_composer_box(screen) else {
        return QueuedMatch::default();
    };
    count_matching_rows_above(screen, top_border, submitted_payload)
}

/// Returns `Some(QueuedMatch)` only when Antigravity's interactive composer is visibly empty
/// AND a new `▸ ` queued row matching `submitted_payload` has appeared above the composer
/// (`count > baseline_matches`).
#[must_use]
pub fn queued_submission_confirmed(
    screen: &Screen,
    submitted_payload: &str,
    baseline_matches: usize,
) -> Option<QueuedMatch> {
    let (top_border, composer_empty) = find_composer_box(screen)?;
    if !composer_empty {
        return None;
    }
    let matched = count_matching_rows_above(screen, top_border, submitted_payload);
    (matched.count > baseline_matches).then_some(matched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Cursor;

    fn cells_from_str(line: &str, dim_after: Option<usize>) -> Vec<Cell> {
        let mut cells = Vec::new();
        for (idx, ch) in line.chars().enumerate() {
            let wide = !ch.is_ascii() && ch != '─' && ch != '▸' && ch != '│' && ch != '…';
            let mut flags = Flags::default();
            if wide {
                flags |= Flags::WIDE;
            }
            if dim_after.is_some_and(|start| idx >= start) && !ch.is_whitespace() {
                flags |= Flags::DIM;
            }
            cells.push(Cell {
                ch,
                flags,
                ..Cell::default()
            });
            if wide {
                cells.push(Cell {
                    ch: ' ',
                    flags: Flags::SPACER,
                    ..Cell::default()
                });
            }
        }
        cells
    }

    fn screen_from_lines(lines: &[(&str, Option<usize>)]) -> Screen {
        let rows: Vec<Vec<Cell>> = lines
            .iter()
            .map(|(line, dim_after)| cells_from_str(line, *dim_after))
            .collect();
        let lines_u16 = u16::try_from(rows.len()).unwrap_or(0);
        Screen {
            cols: 100,
            lines: lines_u16,
            rows,
            cursor: Cursor {
                line: lines_u16.saturating_sub(2),
                column: 2,
                visible: false,
            },
            ..Screen::default()
        }
    }

    const BORDER: &str = "────────────────────────────────────────────────────────────────";

    #[test]
    fn confirms_korean_single_line_queued_prompt_with_wide_spacers() {
        let screen = screen_from_lines(&[
            ("⣽  Running command...", None),
            (
                "▸ 다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라. (alt+m to send immediately)",
                None,
            ),
            (BORDER, None),
            ("> ", None),
            (BORDER, None),
            ("Gemini 4 Neon (XHigh) (1.0M) │ 144.2k/1.0M", None),
        ]);
        let got = queued_submission_confirmed(
            &screen,
            "다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.",
            0,
        );
        assert_eq!(
            got,
            Some(QueuedMatch {
                count: 1,
                send_immediately_hint: true,
            })
        );
    }

    #[test]
    fn confirms_narrow_truncated_and_multiline_queued_prompts() {
        let narrow = screen_from_lines(&[
            (
                "▸ 다 만들고 크릴 아티팩트로 한 눈... (alt+m to send immediately)",
                None,
            ),
            (BORDER, None),
            (">", None),
            (BORDER, None),
        ]);
        assert_eq!(
            queued_submission_confirmed(
                &narrow,
                "다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라. 아주 긴 문장입니다.",
                0,
            ),
            Some(QueuedMatch {
                count: 1,
                send_immediately_hint: true,
            })
        );

        let multiline_after_alt_m = screen_from_lines(&[
            ("▸ 둘째 줄 테스트 첫줄...", None),
            ("  ⎿  Sending to agent...", None),
            (BORDER, None),
            ("> Auto mode: edits & safe commands", Some(2)),
            (BORDER, None),
        ]);
        assert_eq!(
            queued_submission_confirmed(
                &multiline_after_alt_m,
                "둘째 줄 테스트 첫줄\n둘째 줄 테스트 둘째줄",
                0,
            ),
            Some(QueuedMatch {
                count: 1,
                send_immediately_hint: false,
            })
        );
    }

    #[test]
    fn rejects_unsubmitted_draft_still_in_composer() {
        let single_draft = screen_from_lines(&[
            ("⣽  Running command...", None),
            (BORDER, None),
            (
                "> 다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.",
                None,
            ),
            (BORDER, None),
        ]);
        assert_eq!(
            queued_submission_confirmed(
                &single_draft,
                "다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.",
                0,
            ),
            None
        );

        let multiline_draft_with_old_queue = screen_from_lines(&[
            ("▸ repeat prompt (alt+m to send immediately)", None),
            (BORDER, None),
            ("> repeat prompt", None),
            ("  second line", None),
            (BORDER, None),
        ]);
        assert_eq!(
            queued_submission_confirmed(&multiline_draft_with_old_queue, "repeat prompt", 0),
            None
        );
    }

    #[test]
    fn requires_new_queued_count_above_baseline_for_duplicate_prompts() {
        let one = screen_from_lines(&[
            ("▸ 계속해 (alt+m to send immediately)", None),
            (BORDER, None),
            ("> ", None),
            (BORDER, None),
        ]);
        let baseline = count_queued_matches(&one, "계속해").count;
        assert_eq!(baseline, 1);
        assert_eq!(queued_submission_confirmed(&one, "계속해", baseline), None);

        let two = screen_from_lines(&[
            ("▸ 계속해", None),
            ("▸ 계속해 (alt+m to send immediately)", None),
            (BORDER, None),
            ("> ", None),
            (BORDER, None),
        ]);
        assert_eq!(
            queued_submission_confirmed(&two, "계속해", baseline),
            Some(QueuedMatch {
                count: 2,
                send_immediately_hint: true,
            })
        );
    }

    #[test]
    fn ignores_unrelated_bullets_above_non_queue_output_and_trust_dialogs() {
        let history_bullet = screen_from_lines(&[
            (
                "▸ 다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.",
                None,
            ),
            (
                "Assistant response line separating history from composer",
                None,
            ),
            (BORDER, None),
            ("> ", None),
            (BORDER, None),
        ]);
        assert_eq!(
            queued_submission_confirmed(
                &history_bullet,
                "다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.",
                0,
            ),
            None
        );

        let trust_dialog = screen_from_lines(&[
            ("Do you trust the contents of this project?", None),
            ("> Yes, I trust this folder", None),
            ("  No, exit", None),
        ]);
        assert_eq!(
            queued_submission_confirmed(&trust_dialog, "Yes, I trust this folder", 0),
            None
        );
        assert!(!empty_composer_ready(&trust_dialog));
    }

    #[test]
    fn empty_composer_ready_accepts_software_cursor_placeholder_and_rejects_drafts() {
        let mut screen = screen_from_lines(&[
            ("⚠ Conversation already open", None),
            ("│ Introducing Gemini 4 Neon", None),
            ("│ esc to dismiss", None),
            (
                "└ Interrupted · What should Antigravity CLI do instead?",
                None,
            ),
            (BORDER, None),
            (
                "> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)",
                Some(3),
            ),
            (BORDER, None),
        ]);
        // Simulate Ink software block cursor (`INVERSE`) on the first placeholder character (`A`).
        screen.rows[5][2].flags |= Flags::INVERSE;
        assert!(empty_composer_ready(&screen));

        let mut single_char_draft =
            screen_from_lines(&[(BORDER, None), ("> x", None), (BORDER, None)]);
        single_char_draft.rows[1][2].flags |= Flags::INVERSE;
        assert!(!empty_composer_ready(&single_char_draft));

        let draft = screen_from_lines(&[(BORDER, None), ("> hello", None), (BORDER, None)]);
        assert!(!empty_composer_ready(&draft));

        let signing_in = screen_from_lines(&[("Signing in...", None)]);
        assert!(!empty_composer_ready(&signing_in));
    }

    #[test]
    fn detects_agy_stage1_and_stage2_auth_login_screens_and_blocks_composer_ready() {
        let stage1 = screen_from_lines(&[
            ("Antigravity CLI v1.5.17", None),
            (
                "Welcome to the Antigravity CLI. You are currently not signed in.",
                None,
            ),
            ("Select login method:", None),
            ("> 1. Google OAuth", None),
            ("  2. Use a Google Cloud project", None),
            ("↑/↓ Navigate · enter Select", None),
        ]);
        assert!(auth_login_screen(&stage1));
        assert!(!empty_composer_ready(&stage1));

        let stage2 = screen_from_lines(&[
            ("Logging in with Google...", None),
            ("Your browser should open automatically.", None),
            ("→ Click here to authenticate", None),
            ("Paste authorization code here:", None),
        ]);
        assert!(auth_login_screen(&stage2));
        assert!(!empty_composer_ready(&stage2));

        let legacy_banner = screen_from_lines(&[
            ("You are not logged into Antigravity.", None),
            ("Log in with Google", None),
        ]);
        assert!(auth_login_screen(&legacy_banner));

        let normal_chat = screen_from_lines(&[
            ("User: how does Google OAuth work?", None),
            ("Assistant: OAuth exchanges an authorization code.", None),
            (BORDER, None),
            ("> ", None),
            (BORDER, None),
        ]);
        assert!(!auth_login_screen(&normal_chat));
        assert!(empty_composer_ready(&normal_chat));
    }
}
