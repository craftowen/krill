// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! The sidebar Bots section's pure parts, as Orca's `shared/bots` has them: "Bot options"
//! (sort and hide), the search's matching and marks, and which line a bot row says.

use serde::{Deserialize, Serialize};

use crate::i18n::catalog::bots as text;

/// Orca `BOT_SIDEBAR_SORT_CHOICES`: ids and meanings match the projects' Sort by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotSidebarSort {
    /// The saved order. New bots go on top.
    #[default]
    Manual,
    /// "Agent Activity": bots that need you, then most recent activity.
    Smart,
    Recent,
    Name,
}

impl BotSidebarSort {
    pub const ALL: [Self; 4] = [Self::Manual, Self::Smart, Self::Recent, Self::Name];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => text::SBO_SORT_MANUAL.get(),
            Self::Smart => text::SBO_SORT_SMART.get(),
            Self::Recent => text::SBO_SORT_RECENT.get(),
            Self::Name => text::SBO_SORT_NAME.get(),
        }
    }

    #[must_use]
    pub fn description(self) -> Option<&'static str> {
        match self {
            Self::Manual => Some(text::SBO_SORT_MANUAL_DESC.get()),
            Self::Smart => Some(text::SBO_SORT_SMART_DESC.get()),
            Self::Recent | Self::Name => None,
        }
    }
}

/// Orca `BotSidebarView`: how this computer's bots list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BotSidebarView {
    pub sort: BotSidebarSort,
    pub hide_paused: bool,
    /// A bot's worker lines with nothing running drop off, as the projects' Hide sleeping.
    pub hide_sleeping_workers: bool,
}

impl BotSidebarView {
    /// How many options differ from the default, for the button's badge.
    #[must_use]
    pub fn active_filters(self) -> usize {
        usize::from(self.hide_paused) + usize::from(self.hide_sleeping_workers)
    }
}

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

/// One search result: marks in the name, or (name unmatched) the purpose that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub index: usize,
    pub name_ranges: Vec<SearchRange>,
    pub purpose: Option<(String, Vec<SearchRange>)>,
}

/// Orca `createBotSidebarSearchIndex`: name matches, then purpose-only matches, each in the
/// list's order; and how many hidden paused bots match by name.
#[must_use]
pub fn search(
    shown: &[(&str, &str)],
    hidden_paused: &[&str],
    query: &str,
) -> (Vec<SearchMatch>, usize) {
    let mut by_name = Vec::new();
    let mut by_purpose = Vec::new();
    for (index, (name, purpose)) in shown.iter().enumerate() {
        if let Some(name_ranges) = match_text(name, query) {
            by_name.push(SearchMatch {
                index,
                name_ranges,
                purpose: None,
            });
        } else if let Some(ranges) = match_text(purpose, query) {
            by_purpose.push(SearchMatch {
                index,
                name_ranges: Vec::new(),
                purpose: Some(purpose_snippet(purpose, &ranges)),
            });
        }
    }
    let hidden = hidden_paused
        .iter()
        .filter(|name| match_text(name, query).is_some())
        .count();
    by_name.extend(by_purpose);
    (by_name, hidden)
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
}
