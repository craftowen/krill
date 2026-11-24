// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! The sidebar Bots section's pure parts, as Orca's `shared/bots` has them: "Bot options"
//! (sort and hide), the search's matching and marks, and which line a bot row says.

pub use krill_ui::bot_types::{BotSidebarSort, BotSidebarView};

/// What `arrange` reads of each bot.
#[derive(Debug, Clone, Copy)]
pub struct ArrangeFacts<'a> {
    pub name: &'a str,
    pub paused: bool,
    /// It or one of its workers waits on the owner.
    pub needs_you: bool,
    /// Its newest activity (or when it was made), ms.
    pub activity_at: u64,
}

/// Orca `arrangeBotSidebarEntries`: the indexes of the bots to list, in the options' order.
/// Bots come in saved (Manual) order, which breaks ties.
#[must_use]
pub fn arrange(bots: &[ArrangeFacts<'_>], view: BotSidebarView) -> Vec<usize> {
    let mut shown: Vec<usize> = (0..bots.len())
        .filter(|&i| !(view.hide_paused && bots[i].paused))
        .collect();
    match view.sort {
        BotSidebarSort::Manual => {}
        BotSidebarSort::Name => shown.sort_by_cached_key(|&i| bots[i].name.to_lowercase()),
        BotSidebarSort::Recent => shown.sort_by_key(|&i| std::cmp::Reverse(bots[i].activity_at)),
        BotSidebarSort::Smart => shown.sort_by_key(|&i| {
            (
                std::cmp::Reverse(bots[i].needs_you),
                std::cmp::Reverse(bots[i].activity_at),
            )
        }),
    }
    shown
}

/// One interleaved sidebar entry under the Bots section (`SidebarBotsSection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarEntry {
    Bot(usize),
    Group(usize),
}

/// Arrange combined bot + group rows in Manual/Smart/Recent/Name order.
#[must_use]
pub fn arrange_entries(
    bots: &[ArrangeFacts<'_>],
    groups: &[ArrangeFacts<'_>],
    view: BotSidebarView,
) -> Vec<SidebarEntry> {
    let mut entries: Vec<(SidebarEntry, &ArrangeFacts<'_>)> = bots
        .iter()
        .enumerate()
        .filter(|(_, f)| !(view.hide_paused && f.paused))
        .map(|(i, f)| (SidebarEntry::Bot(i), f))
        .chain(
            groups
                .iter()
                .enumerate()
                .filter(|(_, f)| !(view.hide_paused && f.paused))
                .map(|(i, f)| (SidebarEntry::Group(i), f)),
        )
        .collect();

    match view.sort {
        BotSidebarSort::Manual => {}
        BotSidebarSort::Name => entries.sort_by_cached_key(|(_, f)| f.name.to_lowercase()),
        BotSidebarSort::Recent => entries.sort_by_key(|(_, f)| std::cmp::Reverse(f.activity_at)),
        BotSidebarSort::Smart => entries.sort_by_key(|(_, f)| {
            (
                std::cmp::Reverse(f.needs_you),
                std::cmp::Reverse(f.activity_at),
            )
        }),
    }
    entries.into_iter().map(|(entry, _)| entry).collect()
}

/// A marked span, in byte offsets of the searched text.
pub type SearchRange = std::ops::Range<usize>;

/// Orca `matchBotSearchText`: the whole query as a substring, else every query word starting
/// a word of the text, in any order. Case-insensitive; `None` when it doesn't match.
// ponytail: lowercases only (no accent folding); add NFD folding if accented names matter.
#[must_use]
pub fn match_text(text: &str, query: &str) -> Option<Vec<SearchRange>> {
    let phrase = query.trim().to_lowercase();
    if phrase.is_empty() {
        return None;
    }
    // Lowercased text with each char's byte span in the original.
    let mut folded = String::new();
    let mut spans: Vec<SearchRange> = Vec::new();
    for (at, ch) in text.char_indices() {
        for low in ch.to_lowercase() {
            for _ in 0..low.len_utf8() {
                spans.push(at..at + ch.len_utf8());
            }
            folded.push(low);
        }
    }
    let source = |start: usize, end: usize| spans[start].start..spans[end - 1].end;
    if let Some(at) = folded.find(&phrase) {
        return Some(vec![source(at, at + phrase.len())]);
    }
    let tokens: Vec<&str> = phrase.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let mut word_starts: Vec<usize> = Vec::new();
    let mut prev_word = false;
    for (at, ch) in folded.char_indices() {
        let word = ch.is_alphanumeric();
        if word && !prev_word {
            word_starts.push(at);
        }
        prev_word = word;
    }
    let mut ranges = Vec::new();
    for token in tokens {
        let start = *word_starts.iter().find(|&&start| {
            folded
                .get(start..)
                .is_some_and(|rest| rest.starts_with(token))
        })?;
        ranges.push(source(start, start + token.len()));
    }
    Some(merge_ranges(ranges))
}

/// Sorted, with overlapping or touching ranges joined.
#[must_use]
pub fn merge_ranges(mut ranges: Vec<SearchRange>) -> Vec<SearchRange> {
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<SearchRange> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Orca `botPurposeSnippet`: a purpose whose first match is past 24 characters starts at the
/// word before it, after an "…".
#[must_use]
pub fn purpose_snippet(text: &str, ranges: &[SearchRange]) -> (String, Vec<SearchRange>) {
    const LEAD: usize = 24;
    let Some(first) = ranges.first() else {
        return (text.to_owned(), Vec::new());
    };
    let before = text.get(..first.start).unwrap_or_default();
    if before.chars().count() <= LEAD {
        return (text.to_owned(), ranges.to_vec());
    }
    let from = before
        .char_indices()
        .rev()
        .nth(LEAD - 1)
        .map_or(0, |(at, _)| at);
    let cut = before
        .get(from..)
        .and_then(|lead| lead.find(' '))
        .map_or(from, |space| from + space + 1);
    let ellipsis = "…".len();
    (
        format!("…{}", text.get(cut..).unwrap_or_default()),
        ranges
            .iter()
            .map(|r| r.start - cut + ellipsis..r.end - cut + ellipsis)
            .collect(),
    )
}

/// One search result: marks in the name, the purpose that matched, or a chat title/body hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub index: usize,
    pub name_ranges: Vec<SearchRange>,
    pub purpose: Option<(String, Vec<SearchRange>)>,
    /// Matched chat title or latest message snippet (title, snippet, marked ranges in snippet).
    pub chat_hit: Option<(String, String, Vec<SearchRange>)>,
}

/// Orca `createBotSidebarSearchIndex`: name matches, then purpose-only matches, each in the
/// list's order; and how many hidden paused bots match by name.
#[must_use]
pub fn search(
    shown: &[(&str, &str)],
    hidden_paused: &[&str],
    query: &str,
) -> (Vec<SearchMatch>, usize) {
    let candidates: Vec<SearchCandidate<'_>> = shown
        .iter()
        .map(|&(name, purpose)| SearchCandidate {
            name,
            purpose,
            chats: Vec::new(),
        })
        .collect();
    search_with_chats(&candidates, hidden_paused, query)
}

/// One bot candidate for sidebar search, including its live chat titles and latest status text.
#[derive(Debug, Clone)]
pub struct SearchCandidate<'a> {
    pub name: &'a str,
    pub purpose: &'a str,
    /// `(chat_title, latest_status_or_body)` pairs for this bot's chats.
    pub chats: Vec<(&'a str, Option<&'a str>)>,
}

/// Searches bot names, `SOUL.md`/purpose descriptions, and chat titles/bodies in Orca priority order.
#[must_use]
pub fn search_with_chats(
    shown: &[SearchCandidate<'_>],
    hidden_paused: &[&str],
    query: &str,
) -> (Vec<SearchMatch>, usize) {
    let mut by_name = Vec::new();
    let mut by_purpose = Vec::new();
    let mut by_chat = Vec::new();
    for (index, bot) in shown.iter().enumerate() {
        if let Some(name_ranges) = match_text(bot.name, query) {
            by_name.push(SearchMatch {
                index,
                name_ranges,
                purpose: None,
                chat_hit: None,
            });
        } else if let Some(ranges) = match_text(bot.purpose, query) {
            by_purpose.push(SearchMatch {
                index,
                name_ranges: Vec::new(),
                purpose: Some(purpose_snippet(bot.purpose, &ranges)),
                chat_hit: None,
            });
        } else {
            let mut hit = None;
            for &(chat_title, body) in &bot.chats {
                if let Some(ranges) = match_text(chat_title, query) {
                    hit = Some((chat_title.to_owned(), chat_title.to_owned(), ranges));
                    break;
                }
                if let Some(body) = body
                    && let Some(ranges) = match_text(body, query)
                {
                    let (snippet, snippet_ranges) = purpose_snippet(body, &ranges);
                    hit = Some((chat_title.to_owned(), snippet, snippet_ranges));
                    break;
                }
            }
            if let Some(chat_hit) = hit {
                by_chat.push(SearchMatch {
                    index,
                    name_ranges: Vec::new(),
                    purpose: None,
                    chat_hit: Some(chat_hit),
                });
            }
        }
    }
    let hidden = hidden_paused
        .iter()
        .filter(|name| match_text(name, query).is_some())
        .count();
    by_name.extend(by_purpose);
    by_name.extend(by_chat);
    (by_name, hidden)
}

/// Collapsed Bots section preview signal priority (Orca `bot-collapsed-preview.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreviewSignal {
    Attention = 0,
    Unread = 1,
    Working = 2,
}

impl PreviewSignal {
    #[must_use]
    pub const fn from_flags(needs_you: bool, unread: bool, working: bool) -> Option<Self> {
        if needs_you {
            Some(Self::Attention)
        } else if unread {
            Some(Self::Unread)
        } else if working {
            Some(Self::Working)
        } else {
            None
        }
    }

    /// Sort key matching Orca `PRIORITY = { attention: 0, unread: 1, working: 2 }`.
    #[must_use]
    pub const fn rank(signal: Option<Self>) -> u8 {
        match signal {
            Some(Self::Attention) => 0,
            Some(Self::Unread) => 1,
            Some(Self::Working) => 2,
            None => 3,
        }
    }
}

/// Geometry of Orca's virtualized collapsed preview track (`use-bot-preview-scroll.ts`, `#47622`, `#47884`).
pub const PREVIEW_FACE_SIZE: f32 = 20.;
pub const PREVIEW_FACE_GAP: f32 = 4.;
pub const PREVIEW_FACE_STEP: f32 = PREVIEW_FACE_SIZE + PREVIEW_FACE_GAP;
pub const PREVIEW_TRACK_INSET: f32 = 4.;
pub const PREVIEW_EDGE_WIDTH: f32 = 14.;
pub const PREVIEW_TRACK_HEIGHT: f32 = PREVIEW_FACE_SIZE + PREVIEW_TRACK_INSET * 2.;

/// Visible slice of collapsed bot faces and left/right overflow counts (`#47622`, `#47884`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollapsedTrackWindow {
    pub offset: usize,
    pub max_offset: usize,
    pub overflow: bool,
    /// Number of bots hidden off the left edge (drives left `BotPreviewEdge`).
    pub before: usize,
    /// Number of bots hidden off the right edge (drives right `BotPreviewEdge`).
    pub after: usize,
    /// First rendered index (may include 1 clipped edge-peek avatar on the left).
    pub first_visible: usize,
    /// Exclusive end rendered index (may include 1 clipped edge-peek avatar on the right).
    pub end_visible: usize,
    /// First fully-unclipped index (eligible to draw `BotPreviewPip`).
    pub first_full: usize,
    /// Exclusive end fully-unclipped index (eligible to draw `BotPreviewPip`).
    pub end_full: usize,
    /// Paging step when clicking a left/right `BotPreviewEdge` control.
    pub page_step: usize,
}

/// Computes the collapsed preview track window for `total` bots inside `available_px` width.
#[must_use]
pub fn compute_collapsed_track(
    total: usize,
    requested_offset: usize,
    available_px: f32,
) -> CollapsedTrackWindow {
    if total == 0 {
        return CollapsedTrackWindow {
            offset: 0,
            max_offset: 0,
            overflow: false,
            before: 0,
            after: 0,
            first_visible: 0,
            end_visible: 0,
            first_full: 0,
            end_full: 0,
            page_step: 1,
        };
    }
    let total_f = f32::from(u16::try_from(total).unwrap_or(u16::MAX));
    let total_width = total_f * PREVIEW_FACE_STEP + PREVIEW_TRACK_INSET;
    let width = available_px.max(PREVIEW_FACE_STEP + PREVIEW_TRACK_INSET * 2.);
    if total_width <= width {
        return CollapsedTrackWindow {
            offset: 0,
            max_offset: 0,
            overflow: false,
            before: 0,
            after: 0,
            first_visible: 0,
            end_visible: total,
            first_full: 0,
            end_full: total,
            page_step: total.max(1),
        };
    }
    // Reserve room for at least one edge button when overflowing; when scrolled mid-track,
    // both left and right edges appear.
    let usable = (width - PREVIEW_TRACK_INSET - PREVIEW_EDGE_WIDTH * 2.).max(PREVIEW_FACE_STEP);
    let mut full_slots = 1usize;
    while full_slots < total
        && f32::from(u16::try_from(full_slots + 1).unwrap_or(u16::MAX)) * PREVIEW_FACE_STEP
            <= usable
    {
        full_slots += 1;
    }
    let max_offset = total.saturating_sub(full_slots);
    let offset = requested_offset.min(max_offset);
    let first_full = offset;
    let end_full = (offset + full_slots).min(total);
    let before = first_full;
    let after = total.saturating_sub(end_full);
    // Include 1 partially-clipped peek face on each overflowing edge (#47884) whose pip is suppressed.
    let first_visible = first_full.saturating_sub(usize::from(before > 0));
    let end_visible = (end_full + usize::from(after > 0)).min(total);
    let page_step = full_slots.saturating_sub(1).max(1);
    CollapsedTrackWindow {
        offset,
        max_offset,
        overflow: true,
        before,
        after,
        first_visible,
        end_visible,
        first_full,
        end_full,
        page_step,
    }
}

/// Strips block markers and simple inline markdown for `BotHoverLatestLine` (Orca `BotRowActivityLine.tsx`).
#[must_use]
pub fn strip_inline_markdown(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (line_idx, raw_line) in input.lines().enumerate() {
        if line_idx > 0 {
            out.push(' ');
        }
        let mut line = raw_line.trim_start();
        // Strip leading heading / quote / bullet markers.
        if let Some(rest) = line.strip_prefix('#') {
            line = rest.trim_start_matches('#').trim_start();
        } else if let Some(rest) = line.strip_prefix("> ") {
            line = rest;
        } else if let Some(rest) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("+ "))
        {
            line = rest;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // Markdown link `[label](url)` -> `label`.
            if chars[i] == '['
                && let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']')
            {
                let cb = i + 1 + close_bracket;
                if cb + 1 < chars.len()
                    && chars[cb + 1] == '('
                    && let Some(close_paren) = chars[cb + 2..].iter().position(|&c| c == ')')
                {
                    for &c in &chars[i + 1..cb] {
                        out.push(c);
                    }
                    i = cb + 2 + close_paren + 1;
                    continue;
                }
            }
            if matches!(chars[i], '`' | '*' | '~') {
                i += 1;
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
    }
    out.trim().to_owned()
}

/// Where a turn is: `None` for Home Chat, else a side chat's or thread's title.
pub type RowPlace = Option<String>;

/// Orca `BotRowLine`: a bot row's second line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowLine {
    Paused,
    /// A turn is under way and the agent has said nothing of its own yet.
    Replying(RowPlace),
    /// What the agent last said, and when (ms).
    Activity {
        place: RowPlace,
        text: String,
        at: Option<u64>,
    },
    /// Its chats are quiet; this many items, workers and runs wait on the owner.
    NeedYouElsewhere(usize),
    /// Its chats are quiet; this many workers are working.
    WorkingElsewhere(usize),
}

/// One of the bot's own chats, as the line reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChat {
    pub place: RowPlace,
    pub working: bool,
    pub needs_you: bool,
    /// The agent's own words (its terminal title), if any.
    pub status: Option<String>,
    pub updated_at: u64,
}

/// Orca `selectBotRowLine`: paused; else a chat asking the owner; else what else needs them;
/// else a working chat; else other work; else the newest thing a chat said.
#[must_use]
pub fn select_line(
    paused: bool,
    chats: &[LineChat],
    need_you_elsewhere: usize,
    working_elsewhere: usize,
) -> Option<RowLine> {
    if paused {
        return Some(RowLine::Paused);
    }
    // Home Chat first in `chats`, so it wins a tie.
    let newest = |only: &dyn Fn(&LineChat) -> bool| {
        chats.iter().filter(|c| only(c)).reduce(|best, c| {
            if c.updated_at > best.updated_at {
                c
            } else {
                best
            }
        })
    };
    let say = |c: &LineChat| match &c.status {
        Some(text) => RowLine::Activity {
            place: c.place.clone(),
            text: text.clone(),
            at: (c.updated_at > 0).then_some(c.updated_at),
        },
        None => RowLine::Replying(c.place.clone()),
    };
    if let Some(asking) = newest(&|c| c.needs_you) {
        return Some(say(asking));
    }
    if need_you_elsewhere > 0 {
        return Some(RowLine::NeedYouElsewhere(need_you_elsewhere));
    }
    if let Some(working) = newest(&|c| c.working) {
        return Some(say(working));
    }
    if working_elsewhere > 0 {
        return Some(RowLine::WorkingElsewhere(working_elsewhere));
    }
    newest(&|c| c.status.is_some()).map(say)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(name: &str, paused: bool, needs_you: bool, at: u64) -> ArrangeFacts<'_> {
        ArrangeFacts {
            name,
            paused,
            needs_you,
            activity_at: at,
        }
    }

    #[test]
    fn arrange_sorts_and_hides_like_orca() {
        let bots = [
            facts("beta", false, false, 30),
            facts("Alpha", true, false, 10),
            facts("gamma", false, true, 20),
        ];
        let mut view = BotSidebarView::default();
        assert_eq!(arrange(&bots, view), [0, 1, 2]);
        view.sort = BotSidebarSort::Name;
        assert_eq!(arrange(&bots, view), [1, 0, 2]);
        view.sort = BotSidebarSort::Recent;
        assert_eq!(arrange(&bots, view), [0, 2, 1]);
        view.sort = BotSidebarSort::Smart;
        assert_eq!(arrange(&bots, view), [2, 0, 1]);
        view.hide_paused = true;
        assert_eq!(arrange(&bots, view), [2, 0]);
        assert_eq!(view.active_filters(), 1);
    }

    #[test]
    fn search_matches_phrase_then_word_prefixes() {
        assert_eq!(
            match_text("Release Bot", "ease"),
            Some(std::iter::once(3..7).collect())
        );
        assert_eq!(
            match_text("Eng - Reviewer", "rev eng"),
            Some(vec![0..3, 6..9])
        );
        assert_eq!(match_text("Eng - Reviewer", "view eng"), None);
        assert_eq!(
            match_text("쇼핑봇", "핑"),
            Some(std::iter::once(3..6).collect())
        );
        let (matches, hidden) = search(
            &[("Shop", "finds prices"), ("Price watcher", "")],
            &["Price paused"],
            "price",
        );
        assert_eq!(matches[0].index, 1, "name matches come first");
        assert_eq!(matches[1].index, 0);
        assert_eq!(
            matches[1].purpose.as_ref().unwrap().1,
            std::iter::once(6..11).collect::<Vec<_>>()
        );
        assert_eq!(hidden, 1);
    }

    #[test]
    fn purpose_snippet_starts_near_the_first_match() {
        let text = "Watches the shop every morning and reports prices";
        let ranges = match_text(text, "prices").unwrap();
        let (snippet, marked) = purpose_snippet(text, &ranges);
        assert!(snippet.starts_with('…'));
        assert_eq!(snippet.get(marked[0].clone()), Some("prices"));
    }

    fn chat(
        place: Option<&str>,
        working: bool,
        needs: bool,
        status: Option<&str>,
        at: u64,
    ) -> LineChat {
        LineChat {
            place: place.map(str::to_owned),
            working,
            needs_you: needs,
            status: status.map(str::to_owned),
            updated_at: at,
        }
    }

    #[test]
    fn row_line_picks_like_orca() {
        assert_eq!(select_line(true, &[], 3, 0), Some(RowLine::Paused));
        assert_eq!(select_line(false, &[], 0, 0), None);
        let quiet = chat(None, false, false, Some("Fixed the search"), 5);
        assert_eq!(
            select_line(false, std::slice::from_ref(&quiet), 2, 1),
            Some(RowLine::NeedYouElsewhere(2))
        );
        assert_eq!(
            select_line(false, std::slice::from_ref(&quiet), 0, 1),
            Some(RowLine::WorkingElsewhere(1))
        );
        assert_eq!(
            select_line(false, std::slice::from_ref(&quiet), 0, 0),
            Some(RowLine::Activity {
                place: None,
                text: "Fixed the search".to_owned(),
                at: Some(5)
            })
        );
        let side = chat(Some("Claude Chat"), true, false, None, 9);
        assert_eq!(
            select_line(false, &[quiet, side], 0, 3),
            Some(RowLine::Replying(Some("Claude Chat".to_owned())))
        );
    }

    #[test]
    fn search_with_chats_matches_name_purpose_then_chat_title_and_body() {
        let candidates = [
            SearchCandidate {
                name: "Ops Bot",
                purpose: "Watches deploys",
                chats: vec![("CI triage", Some("Checking nightly flake"))],
            },
            SearchCandidate {
                name: "Design Bot",
                purpose: "Reviews mockups",
                chats: vec![("Home Chat", Some("Updated sidebar tokens"))],
            },
        ];
        let (title_hits, _) = search_with_chats(&candidates, &[], "triage");
        assert_eq!(title_hits.len(), 1);
        assert_eq!(title_hits[0].index, 0);
        let (title, snippet, ranges) = title_hits[0].chat_hit.as_ref().unwrap();
        assert_eq!(title, "CI triage");
        assert_eq!(snippet.get(ranges[0].clone()), Some("triage"));

        let (body_hits, _) = search_with_chats(&candidates, &[], "tokens");
        assert_eq!(body_hits.len(), 1);
        assert_eq!(body_hits[0].index, 1);
        let (title, snippet, ranges) = body_hits[0].chat_hit.as_ref().unwrap();
        assert_eq!(title, "Home Chat");
        assert_eq!(snippet.get(ranges[0].clone()), Some("tokens"));
    }

    #[test]
    fn collapsed_track_window_virtualizes_and_clips_edges() {
        // Fits comfortably: no overflow, no edges.
        let fit = compute_collapsed_track(3, 0, 160.);
        assert!(!fit.overflow);
        assert_eq!((fit.before, fit.after), (0, 0));
        assert_eq!((fit.first_full, fit.end_full), (0, 3));

        // Overflows: right edge hides trailing bots and peeks 1 clipped avatar.
        let start = compute_collapsed_track(8, 0, 140.);
        assert!(start.overflow);
        assert_eq!(start.before, 0);
        assert!(start.after > 0);
        assert_eq!(start.first_visible, 0);
        assert_eq!(start.end_visible, start.end_full + 1);

        // Mid-scroll: both left and right edges active, both peek 1 clipped avatar.
        let mid = compute_collapsed_track(8, 2, 140.);
        assert_eq!(mid.before, 2);
        assert!(mid.after > 0);
        assert_eq!(mid.first_visible, 1);
        assert_eq!(mid.end_visible, mid.end_full + 1);

        // Priority ranking: attention -> unread -> working -> quiet.
        assert!(
            PreviewSignal::rank(Some(PreviewSignal::Attention))
                < PreviewSignal::rank(Some(PreviewSignal::Unread))
        );
        assert!(
            PreviewSignal::rank(Some(PreviewSignal::Unread))
                < PreviewSignal::rank(Some(PreviewSignal::Working))
        );
    }

    #[test]
    fn strip_inline_markdown_cleans_links_and_emphasis() {
        assert_eq!(
            strip_inline_markdown("## [인생-방향성.md](file:///Users/a/b) **updated** `now`"),
            "인생-방향성.md updated now"
        );
    }
}
