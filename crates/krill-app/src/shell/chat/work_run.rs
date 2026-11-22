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
    pub(super) fn project_with_notes(
        base_index: usize,
        items: &[Arc<ChatItem>],
        unread_from: Option<usize>,
        notes: &[Arc<ItemNote>],
        turn_times: &[u64],
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
            rows = interleave_notes(rows, notes, turn_times, base_index + items.len());
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

fn item_epoch_secs(item: &ChatItem) -> Option<u64> {
    let raw = item.at.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = u64::try_from(krill_proto::time::parse_reset_epoch(raw)?).ok()?;
    Some(if parsed > 10_000_000_000 {
        parsed / 1_000
    } else {
        parsed
    })
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

fn interleave_notes(
    rows: Vec<DisplayRow>,
    notes: &[Arc<ItemNote>],
    turn_times: &[u64],
    end_abs: usize,
) -> Vec<DisplayRow> {
    // Owner turns, oldest first, paired with their times from the newest end.
    let users: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(pos, row)| match row {
            DisplayRow::Item { item, .. } if item.kind == ChatItemKind::User => Some(pos),
            _ => None,
        })
        .collect();
    let anchors: Vec<(usize, u64)> = users
        .iter()
        .rev()
        .zip(turn_times)
        .map(|(&pos, &at)| (pos, at))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
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
