use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui_kit_assets::IconName;
use krill_app::i18n::catalog::chat as ctext;
#[cfg(test)]
use krill_app::ui;
use krill_proto::agent_history::{ChatItem, ChatItemKind};
use krill_proto::control::AgentState;

pub(super) const MAX_PREAMBLE_CHARS: usize = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RawSpan {
    start: usize,
    end: usize,
}

impl RawSpan {
    pub(super) fn single(abs_idx: usize) -> Self {
        Self {
            start: abs_idx,
            end: abs_idx + 1,
        }
    }

    pub(super) fn start(self) -> usize {
        self.start
    }

    pub(super) fn end(self) -> usize {
        self.end
    }

    pub(super) fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Debug, Default)]
pub(super) struct DisplayFeed {
    rows: Vec<DisplayRow>,
}

impl DisplayFeed {
    pub(super) fn empty() -> Arc<Self> {
        static EMPTY: OnceLock<Arc<DisplayFeed>> = OnceLock::new();
        Arc::clone(EMPTY.get_or_init(|| Arc::new(Self::default())))
    }

    #[cfg(test)]
    pub(super) fn project(base_index: usize, items: &[Arc<ChatItem>]) -> Self {
        Self::project_with_unread(base_index, items, None)
    }

    #[cfg(test)]
    pub(super) fn project_with_unread(
        base_index: usize,
        items: &[Arc<ChatItem>],
        unread_from: Option<usize>,
    ) -> Self {
        Self::project_with_notes(base_index, items, unread_from, &[], &[], None)
    }

    /// Projects the transcript with a bot's item changes as lines between its turns (Orca's bot
    /// item note rows). Desktop chat rows carry no time, so a change goes just before the first
    /// owner turn sent after it (`turn_times`: the newest turns' times, newest first), or at
    /// the end when it is newer than all of them.
    #[cfg(test)]
    pub(super) fn project_with_notes(
        base_index: usize,
        items: &[Arc<ChatItem>],
        unread_from: Option<usize>,
        notes: &[Arc<ItemNote>],
        turn_times: &[u64],
        fold: Option<&TurnFold>,
    ) -> Self {
        let stamps: Vec<TurnStamp> = turn_times
            .iter()
            .copied()
            .map(TurnStamp::time_only)
            .collect();
        let empty = std::collections::BTreeMap::new();
        Self::project_with_turn_stamps(base_index, items, unread_from, notes, &stamps, &empty, fold)
    }

    pub(super) fn project_with_turn_stamps(
        base_index: usize,
        items: &[Arc<ChatItem>],
        unread_from: Option<usize>,
        notes: &[Arc<ItemNote>],
        turn_stamps: &[TurnStamp],
        observed_user_at_ms: &std::collections::BTreeMap<usize, u64>,
        fold: Option<&TurnFold>,
    ) -> Self {
        let foldable = compute_foldable_notes(items);
        let mut rows = Vec::new();
        let mut rel_idx = 0;
        while rel_idx < items.len() {
            let item = &items[rel_idx];
            if !is_work_or_foldable(item.kind, foldable[rel_idx]) {
                let abs_idx = base_index + rel_idx;
                // A bot's launch preamble is its instructions, not something its owner said.
                let item = match (
                    item.kind,
                    krill_proto::bots::strip_launch_preamble(&item.text),
                ) {
                    (ChatItemKind::User, Some("")) => {
                        rel_idx += 1;
                        continue;
                    }
                    (ChatItemKind::User, Some(words)) => Arc::new(ChatItem {
                        text: words.to_owned(),
                        ..(**item).clone()
                    }),
                    _ => Arc::clone(item),
                };
                if item.kind == ChatItemKind::User
                    && let Some(notice) = parse_inbound_notice(&item.text)
                {
                    rows.push(DisplayRow::Notice {
                        abs_idx,
                        notice: Arc::new(notice),
                    });
                    rel_idx += 1;
                    continue;
                }
                rows.push(DisplayRow::Item { abs_idx, item });
                rel_idx += 1;
                continue;
            }
            let mut rel_end = rel_idx + 1;
            while rel_end < items.len()
                && is_work_or_foldable(items[rel_end].kind, foldable[rel_end])
            {
                rel_end += 1;
            }
            let slice = &items[rel_idx..rel_end];
            let has_tool = slice.iter().any(|entry| {
                matches!(
                    entry.kind,
                    ChatItemKind::ToolCall | ChatItemKind::ToolResult
                )
            });
            let start_abs = base_index + rel_idx;
            if !has_tool && slice.len() == 1 {
                rows.push(DisplayRow::Item {
                    abs_idx: start_abs,
                    item: Arc::clone(&slice[0]),
                });
            } else {
                let span = RawSpan {
                    start: start_abs,
                    end: base_index + rel_end,
                };
                rows.push(DisplayRow::WorkRun(WorkRun::from_slice(span, slice)));
            }
            rel_idx = rel_end;
        }
        if !notes.is_empty() {
            rows = interleave_notes(
                rows,
                notes,
                turn_stamps,
                observed_user_at_ms,
                base_index + items.len(),
            );
        }
        if let Some(fold) = fold {
            rows = fold_turns(rows, base_index, items, fold);
        }
        if let Some(cutoff) = unread_from
            && cutoff > base_index
            && let Some(pos) = rows.iter().position(|r| r.span().end() > cutoff)
            && pos > 0
        {
            rows.insert(
                pos,
                DisplayRow::UnreadDivider {
                    first_abs_idx: cutoff,
                },
            );
        }
        Self { rows }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn get(&self, row_ix: usize) -> Option<&DisplayRow> {
        self.rows.get(row_ix)
    }

    pub(super) fn rows(&self) -> &[DisplayRow] {
        &self.rows
    }
}

#[derive(Clone, Debug)]
pub(super) enum DisplayRow {
    Item {
        abs_idx: usize,
        item: Arc<ChatItem>,
    },
    WorkRun(WorkRun),
    UnreadDivider {
        first_abs_idx: usize,
    },
    /// Mail Krill delivered into the session, in place of its owner bubble.
    Notice {
        abs_idx: usize,
        notice: Arc<InboundNotice>,
    },
    /// A change to one of the bot's items, drawn before the transcript row at `anchor_abs_idx`.
    ItemNote {
        anchor_abs_idx: usize,
        note: Arc<ItemNote>,
    },
    /// A settled turn's "16s 작업 완료 >" line under its prompt (Orca's `NativeChatWorkingStatus`):
    /// pressing it shows the work folded behind the turn's answer.
    TurnBar {
        user_abs_idx: usize,
        worked_secs: u64,
        /// Whether anything is folded behind it (no chevron otherwise).
        folds: bool,
        expanded: bool,
    },
}

impl DisplayRow {
    pub(super) fn span(&self) -> RawSpan {
        match self {
            Self::Item { abs_idx, .. } | Self::Notice { abs_idx, .. } => RawSpan::single(*abs_idx),
            Self::WorkRun(run) => run.span,
            Self::UnreadDivider { first_abs_idx } => RawSpan::single(*first_abs_idx),
            Self::ItemNote { anchor_abs_idx, .. } => RawSpan::single(*anchor_abs_idx),
            Self::TurnBar { user_abs_idx, .. } => RawSpan::single(*user_abs_idx),
        }
    }

    /// Which vertical edges of a display row join tightly (`4px`) with an adjacent activity row
    /// (Orca `6edf9f05` / `#46659`, `NATIVE_CHAT_ACTIVITY_ROW_GAP_PX = 4`).
    pub(super) fn activity_edges(&self) -> (bool, bool) {
        match self {
            Self::Notice { .. } => (true, true),
            Self::ItemNote { note, .. } => match note.action {
                NoteAction::Memory(_) | NoteAction::CreateOffer(_) => (false, false),
                _ => (true, true),
            },
            Self::Item { .. }
            | Self::WorkRun(_)
            | Self::UnreadDivider { .. }
            | Self::TurnBar { .. } => (false, false),
        }
    }
}

/// First plain-text preview line of an agent message row (Orca `dcde358a` / `#46739`,
/// `agent-message-preview-line.ts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentMessagePreviewLine {
    pub line: String,
    pub more: bool,
}

const PREVIEW_MAX_CHARS: usize = 200;

/// Extracts the first readable plain-text line from an agent message body and reports whether
/// there is more content behind it (additional lines, fenced blocks, bare URLs, or clipping).
pub(super) fn agent_message_preview_line(body: &str) -> AgentMessagePreviewLine {
    let mut fence: Option<(char, usize)> = None;
    let mut skipped_before = false;
    let mut found: Option<(String, bool)> = None;
    let mut has_after = false;

    for raw_line in body.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed = line.trim();
        if let Some((ch, min_len)) = fence {
            if is_closing_fence(trimmed, ch, min_len) {
                fence = None;
            } else if !trimmed.is_empty() {
                if found.is_some() {
                    has_after = true;
                    break;
                }
                skipped_before = true;
            }
            continue;
        }
        if let Some((ch, len)) = opening_fence(trimmed) {
            fence = Some((ch, len));
            if found.is_some() {
                has_after = true;
                break;
            }
            skipped_before = true;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if found.is_some() {
            has_after = true;
            break;
        }
        let plain = plain_markdown_line(trimmed);
        if plain.is_empty() {
            skipped_before = true;
            continue;
        }
        let has_bare_url = contains_bare_url(trimmed);
        found = Some((plain, has_bare_url));
    }

    let Some((plain, has_bare_url)) = found else {
        return AgentMessagePreviewLine {
            line: String::new(),
            more: skipped_before || !body.trim().is_empty(),
        };
    };

    let (line, clipped) = cut_at_code_point(&plain, PREVIEW_MAX_CHARS);
    AgentMessagePreviewLine {
        line,
        more: skipped_before || has_after || has_bare_url || clipped,
    }
}

fn opening_fence(trimmed: &str) -> Option<(char, usize)> {
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == first).count();
    if count < 3 {
        return None;
    }
    // CommonMark: backtick info strings cannot contain backticks.
    if first == '`' && trimmed.get(count..).is_some_and(|rest| rest.contains('`')) {
        return None;
    }
    Some((first, count))
}

fn is_closing_fence(trimmed: &str, ch: char, min_len: usize) -> bool {
    let count = trimmed.chars().take_while(|&c| c == ch).count();
    count >= min_len
        && trimmed
            .get(count..)
            .is_some_and(|rest| rest.trim().is_empty())
}

fn contains_bare_url(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    for (idx, _) in raw
        .match_indices("https://")
        .chain(raw.match_indices("http://"))
        .chain(raw.match_indices("www."))
    {
        // Ignore URLs already wrapped as markdown link targets `](url)`.
        if idx >= 2 && bytes.get(idx - 2..idx) == Some(b"](") {
            continue;
        }
        if idx == 0 || !bytes.get(idx - 1).is_some_and(u8::is_ascii_alphanumeric) {
            return true;
        }
    }
    false
}

fn cut_at_code_point(text: &str, max_chars: usize) -> (String, bool) {
    match text.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => (
            text.get(..byte_idx).unwrap_or(text).trim_end().to_owned(),
            true,
        ),
        None => (text.to_owned(), false),
    }
}

fn strip_block_prefixes(mut s: &str) -> &str {
    loop {
        let before = s;
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix('>') {
            s = rest.strip_prefix(' ').unwrap_or(rest);
            continue;
        }
        // ATX heading `#{1,6} `
        let hashes = s.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes)
            && let Some(rest) = s.get(hashes..).and_then(|r| r.strip_prefix(' '))
        {
            s = rest;
            continue;
        }
        // Unordered list bullet `- `, `* `, `+ `
        if matches!(s.as_bytes().first(), Some(b'-' | b'*' | b'+'))
            && s.as_bytes().get(1) == Some(&b' ')
            && let Some(rest) = s.get(2..)
        {
            s = rest;
            continue;
        }
        // Ordered list prefix `1. ` or `1) `
        let digits = s.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0
            && matches!(s.as_bytes().get(digits), Some(b'.' | b')'))
            && s.as_bytes().get(digits + 1) == Some(&b' ')
            && let Some(rest) = s.get(digits + 2..)
        {
            s = rest;
            continue;
        }
        // Task list checkbox `[ ] `, `[x] `, `[X] `
        if let Some(rest) = s
            .strip_prefix("[ ] ")
            .or_else(|| s.strip_prefix("[x] "))
            .or_else(|| s.strip_prefix("[X] "))
        {
            s = rest;
            continue;
        }
        if s == before {
            break;
        }
    }
    s
}

fn plain_markdown_line(raw: &str) -> String {
    let stripped = strip_block_prefixes(raw);
    let no_links = strip_markdown_links(stripped);
    let no_code_and_emph = strip_code_and_emphasis(&no_links);
    no_code_and_emph
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replaces `[label](url)` and `![alt](url)` with `label`/`alt`, handling balanced parentheses
/// inside `url` (e.g. Wikipedia links).
fn strip_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;
    while let Some(rest) = input.get(idx..) {
        if rest.is_empty() {
            break;
        }
        let is_image = rest.starts_with("![");
        let bracket_start = if is_image {
            idx + 1
        } else if rest.starts_with('[') {
            idx
        } else {
            let Some(ch) = rest.chars().next() else {
                break;
            };
            out.push(ch);
            idx += ch.len_utf8();
            continue;
        };
        if let Some((label, after_link)) = parse_link_at(input, bracket_start) {
            out.push_str(label);
            idx = after_link;
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn parse_link_at(input: &str, open_bracket: usize) -> Option<(&str, usize)> {
    let after_open = open_bracket + 1;
    let mut depth = 1usize;
    let mut close_bracket = None;
    for (rel, ch) in input.get(after_open..)?.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close_bracket = Some(after_open + rel);
                    break;
                }
            }
            _ => {}
        }
    }
    let close_bracket = close_bracket?;
    if !input.get(close_bracket + 1..)?.starts_with('(') {
        return None;
    }
    let url_start = close_bracket + 2;
    let mut paren_depth = 1usize;
    let mut close_paren = None;
    for (rel, ch) in input.get(url_start..)?.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    close_paren = Some(url_start + rel);
                    break;
                }
            }
            _ => {}
        }
    }
    let close_paren = close_paren?;
    Some((input.get(after_open..close_bracket)?, close_paren + 1))
}

/// Strips inline code backticks while keeping code content untouched by emphasis stripping
/// (so `` `src/**/*.ts` `` and `` `foo_bar` `` survive verbatim), and strips markdown emphasis
/// from non-code text around them.
fn strip_code_and_emphasis(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut text_start = 0;
    let mut idx = 0;
    while let Some(rest) = input.get(idx..) {
        if rest.is_empty() {
            break;
        }
        if rest.starts_with('`') {
            let ticks = rest.bytes().take_while(|&b| b == b'`').count();
            let body_start = idx + ticks;
            if let (Some(delim), Some(after_delim)) = (rest.get(..ticks), input.get(body_start..))
                && let Some(rel_end) = after_delim.find(delim)
                && let Some(inner) = after_delim.get(..rel_end)
            {
                if let Some(before) = input.get(text_start..idx) {
                    out.push_str(&strip_inline_emphasis(before));
                }
                out.push_str(inner);
                idx = body_start + rel_end + ticks;
                text_start = idx;
                continue;
            }
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        idx += ch.len_utf8();
    }
    if let Some(tail) = input.get(text_start..) {
        out.push_str(&strip_inline_emphasis(tail));
    }
    out
}

fn is_word_char(ch: Option<char>) -> bool {
    ch.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn strip_inline_emphasis(input: &str) -> String {
    // Strip paired `**`, `__`, `~~`, then single `*`, `_` while preserving glob stars
    // (`src/**/*.ts`), math (`5 * 3`), and snake_case (`term_0`, `snake_case_name`).
    let s = strip_paired_delim(input, "**", true, false);
    let s = strip_paired_delim(&s, "__", false, true);
    let s = strip_paired_delim(&s, "~~", false, false);
    let s = strip_paired_delim(&s, "*", true, false);
    strip_paired_delim(&s, "_", false, true)
}

fn strip_paired_delim(
    input: &str,
    delim: &str,
    forbid_glob_slash: bool,
    word_boundary_only: bool,
) -> String {
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;
    let dlen = delim.len();
    let dchar = delim.chars().next().unwrap_or('*');
    while let Some(rest) = input.get(idx..) {
        if rest.is_empty() {
            break;
        }
        if rest.starts_with(delim) {
            let prev = input.get(..idx).and_then(|s| s.chars().next_back());
            let after_open = idx + dlen;
            let next_after_open = input.get(after_open..).and_then(|s| s.chars().next());
            let open_ok = next_after_open.is_some_and(|c| !c.is_whitespace() && c != dchar)
                && (!forbid_glob_slash || (prev != Some('/') && prev != Some(dchar)))
                && (!word_boundary_only || !is_word_char(prev));
            if open_ok
                && let Some(close_idx) = find_closing_delim(
                    input,
                    after_open,
                    delim,
                    forbid_glob_slash,
                    word_boundary_only,
                )
                && let Some(inner) = input.get(after_open..close_idx)
            {
                out.push_str(inner);
                idx = close_idx + dlen;
                continue;
            }
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn find_closing_delim(
    input: &str,
    start: usize,
    delim: &str,
    forbid_glob_slash: bool,
    word_boundary_only: bool,
) -> Option<usize> {
    let dlen = delim.len();
    let dchar = delim.chars().next().unwrap_or('*');
    let mut search = start;
    while let Some(rel) = input.get(search..).and_then(|s| s.find(delim)) {
        let pos = search + rel;
        if pos > start {
            let prev = input.get(start..pos).and_then(|s| s.chars().next_back());
            let after_close = pos + dlen;
            let next = input.get(after_close..).and_then(|s| s.chars().next());
            let close_ok = prev.is_some_and(|c| !c.is_whitespace() && c != dchar)
                && next != Some(dchar)
                && (!forbid_glob_slash || next != Some('/'))
                && (!word_boundary_only || !is_word_char(next));
            if close_ok {
                return Some(pos);
            }
        }
        search = pos + dlen;
    }
    None
}

/// Who changed a bot item, as its chat line says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoteActor {
    Bot,
    Owner,
}

/// What happened to a bot item (Orca's `bots.items.chat.*` lines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoteAction {
    Added,
    Commented,
    Linked(String),
    Status(krill_proto::bots::BotItemStatus),
    Updated,
    /// The bot's `MEMORY.md` changed (Orca's `NativeChatMemoryUpdatedRow`).
    Memory(MemoryReceipt),
    /// The bot proposed a new bot (Orca's `NativeChatBotCreateOfferCard`).
    CreateOffer(Box<krill_proto::bots::BotOffer>),
}

/// What a memory change added and removed, as its chat line's receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemoryReceipt {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Lines other than facts changed too (Orca's "MEMORY.md changed").
    pub other_changes: bool,
}

impl MemoryReceipt {
    /// Orca's `botMemoryUpdatedSummary`: "· 1 added, 2 removed", or "· N changes" past two kinds.
    pub(crate) fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.added.is_empty() {
            parts.push(ctext::MEMORY_ADDED_COUNT.format(&[("n", &self.added.len().to_string())]));
        }
        if !self.removed.is_empty() {
            parts.push(
                ctext::MEMORY_REMOVED_COUNT.format(&[("n", &self.removed.len().to_string())]),
            );
        }
        let count = self.added.len() + self.removed.len() + usize::from(self.other_changes);
        let tail = if self.other_changes || parts.len() > 2 {
            ctext::MEMORY_CHANGE_COUNT.format(&[("n", &count.to_string())])
        } else {
            parts.join(", ")
        };
        if tail.is_empty() {
            ctext::MEMORY_UPDATED.get().to_owned()
        } else {
            format!("{} · {tail}", ctext::MEMORY_UPDATED.get())
        }
    }
}

/// Which settled turns fold their work behind a "Worked for" line (bot chats, as Orca's do).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TurnFold {
    /// The last turn is still running: it keeps its work in view.
    pub live: bool,
    /// Prompts (absolute indices) whose turns the reader opened.
    pub expanded: BTreeSet<usize>,
}

fn item_epoch_ms(item: &ChatItem) -> Option<u64> {
    let raw = item.at.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(parsed) = raw.parse::<u64>() {
        return Some(if parsed > 10_000_000_000 {
            parsed
        } else {
            parsed.saturating_mul(1_000)
        });
    }
    let secs = u64::try_from(krill_proto::time::parse_reset_epoch(raw)?).ok()?;
    let frac_ms = raw
        .split_once(['T', 't', ' '])
        .and_then(|(_, time)| time.split_once(['.', ',']))
        .map_or(0, |(_, frac)| {
            let digits: String = frac
                .chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();
            match digits.len() {
                0 => 0,
                1 => digits.parse::<u64>().unwrap_or(0) * 100,
                2 => digits.parse::<u64>().unwrap_or(0) * 10,
                _ => digits.parse::<u64>().unwrap_or(0),
            }
        });
    Some(secs.saturating_mul(1_000).saturating_add(frac_ms))
}

fn item_epoch_secs(item: &ChatItem) -> Option<u64> {
    item_epoch_ms(item).map(|ms| ms / 1_000)
}

/// Orca's turn fold: under each settled prompt a "Worked for" line, and unless opened, the turn's
/// work runs and its earlier replies hidden behind its last reply (the answer).
fn fold_turns(
    rows: Vec<DisplayRow>,
    base_index: usize,
    items: &[Arc<ChatItem>],
    fold: &TurnFold,
) -> Vec<DisplayRow> {
    let users: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(pos, row)| match row {
            DisplayRow::Item { item, .. } if item.kind == ChatItemKind::User => Some(pos),
            _ => None,
        })
        .collect();
    if users.is_empty() {
        return rows;
    }
    let mut out = Vec::with_capacity(rows.len() + users.len());
    let mut rows = rows.into_iter().enumerate().peekable();
    // Rows before the first prompt stay as they are.
    while let Some((_, row)) = rows.next_if(|(pos, _)| *pos < users[0]) {
        out.push(row);
    }
    for turn in 0..users.len() {
        let next_user = users.get(turn + 1).copied().unwrap_or(usize::MAX);
        let Some((_, user_row)) = rows.next() else {
            break;
        };
        let user_abs = user_row.span().start();
        let mut body: Vec<DisplayRow> = Vec::new();
        while let Some((_, row)) = rows.next_if(|(pos, _)| *pos < next_user) {
            body.push(row);
        }
        let last_turn = turn + 1 == users.len();
        let raw_end = body
            .iter()
            .map(|r| r.span().end())
            .max()
            .unwrap_or(user_abs + 1);
        let worked = (!(last_turn && fold.live))
            .then(|| {
                let at = |abs: usize| {
                    abs.checked_sub(base_index)
                        .and_then(|rel| items.get(rel))
                        .and_then(|item| item_epoch_secs(item))
                };
                let start = at(user_abs)?;
                let end = (user_abs + 1..raw_end).rev().find_map(at)?;
                Some(end.saturating_sub(start))
            })
            .flatten();
        out.push(user_row);
        let Some(worked_secs) = worked else {
            out.extend(body);
            continue;
        };
        let answer = body.iter().rposition(
            |r| matches!(r, DisplayRow::Item { item, .. } if item.kind == ChatItemKind::Assistant),
        );
        let folded = |pos: usize, row: &DisplayRow| match row {
            DisplayRow::WorkRun(_) => true,
            DisplayRow::Item { item, .. } => {
                item.kind == ChatItemKind::Assistant && Some(pos) != answer
            }
            DisplayRow::UnreadDivider { .. }
            | DisplayRow::ItemNote { .. }
            | DisplayRow::Notice { .. }
            | DisplayRow::TurnBar { .. } => false,
        };
        let folds = body.iter().enumerate().any(|(pos, row)| folded(pos, row));
        let expanded = fold.expanded.contains(&user_abs);
        out.push(DisplayRow::TurnBar {
            user_abs_idx: user_abs,
            worked_secs,
            folds,
            expanded,
        });
        out.extend(
            body.into_iter()
                .enumerate()
                .filter(|(pos, row)| expanded || !folded(*pos, row))
                .map(|(_, row)| row),
        );
    }
    out.extend(rows.map(|(_, row)| row));
    out
}

/// Orca's `formatNativeChatDuration`: `16s`, `2m 5s`, `1h 3m 0s`.
pub(super) fn format_turn_duration(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let minutes = secs / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", secs % 60);
    }
    format!("{}h {}m {}s", minutes / 60, minutes % 60, secs % 60)
}

/// One change to a bot's item shown in its chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ItemNote {
    pub at_ms: u64,
    pub actor: NoteActor,
    pub action: NoteAction,
    pub title: String,
    /// The item's current status (its chip glyph).
    pub status: krill_proto::bots::BotItemStatus,
    /// A comment's words.
    pub detail: String,
}

/// Whether a delivered notice is a report or something that wants an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    Update,
    Message,
}

/// Mail Krill typed into a bot's session (worker reports, bot mail, external messages), drawn as
/// an activity line naming its senders instead of an owner bubble.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InboundNotice {
    pub kind: NoticeKind,
    pub from: Vec<String>,
    pub detail: String,
}

/// Parses the prompts krilld delivers (`registry.rs` idle mail pointer, `bots/runtime.rs` bot
/// mail and external messages). `None` for anything else.
pub(crate) fn parse_inbound_notice(text: &str) -> Option<InboundNotice> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("[Krill orchestration] Unread mail.") {
        let mut kind = NoticeKind::Update;
        let mut from = Vec::new();
        let mut list = rest
            .trim_start()
            .strip_prefix("From: ")
            .and_then(|s| s.split_once(". Read with:"))
            .map_or("", |(list, _)| list);
        // `'<shell-quoted name>' (<type>)`, comma separated.
        while let Some((name, after)) = list.strip_prefix('\'').and_then(|s| s.split_once("' (")) {
            let Some((ty, after)) = after.split_once(')') else {
                break;
            };
            if matches!(ty, "question" | "escalation" | "decision_gate") {
                kind = NoticeKind::Message;
            }
            from.push(name.replace("'\\''", "'"));
            list = after.strip_prefix(", ").unwrap_or_default();
        }
        if from.is_empty() {
            // Pointers from daemons before senders were named.
            from.push("Krill orchestration".to_owned());
        }
        return Some(InboundNotice {
            kind,
            from,
            detail: String::new(),
        });
    }
    if let Some(rest) = text.strip_prefix("[Krill bot mail from ") {
        let (from, rest) = rest.split_once("] Subject: ")?;
        let (subject, body) = rest.split_once('\n').unwrap_or((rest, ""));
        let detail = [subject.trim(), body.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Some(InboundNotice {
            kind: NoticeKind::Message,
            from: vec![from.to_owned()],
            detail,
        });
    }
    let rest = text.strip_prefix("[Krill external message via ")?;
    let (head, body) = rest.split_once("]\n")?;
    let source = head.split(" | ").next().unwrap_or_default();
    let sender = head
        .split_once(" | from: ")
        .map_or("", |(_, s)| s)
        .rsplit_once(" (")
        .map_or("", |(name, _)| name);
    // Drop the `<<<` / `>>>` fences around the quoted text.
    let mut lines: Vec<&str> = body.lines().collect();
    if lines
        .first()
        .is_some_and(|l| l.len() >= 3 && l.bytes().all(|b| b == b'<'))
    {
        lines.remove(0);
    }
    if let Some(end) = lines
        .iter()
        .rposition(|l| l.len() >= 3 && l.bytes().all(|b| b == b'>'))
    {
        lines.remove(end);
    }
    Some(InboundNotice {
        kind: NoticeKind::Message,
        from: vec![format!("{sender} ({source})")],
        detail: lines.join("\n").trim().to_owned(),
    })
}

/// A recorded prompt turn's timestamp and cleaned text for stable `ItemNote` placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnStamp {
    pub at_ms: u64,
    pub text: String,
}

impl TurnStamp {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn time_only(at_ms: u64) -> Self {
        Self {
            at_ms,
            text: String::new(),
        }
    }
}

fn turn_stamp_matches_user(stamp_text: &str, item: &ChatItem) -> bool {
    let stamp = stamp_text.trim();
    if stamp.is_empty() {
        return false;
    }
    let item_clean = krill_proto::git::clean_prompt_display(
        &item.text,
        krill_proto::git::MAX_PROMPT_DISPLAY_CHARS,
    );
    if item_clean.as_deref() == Some(stamp) {
        return true;
    }
    if item.file_path.is_empty() {
        return false;
    }
    // `krilld::agent_history::chat::take_leading_image_paths` strips leading attached image paths
    // out of `item.text` into `item.file_path`, while `SessionPromptHistory` keeps them in the
    // raw submitted payload (and may truncate at `MAX_PROMPT_DISPLAY_CHARS` with `…`).
    let mut rest = stamp;
    let mut stripped_any = false;
    for path in item
        .file_path
        .lines()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let trimmed = rest.trim_start();
        let after = trimmed
            .strip_prefix(&format!("'{path}'"))
            .or_else(|| trimmed.strip_prefix(&format!("\"{path}\"")))
            .or_else(|| trimmed.strip_prefix(path));
        if let Some(next) = after {
            rest = next.trim_start();
            stripped_any = true;
        }
    }
    if !stripped_any {
        return false;
    }
    let Some(norm) = item_clean.as_deref() else {
        return rest.is_empty();
    };
    if rest == norm
        || krill_proto::git::clean_prompt_display(rest, krill_proto::git::MAX_PROMPT_DISPLAY_CHARS)
            .as_deref()
            == Some(norm)
    {
        return true;
    }
    rest.strip_suffix('…')
        .map(str::trim_end)
        .is_some_and(|prefix| !prefix.is_empty() && norm.starts_with(prefix))
}

fn interleave_notes(
    rows: Vec<DisplayRow>,
    notes: &[Arc<ItemNote>],
    turn_stamps: &[TurnStamp],
    observed_user_at_ms: &std::collections::BTreeMap<usize, u64>,
    end_abs: usize,
) -> Vec<DisplayRow> {
    // Owner turns, oldest first. Prefer each transcript row's own timestamp (`item.at`), then
    // text-matched `SessionPromptHistory` turns (including image-attached prompts), then live
    // observed/submitted timestamps so an ahead-of-transcript or behind-transcript prompt
    // submission never shifts notes across turns.
    let users: Vec<(usize, usize, &ChatItem)> = rows
        .iter()
        .enumerate()
        .filter_map(|(pos, row)| match row {
            DisplayRow::Item { abs_idx, item } if item.kind == ChatItemKind::User => {
                Some((pos, *abs_idx, item.as_ref()))
            }
            _ => None,
        })
        .collect();
    let mut resolved: Vec<Option<u64>> = users
        .iter()
        .map(|&(_, _, item)| item_epoch_ms(item))
        .collect();
    let has_text_stamps = turn_stamps.iter().any(|stamp| !stamp.text.is_empty());
    if has_text_stamps {
        let mut stamp_idx = 0_usize;
        for i in (0..users.len()).rev() {
            let abs_idx = users[i].1;
            let item = users[i].2;
            let anchor_hint = resolved[i].or_else(|| observed_user_at_ms.get(&abs_idx).copied());
            let matched = turn_stamps[stamp_idx..]
                .iter()
                .enumerate()
                .filter(|(_, s)| turn_stamp_matches_user(&s.text, item))
                .find(|(_, s)| {
                    // When we already know this row's approximate timestamp, don't bind it to a
                    // brand-new ahead-of-transcript duplicate-text submission from a later turn.
                    anchor_hint.is_none_or(|hint| hint.abs_diff(s.at_ms) <= 10_000)
                })
                .map(|(rel, _)| stamp_idx + rel);
            if let Some(found) = matched {
                let stamp_ms = turn_stamps[found].at_ms;
                match resolved[i] {
                    None => resolved[i] = Some(stamp_ms),
                    Some(own_ms) if own_ms % 1_000 == 0 && own_ms.abs_diff(stamp_ms) < 2_000 => {
                        resolved[i] = Some(stamp_ms);
                    }
                    Some(_) => {}
                }
                stamp_idx = found + 1;
            }
        }
    } else if !turn_stamps.is_empty() {
        let mut ti = 0_usize;
        for i in (0..users.len()).rev() {
            if resolved[i].is_none()
                && !observed_user_at_ms.contains_key(&users[i].1)
                && let Some(stamp) = turn_stamps.get(ti)
            {
                resolved[i] = Some(stamp.at_ms);
                ti += 1;
            }
        }
    }
    for (slot, &(_, abs_idx, _)) in resolved.iter_mut().zip(&users) {
        if slot.is_none()
            && let Some(&obs_ms) = observed_user_at_ms.get(&abs_idx)
        {
            *slot = Some(obs_ms);
        }
    }
    let anchors: Vec<(usize, u64)> = users
        .iter()
        .zip(resolved)
        .filter_map(|(&(pos, _, _), at)| at.map(|at| (pos, at)))
        .collect();
    let mut before: Vec<Vec<Arc<ItemNote>>> = vec![Vec::new(); rows.len() + 1];
    let mut sorted: Vec<&Arc<ItemNote>> = notes.iter().collect();
    sorted.sort_by_key(|n| n.at_ms);
    for note in sorted {
        let at = anchors
            .iter()
            .find(|(_, turn_at)| *turn_at > note.at_ms)
            .map_or(rows.len(), |(pos, _)| *pos);
        if let Some(slot) = before.get_mut(at) {
            slot.push(Arc::clone(note));
        }
    }
    let mut out = Vec::with_capacity(rows.len() + notes.len());
    let mut slots = before.into_iter();
    for row in rows {
        let anchor_abs_idx = row.span().start();
        for note in slots.next().unwrap_or_default() {
            out.push(DisplayRow::ItemNote {
                anchor_abs_idx,
                note,
            });
        }
        out.push(row);
    }
    for note in slots.next().unwrap_or_default() {
        out.push(DisplayRow::ItemNote {
            anchor_abs_idx: end_abs.saturating_sub(1),
            note,
        });
    }
    out
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WorkCounts {
    pub read: usize,
    pub edit: usize,
    pub search: usize,
    pub command: usize,
    pub other: usize,
    pub reasoning: usize,
    pub tool_calls: usize,
    pub errors: usize,
}

#[derive(Clone, Debug)]
pub(super) struct WorkRun {
    pub span: RawSpan,
    pub steps: Vec<WorkStep>,
    pub counts: WorkCounts,
    pub category_counts: Vec<(ToolCategory, usize)>,
    pub latest_detail: String,
    pub changed_files: usize,
    pub diff_added: usize,
    pub diff_deleted: usize,
}

/// Returns true when `text` contains recognizable `unified/apply_patch` diff headers and lines.
pub(super) fn is_unified_diff(text: &str) -> bool {
    let mut has_marker = false;
    let mut has_delta = false;
    for line in text.lines() {
        if line.starts_with("@@")
            || line.starts_with("*** Begin Patch")
            || line.starts_with("diff --git ")
            || line.starts_with("+++ ")
            || line.starts_with("--- ")
        {
            has_marker = true;
        } else if (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"))
        {
            has_delta = true;
        }
        if has_marker && has_delta {
            return true;
        }
    }
    false
}

/// Counts added/removed lines and modified file headers in unified diffs.
pub(super) fn parse_diff_stats(text: &str, files: &mut BTreeSet<String>) -> (usize, usize) {
    if !is_unified_diff(text) {
        return (0, 0);
    }
    let mut added = 0usize;
    let mut deleted = 0usize;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let clean = rest.trim().strip_prefix("b/").unwrap_or(rest.trim());
            if !clean.is_empty() && clean != "/dev/null" {
                files.insert(clean.to_owned());
            }
        } else if let Some(rest) = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Add File: "))
            .or_else(|| line.strip_prefix("*** Delete File: "))
        {
            let clean = rest.trim();
            if !clean.is_empty() {
                files.insert(clean.to_owned());
            }
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            deleted += 1;
        }
    }
    (added, deleted)
}

impl WorkRun {
    fn from_slice(span: RawSpan, slice: &[Arc<ChatItem>]) -> Self {
        let mut steps: Vec<WorkStep> = Vec::with_capacity(slice.len());
        let mut counts = WorkCounts::default();
        let mut category_counts: Vec<(ToolCategory, usize)> = Vec::new();
        let mut tool_detail = None;
        let mut thought_detail = None;
        let mut files = BTreeSet::new();
        let mut diff_added = 0usize;
        let mut diff_deleted = 0usize;

        for (offset, item) in slice.iter().enumerate() {
            let abs_idx = span.start() + offset;
            match item.kind {
                ChatItemKind::ToolCall => {
                    counts.tool_calls += 1;
                    let category = ToolCategory::from_name_and_title(&item.tool_name, &item.title);
                    match category {
                        ToolCategory::Read => counts.read += 1,
                        ToolCategory::Edit => {
                            counts.edit += 1;
                            if !item.file_path.trim().is_empty() {
                                files.insert(item.file_path.trim().to_owned());
                            }
                        }
                        ToolCategory::Search | ToolCategory::ListFiles => counts.search += 1,
                        ToolCategory::Command => counts.command += 1,
                        ToolCategory::WebFetch
                        | ToolCategory::WebSearch
                        | ToolCategory::Agent
                        | ToolCategory::Todo
                        | ToolCategory::Mcp
                        | ToolCategory::Other => counts.other += 1,
                    }
                    let (a, d) = parse_diff_stats(&item.text, &mut files);
                    diff_added += a;
                    diff_deleted += d;
                    if let Some((_, count)) =
                        category_counts.iter_mut().find(|(cat, _)| *cat == category)
                    {
                        *count += 1;
                    } else {
                        category_counts.push((category, 1));
                    }
                    if item.is_error {
                        counts.errors += 1;
                    }
                    let detail = call_detail(item);
                    if !detail.is_empty() {
                        tool_detail = Some(detail);
                    }
                    steps.push(WorkStep::Tool(ToolPair::CallOnly {
                        call_abs_idx: abs_idx,
                        call: Arc::clone(item),
                    }));
                }
                ChatItemKind::ToolResult => {
                    let matched_step = if item.tool_id.is_empty() {
                        steps
                            .iter()
                            .position(|step| {
                                matches!(
                                    step,
                                    WorkStep::Tool(ToolPair::CallOnly { call, .. })
                                        if call.tool_id.is_empty()
                                )
                            })
                            .or_else(|| {
                                steps.iter().position(|step| {
                                    matches!(step, WorkStep::Tool(ToolPair::CallOnly { .. }))
                                })
                            })
                    } else {
                        steps.iter().position(|step| {
                            matches!(
                                step,
                                WorkStep::Tool(ToolPair::CallOnly { call, .. })
                                    if call.tool_id == item.tool_id
                            )
                        })
                    };

                    if let Some(step_idx) = matched_step
                        && let Some(WorkStep::Tool(pair)) = steps.get_mut(step_idx)
                        && let ToolPair::CallOnly { call_abs_idx, call } = pair
                    {
                        let call_abs_idx = *call_abs_idx;
                        let call = Arc::clone(call);
                        if item.is_error && !call.is_error {
                            counts.errors += 1;
                        }
                        *pair = ToolPair::CallAndResult {
                            call_abs_idx,
                            call,
                            result_abs_idx: abs_idx,
                            result: Arc::clone(item),
                        };
                    } else {
                        if item.is_error {
                            counts.errors += 1;
                        }
                        let detail = orphan_result_detail(item);
                        if !detail.is_empty() {
                            tool_detail = Some(detail);
                        }
                        steps.push(WorkStep::Tool(ToolPair::OrphanResult {
                            result_abs_idx: abs_idx,
                            result: Arc::clone(item),
                        }));
                    }
                }
                ChatItemKind::Reasoning => {
                    counts.reasoning += 1;
                    let detail = first_non_empty_line(&item.text);
                    if !detail.is_empty() {
                        thought_detail = Some(detail);
                    }
                    steps.push(WorkStep::Reasoning {
                        abs_idx,
                        item: Arc::clone(item),
                    });
                }
                ChatItemKind::Assistant => {
                    steps.push(WorkStep::Note {
                        abs_idx,
                        item: Arc::clone(item),
                    });
                }
                _ => {}
            }
        }

        Self {
            span,
            steps,
            counts,
            category_counts,
            latest_detail: tool_detail.or(thought_detail).unwrap_or("").to_owned(),
            changed_files: files.len(),
            diff_added,
            diff_deleted,
        }
    }

    pub(super) fn diff_summary_label(&self) -> Option<String> {
        if self.changed_files == 0 && self.diff_added == 0 && self.diff_deleted == 0 {
            return None;
        }
        Some(ctext::RUN_CHANGED_FILES.format(&[
            ("files", &self.changed_files.max(1).to_string()),
            ("added", &self.diff_added.to_string()),
            ("deleted", &self.diff_deleted.to_string()),
        ]))
    }

    #[cfg(test)]
    pub(super) fn summary_label(&self) -> String {
        self.header_label(false)
    }

    pub(super) fn header_label(&self, is_live: bool) -> String {
        if self.counts.tool_calls == 0 {
            if self.counts.reasoning > 0 {
                return if is_live {
                    ctext::RUN_REASONING_LIVE.get().to_owned()
                } else {
                    ctext::RUN_REASONING.format(&[("n", &self.counts.reasoning.to_string())])
                };
            }
            return ctext::RUN_ACTIVITY.get().to_owned();
        }
        if self.category_counts.is_empty() {
            return ctext::RUN_ACTIVITY.get().to_owned();
        }
        let mut parts = Vec::with_capacity(self.category_counts.len());
        let last_ix = self.category_counts.len() - 1;
        for (ix, &(category, count)) in self.category_counts.iter().enumerate() {
            let clause = category.summary_clause(count, is_live && ix == last_ix);
            parts.push(if ix == 0 {
                clause
            } else {
                lowercase_ascii_initial(clause)
            });
        }
        parts.join(", ")
    }

    pub(super) fn header_icon(&self) -> IconName {
        self.category_counts
            .first()
            .map_or(IconName::Brain, |(cat, _)| cat.icon())
    }

    pub(super) fn failed_count_label(&self) -> Option<String> {
        (self.counts.errors > 0)
            .then(|| ctext::RUN_FAILED_N.format(&[("n", &self.counts.errors.to_string())]))
    }

    pub(super) fn total_tools(&self) -> usize {
        self.counts.tool_calls
    }

    pub(super) fn completed_tools(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| matches!(step, WorkStep::Tool(ToolPair::CallAndResult { .. })))
            .count()
    }

    pub(super) fn progress_label(&self, is_live: bool) -> Option<String> {
        let total = self.total_tools();
        (is_live && total > 0).then(|| format!("{}/{}", self.completed_tools(), total))
    }

    pub(super) fn active_detail(&self) -> &str {
        for step in self.steps.iter().rev() {
            if let WorkStep::Tool(ToolPair::CallOnly { call, .. }) = step {
                let detail = call_detail(call);
                if !detail.is_empty() {
                    return detail;
                }
            }
        }
        self.latest_detail.as_str()
    }
}

#[derive(Clone, Debug)]
pub(super) enum ToolPair {
    CallOnly {
        call_abs_idx: usize,
        call: Arc<ChatItem>,
    },
    CallAndResult {
        call_abs_idx: usize,
        call: Arc<ChatItem>,
        result_abs_idx: usize,
        result: Arc<ChatItem>,
    },
    OrphanResult {
        result_abs_idx: usize,
        result: Arc<ChatItem>,
    },
}

#[derive(Clone, Debug)]
pub(super) enum WorkStep {
    Tool(ToolPair),
    Reasoning { abs_idx: usize, item: Arc<ChatItem> },
    Note { abs_idx: usize, item: Arc<ChatItem> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StepBodySection<'a> {
    pub abs_idx: usize,
    pub label: Option<&'static str>,
    pub text: &'a str,
}

impl WorkStep {
    pub(super) fn primary_abs_idx(&self) -> usize {
        match self {
            Self::Tool(
                ToolPair::CallOnly { call_abs_idx, .. }
                | ToolPair::CallAndResult { call_abs_idx, .. },
            ) => *call_abs_idx,
            Self::Tool(ToolPair::OrphanResult { result_abs_idx, .. }) => *result_abs_idx,
            Self::Reasoning { abs_idx, .. } | Self::Note { abs_idx, .. } => *abs_idx,
        }
    }

    pub(super) fn is_error(&self) -> bool {
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. }) => call.is_error,
            Self::Tool(ToolPair::CallAndResult { call, result, .. }) => {
                call.is_error || result.is_error
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => result.is_error,
            Self::Reasoning { .. } | Self::Note { .. } => false,
        }
    }

    pub(super) fn is_pending_call(&self) -> bool {
        matches!(self, Self::Tool(ToolPair::CallOnly { .. }))
    }

    pub(super) fn icon(&self) -> IconName {
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. } | ToolPair::CallAndResult { call, .. }) => {
                ToolCategory::from_name_and_title(&call.tool_name, &call.title).icon()
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => {
                ToolCategory::from_name_and_title(&result.tool_name, &result.title).icon()
            }
            Self::Reasoning { .. } => IconName::Brain,
            Self::Note { .. } => IconName::MessageSquareMore,
        }
    }

    pub(super) fn verb(&self, run_is_live: bool) -> &'static str {
        let failed = self.is_error();
        let active = run_is_live && self.is_pending_call();
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. } | ToolPair::CallAndResult { call, .. }) => {
                ToolCategory::from_name_and_title(&call.tool_name, &call.title).verb(active, failed)
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => {
                ToolCategory::from_name_and_title(&result.tool_name, &result.title)
                    .verb(active, failed)
            }
            Self::Reasoning { .. } => ctext::REASONING.get(),
            Self::Note { .. } => ctext::STEP_NOTE.get(),
        }
    }

    #[cfg(test)]
    pub(super) fn badge(&self) -> (ui::BadgeTone, &'static str) {
        if self.is_error() {
            return (ui::BadgeTone::Destructive, ctext::TOOL_ERROR.get());
        }
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. } | ToolPair::CallAndResult { call, .. }) => {
                let kind = ToolStepKind::from_name_and_title(&call.tool_name, &call.title);
                (kind.tone(), kind.label())
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => {
                let kind = ToolStepKind::from_name_and_title(&result.tool_name, &result.title);
                (kind.tone(), kind.label())
            }
            Self::Reasoning { .. } | Self::Note { .. } => {
                (ui::BadgeTone::Muted, ctext::REASONING.get())
            }
        }
    }

    pub(super) fn headline(&self) -> String {
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. }) => {
                let detail = call_detail(call);
                if detail.is_empty() {
                    ctext::TOOL_CALL.get().to_owned()
                } else {
                    detail.to_owned()
                }
            }
            Self::Tool(ToolPair::CallAndResult { call, result, .. }) => {
                let detail = call_detail(call);
                if detail.is_empty() {
                    let fallback = orphan_result_detail(result);
                    if fallback.is_empty() {
                        ctext::TOOL_CALL.get().to_owned()
                    } else {
                        fallback.to_owned()
                    }
                } else {
                    detail.to_owned()
                }
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => {
                let title = first_non_empty_line(&result.title);
                if title.is_empty() {
                    let detail = orphan_result_detail(result);
                    if detail.is_empty() {
                        ctext::TOOL_RESULT.get().to_owned()
                    } else {
                        detail.to_owned()
                    }
                } else {
                    title.to_owned()
                }
            }
            Self::Reasoning { item, .. } => {
                let line = first_non_empty_line(&item.text);
                if line.is_empty() {
                    let title = first_non_empty_line(&item.title);
                    if title.is_empty() {
                        ctext::REASONING.get().to_owned()
                    } else {
                        title.to_owned()
                    }
                } else {
                    line.to_owned()
                }
            }
            Self::Note { item, .. } => item.text.trim().to_owned(),
        }
    }

    pub(super) fn file_path(&self) -> &str {
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. }) => call.file_path.trim(),
            Self::Tool(ToolPair::CallAndResult { call, result, .. }) => {
                let call_path = call.file_path.trim();
                if call_path.is_empty() {
                    result.file_path.trim()
                } else {
                    call_path
                }
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => result.file_path.trim(),
            Self::Reasoning { .. } | Self::Note { .. } => "",
        }
    }

    pub(super) fn body_sections(&self) -> Vec<StepBodySection<'_>> {
        match self {
            Self::Tool(ToolPair::CallOnly { call_abs_idx, call }) => {
                if call.text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![StepBodySection {
                        abs_idx: *call_abs_idx,
                        label: Some(ctext::STEP_INPUT.get()),
                        text: &call.text,
                    }]
                }
            }
            Self::Tool(ToolPair::CallAndResult {
                call_abs_idx,
                call,
                result_abs_idx,
                result,
            }) => {
                let mut sections = Vec::with_capacity(2);
                let call_trimmed = call.text.trim();
                let result_trimmed = result.text.trim();
                let headline = self.headline();
                let suppress_input = !result_trimmed.is_empty()
                    && (call_trimmed.is_empty()
                        || call_trimmed == call.file_path.trim()
                        || call_trimmed == headline);
                if !call_trimmed.is_empty() && !suppress_input {
                    sections.push(StepBodySection {
                        abs_idx: *call_abs_idx,
                        label: Some(ctext::STEP_INPUT.get()),
                        text: &call.text,
                    });
                }
                if !result_trimmed.is_empty() {
                    sections.push(StepBodySection {
                        abs_idx: *result_abs_idx,
                        label: Some(if result.is_error {
                            ctext::TOOL_ERROR.get()
                        } else {
                            ctext::STEP_OUTPUT.get()
                        }),
                        text: &result.text,
                    });
                }
                sections
            }
            Self::Tool(ToolPair::OrphanResult {
                result_abs_idx,
                result,
            }) => {
                if result.text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![StepBodySection {
                        abs_idx: *result_abs_idx,
                        label: None,
                        text: &result.text,
                    }]
                }
            }
            Self::Reasoning { abs_idx, item } => {
                if item.text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![StepBodySection {
                        abs_idx: *abs_idx,
                        label: None,
                        text: &item.text,
                    }]
                }
            }
            Self::Note { .. } => Vec::new(),
        }
    }

    pub(super) fn copy_text(&self) -> String {
        let mut parts = Vec::new();
        for section in self.body_sections() {
            let cleaned = krill_term::select::clean_text(section.text);
            if !cleaned.is_empty() {
                parts.push(cleaned);
            }
        }
        parts.join("\n\n")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum ToolCategory {
    Read,
    Edit,
    ListFiles,
    Search,
    Command,
    WebFetch,
    WebSearch,
    Agent,
    Todo,
    Mcp,
    Other,
}

impl ToolCategory {
    pub(super) fn from_name_and_title(name: &str, title: &str) -> Self {
        let n = name.trim().to_ascii_lowercase();
        let t = title.trim().to_ascii_lowercase();
        if n.starts_with("mcp__") || n.starts_with("mcp:") {
            Self::Mcp
        } else if n.contains("websearch")
            || n.contains("web_search")
            || n == "google_search"
            || (n.contains("search") && t.contains("web"))
        {
            Self::WebSearch
        } else if n.contains("webfetch") || n.contains("web_fetch") || n == "fetch" {
            Self::WebFetch
        } else if n == "agent"
            || n == "task"
            || n.contains("subagent")
            || n.contains("dispatch_agent")
        {
            Self::Agent
        } else if n.contains("todo") || n == "task_write" || t.contains("todo") {
            Self::Todo
        } else if n == "ls" || n.contains("list_dir") || n.contains("list_files") {
            Self::ListFiles
        } else if n.contains("read")
            || n.contains("view")
            || n == "cat"
            || n == "open_file"
            || t.contains("read")
            || t.contains("view")
        {
            Self::Read
        } else if n.contains("edit")
            || n.contains("write")
            || n.contains("patch")
            || n.contains("replace")
            || t.contains("edit")
            || t.contains("write")
        {
            Self::Edit
        } else if n.contains("grep")
            || n.contains("glob")
            || n.contains("search")
            || n.contains("find")
            || n == "rg"
            || t.contains("search")
            || t.contains("grep")
        {
            Self::Search
        } else if n.contains("bash")
            || n.contains("shell")
            || n.contains("exec")
            || n.contains("command")
            || t.contains("run")
            || t.contains("command")
        {
            Self::Command
        } else {
            Self::Other
        }
    }

    pub(super) fn icon(self) -> IconName {
        match self {
            Self::Read => IconName::Eye,
            Self::Edit => IconName::Pencil,
            Self::ListFiles => IconName::Folder,
            Self::Search => IconName::Search,
            Self::Command => IconName::SquareTerminal,
            Self::WebFetch | Self::WebSearch => IconName::Globe,
            Self::Agent => IconName::Bot,
            Self::Todo => IconName::ListChecks,
            Self::Mcp => IconName::Plug,
            Self::Other => IconName::Wrench,
        }
    }

    fn summary_clause(self, count: usize, is_live: bool) -> String {
        let count_str = count.to_string();
        match (self, is_live, count == 1) {
            (Self::Read, true, true) => ctext::RUN_READ_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::Read, true, false) => ctext::RUN_READ_LIVE_MANY.format(&[("n", &count_str)]),
            (Self::Read, false, true) => ctext::RUN_READ_ONE.format(&[("n", &count_str)]),
            (Self::Read, false, false) => ctext::RUN_READ_MANY.format(&[("n", &count_str)]),
            (Self::Edit, true, true) => ctext::RUN_EDIT_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::Edit, true, false) => ctext::RUN_EDIT_LIVE_MANY.format(&[("n", &count_str)]),
            (Self::Edit, false, true) => ctext::RUN_EDIT_ONE.format(&[("n", &count_str)]),
            (Self::Edit, false, false) => ctext::RUN_EDIT_MANY.format(&[("n", &count_str)]),
            (Self::ListFiles, true, true) => ctext::RUN_LIST_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::ListFiles, true, false) => {
                ctext::RUN_LIST_LIVE_MANY.format(&[("n", &count_str)])
            }
            (Self::ListFiles, false, true) => ctext::RUN_LIST_ONE.format(&[("n", &count_str)]),
            (Self::ListFiles, false, false) => ctext::RUN_LIST_MANY.format(&[("n", &count_str)]),
            (Self::Search, true, true) => ctext::RUN_SEARCH_LIVE.get().to_owned(),
            (Self::Search, true, false) => ctext::RUN_SEARCH_LIVE_MANY.format(&[("n", &count_str)]),
            (Self::Search, false, true) => ctext::RUN_SEARCH_ONE.format(&[("n", &count_str)]),
            (Self::Search, false, false) => ctext::RUN_SEARCH_MANY.format(&[("n", &count_str)]),
            (Self::Command, true, true) => ctext::RUN_COMMAND_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::Command, true, false) => {
                ctext::RUN_COMMAND_LIVE_MANY.format(&[("n", &count_str)])
            }
            (Self::Command, false, true) => ctext::RUN_COMMAND_ONE.format(&[("n", &count_str)]),
            (Self::Command, false, false) => ctext::RUN_COMMAND_MANY.format(&[("n", &count_str)]),
            (Self::WebFetch, true, true) => ctext::RUN_FETCH_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::WebFetch, true, false) => {
                ctext::RUN_FETCH_LIVE_MANY.format(&[("n", &count_str)])
            }
            (Self::WebFetch, false, true) => ctext::RUN_FETCH_ONE.format(&[("n", &count_str)]),
            (Self::WebFetch, false, false) => ctext::RUN_FETCH_MANY.format(&[("n", &count_str)]),
            (Self::WebSearch, true, _) => ctext::RUN_WEB_LIVE.get().to_owned(),
            (Self::WebSearch, false, true) => ctext::RUN_WEB_ONE.format(&[("n", &count_str)]),
            (Self::WebSearch, false, false) => ctext::RUN_WEB_MANY.format(&[("n", &count_str)]),
            (Self::Agent, true, true) => ctext::RUN_AGENT_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::Agent, true, false) => ctext::RUN_AGENT_LIVE_MANY.format(&[("n", &count_str)]),
            (Self::Agent, false, true) => ctext::RUN_AGENT_ONE.format(&[("n", &count_str)]),
            (Self::Agent, false, false) => ctext::RUN_AGENT_MANY.format(&[("n", &count_str)]),
            (Self::Todo, true, true) => ctext::RUN_TODO_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::Todo, true, false) => ctext::RUN_TODO_LIVE_MANY.format(&[("n", &count_str)]),
            (Self::Todo, false, true) => ctext::RUN_TODO_ONE.format(&[("n", &count_str)]),
            (Self::Todo, false, false) => ctext::RUN_TODO_MANY.format(&[("n", &count_str)]),
            (Self::Mcp, true, true) => ctext::RUN_MCP_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::Mcp, true, false) => ctext::RUN_MCP_LIVE_MANY.format(&[("n", &count_str)]),
            (Self::Mcp, false, true) => ctext::RUN_MCP_ONE.format(&[("n", &count_str)]),
            (Self::Mcp, false, false) => ctext::RUN_MCP_MANY.format(&[("n", &count_str)]),
            (Self::Other, true, true) => ctext::RUN_TOOL_LIVE_ONE.format(&[("n", &count_str)]),
            (Self::Other, true, false) => ctext::RUN_TOOL_LIVE_MANY.format(&[("n", &count_str)]),
            (Self::Other, false, true) => ctext::RUN_TOOL_ONE.format(&[("n", &count_str)]),
            (Self::Other, false, false) => ctext::RUN_TOOL_MANY.format(&[("n", &count_str)]),
        }
    }

    fn verb(self, is_live: bool, is_error: bool) -> &'static str {
        match (self, is_error, is_live) {
            (Self::Read, true, _) => ctext::VERB_READ_FAIL.get(),
            (Self::Read, false, true) => ctext::VERB_READ_LIVE.get(),
            (Self::Read, false, false) => ctext::VERB_READ_DONE.get(),
            (Self::Edit, true, _) => ctext::VERB_EDIT_FAIL.get(),
            (Self::Edit, false, true) => ctext::VERB_EDIT_LIVE.get(),
            (Self::Edit, false, false) => ctext::VERB_EDIT_DONE.get(),
            (Self::ListFiles, true, _) => ctext::VERB_LIST_FAIL.get(),
            (Self::ListFiles, false, true) => ctext::VERB_LIST_LIVE.get(),
            (Self::ListFiles, false, false) => ctext::VERB_LIST_DONE.get(),
            (Self::Search, true, _) => ctext::VERB_SEARCH_FAIL.get(),
            (Self::Search, false, true) => ctext::VERB_SEARCH_LIVE.get(),
            (Self::Search, false, false) => ctext::VERB_SEARCH_DONE.get(),
            (Self::Command, true, _) => ctext::VERB_RUN_FAIL.get(),
            (Self::Command, false, true) => ctext::VERB_RUN_LIVE.get(),
            (Self::Command, false, false) => ctext::VERB_RUN_DONE.get(),
            (Self::WebFetch, true, _) => ctext::VERB_FETCH_FAIL.get(),
            (Self::WebFetch, false, true) => ctext::VERB_FETCH_LIVE.get(),
            (Self::WebFetch, false, false) => ctext::VERB_FETCH_DONE.get(),
            (Self::WebSearch, true, _) => ctext::VERB_WEB_FAIL.get(),
            (Self::WebSearch, false, true) => ctext::VERB_WEB_LIVE.get(),
            (Self::WebSearch, false, false) => ctext::VERB_WEB_DONE.get(),
            (Self::Agent, true, _) => ctext::VERB_AGENT_FAIL.get(),
            (Self::Agent, false, true) => ctext::VERB_AGENT_LIVE.get(),
            (Self::Agent, false, false) => ctext::VERB_AGENT_DONE.get(),
            (Self::Todo, true, _) => ctext::VERB_TODO_FAIL.get(),
            (Self::Todo, false, true) => ctext::VERB_TODO_LIVE.get(),
            (Self::Todo, false, false) => ctext::VERB_TODO_DONE.get(),
            (Self::Mcp, true, _) => ctext::VERB_MCP_FAIL.get(),
            (Self::Mcp, false, true) => ctext::VERB_MCP_LIVE.get(),
            (Self::Mcp, false, false) => ctext::VERB_MCP_DONE.get(),
            (Self::Other, true, _) => ctext::VERB_OTHER_FAIL.get(),
            (Self::Other, false, true) => ctext::VERB_OTHER_LIVE.get(),
            (Self::Other, false, false) => ctext::VERB_OTHER_DONE.get(),
        }
    }
}

fn lowercase_ascii_initial(text: String) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return text;
    };
    if first.is_ascii_uppercase() {
        let mut out = String::with_capacity(text.len());
        out.push(first.to_ascii_lowercase());
        out.push_str(chars.as_str());
        out
    } else {
        text
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum ToolStepKind {
    Read,
    Edit,
    Search,
    Command,
    Other,
}

#[cfg(test)]
impl ToolStepKind {
    fn from_name_and_title(name: &str, title: &str) -> Self {
        match ToolCategory::from_name_and_title(name, title) {
            ToolCategory::Read => Self::Read,
            ToolCategory::Edit => Self::Edit,
            ToolCategory::Search | ToolCategory::ListFiles => Self::Search,
            ToolCategory::Command => Self::Command,
            ToolCategory::WebFetch
            | ToolCategory::WebSearch
            | ToolCategory::Agent
            | ToolCategory::Todo
            | ToolCategory::Mcp
            | ToolCategory::Other => Self::Other,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Read => ctext::STEP_READ.get(),
            Self::Edit => ctext::STEP_EDIT.get(),
            Self::Search => ctext::STEP_SEARCH.get(),
            Self::Command => ctext::STEP_COMMAND.get(),
            Self::Other => ctext::STEP_OTHER.get(),
        }
    }

    fn tone(self) -> ui::BadgeTone {
        match self {
            Self::Edit => ui::BadgeTone::Warning,
            Self::Read | Self::Search | Self::Command | Self::Other => ui::BadgeTone::Neutral,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct WorkRunTiming {
    started_at: Instant,
    completed_at: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct WorkRunTimingState {
    runs: HashMap<usize, WorkRunTiming>,
}

impl WorkRunTimingState {
    pub(super) fn clear(&mut self) {
        self.runs.clear();
    }

    pub(super) fn sync(&mut self, feed: &DisplayFeed, agent_state: AgentState, now: Instant) {
        let rows = feed.rows();
        let last_ix = rows.len().saturating_sub(1);
        let mut live_keys = Vec::new();
        for (ix, row) in rows.iter().enumerate() {
            let DisplayRow::WorkRun(run) = row else {
                continue;
            };
            let key = run.span.start();
            live_keys.push(key);
            let is_live = ix == last_ix && agent_state == AgentState::Working;
            if let Some(entry) = self.runs.get_mut(&key) {
                if is_live {
                    entry.completed_at = None;
                } else if entry.completed_at.is_none() {
                    entry.completed_at = Some(now);
                }
            } else if is_live {
                self.runs.insert(
                    key,
                    WorkRunTiming {
                        started_at: now,
                        completed_at: None,
                    },
                );
            }
        }
        self.runs.retain(|key, _| live_keys.contains(key));
    }

    pub(super) fn migrate_head_drain(&mut self, old_run_abs_start: Option<usize>, new_base: usize) {
        if let Some(old) = old_run_abs_start
            && old < new_base
            && let Some(timing) = self.runs.remove(&old)
        {
            self.runs.entry(new_base).or_insert(timing);
        }
        self.runs.retain(|&key, _| key >= new_base);
    }

    pub(super) fn elapsed_secs(&self, run_start: usize, now: Instant) -> Option<u64> {
        let timing = self.runs.get(&run_start)?;
        let end = timing.completed_at.unwrap_or(now);
        Some(end.saturating_duration_since(timing.started_at).as_secs())
    }
}

pub(super) fn format_elapsed_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let mins = secs / 60;
        let rem = secs % 60;
        if rem == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m {rem}s")
        }
    } else {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {mins}m")
        }
    }
}

fn is_turn_boundary(kind: ChatItemKind) -> bool {
    matches!(
        kind,
        ChatItemKind::User | ChatItemKind::Question | ChatItemKind::Permission
    )
}

pub(super) fn is_foldable_preamble(item: &ChatItem) -> bool {
    if item.kind != ChatItemKind::Assistant {
        return false;
    }
    let trimmed = item.text.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= MAX_PREAMBLE_CHARS
        && !trimmed.contains('\n')
        && !trimmed.contains("```")
        && item.file_path.trim().is_empty()
}

pub(super) fn compute_foldable_notes(items: &[Arc<ChatItem>]) -> Vec<bool> {
    let mut foldable = vec![false; items.len()];
    let mut seen_tool_ahead = false;
    for (idx, item) in items.iter().enumerate().rev() {
        if is_turn_boundary(item.kind) {
            seen_tool_ahead = false;
            continue;
        }
        if matches!(item.kind, ChatItemKind::ToolCall | ChatItemKind::ToolResult) {
            seen_tool_ahead = true;
            continue;
        }
        if item.kind == ChatItemKind::Assistant {
            if seen_tool_ahead && is_foldable_preamble(item) {
                foldable[idx] = true;
            } else {
                seen_tool_ahead = false;
            }
        }
    }
    foldable
}

fn is_work_or_foldable(kind: ChatItemKind, folded: bool) -> bool {
    is_work_kind(kind) || folded
}

pub(super) fn straddling_run_rel_start(items: &[Arc<ChatItem>], excess: usize) -> Option<usize> {
    if excess == 0 || excess >= items.len() {
        return None;
    }
    let foldable = compute_foldable_notes(items);
    let mut rel_idx = 0;
    while rel_idx < items.len() {
        if !is_work_or_foldable(items[rel_idx].kind, foldable[rel_idx]) {
            rel_idx += 1;
            continue;
        }
        let mut rel_end = rel_idx + 1;
        while rel_end < items.len() && is_work_or_foldable(items[rel_end].kind, foldable[rel_end]) {
            rel_end += 1;
        }
        let has_tool = items[rel_idx..rel_end].iter().any(|entry| {
            matches!(
                entry.kind,
                ChatItemKind::ToolCall | ChatItemKind::ToolResult
            )
        });
        if (has_tool || rel_end - rel_idx > 1) && rel_idx < excess && excess < rel_end {
            return Some(rel_idx);
        }
        rel_idx = rel_end;
    }
    None
}

pub(super) fn is_work_kind(kind: ChatItemKind) -> bool {
    matches!(
        kind,
        ChatItemKind::ToolCall | ChatItemKind::ToolResult | ChatItemKind::Reasoning
    )
}

pub(super) fn first_non_empty_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map_or("", str::trim)
}

fn call_detail(call: &ChatItem) -> &str {
    for candidate in [
        first_non_empty_line(&call.title),
        first_non_empty_line(&call.file_path),
        first_non_empty_line(&call.text),
        first_non_empty_line(&call.tool_name),
    ] {
        if !candidate.is_empty() {
            return candidate;
        }
    }
    ""
}

fn orphan_result_detail(result: &ChatItem) -> &str {
    let path = first_non_empty_line(&result.file_path);
    if path.is_empty() {
        first_non_empty_line(&result.text)
    } else {
        path
    }
}

#[cfg(test)]
mod preview_and_spacing_tests {
    use super::*;

    #[test]
    fn activity_edges_only_join_notices_and_one_line_item_notes() {
        let notice = DisplayRow::Notice {
            abs_idx: 0,
            notice: Arc::new(InboundNotice {
                kind: NoticeKind::Message,
                from: vec!["Scout".into()],
                detail: "hello".into(),
            }),
        };
        assert_eq!(notice.activity_edges(), (true, true));

        let started_note = DisplayRow::ItemNote {
            anchor_abs_idx: 0,
            note: Arc::new(ItemNote {
                at_ms: 1,
                actor: NoteActor::Bot,
                action: NoteAction::Status(krill_proto::bots::BotItemStatus::Working),
                status: krill_proto::bots::BotItemStatus::Working,
                title: "Audit".into(),
                detail: String::new(),
            }),
        };
        assert_eq!(started_note.activity_edges(), (true, true));

        let memory_note = DisplayRow::ItemNote {
            anchor_abs_idx: 0,
            note: Arc::new(ItemNote {
                at_ms: 1,
                actor: NoteActor::Bot,
                action: NoteAction::Memory(MemoryReceipt {
                    added: vec!["fact".into()],
                    removed: vec![],
                    other_changes: false,
                }),
                status: krill_proto::bots::BotItemStatus::Done,
                title: String::new(),
                detail: String::new(),
            }),
        };
        assert_eq!(memory_note.activity_edges(), (false, false));

        let msg = DisplayRow::Item {
            abs_idx: 1,
            item: Arc::new(ChatItem {
                kind: ChatItemKind::Assistant,
                text: "Done".into(),
                ..Default::default()
            }),
        };
        assert_eq!(msg.activity_edges(), (false, false));
    }

    #[test]
    fn agent_message_preview_line_matches_orca_spec() {
        // 1. Plain single line -> no `more`
        assert_eq!(
            agent_message_preview_line("All green on main."),
            AgentMessagePreviewLine {
                line: "All green on main.".into(),
                more: false,
            }
        );

        // 2. Multiple lines -> first line + `more: true`
        assert_eq!(
            agent_message_preview_line("First line\nSecond line"),
            AgentMessagePreviewLine {
                line: "First line".into(),
                more: true,
            }
        );

        // 3. Strips headings, quotes, bullets, numbered lists, checkboxes
        assert_eq!(
            agent_message_preview_line("> ## - [x] Ship release"),
            AgentMessagePreviewLine {
                line: "Ship release".into(),
                more: false,
            }
        );
        assert_eq!(
            agent_message_preview_line("1. First step"),
            AgentMessagePreviewLine {
                line: "First step".into(),
                more: false,
            }
        );

        // 4. Strips markdown links & images, including nested parens in URLs
        assert_eq!(
            agent_message_preview_line(
                "See [Rust (lang)](https://en.wikipedia.org/wiki/Rust_(programming_language)) and ![shot](https://example.com/a.png)"
            ),
            AgentMessagePreviewLine {
                line: "See Rust (lang) and shot".into(),
                more: false,
            }
        );

        // 5. Strips paired **, __, ~~, inline code while keeping glob stars `src/**/*.ts`
        assert_eq!(
            agent_message_preview_line(
                "Updated **auth** and __core__ with `src/**/*.ts` and ~~old~~"
            ),
            AgentMessagePreviewLine {
                line: "Updated auth and core with src/**/*.ts and old".into(),
                more: false,
            }
        );

        // 6. Keeps math `5 * 3` and snake_case `term_0`, `snake_case_name`
        assert_eq!(
            agent_message_preview_line(
                "Check *now* and 5 * 3 on term_0 and snake_case_name with _italics_"
            ),
            AgentMessagePreviewLine {
                line: "Check now and 5 * 3 on term_0 and snake_case_name with italics".into(),
                more: false,
            }
        );

        // 7. Bare URL forces `more: true`
        assert_eq!(
            agent_message_preview_line("Check https://example.com/logs"),
            AgentMessagePreviewLine {
                line: "Check https://example.com/logs".into(),
                more: true,
            }
        );
        assert_eq!(
            agent_message_preview_line("Visit www.example.com"),
            AgentMessagePreviewLine {
                line: "Visit www.example.com".into(),
                more: true,
            }
        );

        // 8. Skips leading fenced code block and marks `more: true`
        assert_eq!(
            agent_message_preview_line("```rust\nlet x = 1;\n```\nSummary here"),
            AgentMessagePreviewLine {
                line: "Summary here".into(),
                more: true,
            }
        );

        // 9. Only fenced code -> empty preview line + `more: true`
        assert_eq!(
            agent_message_preview_line("```\nonly code\n```"),
            AgentMessagePreviewLine {
                line: String::new(),
                more: true,
            }
        );

        // 10. Clips at 200 code points safely on multibyte chars
        let long_kr = "가".repeat(220);
        let clipped = agent_message_preview_line(&long_kr);
        assert_eq!(clipped.line.chars().count(), 200);
        assert!(clipped.more);

        // 11. Empty/whitespace body
        assert_eq!(
            agent_message_preview_line("   \n  "),
            AgentMessagePreviewLine {
                line: String::new(),
                more: false,
            }
        );
    }
}
