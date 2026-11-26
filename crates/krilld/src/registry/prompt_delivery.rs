//! Reliable automated prompt delivery for scheduled automations and bots.
//!
//! Automated senders (`schedule.rs`, `bots/runtime.rs`) cannot assume a newly spawned TUI agent is
//! ready merely because the PTY emitted initial setup bytes and paused for 150ms (Windows `ConPTY`
//! emits setup sequences immediately while Claude Code / Codex are still booting). If the prompt
//! and `\r` are written into the PTY input buffer before the TUI mounts, the TUI reads them in one
//! chunk as a paste burst and leaves the prompt sitting unsubmitted in the composer.
//!
//! This module provides two coordinated safeguards:
//! 1. [`wait_for_agent_ready`]: waits for an empty structured composer on Claude / Codex / Agy
//!    (refusing auth/trust/lock blockers) before writing the initial prompt.
//! 2. [`confirm_submit_or_nudge`]: after [`Registry::deliver_input_to_tap`], verifies whether the
//!    submitted prompt remained trapped as a matching composer draft and safely re-sends `\r` via
//!    `write_input_if_untouched` (aborting immediately on user input, blockers, or working state).

use std::io;
use std::time::{Duration, Instant};

use krill_proto::screen::{Cell, Color, Flags, Screen};

use super::Registry;
use crate::session::Tap;

const MAX_COMPOSER_INTERIOR_ROWS: usize = 24;
const MAX_CODEX_SCAN_ROWS: usize = 28;
const MAX_SUBMIT_NUDGES: usize = 3;
const CODEX_WRITER_LOCKED: &str = "This conversation is open in another app";

#[cfg(not(test))]
const STRUCTURED_READY_QUIET_MS: u64 = 200;
#[cfg(test)]
const STRUCTURED_READY_QUIET_MS: u64 = 30;

#[cfg(not(test))]
const FALLBACK_READY_QUIET_MS: u64 = 250;
#[cfg(test)]
const FALLBACK_READY_QUIET_MS: u64 = 40;

#[cfg(not(test))]
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(test)]
const READY_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(not(test))]
const EMPTY_CONFIRM_MIN: Duration = Duration::from_millis(120);
#[cfg(test)]
const EMPTY_CONFIRM_MIN: Duration = Duration::from_millis(20);

#[cfg(not(test))]
const NUDGE_SETTLE_MIN: Duration = Duration::from_millis(550);
#[cfg(test)]
const NUDGE_SETTLE_MIN: Duration = Duration::from_millis(80);

#[cfg(not(test))]
const NUDGE_QUIET_MS: u64 = 300;
#[cfg(test)]
const NUDGE_QUIET_MS: u64 = 40;

#[cfg(not(test))]
const NUDGE_ROUND_TIMEOUT: Duration = Duration::from_millis(1_800);
#[cfg(test)]
const NUDGE_ROUND_TIMEOUT: Duration = Duration::from_millis(400);

#[cfg(not(test))]
const NOT_VISIBLE_GRACE: Duration = Duration::from_millis(350);
#[cfg(test)]
const NOT_VISIBLE_GRACE: Duration = Duration::from_millis(60);

#[cfg(not(test))]
const CONFIRM_POLL_INTERVAL: Duration = Duration::from_millis(40);
#[cfg(test)]
const CONFIRM_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerState {
    /// No structured composer is visible (still booting, mid-scroll, or blocked by a modal/prompt).
    NotVisible,
    /// Structured composer is visible and empty (blank or placeholder-only).
    Empty,
    /// Structured composer is visible and contains non-empty draft text.
    HasDraft(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadyWaitOutcome {
    /// Interactive composer (or non-structured PTY output) settled and is ready for input.
    Ready,
    /// Timed out waiting for readiness.
    TimedOut,
    /// PTY closed before readiness.
    Closed,
    /// Antigravity OAuth/sign-in screen detected; caller must abort without injecting input.
    AuthRequired,
}

#[must_use]
pub(crate) fn has_structured_composer(canonical_agent: &str) -> bool {
    matches!(canonical_agent, "claude" | "codex" | "agy")
}

#[must_use]
pub(crate) fn screen_plain_text(screen: &Screen) -> String {
    let mut out = String::new();
    for (idx, row) in screen.rows.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&row_text(row));
    }
    out
}

#[must_use]
pub(crate) fn blocking_or_loading_screen(canonical_agent: &str, text: &str) -> bool {
    const BLOCKERS: &[&str] = &[
        "signing in...",
        "sign in to ",
        "select login method",
        "do you trust the contents",
        "do you trust the files",
        "yes, trust this folder",
        "yes, i trust this folder",
        "press enter to confirm",
        "esc to cancel",
        "do you want to proceed",
        "allow once",
        "allow for this session",
        "yes, allow",
        "enter to select",
    ];
    if krill_proto::screen::agy::auth_login_text(text) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    if canonical_agent == "codex"
        && (text.contains(CODEX_WRITER_LOCKED)
            || lower.contains("close it there and press r to continue here"))
    {
        return true;
    }
    BLOCKERS.iter().any(|needle| lower.contains(needle))
}

#[must_use]
pub(crate) fn composer_state(canonical_agent: &str, screen: &Screen) -> ComposerState {
    if krill_proto::screen::agy::auth_login_screen(screen) {
        return ComposerState::NotVisible;
    }
    let plain = screen_plain_text(screen);
    if blocking_or_loading_screen(canonical_agent, &plain) {
        return ComposerState::NotVisible;
    }
    match canonical_agent {
        "claude" => claude_composer_state(screen),
        "codex" => codex_composer_state(screen),
        "agy" => agy_composer_state(screen),
        _ => ComposerState::NotVisible,
    }
}

/// Verifies that the visible composer draft corresponds to the prompt we just delivered, preventing
/// accidental Enter nudges on unrelated drafts or unrecognized TUI widgets.
#[must_use]
pub(crate) fn draft_matches_submitted_prompt(draft: &str, prompt: &str) -> bool {
    let draft_norm = normalize_ws(draft);
    if draft_norm.is_empty() {
        return false;
    }
    let draft_lower = draft_norm.to_ascii_lowercase();
    if draft_lower.contains("[pasted text")
        || draft_lower.contains("[pasted content")
        || draft_lower.contains("[image")
    {
        return true;
    }
    let first_line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(normalize_ws)
        .unwrap_or_default();
    if first_line.is_empty() {
        return false;
    }
    let full_norm = normalize_ws(prompt);
    if draft_norm == first_line || draft_norm == full_norm {
        return true;
    }
    let probe: String = first_line.chars().take(12).collect();
    if !probe.is_empty() && draft_norm.contains(&probe) {
        return true;
    }
    let draft_chars = draft_norm.chars().count();
    let min_prefix = if first_line.chars().count() >= 4 {
        3
    } else {
        1
    };
    if draft_chars >= min_prefix {
        let trimmed = draft_norm
            .strip_suffix("...")
            .or_else(|| draft_norm.strip_suffix('…'))
            .map_or(draft_norm.as_str(), str::trim_end);
        if trimmed.chars().count() >= min_prefix
            && (first_line.starts_with(trimmed) || full_norm.starts_with(trimmed))
        {
            return true;
        }
    }
    false
}

/// Waits for a newly spawned agent PTY to reach a ready prompt before delivering initial input.
///
/// For structured TUI agents (`claude`, `codex`, `agy`), requires their interactive empty composer
/// to appear and settle instead of firing on the first 150ms pause after PTY startup bytes.
pub(crate) fn wait_for_agent_ready(
    tap: &Tap,
    canonical_agent: Option<&str>,
    timeout: Duration,
) -> ReadyWaitOutcome {
    let canonical = canonical_agent.unwrap_or("");
    let structured = has_structured_composer(canonical);
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        let pulse = tap.pulse();
        if pulse.closed {
            return ReadyWaitOutcome::Closed;
        }
        let screen = tap.capture().0;
        if krill_proto::screen::agy::auth_login_screen(&screen) {
            return ReadyWaitOutcome::AuthRequired;
        }
        if pulse.end > 0 {
            let text = screen_plain_text(&screen);
            if !blocking_or_loading_screen(canonical, &text) {
                if structured {
                    if tap.quiet_ms() >= STRUCTURED_READY_QUIET_MS
                        && composer_state(canonical, &screen) == ComposerState::Empty
                    {
                        return ReadyWaitOutcome::Ready;
                    }
                } else if tap.quiet_ms() >= FALLBACK_READY_QUIET_MS {
                    return ReadyWaitOutcome::Ready;
                }
            }
        }
        std::thread::sleep(READY_POLL_INTERVAL);
    }

    if tap.pulse().closed {
        return ReadyWaitOutcome::Closed;
    }
    if krill_proto::screen::agy::auth_login_screen(&tap.capture().0) {
        return ReadyWaitOutcome::AuthRequired;
    }
    ReadyWaitOutcome::TimedOut
}

/// Delivers `bytes` with submit (`\r`) and then verifies submission on structured TUI agents,
/// safely nudging `\r` if the prompt remains stuck in the composer input box.
pub(crate) fn deliver_and_confirm_submit(
    tap: &Tap,
    canonical_agent: Option<&str>,
    bytes: &[u8],
    is_working: &dyn Fn() -> bool,
) -> io::Result<()> {
    let canonical = canonical_agent.unwrap_or("");
    let prompt_str = std::str::from_utf8(bytes).ok();
    let agy_baseline = if canonical == "agy"
        && let Some(prompt) = prompt_str
    {
        krill_proto::screen::agy::count_queued_matches(&tap.capture().0, prompt).count
    } else {
        0
    };

    Registry::deliver_input_to_tap(tap, bytes, true)?;
    let after_ns = tap.last_input_ns();

    if let Some(prompt) = prompt_str {
        let _ = confirm_submit_or_nudge(
            tap,
            canonical_agent,
            prompt,
            after_ns,
            agy_baseline,
            is_working,
        );
    }
    Ok(())
}

/// Observes a structured TUI composer after an automated `submit=true` delivery and re-sends `\r`
/// (up to [`MAX_SUBMIT_NUDGES`] times) only when positive evidence shows the just-sent prompt is
/// still sitting unsubmitted in the composer and no other input has occurred.
///
/// Returns the number of Enter nudges sent.
pub(crate) fn confirm_submit_or_nudge(
    tap: &Tap,
    canonical_agent: Option<&str>,
    prompt: &str,
    mut expected_input_ns: u64,
    agy_baseline: usize,
    is_working: &dyn Fn() -> bool,
) -> usize {
    let canonical = canonical_agent.unwrap_or("");
    if !has_structured_composer(canonical) || prompt.trim().is_empty() {
        return 0;
    }

    let mut nudges = 0_usize;
    for attempt in 1..=MAX_SUBMIT_NUDGES {
        let round_start = Instant::now();
        let mut saw_draft = false;

        loop {
            if tap.pulse().closed {
                return nudges;
            }
            if tap.last_input_ns() != expected_input_ns {
                return nudges;
            }
            if is_working() {
                return nudges;
            }

            let screen = tap.capture().0;
            if krill_proto::screen::agy::auth_login_screen(&screen) {
                return nudges;
            }
            let text = screen_plain_text(&screen);
            if blocking_or_loading_screen(canonical, &text) {
                return nudges;
            }
            if canonical == "agy"
                && krill_proto::screen::agy::queued_submission_confirmed(
                    &screen,
                    prompt,
                    agy_baseline,
                )
                .is_some()
            {
                return nudges;
            }

            match composer_state(canonical, &screen) {
                ComposerState::Empty => {
                    if saw_draft || round_start.elapsed() >= EMPTY_CONFIRM_MIN {
                        return nudges;
                    }
                }
                ComposerState::HasDraft(ref draft) => {
                    if !draft_matches_submitted_prompt(draft, prompt) {
                        return nudges;
                    }
                    saw_draft = true;
                    if round_start.elapsed() >= NUDGE_SETTLE_MIN && tap.quiet_ms() >= NUDGE_QUIET_MS
                    {
                        match tap.write_input_if_untouched(b"\r", expected_input_ns) {
                            Ok(Some(next_ns)) => {
                                nudges = nudges.saturating_add(1);
                                expected_input_ns = next_ns;
                                tracing::info!(
                                    agent = canonical,
                                    attempt,
                                    "re-sent Enter after automated prompt remained in composer draft"
                                );
                                break;
                            }
                            Ok(None) | Err(_) => return nudges,
                        }
                    }
                }
                ComposerState::NotVisible => {
                    if !saw_draft && round_start.elapsed() >= NOT_VISIBLE_GRACE {
                        return nudges;
                    }
                }
            }

            if round_start.elapsed() >= NUDGE_ROUND_TIMEOUT {
                return nudges;
            }
            std::thread::sleep(CONFIRM_POLL_INTERVAL);
        }
    }

    nudges
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

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_border_row(cells: &[Cell]) -> bool {
    let text = row_text(cells);
    text.chars().count() >= 20 && text.chars().all(|c| c == '─')
}

fn is_empty_prompt_tail(tail: &[&Cell], prompt_fg: Color) -> bool {
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

fn is_empty_prefixed_row(cells: &[Cell], glyph: char, allow_nbsp: bool) -> bool {
    let mut visible = cells.iter().filter(|c| !c.flags.contains(Flags::SPACER));
    let Some(first) = visible.next() else {
        return false;
    };
    if first.ch != glyph || !first.zerowidth.is_empty() {
        return false;
    }
    let prompt_fg = first.fg;
    if let Some(second) = visible.next() {
        let valid_sep = second.zerowidth.is_empty()
            && (second.ch == ' ' || (allow_nbsp && second.ch == '\u{a0}'));
        if !valid_sep {
            return false;
        }
    }
    let tail: Vec<&Cell> = visible.collect();
    is_empty_prompt_tail(&tail, prompt_fg)
}

fn claude_composer_state(screen: &Screen) -> ComposerState {
    let rows = &screen.rows;
    if rows.len() < 3 {
        return ComposerState::NotVisible;
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
            let first_text = row_text(interior);
            if first_text.starts_with('❯') {
                if bottom == top.saturating_add(2) && is_empty_prefixed_row(interior, '❯', true) {
                    return ComposerState::Empty;
                }
                let mut lines = Vec::new();
                for y in interior_idx..bottom {
                    let Some(cells) = rows.get(y) else {
                        break;
                    };
                    let raw = row_text(cells);
                    let line = if y == interior_idx
                        && let Some(rest) = raw.strip_prefix('❯')
                    {
                        rest.trim_start_matches([' ', '\u{a0}']).to_owned()
                    } else {
                        raw
                    };
                    lines.push(line);
                }
                let draft = lines.join("\n").trim().to_owned();
                return if draft.is_empty() {
                    ComposerState::Empty
                } else {
                    ComposerState::HasDraft(draft)
                };
            }
            break;
        }
    }
    ComposerState::NotVisible
}

fn is_codex_footer_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    (line.starts_with("  ")
        && (trimmed.contains('·')
            || lower.contains("% left")
            || lower.contains("context left")
            || lower.contains("shift+tab")))
        || lower.starts_with("? for shortcuts")
}

fn codex_composer_state(screen: &Screen) -> ComposerState {
    let rows = &screen.rows;
    let scan_floor = rows.len().saturating_sub(MAX_CODEX_SCAN_ROWS);
    for y in (scan_floor..rows.len()).rev() {
        let Some(cells) = rows.get(y) else {
            continue;
        };
        let text = row_text(cells);
        if text.is_empty() {
            continue;
        }
        if text.starts_with('›') {
            if is_empty_prefixed_row(cells, '›', false) {
                let has_continuation = rows
                    .get(y.saturating_add(1)..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|row| !is_codex_footer_line(&row_text(row)))
                    .any(|row| !row_text(row).trim().is_empty());
                if !has_continuation {
                    return ComposerState::Empty;
                }
            }
            let mut lines = Vec::new();
            let first = text
                .strip_prefix('›')
                .map_or("", str::trim_start)
                .to_owned();
            lines.push(first);
            for next_row in rows.get(y.saturating_add(1)..).unwrap_or_default() {
                let line = row_text(next_row);
                if is_codex_footer_line(&line) {
                    break;
                }
                lines.push(line);
            }
            let draft = lines.join("\n").trim().to_owned();
            return if draft.is_empty() {
                ComposerState::Empty
            } else {
                ComposerState::HasDraft(draft)
            };
        }
    }
    ComposerState::NotVisible
}

fn agy_composer_state(screen: &Screen) -> ComposerState {
    if crate::mobile::agy_composer_ready(screen)
        || krill_proto::screen::agy::empty_composer_ready(screen)
    {
        return ComposerState::Empty;
    }
    match krill_proto::screen::agy::composer_draft_text(screen) {
        Some(draft) if draft.trim().is_empty() => ComposerState::Empty,
        Some(draft) => ComposerState::HasDraft(draft),
        None => ComposerState::NotVisible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::screen::Cursor;

    const BORDER: &str = "────────────────────────────────────────────────────────────────";

    fn cells_from_str(line: &str, dim_after: Option<usize>) -> Vec<Cell> {
        let mut cells = Vec::new();
        for (idx, ch) in line.chars().enumerate() {
            let mut flags = Flags::default();
            if dim_after.is_some_and(|start| idx >= start) && !ch.is_whitespace() {
                flags |= Flags::DIM;
            }
            cells.push(Cell {
                ch,
                flags,
                ..Cell::default()
            });
        }
        cells
    }

    fn screen_from_lines(lines: &[(&str, Option<usize>)]) -> Screen {
        let rows: Vec<Vec<Cell>> = lines
            .iter()
            .map(|(line, dim)| cells_from_str(line, *dim))
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

    #[test]
    fn detects_claude_codex_agy_composer_states_and_matches_drafts() {
        let claude_empty = screen_from_lines(&[
            (BORDER, None),
            ("❯\u{a0}Try \"fix the build\"", Some(2)),
            (BORDER, None),
            ("  bypass permissions on", None),
        ]);
        assert_eq!(
            composer_state("claude", &claude_empty),
            ComposerState::Empty
        );

        let claude_korean_draft = screen_from_lines(&[
            (BORDER, None),
            ("❯\u{a0}오늘 오류검수 확인해줘", None),
            (BORDER, None),
            ("  bypass permissions on", None),
        ]);
        assert_eq!(
            composer_state("claude", &claude_korean_draft),
            ComposerState::HasDraft("오늘 오류검수 확인해줘".to_owned())
        );
        assert!(draft_matches_submitted_prompt(
            "오늘 오류검수 확인해줘",
            "오늘 오류검수 확인해줘\n상세 리포트 작성"
        ));
        assert!(!draft_matches_submitted_prompt(
            "완전히 다른 문장",
            "오늘 오류검수 확인해줘"
        ));

        let claude_pasted = screen_from_lines(&[
            (BORDER, None),
            ("❯\u{a0}[Pasted text #1 +12 lines]", None),
            (BORDER, None),
        ]);
        assert_eq!(
            composer_state("claude", &claude_pasted),
            ComposerState::HasDraft("[Pasted text #1 +12 lines]".to_owned())
        );
        assert!(draft_matches_submitted_prompt(
            "[Pasted text #1 +12 lines]",
            "any long prompt"
        ));

        // Permission modal must never look like an actionable composer.
        let claude_permission = screen_from_lines(&[
            ("Do you want to proceed?", None),
            (BORDER, None),
            ("❯ 1. Yes", None),
            (BORDER, None),
            ("Esc to cancel", None),
        ]);
        assert_eq!(
            composer_state("claude", &claude_permission),
            ComposerState::NotVisible
        );

        let codex_empty = screen_from_lines(&[
            ("› Ask Codex to do anything", Some(2)),
            ("", None),
            ("  gpt-5 · repo · 90% left", None),
        ]);
        assert_eq!(composer_state("codex", &codex_empty), ComposerState::Empty);

        let codex_draft = screen_from_lines(&[
            ("› daily error audit check", None),
            ("  line two", None),
            ("", None),
            ("  gpt-5 · repo · 90% left", None),
        ]);
        assert_eq!(
            composer_state("codex", &codex_draft),
            ComposerState::HasDraft("daily error audit check\n  line two".to_owned())
        );
        assert!(draft_matches_submitted_prompt(
            "daily error audit check\n  line two",
            "daily error audit check"
        ));

        let agy_draft = screen_from_lines(&[
            (BORDER, None),
            ("> check scheduled errors", None),
            (BORDER, None),
        ]);
        assert_eq!(
            composer_state("agy", &agy_draft),
            ComposerState::HasDraft("check scheduled errors".to_owned())
        );

        let agy_auth = screen_from_lines(&[
            (
                "Welcome to the Antigravity CLI. You are currently not signed in.",
                None,
            ),
            (BORDER, None),
            ("> 1. Google OAuth", None),
            (BORDER, None),
        ]);
        assert_eq!(composer_state("agy", &agy_auth), ComposerState::NotVisible);
    }

    #[cfg(unix)]
    #[test]
    fn wait_for_agent_ready_waits_for_claude_composer_after_startup_banner_pause() -> io::Result<()>
    {
        use std::io::Write as _;
        use std::os::fd::OwnedFd;
        use std::path::PathBuf;
        use std::sync::Arc;

        use krill_proto::SessionId;
        use krill_proto::holder::WinSize;
        use krill_proto::os::net::UnixStream;

        use crate::{Appearance, Session};

        let look = Arc::new(Appearance::load(PathBuf::new()));
        let (master, mut terminal) = UnixStream::pair()?;
        let session = Session::start(
            SessionId(9101),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            look,
            None,
        )?;
        let tap = session.tap();

        let started = Instant::now();
        let booter = std::thread::spawn(move || -> io::Result<()> {
            // Simulate ConPTY/Claude startup bytes followed by a 250ms MCP loading pause
            // before the interactive `❯` composer mounts.
            terminal
                .write_all(b"\x1b[2J\x1b[HClaude Code v2.1.314\r\nLoading MCP servers...\r\n")?;
            std::thread::sleep(Duration::from_millis(250));
            let composer = format!("{BORDER}\r\n\u{276f} \r\n{BORDER}\r\n");
            terminal.write_all(composer.as_bytes())?;
            std::thread::sleep(Duration::from_millis(200));
            Ok(())
        });

        let outcome = wait_for_agent_ready(&tap, Some("claude"), Duration::from_secs(2));
        let elapsed = started.elapsed();
        booter.join().unwrap()?;
        assert_eq!(outcome, ReadyWaitOutcome::Ready);
        assert!(
            elapsed >= Duration::from_millis(240),
            "must wait until Claude composer renders instead of firing during startup pause, got {elapsed:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn deliver_and_confirm_submit_nudges_enter_when_claude_keeps_draft_and_stops_once_cleared()
    -> io::Result<()> {
        use std::io::{Read as _, Write as _};
        use std::os::fd::OwnedFd;
        use std::path::PathBuf;
        use std::sync::Arc;

        use krill_proto::SessionId;
        use krill_proto::holder::WinSize;
        use krill_proto::os::net::UnixStream;

        use crate::{Appearance, Session};

        let look = Arc::new(Appearance::load(PathBuf::new()));
        let (master, mut terminal) = UnixStream::pair()?;
        terminal.set_read_timeout(Some(Duration::from_secs(3)))?;
        let session = Session::start(
            SessionId(9102),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            look,
            None,
        )?;
        let tap = session.tap();

        let empty_box = format!("\x1b[2J\x1b[H{BORDER}\r\n\u{276f} \r\n{BORDER}\r\n");
        terminal.write_all(empty_box.as_bytes())?;
        tap.wait_timeout(Duration::from_secs(1), |p| p.end > 0);
        std::thread::sleep(Duration::from_millis(40));

        let tui = std::thread::spawn(move || -> io::Result<usize> {
            let mut buf = [0_u8; 512];
            // 1. Receive pasted prompt text and render it inside the composer.
            let n1 = terminal.read(&mut buf)?;
            let text = String::from_utf8_lossy(&buf[..n1]);
            assert!(text.contains("daily error check"), "got {text:?}");
            let draft_box =
                format!("\x1b[2J\x1b[H{BORDER}\r\n\u{276f} daily error check\r\n{BORDER}\r\n");
            terminal.write_all(draft_box.as_bytes())?;

            // 2. Receive first Enter (`\r`) and simulate the bug where TUI swallows it, leaving
            //    the prompt stuck in the input box.
            let n2 = terminal.read(&mut buf)?;
            assert_eq!(&buf[..n2], b"\r");

            // 3. Receive the automatic nudge Enter (`\r`) and clear the composer as submitted.
            let n3 = terminal.read(&mut buf)?;
            assert_eq!(&buf[..n3], b"\r");
            let cleared_box = format!(
                "\x1b[2J\x1b[H> daily error check\r\nWorking...\r\n{BORDER}\r\n\u{276f} \r\n{BORDER}\r\n"
            );
            terminal.write_all(cleared_box.as_bytes())?;

            // 4. Confirm no extra Enter is sent after submission succeeds.
            terminal.set_read_timeout(Some(Duration::from_millis(200)))?;
            let extra = match terminal.read(&mut buf) {
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    0
                }
                Err(e) => return Err(e),
            };
            Ok(extra)
        });

        deliver_and_confirm_submit(&tap, Some("claude"), b"daily error check", &|| false)?;
        let extra = tui.join().unwrap()?;
        assert_eq!(extra, 0, "must not send extra Enter after composer clears");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn confirm_submit_or_nudge_aborts_on_user_input_or_permission_screen() -> io::Result<()> {
        use std::io::Write as _;
        use std::os::fd::OwnedFd;
        use std::path::PathBuf;
        use std::sync::Arc;

        use krill_proto::SessionId;
        use krill_proto::holder::WinSize;
        use krill_proto::os::net::UnixStream;

        use crate::{Appearance, Session};

        let look = Arc::new(Appearance::load(PathBuf::new()));
        let (master, mut terminal) = UnixStream::pair()?;
        let session = Session::start(
            SessionId(9103),
            OwnedFd::from(master),
            WinSize { cols: 80, rows: 24 },
            look,
            None,
        )?;
        let tap = session.tap();

        // Permission modal with `❯` must trigger zero nudges.
        let modal = format!(
            "\x1b[2J\x1b[HDo you want to proceed?\r\n{BORDER}\r\n\u{276f} daily error check\r\n{BORDER}\r\nEsc to cancel\r\n"
        );
        terminal.write_all(modal.as_bytes())?;
        tap.wait_timeout(Duration::from_secs(1), |p| p.end > 0);
        let stamp = tap.last_input_ns();
        let nudges =
            confirm_submit_or_nudge(&tap, Some("claude"), "daily error check", stamp, 0, &|| {
                false
            });
        assert_eq!(nudges, 0);

        // Newer user input stamp must also trigger zero nudges.
        let draft = format!("\x1b[2J\x1b[H{BORDER}\r\n\u{276f} daily error check\r\n{BORDER}\r\n");
        terminal.write_all(draft.as_bytes())?;
        tap.wait_timeout(Duration::from_secs(1), |p| p.end > 50);
        let stale_stamp = tap.last_input_ns();
        tap.write_input(b"x", None)?;
        let nudges_after_user = confirm_submit_or_nudge(
            &tap,
            Some("claude"),
            "daily error check",
            stale_stamp,
            0,
            &|| false,
        );
        assert_eq!(nudges_after_user, 0);
        Ok(())
    }
}
