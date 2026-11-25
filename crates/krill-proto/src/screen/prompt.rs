//! Live terminal screen prompt inspection and fail-closed reply keystroke planning.
//!
//! Used by both the daemon (mobile RPC) and the desktop UI so screen-grounded
//! verification is shared in one place without altering postcard wire layouts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{Cell, Color, Flags};
use crate::agent_history::MAX_QUESTION_OPTIONS;

const MAX_OTHER_TEXT_BYTES: usize = 4096;
const TRAILING_WINDOW_ROWS: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptOption {
    pub number: Option<usize>,
    pub label: String,
    pub is_other: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptKind {
    YesNo {
        prompt: String,
    },
    NumberedMenu {
        prompt: String,
        options: Vec<PromptOption>,
        cursor: usize,
        checked: Vec<usize>,
        is_plan_approval: bool,
        allows_other: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptReply {
    YesNo(bool),
    Choice(usize),
    MultiSelect(Vec<usize>),
    Other { option_index: usize, text: String },
}

#[derive(Debug, Clone)]
struct DetectedMenu {
    kind: PromptKind,
    has_checkboxes: bool,
    cursor_found: bool,
}

#[derive(Debug, Clone)]
struct RowInfo {
    plain: String,
    stripped: String,
    has_highlight: bool,
    distinct_bg: Option<Color>,
}

/// Renders a single terminal row into trimmed plain text.
pub fn row_plain_text(cells: &[Cell]) -> String {
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

fn strip_box_chrome(line: &str) -> String {
    let trimmed = line.trim();
    let trimmed = trimmed
        .strip_prefix('│')
        .or_else(|| trimmed.strip_prefix('┃'))
        .unwrap_or(trimmed);
    let trimmed = trimmed
        .strip_suffix('│')
        .or_else(|| trimmed.strip_suffix('┃'))
        .unwrap_or(trimmed);
    trimmed.trim().to_owned()
}

fn is_box_or_rule(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars().all(|c| {
        matches!(
            c,
            '─' | '━'
                | '═'
                | '│'
                | '┃'
                | '╭'
                | '╮'
                | '╰'
                | '╯'
                | '┌'
                | '┐'
                | '└'
                | '┘'
                | '├'
                | '┤'
                | '┬'
                | '┴'
                | '┼'
                | '-'
                | '='
                | '_'
                | ' '
        )
    })
}

fn is_pointer_char(ch: char) -> bool {
    matches!(ch, '❯' | '›' | '▶' | '▸' | '>' | '→')
}

fn is_hint_footer_line(line: &str) -> bool {
    let stripped = strip_box_chrome(line);
    if stripped.is_empty() || is_box_or_rule(&stripped) {
        return true;
    }
    let lower = stripped.to_ascii_lowercase();
    if stripped.starts_with('>') && stripped.len() <= 2 {
        return true;
    }
    let hint_keywords = [
        "enter to select",
        "enter to confirm",
        "press enter",
        "to navigate",
        "to move",
        "to select",
        "to toggle",
        "to cancel",
        "esc to cancel",
        "esc to go back",
        "space to toggle",
        "ctrl+c",
        "ctrl-c",
        "tab to",
        "↑/↓",
        "↑↓",
    ];
    hint_keywords.iter().any(|k| lower.contains(k))
        || (lower.contains("enter") && (lower.contains("esc") || lower.contains("select")))
}

fn consume_checkbox(s: &str) -> (Option<bool>, &str) {
    let s = s.trim_start();
    for (prefix, checked) in [
        ("[ ]", false),
        ("[x]", true),
        ("[X]", true),
        ("[✓]", true),
        ("[✔]", true),
        ("☐", false),
        ("☑", true),
        ("☒", true),
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return (Some(checked), rest.trim_start());
        }
    }
    (None, s)
}

#[derive(Debug, Clone)]
struct ParsedOptionLine {
    number: usize,
    label: String,
    has_pointer: bool,
    checked: Option<bool>,
    is_other: bool,
}

fn is_other_label(label: &str) -> bool {
    let lower = label.trim().to_ascii_lowercase();
    lower == "other"
        || lower.starts_with("other.")
        || lower.starts_with("other…")
        || lower.starts_with("other...")
        || lower.starts_with("other (")
        || lower.starts_with("other:")
        || lower.starts_with("type something")
}

fn parse_numbered_option_line(raw: &str) -> Option<ParsedOptionLine> {
    let stripped = strip_box_chrome(raw);
    if stripped.is_empty() {
        return None;
    }
    let mut rest = stripped.as_str();
    let mut has_pointer = false;
    if let Some(first) = rest.chars().next()
        && is_pointer_char(first)
        && let Some(after) = rest.get(first.len_utf8()..)
        && after.starts_with(char::is_whitespace)
    {
        has_pointer = true;
        rest = after.trim_start();
    }

    let (cb_before, after_cb) = consume_checkbox(rest);
    rest = after_cb;

    let digits_end = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits_end == 0 {
        return None;
    }
    let number: usize = rest.get(..digits_end)?.parse().ok()?;
    if number == 0 || number > MAX_QUESTION_OPTIONS {
        return None;
    }
    let after_digits = rest.get(digits_end..)?;
    let after_punct = after_digits
        .strip_prefix('.')
        .or_else(|| after_digits.strip_prefix(')'))?;
    if !after_punct.starts_with(char::is_whitespace) {
        return None;
    }
    let after_punct = after_punct.trim_start();
    let (cb_after, after_cb2) = if cb_before.is_none() {
        consume_checkbox(after_punct)
    } else {
        (cb_before, after_punct)
    };
    let label = after_cb2.trim();
    if label.is_empty() {
        return None;
    }
    let is_other = is_other_label(label);
    Some(ParsedOptionLine {
        number,
        label: label.to_owned(),
        has_pointer,
        checked: cb_after,
        is_other,
    })
}

fn row_highlight_info(cells: &[Cell]) -> (bool, Option<Color>) {
    let mut inverse_count = 0usize;
    let mut bg_color: Option<Color> = None;
    let mut uniform_bg = true;
    let mut non_space_with_bg = 0usize;
    for cell in cells {
        if cell.ch == ' ' && cell.bg == Color::Default && !cell.flags.contains(Flags::INVERSE) {
            continue;
        }
        if cell.flags.contains(Flags::INVERSE) {
            inverse_count = inverse_count.saturating_add(1);
        }
        if cell.bg != Color::Default {
            non_space_with_bg = non_space_with_bg.saturating_add(1);
            match bg_color {
                None => bg_color = Some(cell.bg),
                Some(prev) if prev != cell.bg => uniform_bg = false,
                Some(_) => {}
            }
        }
    }
    let has_inverse = inverse_count >= 2;
    let distinct_bg = if uniform_bg && non_space_with_bg >= 3 {
        bg_color
    } else {
        None
    };
    (has_inverse, distinct_bg)
}

fn normalize_for_match(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = true;
    for ch in input.chars() {
        if matches!(ch, '│' | '┃' | '─' | '━' | '═' | '╭' | '╮' | '╰' | '╯') {
            continue;
        }
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            last_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn is_plan_approval_menu(prompt: &str, options: &[PromptOption]) -> bool {
    let prompt_lower = prompt.to_ascii_lowercase();
    if prompt_lower.contains("plan")
        && (prompt_lower.contains("proceed")
            || prompt_lower.contains("approve")
            || prompt_lower.contains("accept")
            || prompt_lower.contains("implement")
            || prompt_lower.contains("ready")
            || prompt_lower.contains("execute"))
    {
        return true;
    }
    options.iter().any(|opt| {
        let l = opt.label.to_ascii_lowercase();
        l.contains("keep planning")
            || l.contains("implement this plan")
            || l.contains("auto-accept edits")
            || l.contains("bypass permissions")
            || l.contains("approve plan")
            || l.contains("clear context and")
    })
}

fn extract_prompt_above(rows: &[RowInfo], first_option_idx: usize) -> String {
    let mut lines = Vec::new();
    let start = first_option_idx.saturating_sub(6);
    let mut blank_run = 0usize;
    let Some(slice) = rows.get(start..first_option_idx) else {
        return String::new();
    };
    for row in slice.iter().rev() {
        let s = &row.stripped;
        if s.is_empty() {
            blank_run = blank_run.saturating_add(1);
            if !lines.is_empty() || blank_run > 1 {
                break;
            }
            continue;
        }
        if is_box_or_rule(s) {
            if !lines.is_empty() {
                break;
            }
            continue;
        }
        blank_run = 0;
        if lines.len() >= 4 {
            break;
        }
        lines.push(s.clone());
    }
    lines.reverse();
    lines.join("\n")
}

fn detect_numbered_menu(rows: &[RowInfo]) -> Option<DetectedMenu> {
    let mut end_idx = rows.len();
    while let Some(prev) = end_idx.checked_sub(1) {
        let Some(r) = rows.get(prev) else {
            break;
        };
        if r.stripped.is_empty() || is_hint_footer_line(&r.plain) {
            end_idx = prev;
        } else {
            break;
        }
    }
    if end_idx == 0 {
        return None;
    }

    // The final non-hint row must be either the last numbered option or at most
    // one indented continuation description line belonging to that last option.
    let last_one = end_idx.checked_sub(1)?;
    let last_idx = if rows
        .get(last_one)
        .and_then(|r| parse_numbered_option_line(&r.plain))
        .is_some()
    {
        last_one
    } else if let Some(prev_two) = end_idx.checked_sub(2)
        && rows
            .get(last_one)
            .is_some_and(|r| r.plain.starts_with("  "))
        && rows
            .get(prev_two)
            .and_then(|r| parse_numbered_option_line(&r.plain))
            .is_some()
    {
        prev_two
    } else {
        return None;
    };
    let last_parsed = parse_numbered_option_line(&rows.get(last_idx)?.plain)?;
    if last_parsed.number < 2 {
        return None;
    }

    // Collect options backwards in descending sequential order down to 1.
    let mut expected_num = last_parsed.number;
    let mut collected: Vec<(usize, ParsedOptionLine)> = Vec::new();
    let mut cursor_idx = last_idx;
    let mut gap = 0usize;
    loop {
        let row = rows.get(cursor_idx)?;
        if let Some(parsed) = parse_numbered_option_line(&row.plain)
            && parsed.number == expected_num
        {
            collected.push((cursor_idx, parsed));
            gap = 0;
            if expected_num == 1 {
                break;
            }
            expected_num = expected_num.checked_sub(1)?;
        } else {
            let line = &row.stripped;
            if !line.is_empty() && !is_box_or_rule(line) {
                gap = gap.saturating_add(1);
                if gap > 2 {
                    return None;
                }
            }
        }
        cursor_idx = cursor_idx.checked_sub(1)?;
    }
    collected.reverse();
    if collected.len() < 2 || collected.len() > MAX_QUESTION_OPTIONS {
        return None;
    }

    let first_row_idx = collected.first()?.0;
    let mut pointer_idx = None;
    let mut inverse_idx = None;
    let mut bg_candidates: Vec<(usize, Color)> = Vec::new();
    let mut has_checkboxes = false;
    let mut checked = Vec::new();
    let mut options = Vec::with_capacity(collected.len());

    for (opt_i, (row_idx, parsed)) in collected.iter().enumerate() {
        let row = rows.get(*row_idx)?;
        if parsed.has_pointer && pointer_idx.is_none() {
            pointer_idx = Some(opt_i);
        }
        if row.has_highlight && inverse_idx.is_none() {
            inverse_idx = Some(opt_i);
        }
        if let Some(bg) = row.distinct_bg {
            bg_candidates.push((opt_i, bg));
        }
        if let Some(is_checked) = parsed.checked {
            has_checkboxes = true;
            if is_checked {
                checked.push(opt_i);
            }
        }
        options.push(PromptOption {
            number: Some(parsed.number),
            label: parsed.label.clone(),
            is_other: parsed.is_other,
        });
    }

    let unique_bg_idx = if bg_candidates.len() == 1 && options.len() > 1 {
        bg_candidates.first().map(|(i, _)| *i)
    } else {
        None
    };

    let (cursor, cursor_found) = match pointer_idx.or(inverse_idx).or(unique_bg_idx) {
        Some(idx) => (idx, true),
        None => (0, false),
    };
    if !cursor_found {
        return None;
    }

    let prompt = extract_prompt_above(rows, first_row_idx);
    let is_plan_approval = is_plan_approval_menu(&prompt, &options);
    let allows_other = options.iter().any(|opt| opt.is_other);

    Some(DetectedMenu {
        kind: PromptKind::NumberedMenu {
            prompt,
            options,
            cursor,
            checked,
            is_plan_approval,
            allows_other,
        },
        has_checkboxes,
        cursor_found,
    })
}

fn detect_yes_no(rows: &[RowInfo]) -> Option<PromptKind> {
    let non_empty: Vec<&RowInfo> = rows.iter().filter(|r| !r.stripped.is_empty()).collect();
    let tail_start = non_empty.len().saturating_sub(5);
    let tail = non_empty.get(tail_start..)?;
    for (offset, row) in tail.iter().enumerate() {
        let lower = row.stripped.to_ascii_lowercase();
        let has_hint = ["(y/n)", "[y/n]", "(y/n/a)", "[y/n/a]"]
            .iter()
            .any(|hint| lower.contains(hint));
        if !has_hint || row.stripped.chars().count() > 240 {
            continue;
        }
        // Fail closed if output or another prompt appeared beneath the y/n line.
        let after = tail_start.saturating_add(offset).saturating_add(1);
        let trailing = non_empty.get(after..).unwrap_or(&[]);
        if !trailing.iter().all(|r| is_hint_footer_line(&r.plain)) {
            continue;
        }
        return Some(PromptKind::YesNo {
            prompt: row.stripped.clone(),
        });
    }
    None
}

fn build_rows(rows: &[Vec<Cell>]) -> Vec<RowInfo> {
    let start = rows.len().saturating_sub(TRAILING_WINDOW_ROWS);
    rows.get(start..)
        .unwrap_or(rows)
        .iter()
        .map(|cells| {
            let plain = row_plain_text(cells);
            let stripped = strip_box_chrome(&plain);
            let (has_highlight, distinct_bg) = row_highlight_info(cells);
            RowInfo {
                plain,
                stripped,
                has_highlight,
                distinct_bg,
            }
        })
        .collect()
}

fn detect_internal(rows: &[Vec<Cell>]) -> Option<DetectedMenu> {
    let built = build_rows(rows);
    let plain_screen = built
        .iter()
        .map(|row| row.plain.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if super::agy::auth_login_text(&plain_screen) {
        return None;
    }
    if let Some(menu) = detect_numbered_menu(&built) {
        return Some(menu);
    }
    detect_yes_no(&built).map(|kind| DetectedMenu {
        kind,
        has_checkboxes: false,
        cursor_found: true,
    })
}

/// Inspects a live terminal screen for an active `(y/n)` or cursor-marked numbered prompt.
pub fn detect(rows: &[Vec<Cell>]) -> Option<PromptKind> {
    detect_internal(rows).map(|d| d.kind)
}

fn step_cursor(steps: &mut Vec<Vec<u8>>, cursor: &mut usize, target: usize) {
    while *cursor < target {
        steps.push(b"\x1b[B".to_vec());
        *cursor = cursor.saturating_add(1);
    }
    while *cursor > target {
        steps.push(b"\x1b[A".to_vec());
        *cursor = cursor.saturating_sub(1);
    }
}

fn encode_custom_text(text: &str, bracketed: bool) -> Result<Vec<u8>, &'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("custom reply cannot be empty");
    }
    if trimmed.len() > MAX_OTHER_TEXT_BYTES {
        return Err("custom reply exceeds length limit");
    }
    if trimmed.chars().any(char::is_control) {
        return Err("custom reply cannot contain control characters");
    }
    if bracketed {
        let mut out = Vec::with_capacity(trimmed.len().saturating_add(12));
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(trimmed.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        Ok(out)
    } else {
        Ok(trimmed.as_bytes().to_vec())
    }
}

fn expected_matches_screen(expected: &str, detected_prompt: &str, rows: &[Vec<Cell>]) -> bool {
    let expected_norm = normalize_for_match(expected);
    if expected_norm.chars().count() < 3 {
        return false;
    }
    let prompt_norm = normalize_for_match(detected_prompt);
    if !prompt_norm.is_empty()
        && (prompt_norm.contains(&expected_norm)
            || (prompt_norm.chars().count() >= 6 && expected_norm.contains(&prompt_norm)))
    {
        return true;
    }
    let start = rows.len().saturating_sub(TRAILING_WINDOW_ROWS);
    let screen_norm = normalize_for_match(
        &rows
            .get(start..)
            .unwrap_or(rows)
            .iter()
            .map(|r| row_plain_text(r))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    screen_norm.contains(&expected_norm)
}

/// Verifies the live screen still presents `expected_prompt` and plans deterministic keystrokes.
pub fn verify_and_plan(
    rows: &[Vec<Cell>],
    expected_prompt: &str,
    reply: &PromptReply,
    bracketed: bool,
) -> Result<Vec<Vec<u8>>, &'static str> {
    let detected = detect_internal(rows).ok_or("no active prompt detected on live screen")?;
    if !detected.cursor_found {
        return Err("live prompt cursor not detected");
    }

    let prompt_text = match &detected.kind {
        PromptKind::YesNo { prompt } | PromptKind::NumberedMenu { prompt, .. } => prompt,
    };
    if !expected_matches_screen(expected_prompt, prompt_text, rows) {
        return Err("live screen does not match expected prompt");
    }

    match (&detected.kind, reply) {
        (PromptKind::YesNo { .. }, PromptReply::YesNo(yes)) => Ok(vec![if *yes {
            b"y\r".to_vec()
        } else {
            b"n\r".to_vec()
        }]),
        (PromptKind::YesNo { .. }, _) => Err("reply does not match active yes/no prompt"),
        (
            PromptKind::NumberedMenu {
                options, cursor, ..
            },
            PromptReply::Choice(choice),
        ) => {
            if *choice >= options.len() {
                return Err("selected option index out of range");
            }
            if detected.has_checkboxes {
                return Err("checkbox menu requires multi-select reply");
            }
            let mut steps = Vec::new();
            let mut cur = *cursor;
            step_cursor(&mut steps, &mut cur, *choice);
            steps.push(b"\r".to_vec());
            Ok(steps)
        }
        (
            PromptKind::NumberedMenu {
                options,
                cursor,
                checked,
                ..
            },
            PromptReply::MultiSelect(selected),
        ) => {
            if !detected.has_checkboxes {
                return Err("live prompt is not a multi-select checkbox menu");
            }
            if selected.is_empty() || selected.iter().any(|&idx| idx >= options.len()) {
                return Err("multi-select indices invalid or empty");
            }
            let current: BTreeSet<usize> = checked.iter().copied().collect();
            let desired: BTreeSet<usize> = selected.iter().copied().collect();
            let mut steps = Vec::new();
            let mut cur = *cursor;
            for &target in current.symmetric_difference(&desired) {
                step_cursor(&mut steps, &mut cur, target);
                steps.push(b" ".to_vec());
            }
            steps.push(b"\r".to_vec());
            Ok(steps)
        }
        (
            PromptKind::NumberedMenu {
                options, cursor, ..
            },
            PromptReply::Other { option_index, text },
        ) => {
            let opt = options
                .get(*option_index)
                .ok_or("other option index out of range")?;
            if !opt.is_other {
                return Err("selected option is not a custom input option");
            }
            let encoded = encode_custom_text(text, bracketed)?;
            let mut steps = Vec::new();
            let mut cur = *cursor;
            step_cursor(&mut steps, &mut cur, *option_index);
            if detected.has_checkboxes {
                steps.push(b" ".to_vec());
            } else {
                steps.push(b"\r".to_vec());
            }
            steps.push(encoded);
            steps.push(b"\r".to_vec());
            Ok(steps)
        }
        (PromptKind::NumberedMenu { .. }, PromptReply::YesNo(_)) => {
            Err("numbered menu requires option choice")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_from_str(text: &str) -> Vec<Vec<Cell>> {
        text.lines()
            .map(|line| {
                line.chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn detects_and_plans_yes_no_prompt() {
        let rows = rows_from_str("Allow bash: cargo nextest run? (y/n)");
        let detected = detect(&rows).expect("should detect y/n");
        assert_eq!(
            detected,
            PromptKind::YesNo {
                prompt: "Allow bash: cargo nextest run? (y/n)".into()
            }
        );
        let yes = verify_and_plan(&rows, "cargo nextest run", &PromptReply::YesNo(true), false)
            .expect("yes should verify");
        assert_eq!(yes, vec![b"y\r".to_vec()]);
        let no = verify_and_plan(
            &rows,
            "cargo nextest run",
            &PromptReply::YesNo(false),
            false,
        )
        .expect("no should verify");
        assert_eq!(no, vec![b"n\r".to_vec()]);
    }

    #[test]
    fn detects_claude_numbered_permission_menu_and_plans_arrows() {
        let screen = "\
Allow Bash command: git status?
❯ 1. Yes
  2. Yes, allow reading from repo during this session
  3. No
Enter to select · Esc to cancel";
        let rows = rows_from_str(screen);
        let Some(PromptKind::NumberedMenu {
            prompt,
            options,
            cursor,
            checked,
            is_plan_approval,
            allows_other,
        }) = detect(&rows)
        else {
            panic!("expected numbered menu");
        };
        assert_eq!(prompt, "Allow Bash command: git status?");
        assert_eq!(options.len(), 3);
        assert_eq!(cursor, 0);
        assert!(checked.is_empty());
        assert!(!is_plan_approval);
        assert!(!allows_other);

        let steps = verify_and_plan(&rows, "git status", &PromptReply::Choice(1), false)
            .expect("choice 1 should plan down arrow + enter");
        assert_eq!(steps, vec![b"\x1b[B".to_vec(), b"\r".to_vec()]);
    }

    #[test]
    fn detects_codex_exit_plan_menu_and_inverse_cursor() {
        let screen = "\
Ready to implement this plan?
  1. Approve plan and start coding
› 2. Auto-accept edits for this plan
  3. Keep planning
Press enter to confirm";
        let rows = rows_from_str(screen);
        let Some(PromptKind::NumberedMenu {
            options,
            cursor,
            is_plan_approval,
            ..
        }) = detect(&rows)
        else {
            panic!("expected plan menu");
        };
        assert!(is_plan_approval);
        assert_eq!(cursor, 1);
        assert_eq!(
            options.get(2).map(|opt| opt.label.as_str()),
            Some("Keep planning")
        );

        let steps = verify_and_plan(&rows, "Ready to implement", &PromptReply::Choice(0), false)
            .expect("choice 0 from cursor 1");
        assert_eq!(steps, vec![b"\x1b[A".to_vec(), b"\r".to_vec()]);

        // Highlight via Flags::INVERSE without pointer char.
        let mut inv_rows = rows_from_str("Approve?\n  1. Yes\n  2. No");
        if let Some(row) = inv_rows.get_mut(2) {
            for cell in row {
                cell.flags |= Flags::INVERSE;
            }
        }
        let Some(PromptKind::NumberedMenu { cursor, .. }) = detect(&inv_rows) else {
            panic!("expected inverse-highlighted menu");
        };
        assert_eq!(cursor, 1);
    }

    #[test]
    fn detects_other_option_and_plans_custom_text() {
        let screen = "\
Which deployment environment?
  1. Staging
❯ 2. Production
  3. Other (type custom response)
Enter to select";
        let rows = rows_from_str(screen);
        let Some(PromptKind::NumberedMenu {
            allows_other,
            options,
            cursor,
            ..
        }) = detect(&rows)
        else {
            panic!("expected menu with other");
        };
        assert!(allows_other);
        assert!(options.get(2).is_some_and(|opt| opt.is_other));
        assert_eq!(cursor, 1);

        let steps = verify_and_plan(
            &rows,
            "deployment environment",
            &PromptReply::Other {
                option_index: 2,
                text: "Canary EU".into(),
            },
            true,
        )
        .expect("other reply should plan");
        assert_eq!(
            steps,
            vec![
                b"\x1b[B".to_vec(),
                b"\r".to_vec(),
                b"\x1b[200~Canary EU\x1b[201~".to_vec(),
                b"\r".to_vec(),
            ]
        );

        let rejected = verify_and_plan(
            &rows,
            "deployment environment",
            &PromptReply::Other {
                option_index: 2,
                text: "bad\nline".into(),
            },
            false,
        );
        assert!(rejected.is_err());
    }

    #[test]
    fn detects_multiselect_checkbox_diff() {
        let screen = "\
Pick targets:
❯ 1. [x] macOS
  2. [ ] Windows
  3. [✓] Linux
Space to toggle · Enter to submit";
        let rows = rows_from_str(screen);
        let Some(PromptKind::NumberedMenu {
            checked, cursor, ..
        }) = detect(&rows)
        else {
            panic!("expected checkbox menu");
        };
        assert_eq!(cursor, 0);
        assert_eq!(checked, vec![0, 2]);

        let steps = verify_and_plan(
            &rows,
            "Pick targets",
            &PromptReply::MultiSelect(vec![1, 2]),
            false,
        )
        .expect("should toggle symmetric diff");
        assert_eq!(
            steps,
            vec![
                b" ".to_vec(),
                b"\x1b[B".to_vec(),
                b" ".to_vec(),
                b"\r".to_vec(),
            ]
        );
    }

    #[test]
    fn fails_closed_on_markdown_lists_stale_scrollback_and_prompt_mismatch() {
        let md_list = rows_from_str("Steps:\n  1. Build crate\n  2. Run tests");
        assert!(detect(&md_list).is_none());

        let stale_menu = rows_from_str("Allow?\n❯ 1. Yes\n  2. No\n$ cargo build\nFinished");
        assert!(detect(&stale_menu).is_none());

        let active = rows_from_str("Allow running tests?\n❯ 1. Yes\n  2. No");
        let mismatch = verify_and_plan(&active, "unrelated prompt", &PromptReply::Choice(0), false);
        assert!(mismatch.is_err());
    }

    #[test]
    fn detects_and_plans_wide_cjk_and_zerowidth_prompts() {
        let wide_rows = |text: &str| -> Vec<Vec<Cell>> {
            text.lines()
                .map(|line| {
                    let mut cells: Vec<Cell> = Vec::new();
                    for ch in line.chars() {
                        if ch == '\u{0301}' {
                            if let Some(prev) = cells.last_mut() {
                                prev.zerowidth.push(ch);
                            }
                            continue;
                        }
                        let wide = ('\u{AC00}'..='\u{D7AF}').contains(&ch);
                        cells.push(Cell {
                            ch,
                            flags: if wide { Flags::WIDE } else { Flags::default() },
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
                })
                .collect()
        };

        let row = wide_rows("cafe\u{0301} 배포할까요?");
        assert_eq!(row_plain_text(&row[0]), "cafe\u{0301} 배포할까요?");

        let screen = "\
배포할까요?
❯ 1. 예 (cafe\u{0301})
  2. 아니오
Enter to select";
        let rows = wide_rows(screen);
        let Some(PromptKind::NumberedMenu {
            prompt,
            options,
            cursor,
            ..
        }) = detect(&rows)
        else {
            panic!("expected wide-char numbered menu");
        };
        assert_eq!(prompt, "배포할까요?");
        assert_eq!(cursor, 0);
        assert_eq!(options[0].label, "예 (cafe\u{0301})");
        assert_eq!(options[1].label, "아니오");

        let steps = verify_and_plan(&rows, "배포할까요?", &PromptReply::Choice(1), false)
            .expect("wide-char prompt should match without spurious spaces");
        assert_eq!(steps, vec![b"\x1b[B".to_vec(), b"\r".to_vec()]);
    }

    #[test]
    fn ignores_agy_oauth_login_method_selector() {
        let stage1 = rows_from_str(
            "Welcome to the Antigravity CLI. You are currently not signed in.\n\
             Select login method:\n\
             > 1. Google OAuth\n\
               2. Use a Google Cloud project\n\
             ↑/↓ Navigate · enter Select",
        );
        assert!(detect(&stage1).is_none());

        let stage2 = rows_from_str(
            "Your browser should open automatically.\n\
             → Click here to authenticate\n\
             Paste authorization code here:",
        );
        assert!(detect(&stage2).is_none());
    }
}
