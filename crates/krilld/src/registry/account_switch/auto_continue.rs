//! Automatic work continuation after an interrupted or rate-limited account handoff.
//!
//! Only sessions whose active turn was interrupted (`AgentState::Working`) or stopped by a
//! provider rate limit receive a continuation prompt after switching accounts. Idle, done,
//! waiting-for-approval, and user-typed/held-input sessions never receive synthetic input.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::AgentState;
use krill_proto::screen::{Cell, Color, Flags, Screen};
use krill_proto::usage::{AccountInfo, ProviderKind};

use super::{
    CODEX_WRITER_LOCKED, Registry, SwitchScope, accounts, effective_foreground_pid, identity, lock,
    resume_identity,
};
use crate::session::Tap;

pub(super) const ACCOUNT_CONTINUE_PROMPT: &[u8] = b"Continue from where you left off.";
pub(super) const AGY_INTERRUPTED_MARKER: &str =
    "Interrupted · What should Antigravity CLI do instead?";

const CONTINUE_DEADLINE: Duration = Duration::from_secs(20);
const CONTINUE_POLL: Duration = Duration::from_millis(25);
const CONTINUE_IDENTITY_PROBE: Duration = Duration::from_millis(200);
const CONTINUE_COMPOSER_QUIET_MS: u64 = 250;
const CONTINUE_FALLBACK_QUIET_MS: u64 = 600;
const CONTINUE_CLIENT_HANDOFF_GRACE: Duration = Duration::from_millis(900);
const CONTINUE_CLIENT_FLUSH_SETTLE: Duration = Duration::from_millis(200);
const MAX_CLAUDE_COMPOSER_INTERIOR_ROWS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContinueMode {
    /// The original session was actively working or stopped by a rate limit.
    Always,
    /// Antigravity `interrupt_busy` switch where hook state may have dipped momentarily:
    /// continue only if the replacement freshly displays Antigravity's interrupted marker.
    IfAgyInterrupted,
}

pub(super) struct ContinuationPlan {
    pub old: SessionId,
    pub new: SessionId,
    pub provider: ProviderKind,
    pub new_pid: u32,
    pub tap: Tap,
    pub session_id: String,
    pub home: PathBuf,
    pub claude_homes: Vec<PathBuf>,
    pub target: AccountInfo,
    pub baseline_input_ns: u64,
    pub old_had_clients: bool,
    pub mode: ContinueMode,
}

struct FinishAccountResumeGuard<'a> {
    registry: &'a Registry,
    session: SessionId,
}

impl Drop for FinishAccountResumeGuard<'_> {
    fn drop(&mut self) {
        self.registry.agents.finish_account_resume(self.session);
    }
}

impl Registry {
    pub(super) fn spawn_account_continuation(self: &Arc<Self>, plan: ContinuationPlan) {
        let this = Arc::clone(self);
        let new = plan.new;
        let old = plan.old;
        let spawned = std::thread::Builder::new()
            .name(format!("account-continue-{}", new.0))
            .spawn(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    this.continue_after_account_switch(&plan);
                }))
                .is_err()
                {
                    tracing::warn!(
                        session = new.0,
                        from = old.0,
                        "continuation watcher panicked"
                    );
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(
                session = new.0,
                from = old.0,
                error = %error,
                "continuation watcher did not start"
            );
            self.agents.finish_account_resume(new);
        }
    }

    fn continue_after_account_switch(&self, plan: &ContinuationPlan) {
        let _finish = FinishAccountResumeGuard {
            registry: self,
            session: plan.new,
        };
        let started = Instant::now();
        let deadline = started + CONTINUE_DEADLINE;
        let mut next_identity_probe = started;
        let mut client_attached_at = plan.tap.has_clients().then_some(started);

        while Instant::now() < deadline {
            if self.continuation_aborted(plan) {
                return;
            }
            if plan.old_had_clients && client_attached_at.is_none() && plan.tap.has_clients() {
                client_attached_at = Some(Instant::now());
            }
            if !self
                .agents
                .account_resume_observed(plan.new, &plan.session_id)
                && Instant::now() >= next_identity_probe
            {
                next_identity_probe = Instant::now() + CONTINUE_IDENTITY_PROBE;
                self.probe_replacement_identity(plan);
            }
            let client_grace_done = if !plan.old_had_clients {
                true
            } else if let Some(attached_at) = client_attached_at {
                attached_at.elapsed() >= CONTINUE_CLIENT_FLUSH_SETTLE
            } else {
                started.elapsed() >= CONTINUE_CLIENT_HANDOFF_GRACE
            };
            if client_grace_done {
                let pulse = plan.tap.pulse();
                if pulse.closed {
                    return;
                }
                let quiet_ms = plan.tap.quiet_ms();
                if pulse.end > 0 && quiet_ms >= CONTINUE_COMPOSER_QUIET_MS {
                    let screen = plan.tap.capture().0;
                    let text = screen_plain_text(&screen);
                    if !provider_blocking_or_loading_screen(plan.provider, &text) {
                        let composer_ready = provider_empty_composer_ready(plan.provider, &screen);
                        let fallback_ready = quiet_ms >= CONTINUE_FALLBACK_QUIET_MS
                            && self
                                .agents
                                .account_resume_observed(plan.new, &plan.session_id)
                            && screen_has_idle_prompt_line(&screen);
                        if composer_ready || fallback_ready {
                            if self.continuation_aborted(plan) {
                                return;
                            }
                            if plan.mode == ContinueMode::IfAgyInterrupted
                                && !text.contains(AGY_INTERRUPTED_MARKER)
                            {
                                return;
                            }
                            match Self::deliver_input_to_tap_if_untouched(
                                &plan.tap,
                                ACCOUNT_CONTINUE_PROMPT,
                                true,
                                plan.baseline_input_ns,
                            ) {
                                Ok(true) => {
                                    tracing::info!(
                                        session = plan.new.0,
                                        from = plan.old.0,
                                        provider = plan.provider.as_str(),
                                        composer_ready,
                                        "sent automatic continuation after account switch"
                                    );
                                }
                                Ok(false) => {
                                    tracing::debug!(
                                        session = plan.new.0,
                                        from = plan.old.0,
                                        "withheld automatic continuation because newer input arrived"
                                    );
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        session = plan.new.0,
                                        from = plan.old.0,
                                        error = %error,
                                        "failed to send continuation prompt after account switch"
                                    );
                                }
                            }
                            return;
                        }
                    }
                }
            }
            std::thread::sleep(CONTINUE_POLL);
        }
        tracing::warn!(
            session = plan.new.0,
            from = plan.old.0,
            provider = plan.provider.as_str(),
            "continuation prompt timed out waiting for an idle composer after account switch"
        );
    }

    fn continuation_aborted(&self, plan: &ContinuationPlan) -> bool {
        if plan.tap.pulse().closed || lock(&self.state).exited.contains_key(&plan.new) {
            return true;
        }
        if plan.tap.last_input_ns() != plan.baseline_input_ns {
            return true;
        }
        if self.status.get(plan.new).state == AgentState::Working {
            return true;
        }
        self.agents.account_binding_retiring(plan.new)
            || lock(&self.account_handoffs).contains_key(&plan.new)
            || lock(&self.account_work.pending)
                .keys()
                .any(|scope| matches!(scope, SwitchScope::Session(id, _) if *id == plan.new))
    }

    fn probe_replacement_identity(&self, plan: &ContinuationPlan) {
        let new_fg = effective_foreground_pid(plan.new_pid, plan.tap.foreground(), false);
        let Ok(observed) = resume_identity(|| {
            let observed =
                identity::discover_resumed(plan.provider, plan.new_pid, new_fg, &plan.session_id)?;
            if observed.is_none() && plan.provider == ProviderKind::Claude {
                identity::discover_claude_with(
                    &plan.claude_homes,
                    plan.new_pid,
                    new_fg,
                    identity::ExpectedId::Exact(&plan.session_id),
                )
            } else {
                Ok(observed)
            }
        }) else {
            return;
        };
        let Some(found) = observed.filter(|found| {
            if plan.provider != ProviderKind::Antigravity {
                return true;
            }
            let expected = if plan.target.host_default {
                PathBuf::from(&plan.target.profile_dir)
            } else {
                PathBuf::from(&plan.target.profile_dir).join(".gemini/antigravity-cli")
            };
            let host_expected = accounts::host_default_dir(&plan.home, ProviderKind::Antigravity);
            let actual = found.home.canonicalize().ok();
            actual
                .as_ref()
                .zip(expected.canonicalize().ok().as_ref())
                .is_some_and(|(a, b)| a == b)
                || actual
                    .as_ref()
                    .zip(host_expected.canonicalize().ok().as_ref())
                    .is_some_and(|(a, b)| a == b)
        }) else {
            return;
        };
        if found.id == plan.session_id {
            self.agents
                .bind_discovered_session(plan.new, &found.id, Some(&found.path));
            self.agents
                .observe_account_resume(plan.new, &plan.session_id);
        }
    }
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

fn screen_plain_text(screen: &Screen) -> String {
    let mut out = String::new();
    for (idx, row) in screen.rows.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&row_text(row));
    }
    out
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

fn claude_empty_composer_ready(screen: &Screen) -> bool {
    let rows = &screen.rows;
    if rows.len() < 3 {
        return false;
    }
    let mut bottom = rows.len();
    while bottom > 2 {
        bottom = bottom.saturating_sub(1);
        if !rows.get(bottom).is_some_and(|r| is_border_row(r)) {
            continue;
        }
        let min_top = bottom.saturating_sub(MAX_CLAUDE_COMPOSER_INTERIOR_ROWS.saturating_add(1));
        for top in (min_top..bottom.saturating_sub(1)).rev() {
            if !rows.get(top).is_some_and(|r| is_border_row(r)) {
                continue;
            }
            let Some(interior) = rows.get(top.saturating_add(1)) else {
                break;
            };
            let text = row_text(interior);
            if text.starts_with('❯') {
                return bottom == top.saturating_add(2)
                    && is_empty_prefixed_row(interior, '❯', true);
            }
            break;
        }
    }
    false
}

fn codex_empty_composer_ready(screen: &Screen) -> bool {
    let rows = &screen.rows;
    for y in (0..rows.len()).rev() {
        let Some(cells) = rows.get(y) else {
            continue;
        };
        let text = row_text(cells);
        if text.is_empty() {
            continue;
        }
        if text.starts_with('›') {
            return is_empty_prefixed_row(cells, '›', false);
        }
        // Scan a few footer rows below the composer (`model · dir · % left`) before stopping.
        if rows.len().saturating_sub(y) > 8 {
            break;
        }
    }
    false
}

pub(super) fn provider_empty_composer_ready(provider: ProviderKind, screen: &Screen) -> bool {
    match provider {
        ProviderKind::Antigravity => krill_proto::screen::agy::empty_composer_ready(screen),
        ProviderKind::Claude => claude_empty_composer_ready(screen),
        ProviderKind::Codex => codex_empty_composer_ready(screen),
        ProviderKind::OpenCode | ProviderKind::Gemini => false,
    }
}

pub(super) fn provider_blocking_or_loading_screen(provider: ProviderKind, text: &str) -> bool {
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
    ];
    let lower = text.to_ascii_lowercase();
    if provider == ProviderKind::Codex
        && (text.contains(CODEX_WRITER_LOCKED)
            || lower.contains("close it there and press r to continue here"))
    {
        return true;
    }
    BLOCKERS.iter().any(|needle| lower.contains(needle))
}

pub(super) fn screen_has_idle_prompt_line(screen: &Screen) -> bool {
    for cells in screen.rows.iter().rev() {
        let text = row_text(cells);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        return trimmed.ends_with('>')
            || trimmed.ends_with('❯')
            || trimmed.ends_with('›')
            || is_empty_prefixed_row(cells, '>', false)
            || is_empty_prefixed_row(cells, '❯', true)
            || is_empty_prefixed_row(cells, '›', false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::screen::Cursor;

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

    const BORDER: &str = "────────────────────────────────────────────────────────────────";

    #[test]
    fn recognizes_empty_composers_and_rejects_drafts_across_providers() {
        let mut agy_ready = screen_from_lines(&[
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
        agy_ready.rows[5][2].flags |= Flags::INVERSE;
        assert!(provider_empty_composer_ready(
            ProviderKind::Antigravity,
            &agy_ready
        ));
        assert!(!provider_blocking_or_loading_screen(
            ProviderKind::Antigravity,
            &screen_plain_text(&agy_ready)
        ));

        let claude_ready = screen_from_lines(&[
            (BORDER, None),
            ("❯\u{a0}Try \"fix the build\"", Some(2)),
            (BORDER, None),
            ("  bypass permissions on", None),
        ]);
        assert!(provider_empty_composer_ready(
            ProviderKind::Claude,
            &claude_ready
        ));

        let claude_draft = screen_from_lines(&[
            (BORDER, None),
            ("❯\u{a0}user typed text", None),
            (BORDER, None),
        ]);
        assert!(!provider_empty_composer_ready(
            ProviderKind::Claude,
            &claude_draft
        ));

        let codex_ready = screen_from_lines(&[
            ("› Ask Codex to do anything", Some(2)),
            ("", None),
            ("  gpt-5 · repo · 90% left", None),
        ]);
        assert!(provider_empty_composer_ready(
            ProviderKind::Codex,
            &codex_ready
        ));

        let codex_draft = screen_from_lines(&[
            ("› user draft", None),
            ("", None),
            ("  gpt-5 · repo · 90% left", None),
        ]);
        assert!(!provider_empty_composer_ready(
            ProviderKind::Codex,
            &codex_draft
        ));

        let codex_locked = screen_from_lines(&[
            ("This conversation is open in another app", None),
            ("Close it there and press R to continue here.", None),
        ]);
        assert!(provider_blocking_or_loading_screen(
            ProviderKind::Codex,
            &screen_plain_text(&codex_locked)
        ));

        let mock_prompt = screen_from_lines(&[("resumed prompt > ", None)]);
        assert!(screen_has_idle_prompt_line(&mock_prompt));

        let signing_in = screen_from_lines(&[("Signing in...", None)]);
        assert!(provider_blocking_or_loading_screen(
            ProviderKind::Antigravity,
            &screen_plain_text(&signing_in)
        ));
        assert!(!screen_has_idle_prompt_line(&signing_in));
    }
}
