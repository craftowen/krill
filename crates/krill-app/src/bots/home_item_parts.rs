// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Parts of Home's open item: its chat (`BotItemView`'s Chat row and the header's "Item N · chat"),
//! the previous / next stepper and chords (`BotItemStepper`, `use-bot-item-chords`), link chips
//! with their PR and worker state (`BotItemLinkChips`), Markdown text (`BotItemMarkdown`), the
//! Activity's field changes and worker lines (`BotItemActivityList`), an ask of several questions
//! one line at a time (`BotItemQuestionStep`), and the comment box's Replying to, attachments and
//! @ mentions (`BotItemCommentBox`, `BotItemReplyingTo`).

use std::path::PathBuf;
use std::time::Instant;

use gpui::{AnyElement, Context, FontWeight, SharedString, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::text::TextView;
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::SessionId;
use krill_proto::bots::{
    BotConfig, BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind, BotItemLink,
    BotRpcRequest, BotSessionSummary,
};

use super::home::grouped;
use super::rpc::call_checked;
use super::{OpenSession, View, now_unix_ms};
use crate::i18n::catalog::bot_home as hm;
use crate::pull_request::Status as PrStatus;
use crate::ui;

/// Events Activity shows before "Show earlier" (Orca reads a page of the newest).
pub(super) const ACTIVITY_PAGE: usize = 30;
/// A PR chip's state is read again after this.
const PR_REFRESH_MS: u64 = 60_000;

/// The chat an item is from: the session whose bot made it, else the bot's Home Chat.
#[derive(Debug, Clone)]
pub(super) struct ItemChat {
    pub session: Option<SessionId>,
    pub title: String,
    pub home: bool,
}

/// A PR chip's state as read from GitHub.
#[derive(Debug, Clone)]
pub(super) struct PrRead {
    pub status: Option<PrStatus>,
    pub title: String,
    pub at: u64,
}

/// An open ask of several questions, stepped one line at a time.
#[derive(Debug, Clone, Default)]
pub(super) struct AskSteps {
    /// The line explicitly stepped to (`‹`/`›` or clicking a held line); `None` follows the first
    /// open unheld line.
    pub chosen: Option<usize>,
    /// Per line, a quick reply picked here and not sent yet.
    pub held: Vec<Option<String>>,
}

/// Derived state for a waiting multi-question ask, matching Orca `useBotItemQuestionSteps`.
pub(super) struct QuestionStepState {
    pub answered: Vec<Option<String>>,
    pub lines: Vec<usize>,
    pub kept: Vec<Option<String>>,
    pub current: usize,
}

pub(super) fn question_step_state(
    item: &BotItem,
    ask: &BotItemEvent,
    steps: Option<&AskSteps>,
) -> QuestionStepState {
    let answered = answered_lines(item, ask);
    let lines: Vec<usize> = (0..ask.decisions.len())
        .filter(|&i| answered[i].is_none())
        .collect();
    let kept: Vec<Option<String>> = (0..ask.decisions.len())
        .map(|i| {
            if answered[i].is_some() {
                None
            } else {
                steps.and_then(|s| s.held.get(i).cloned().flatten())
            }
        })
        .collect();
    let first_open = lines
        .iter()
        .copied()
        .find(|&i| kept[i].is_none())
        .or_else(|| lines.first().copied())
        .unwrap_or(0);
    let current = steps
        .and_then(|s| s.chosen)
        .filter(|c| lines.contains(c))
        .unwrap_or(first_open);
    QuestionStepState {
        answered,
        lines,
        kept,
        current,
    }
}

/// Raw first non-empty line of a question, trimmed (for back-compat with historical/CLI/Modal
/// answer lines written before Markdown normalization).
fn raw_question_line(q: &str) -> &str {
    q.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Orca's `botItemQuestionLine`: a question's first line with leading Markdown marks stripped.
pub(super) fn question_line(q: &str) -> String {
    let first = raw_question_line(q);
    let stripped = first.trim_start_matches(|c: char| {
        matches!(c, '#' | '>' | '-' | '*' | '+') || c.is_whitespace()
    });
    stripped
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .trim()
        .to_owned()
}

/// Orca's `botItemNamedAnswers`: per question, what an owner's comment answered it with
/// ("Question → answer" lines). Matches both normalized (`question_line`) and raw-Markdown
/// prefixes on read for backwards compatibility with historical/CLI/Modal answers.
pub(super) fn named_answers(decisions: &[BotItemDecision], text: &str) -> Vec<Option<String>> {
    let heads: Vec<Vec<String>> = decisions
        .iter()
        .map(|d| {
            let norm = question_line(&d.question);
            let raw = raw_question_line(&d.question);
            let mut candidates = Vec::new();
            if !norm.is_empty() {
                candidates.push(format!("{norm} →"));
            }
            if !raw.is_empty() && raw != norm {
                candidates.push(format!("{raw} →"));
            }
            candidates
        })
        .collect();
    let mut answers = vec![None; decisions.len()];
    for line in text.lines().map(str::trim) {
        let mut best_len = 0_usize;
        let mut best_matches: Vec<(usize, usize)> = Vec::new();
        for (idx, candidates) in heads.iter().enumerate() {
            if let Some(matched_len) = candidates
                .iter()
                .filter(|h| line.starts_with(h.as_str()))
                .map(String::len)
                .max()
            {
                match matched_len.cmp(&best_len) {
                    std::cmp::Ordering::Greater => {
                        best_len = matched_len;
                        best_matches.clear();
                        best_matches.push((idx, matched_len));
                    }
                    std::cmp::Ordering::Equal => {
                        best_matches.push((idx, matched_len));
                    }
                    std::cmp::Ordering::Less => {}
                }
            }
        }
        let chosen = best_matches
            .iter()
            .copied()
            .find(|&(idx, _)| answers[idx].is_none())
            .or_else(|| best_matches.first().copied());
        if let Some((i, prefix_len)) = chosen {
            let words = line.get(prefix_len..).unwrap_or_default().trim();
            if !words.is_empty() && answers[i].is_none() {
                answers[i] = Some(words.to_owned());
            }
        }
    }
    answers
}

/// Orca's `botItemAnswersText`: one comment, a "Question → answer" line per answered question.
pub(super) fn answers_text(decisions: &[BotItemDecision], answers: &[Option<String>]) -> String {
    decisions
        .iter()
        .zip(answers)
        .filter_map(|(d, a)| {
            let words = a
                .as_deref()?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            (!words.is_empty()).then(|| format!("{} → {words}", question_line(&d.question)))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Per line of the waiting ask, what the owner's comments after it answered.
pub(super) fn answered_lines(item: &BotItem, ask: &BotItemEvent) -> Vec<Option<String>> {
    let mut answered = vec![None; ask.decisions.len()];
    for e in item.events.iter().filter(|e| {
        e.id > ask.id
            && e.kind == BotItemEventKind::Commented
            && matches!(e.actor, BotItemActor::Owner { .. })
    }) {
        for (slot, words) in answered
            .iter_mut()
            .zip(named_answers(&ask.decisions, &e.text))
        {
            if slot.is_none() {
                *slot = words;
            }
        }
    }
    answered
}

/// The edit's changed fields, before and after (krilld writes `{"fields": {...}}`).
pub(super) fn edited_fields(event: &BotItemEvent) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&event.text) else {
        return Vec::new();
    };
    let Some(fields) = v.get("fields").and_then(|f| f.as_object()) else {
        return Vec::new();
    };
    let s = |v: Option<&serde_json::Value>| {
        v.and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    fields
        .iter()
        .map(|(name, change)| (name.clone(), s(change.get("from")), s(change.get("to"))))
        .collect()
}

fn field_name(field: &str) -> &'static str {
    match field {
        "doneMeans" => hm::FIELD_DONE_MEANS.get(),
        "description" => hm::FIELD_DESCRIPTION.get(),
        _ => hm::FIELD_TITLE.get(),
    }
}

/// A worker session as its chips and lines name it.
fn worker_title(s: Option<&BotSessionSummary>, id: &str) -> String {
    s.map(|s| s.label.trim().to_owned())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| hm::WORKER_FALLBACK.format(&[("id", id.trim())]))
}

/// Markdown as Orca's `BotItemMarkdown` shows it: selectable, links open in the browser.
pub(super) fn markdown(id: impl Into<SharedString>, src: &str) -> AnyElement {
    TextView::markdown(gpui::ElementId::Name(id.into()), src.to_owned())
        .scrollable(false)
        .selectable(true)
        .on_link_click(|url, _, _, cx| {
            if let Some(clean) = super::sanitize_http_url(url) {
                cx.open_url(&clean);
            }
        })
        .into_any_element()
}

impl View {
    /// The chat an item came from (Orca's `item.chat`).
    pub(super) fn item_chat(&self, bot: &BotConfig, item: &BotItem) -> ItemChat {
        let sessions: Vec<&BotSessionSummary> = self
            .bot_sessions
            .iter()
            .filter(|s| s.bot_id == bot.id)
            .collect();
        let home = sessions
            .iter()
            .find(|s| s.link_id.is_none())
            .map(|s| s.session_id);
        let made = item
            .events
            .iter()
            .find(|e| e.kind == BotItemEventKind::Made)
            .and_then(|e| match e.actor {
                BotItemActor::Bot { session_id, .. } => session_id,
                _ => None,
            });
        let session = made.or(home);
        if session.is_some() && session == home {
            return ItemChat {
                session,
                title: hm::CHAT_HOME_TITLE.get().to_owned(),
                home: true,
            };
        }
        let summary = sessions.iter().find(|s| Some(s.session_id) == session);
        let title = summary
            .and_then(|s| {
                let label = s.label.trim();
                (!label.is_empty() && label != bot.name.trim())
                    .then(|| label.to_owned())
                    .or_else(|| s.link_id.clone())
            })
            .unwrap_or_else(|| hm::CHAT_FALLBACK.get().to_owned());
        ItemChat {
            session,
            title,
            home: false,
        }
    }

    /// The items in the list's order, as ↑ ↓ walk them (Orca `use-bot-item-walk`).
    pub(super) fn walk_order(&self) -> Vec<String> {
        let now = now_unix_ms();
        if self.group_panel.active {
            let filter = &self.group_panel.filter;
            let members = &self.group_panel.member_ids;
            let shown = self.items.items.iter().filter(|item| {
                members.contains(&item.owner_bot_id)
                    && (filter.bot_ids.is_empty() || filter.bot_ids.contains(&item.owner_bot_id))
                    && super::group_panel::item_matches_group_scope(
                        item,
                        &self.group_panel.group_id,
                        &self.group_panel.thread_ids,
                        &filter.chat_scope,
                    )
                    && (!filter.needs_you_only
                        || item.status == krill_proto::bots::BotItemStatus::NeedsYou)
            });
            return grouped(shown, |i| self.item_live(i), Some(5), now)
                .iter()
                .flat_map(|g| g.iter().map(|i| i.uid.clone()))
                .collect();
        }
        let filter_on = self.items.needs_you_only;
        let selected_bot = self.selected_bot_id.as_ref();
        let shown = self.items.items.iter().filter(|i| {
            selected_bot.is_none_or(|id| &i.owner_bot_id == id)
                && (!filter_on || i.status == krill_proto::bots::BotItemStatus::NeedsYou)
        });
        let stale_days = self.selected_bot_id.as_ref().map_or_else(
            || krill_proto::bots::BotItemsStale::default().days(),
            |id| self.settings_extras(id).items_stale.days(),
        );
        grouped(shown, |i| self.item_live(i), stale_days, now)
            .iter()
            .flat_map(|g| g.iter().map(|i| i.uid.clone()))
            .collect()
    }

    /// Opens the previous (`-1`) or next (`1`) item.
    pub(super) fn step_item(&mut self, direction: i32, cx: &mut Context<Self>) {
        let order = self.walk_order();
        let next = match self
            .items
            .open_uid
            .as_ref()
            .and_then(|uid| order.iter().position(|u| u == uid))
        {
            Some(at) if direction < 0 => at.checked_sub(1),
            Some(at) => Some(at + 1).filter(|&n| n < order.len()),
            None if direction > 0 && !order.is_empty() => Some(0),
            None => None,
        };
        if let Some(uid) = next.and_then(|n| order.get(n)).cloned() {
            self.open_home_item(uid, cx);
        }
    }

    /// Orca's `BotItemStepper`: compact `^ v` + divider in the single-line comment pill (and
    /// `"N / M"` when replying/multiline), matching `owner-orca-reference.png`.
    pub(super) fn render_stepper(
        &self,
        show_counter: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let order = self.walk_order();
        let total = order.len();
        let pos = self
            .items
            .open_uid
            .as_ref()
            .and_then(|uid| order.iter().position(|u| u == uid));
        let (display_index, prev_disabled, next_disabled) = match pos {
            Some(i) => {
                let one_based = i + 1;
                (one_based.to_string(), one_based <= 1, one_based >= total)
            }
            None => ("—".to_owned(), true, total == 0),
        };
        let arrow = |id: &'static str, icon: IconName, label: &'static str, keys: &'static str| {
            Button::new(id)
                .debug_selector(move || id.into())
                .ghost()
                .xsmall()
                .icon(Icon::new(icon).size(px(14.)).text_color(p.muted_foreground))
                .tooltip(format!("{label}  {keys}"))
                .accessibility_label(label)
        };
        div()
            .id("bot-item-stepper")
            .debug_selector(|| "bot-item-stepper".into())
            .role(gpui::Role::Group)
            .aria_label(SharedString::from(hm::ITEM_NAV.format(&[
                ("index", &display_index),
                ("total", &total.to_string()),
            ])))
            .when(show_counter, |d| d.flex_1().min_w_0())
            .when(!show_counter, gpui::Styled::flex_none)
            .flex()
            .items_center()
            .gap_0p5()
            .when(show_counter && total > 0, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .justify_end()
                        .overflow_hidden()
                        .child(
                            div()
                                .px_1()
                                .whitespace_nowrap()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(format!("{display_index} / {total}")),
                        ),
                )
            })
            .child(
                arrow(
                    "bot-item-prev",
                    IconName::ChevronUp,
                    hm::PREV_ITEM.get(),
                    "⌥⌘↑",
                )
                .disabled(prev_disabled)
                .on_click(cx.listener(|this, _, _, cx| this.step_item(-1, cx))),
            )
            .child(
                arrow(
                    "bot-item-next",
                    IconName::ChevronDown,
                    hm::NEXT_ITEM.get(),
                    "⌥⌘↓",
                )
                .disabled(next_disabled)
                .on_click(cx.listener(|this, _, _, cx| this.step_item(1, cx))),
            )
            .child(div().mx_1().h(px(16.)).w(px(1.)).flex_none().bg(p.border))
    }

    /// Orca's item chords: Esc and ⌘[ (or ⌥← when not editing text) go back, ⌥⌘↑ / ⌥⌘↓ step.
    pub(super) fn item_key(
        &mut self,
        ev: &gpui::KeyDownEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let k = &ev.keystroke;
        let m = k.modifiers;
        let step = m.alt && m.platform && !m.shift && !m.control;
        match k.key.as_str() {
            "up" if step => {
                cx.stop_propagation();
                self.step_item(-1, cx);
            }
            "down" if step => {
                cx.stop_propagation();
                self.step_item(1, cx);
            }
            "[" if m.platform && !m.alt && !m.shift && !m.control => {
                cx.stop_propagation();
                self.close_home_item(cx);
            }
            "left" if m.alt && !m.platform && !m.shift && !m.control => {
                // Leave Option+Left alone while typing in a text input (macOS MoveToPreviousWord).
                if !self.items.any_input_focused(window, cx) {
                    cx.stop_propagation();
                    self.close_home_item(cx);
                }
            }
            "escape" if !m.modified() => {
                // An empty comment box gives Esc back to the pane.
                let typed = self
                    .items
                    .open_uid
                    .as_ref()
                    .and_then(|uid| self.items.comment_inputs.get(uid))
                    .is_some_and(|i| !i.read(cx).value().trim().is_empty());
                if !typed && !self.items.items_input_composing(window, cx) {
                    cx.stop_propagation();
                    self.close_home_item(cx);
                }
            }
            _ => {}
        }
    }

    /// The PR's state and title, read once a minute per URL while its item is open.
    pub(super) fn ensure_pr_reads(&mut self, item: &BotItem, cx: &mut Context<Self>) {
        // A mock host (tests, previews) never runs gh.
        if self.rpc_is_mock {
            return;
        }
        let now = now_unix_ms();
        let urls: Vec<String> = item
            .links
            .iter()
            .filter_map(|l| match l {
                BotItemLink::Pr(u) if u.starts_with("https://github.com/") => Some(u.clone()),
                _ => None,
            })
            .filter(|u| {
                self.items
                    .pr_reads
                    .get(u)
                    .is_none_or(|r| now.saturating_sub(r.at) >= PR_REFRESH_MS)
            })
            .collect();
        for url in urls {
            // Marked read now, so a render while gh runs never starts another.
            let prior = self.items.pr_reads.get(&url).cloned();
            self.items.pr_reads.insert(
                url.clone(),
                PrRead {
                    status: prior.as_ref().and_then(|p| p.status.clone()),
                    title: prior.map(|p| p.title).unwrap_or_default(),
                    at: now,
                },
            );
            let key = url.clone();
            cx.spawn(async move |this, cx| {
                let read = cx
                    .background_executor()
                    .spawn(
                        async move { crate::pull_request::shared().view_url(&url, Instant::now()) },
                    )
                    .await;
                this.update(cx, |this, cx| {
                    if let Ok((status, title)) = read
                        && let Some(slot) = this.items.pr_reads.get_mut(&key)
                    {
                        slot.status = Some(status);
                        slot.title = title;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// Orca's `BotItemLinkChips`: full-width stacked PR/worker cards matching
    /// `owner-orca-reference.png` (PR merge/open/closed icon + title + `#N`, worker state +
    /// title + Stop while running; finished workers go last; >2 links expand via `+N more`).
    pub(super) fn render_link_chips(
        &self,
        bot: &BotConfig,
        item: &BotItem,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let session = |id: &str| {
            id.trim().parse::<u64>().ok().and_then(|n| {
                self.bot_sessions
                    .iter()
                    .find(|s| s.session_id == SessionId(n))
            })
        };
        let settled = |l: &BotItemLink| match l {
            BotItemLink::Dispatch(id) => session(id).is_none_or(|s| !(s.live && s.working)),
            _ => false,
        };
        let mut links: Vec<(usize, &BotItemLink)> = item.links.iter().enumerate().collect();
        links.sort_by_key(|(_, l)| settled(l));
        let expand_key = format!("links:{}", item.uid);
        let expanded = self.items.open_changes.contains(&expand_key);
        let visible_cap = if expanded { links.len() } else { 2 };
        let hidden_count = links.len().saturating_sub(visible_cap);
        let mut col = div()
            .debug_selector(|| "bot-item-link-chips".into())
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .pt_0p5();
        for (i, link) in links.into_iter().take(visible_cap) {
            let card = |id: SharedString| {
                div()
                    .id(id)
                    .w_full()
                    .h(px(28.))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(px(10.))
                    .rounded(ui::RADIUS_SM)
                    .bg(p.muted.opacity(0.75))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .cursor_pointer()
                    .hover(|d| d.bg(p.muted))
            };
            let el = match link {
                BotItemLink::Dispatch(id) => {
                    let s = session(id);
                    let running = s.is_some_and(|s| s.live && s.working);
                    let title = worker_title(s, id);
                    let icon: AnyElement = if running {
                        ui::status::status_glyph(
                            krill_proto::control::AgentState::Working,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        )
                        .into_any_element()
                    } else if s.is_some() {
                        Icon::new(IconName::CircleCheck)
                            .size(px(14.))
                            .text_color(p.success)
                            .into_any_element()
                    } else {
                        Icon::new(IconName::CircleSlash)
                            .size(px(14.))
                            .text_color(p.muted_foreground)
                            .into_any_element()
                    };
                    let (open_bot, sid) = (bot.id.clone(), s.map(|s| s.session_id));
                    let (stop_bot, stop_sid) = (bot.id.clone(), sid);
                    card(format!("bot-item-link-{i}").into())
                        .when(running, |d| d.pr(px(4.)))
                        .child(
                            div()
                                .size(px(14.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(title.clone()))
                        .when(running, |d| {
                            d.child(
                                Button::new(("bot-item-worker-stop", i))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::Square).size(px(10.)))
                                    .tooltip(hm::STOP.get())
                                    .accessibility_label(SharedString::from(
                                        hm::STOP_NAMED.format(&[("name", &title)]),
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        if let Some(sid) = stop_sid {
                                            this.stop_worker(stop_bot.clone(), sid, cx);
                                        }
                                    })),
                            )
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            if let Some(session_id) = sid {
                                cx.emit(OpenSession {
                                    bot_id: open_bot.clone(),
                                    session_id,
                                });
                            }
                        }))
                        .into_any_element()
                }
                BotItemLink::Pr(url) => {
                    let read = self.items.pr_reads.get(url);
                    let number = url
                        .rsplit('/')
                        .next()
                        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
                    let label = read
                        .map(|r| r.title.clone())
                        .filter(|t| !t.is_empty())
                        .or_else(|| number.map(|n| hm::PR_NUMBER.format(&[("n", n)])))
                        .unwrap_or_else(|| url.clone());
                    let status = read.and_then(|r| r.status.clone());
                    let (icon_name, color) = match status {
                        Some(PrStatus::Merged) => (IconName::GitMerge, p.success),
                        Some(PrStatus::Open) => (IconName::GitPullRequest, p.success),
                        Some(PrStatus::Closed) => (IconName::GitPullRequestClosed, p.destructive),
                        _ => (IconName::GitPullRequest, p.muted_foreground),
                    };
                    let tip = [
                        Some(
                            status
                                .as_ref()
                                .and_then(PrStatus::badge)
                                .unwrap_or(hm::NOT_READ.get())
                                .to_owned(),
                        ),
                        repo_of(url),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    let tip = format!("{label} · {tip}");
                    let open = url.clone();
                    card(format!("bot-item-link-{i}").into())
                        .child(
                            Icon::new(icon_name)
                                .size(px(14.))
                                .flex_none()
                                .text_color(color),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .children(
                            number
                                .filter(|_| read.is_some_and(|r| !r.title.is_empty()))
                                .map(|n| {
                                    div()
                                        .flex_none()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(format!("#{n}"))
                                }),
                        )
                        .tooltip(move |w, cx| ui::tooltip_view("bot-item-pr", tip.clone(), w, cx))
                        .on_click(move |_, _, cx| {
                            if let Some(clean) = super::sanitize_http_url(&open) {
                                cx.open_url(&clean);
                            }
                        })
                        .into_any_element()
                }
                BotItemLink::Url(v) => {
                    let open = v.clone();
                    let label = v
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .to_owned();
                    card(format!("bot-item-link-{i}").into())
                        .child(
                            Icon::new(IconName::Globe)
                                .size(px(14.))
                                .flex_none()
                                .text_color(p.muted_foreground),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .on_click(move |_, _, cx| {
                            if let Some(clean) = super::sanitize_http_url(&open) {
                                cx.open_url(&clean);
                            }
                        })
                        .into_any_element()
                }
                BotItemLink::File(v) => {
                    let path = v.trim().to_owned();
                    card(format!("bot-item-link-{i}").into())
                        .child(
                            Icon::new(IconName::File)
                                .size(px(14.))
                                .flex_none()
                                .text_color(p.muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned()),
                        )
                        .on_click(move |_, _, cx| {
                            if !path.is_empty() {
                                cx.reveal_path(std::path::Path::new(&path));
                            }
                        })
                        .into_any_element()
                }
            };
            col = col.child(el);
        }
        if hidden_count > 0 {
            let toggle_key = expand_key;
            col = col.child(
                div()
                    .id("bot-item-links-more")
                    .debug_selector(|| "bot-item-links-more".into())
                    .self_start()
                    .px_1p5()
                    .py_0p5()
                    .rounded(ui::RADIUS_XS)
                    .cursor_pointer()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .hover(|d| d.bg(p.muted.opacity(0.6)).text_color(p.foreground))
                    .child(hm::MORE_LINKS.format(&[("n", &hidden_count.to_string())]))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.items.open_changes.insert(toggle_key.clone());
                        cx.notify();
                    })),
            );
        }
        col
    }

    /// Stops a worker session from its chip.
    fn stop_worker(
        &mut self,
        bot_id: krill_proto::bots::BotId,
        sid: SessionId,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        self.error = None;
        self.notice = None;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::StopBotSession {
                            bot_id,
                            session_id: sid,
                        },
                    )?;
                    let sessions = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListBotSessions { bot_id: None },
                    ) {
                        Ok(krill_proto::bots::BotRpcResponse::BotSessions(list)) => Some(list),
                        _ => None,
                    };
                    Ok::<_, String>(sessions)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host {
                    return;
                }
                match res {
                    Ok(Some(list)) => {
                        this.bot_sessions = list;
                        this.emit_sessions_changed(cx);
                    }
                    Ok(None) => {
                        if let Some(sess) =
                            this.bot_sessions.iter_mut().find(|s| s.session_id == sid)
                        {
                            sess.live = false;
                            sess.working = false;
                        }
                        this.emit_sessions_changed(cx);
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                        this.notice = None;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// An Edited event: "changed {field} ›" per field, its before and after on click.
    pub(super) fn render_field_changes(
        &self,
        event: &BotItemEvent,
        who: AnyElement,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let fields = edited_fields(event);
        if fields.is_empty() {
            return None;
        }
        let mut col = div().flex_1().min_w_0().flex().flex_col();
        let mut who = Some(who);
        for (name, from, to) in fields {
            let key = format!("{}:{name}", event.id);
            let open = self.items.open_changes.contains(&key);
            let toggle_key = key.clone();
            col = col.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_1()
                    .children(who.take())
                    .child(
                        div()
                            .id(SharedString::from(format!("bot-item-change-{key}")))
                            .debug_selector({
                                let key = key.clone();
                                move || format!("bot-item-change-{key}")
                            })
                            .role(gpui::Role::Button)
                            .aria_expanded(open)
                            .flex()
                            .items_center()
                            .gap_0p5()
                            .cursor_pointer()
                            .hover(|d| d.text_color(p.foreground))
                            .child(hm::CHANGED_FIELD.format(&[("field", field_name(&name))]))
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(px(12.)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.items.open_changes.remove(&toggle_key) {
                                    this.items.open_changes.insert(toggle_key.clone());
                                }
                                cx.notify();
                            })),
                    ),
            );
            if open {
                col = col.child(
                    div()
                        .mt_0p5()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .border_l_2()
                        .border_color(p.border)
                        .pl_2()
                        .when(!from.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_color(p.muted_foreground)
                                    .line_through()
                                    .child(from),
                            )
                        })
                        .when(!to.is_empty(), |d| {
                            d.child(div().text_color(p.foreground).child(to))
                        }),
                );
            }
        }
        Some(col)
    }

    /// A linked worker's Activity lines: "Worker started:" where it was linked, and "Worker
    /// finished:" / "Worker stopped:" once its session is no longer working, with timestamp and
    /// stop reason (Orca `#47543`).
    #[expect(
        clippy::type_complexity,
        reason = "compact internal tuple for worker started + optional finished/stopped state"
    )]
    pub(super) fn worker_lines(
        &self,
        event: &BotItemEvent,
    ) -> Option<(
        String,
        Option<(bool, String, Option<u64>, Option<&'static str>)>,
    )> {
        let id = event.text.strip_prefix("dispatch:")?.trim();
        let s = id.parse::<u64>().ok().and_then(|n| {
            self.bot_sessions
                .iter()
                .find(|s| s.session_id == SessionId(n))
        });
        let title = worker_title(s, id);
        let finished = match s {
            Some(s) if s.live && s.working => None,
            Some(s) if !s.live => {
                let reason = if self.host.is_local() {
                    Some(hm::WORKER_STOP_NO_SESSION.get())
                } else {
                    Some(hm::WORKER_STOP_OFFLINE.get())
                };
                Some((
                    true,
                    title.clone(),
                    Some(s.created_at_ms.max(event.created_at_ms)),
                    reason,
                ))
            }
            Some(s) => Some((
                false,
                title.clone(),
                Some(s.created_at_ms.max(event.created_at_ms)),
                None,
            )),
            None => {
                let reason = if self.host.is_local() {
                    Some(hm::WORKER_STOP_NO_SESSION.get())
                } else {
                    Some(hm::WORKER_STOP_OFFLINE.get())
                };
                Some((true, title.clone(), Some(event.created_at_ms), reason))
            }
        };
        Some((title, finished))
    }

    /// Orca's `BotItemQuestionStep` (#46693): the lines answered so far fold above, the open one
    /// shows with ‹ n of total › and its quick replies; picks wait until the last line or Send now.
    /// No "Type your answer" chip on the step card — the comment box below is the one place to type.
    pub(super) fn render_question_steps(
        &self,
        item: &BotItem,
        ask: &BotItemEvent,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let key = (item.uid.clone(), ask.id);
        let state = question_step_state(item, ask, self.items.ask_steps.get(&key));
        let answered = state.answered;
        let lines = state.lines;
        let kept = state.kept;
        let current = state.current;
        let held = |i: usize| kept.get(i).cloned().flatten();
        let mut out = div().flex().flex_col();
        // Lines answered (or held here) fold above as "Question → answer".
        let folded: Vec<(usize, String, bool)> = (0..ask.decisions.len())
            .filter(|&i| i != current)
            .filter_map(|i| {
                answered[i]
                    .clone()
                    .map(|w| (i, w, false))
                    .or_else(|| held(i).map(|w| (i, w, true)))
            })
            .collect();
        if !folded.is_empty() {
            let mut col = div().mb_3().flex().flex_col().gap_1();
            for (i, words, reopens) in folded {
                let q = question_line(&ask.decisions[i].question);
                let reopen_key = key.clone();
                col = col.child(
                    div()
                        .id(("bot-item-answer-line", i))
                        .debug_selector(move || format!("bot-item-answer-line-{i}"))
                        .text_size(ui::TEXT_MD)
                        .line_height(px(19.))
                        .text_color(p.muted_foreground)
                        .child(format!("{q} → "))
                        .child(div().text_color(p.foreground.opacity(0.72)).child(words))
                        .when(reopens, |d| {
                            d.child(format!(" · {}", hm::NOT_SENT_YET.get()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.items
                                        .ask_steps
                                        .entry(reopen_key.clone())
                                        .or_default()
                                        .chosen = Some(i);
                                    cx.notify();
                                }))
                        })
                        .flex()
                        .flex_wrap(),
                );
            }
            out = out.child(col);
        }
        let Some(decision) = ask.decisions.get(current) else {
            return out;
        };
        let at = lines.iter().position(|&l| l == current).unwrap_or(0);
        let nav_btn = |id: &'static str, icon: IconName, label: &'static str, to: Option<usize>| {
            let k = key.clone();
            Button::new(id)
                .debug_selector(move || id.into())
                .ghost()
                .xsmall()
                .icon(Icon::new(icon).size(px(14.)))
                .accessibility_label(label)
                .disabled(to.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(to) = to {
                        this.items.ask_steps.entry(k.clone()).or_default().chosen = Some(to);
                        cx.notify();
                    }
                }))
        };
        let prev = at.checked_sub(1).and_then(|a| lines.get(a)).copied();
        let next = lines.get(at + 1).copied();
        out = out.child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui::TEXT_LG)
                        .line_height(px(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.foreground)
                        .child(decision.question.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(nav_btn(
                            "bot-item-ask-prev",
                            IconName::ChevronLeft,
                            hm::PREV_QUESTION.get(),
                            prev,
                        ))
                        .child(div().px_0p5().child(hm::STEP_OF.format(&[
                            ("n", &(at + 1).to_string()),
                            ("total", &lines.len().to_string()),
                        ])))
                        .child(nav_btn(
                            "bot-item-ask-next",
                            IconName::ChevronRight,
                            hm::NEXT_QUESTION.get(),
                            next,
                        )),
                ),
        );
        let picked = held(current);
        let mut replies = div().mt(px(10.)).flex().flex_wrap().gap_2();
        for (o, option) in decision.options.iter().enumerate() {
            let is_picked = picked.as_deref() == Some(option.as_str());
            let filled = decision.recommended == Some(o) && picked.is_none();
            let (k, words, uid) = (key.clone(), option.clone(), item.uid.clone());
            let decisions = ask.decisions.clone();
            let lines_now = lines.clone();
            let kept_now = kept.clone();
            replies = replies.child(
                div()
                    .id(SharedString::from(format!(
                        "bot-item-step-reply-{}-{current}-{o}",
                        ask.id
                    )))
                    .debug_selector(move || format!("bot-item-step-reply-{current}-{o}"))
                    .min_h(px(32.))
                    .max_w_full()
                    .rounded(px(16.))
                    .border_1()
                    .px(px(14.))
                    .py(px(6.))
                    .text_size(ui::TEXT_MD)
                    .line_height(px(18.))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .when(is_picked, |c| {
                        c.border_color(p.foreground.opacity(0.28))
                            .bg(p.foreground.opacity(0.08))
                            .text_color(p.foreground)
                    })
                    .when(filled, |c| {
                        c.border_color(p.foreground)
                            .bg(p.foreground)
                            .text_color(p.background)
                    })
                    .when(!is_picked && !filled, |c| {
                        c.border_color(p.border)
                            .text_color(p.foreground)
                            .hover(|c| c.bg(p.foreground.opacity(0.05)))
                    })
                    .when(is_picked, |c| {
                        c.child(Icon::new(IconName::Check).size(px(12.)))
                    })
                    .when(decision.recommended == Some(o), |c| {
                        c.child(div().text_size(ui::TEXT_2XS).opacity(0.8).child("★"))
                    })
                    .child(option.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut next_held = kept_now.clone();
                        next_held.resize(decisions.len(), None);
                        next_held[current] = Some(words.clone());
                        // Quick reply answers the line; when every open line has a pick, send all.
                        let all_open_answered = lines_now.iter().all(|&i| next_held[i].is_some());
                        if all_open_answered {
                            this.send_held_answers(
                                uid.clone(),
                                &k,
                                &decisions,
                                next_held,
                                None,
                                None,
                                None,
                                cx,
                            );
                        } else {
                            let steps = this.items.ask_steps.entry(k.clone()).or_default();
                            steps.held = next_held;
                            steps.chosen = None;
                            cx.notify();
                        }
                    })),
            );
        }
        out = out.child(replies);
        let held_count = lines.iter().filter(|&&i| held(i).is_some()).count();
        if held_count > 0 {
            let (k, uid, decisions, kept_send) = (
                key.clone(),
                item.uid.clone(),
                ask.decisions.clone(),
                kept.clone(),
            );
            // Orca `#47206` & `#47904`: "N answers not sent yet, send anyway" skips remaining
            // unanswered questions and posts the held picks immediately.
            out = out.child(
                div()
                    .mt_3()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .line_height(px(19.))
                    .text_color(p.muted_foreground)
                    .child(if held_count == 1 {
                        hm::HELD_SEND_ANYWAY_ONE.get().to_owned()
                    } else {
                        hm::HELD_SEND_ANYWAY_MANY.format(&[("n", &held_count.to_string())])
                    })
                    .child(
                        div()
                            .id("bot-item-send-held")
                            .debug_selector(|| "bot-item-send-held".into())
                            .cursor_pointer()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.foreground)
                            .underline()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.items.skipped_asks.insert(k.clone());
                                this.send_held_answers(
                                    uid.clone(),
                                    &k,
                                    &decisions,
                                    kept_send.clone(),
                                    None,
                                    None,
                                    None,
                                    cx,
                                );
                            }))
                            .child(hm::SEND_ANYWAY.get()),
                    ),
            );
        }
        out
    }

    /// Posts the answers held for an ask (and `typed` for its current line, or a trailing plain
    /// `extra_comment` when "Replying to" was dismissed) as one comment.
    #[expect(
        clippy::too_many_arguments,
        reason = "posts held picks with optional typed line answer or trailing comment"
    )]
    pub(super) fn send_held_answers(
        &mut self,
        uid: String,
        key: &(String, u64),
        decisions: &[BotItemDecision],
        answers: Vec<Option<String>>,
        typed: Option<(usize, String)>,
        extra_comment: Option<String>,
        clear_input: Option<gpui::Entity<gpui_component::input::InputState>>,
        cx: &mut Context<Self>,
    ) {
        self.send_held_answers_with_attachments(
            uid,
            key,
            decisions,
            answers,
            typed,
            extra_comment,
            clear_input,
            false,
            cx,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "posts held multi-question answers with deferred cleanup"
    )]
    pub(super) fn send_held_answers_with_attachments(
        &mut self,
        uid: String,
        key: &(String, u64),
        decisions: &[BotItemDecision],
        mut answers: Vec<Option<String>>,
        typed: Option<(usize, String)>,
        extra_comment: Option<String>,
        clear_input: Option<gpui::Entity<gpui_component::input::InputState>>,
        clear_attachments: bool,
        cx: &mut Context<Self>,
    ) {
        if self.items.busy {
            return;
        }
        answers.resize(decisions.len(), None);
        if let Some((i, words)) = typed
            && let Some(slot) = answers.get_mut(i)
        {
            *slot = Some(words);
        }
        let mut body = answers_text(decisions, &answers);
        if let Some(extra) = extra_comment {
            let extra = extra.trim();
            if !extra.is_empty() {
                if body.is_empty() {
                    extra.clone_into(&mut body);
                } else {
                    body = format!("{body}\n{extra}");
                }
            }
        }
        if body.is_empty() {
            return;
        }
        self.answer_item_decision_with_cleanup(
            uid,
            Some(body),
            None,
            None,
            clear_input,
            clear_attachments,
            Some(key.clone()),
            cx,
        );
    }

    /// The workers each of the bot's chats started (Orca's chat tree): the Dispatch links its
    /// session put on the bot's items, newest first (numeric session IDs only).
    #[must_use]
    pub fn chat_workers(
        &self,
        bot_id: &krill_proto::bots::BotId,
    ) -> Vec<(SessionId, SessionId, u64)> {
        let mut out: Vec<(SessionId, SessionId, u64)> = Vec::new();
        for (chat, target, at) in self.chat_worker_links(bot_id) {
            if let LinkedDispatchRef::Session(sid) = target
                && !out.iter().any(|(_, w, _)| *w == sid)
            {
                out.push((chat, sid, at));
            }
        }
        out
    }

    /// Dispatch references recorded on a bot's items (`dispatch:<session-id>` or
    /// `dispatch:<disp-id>`), newest first.
    #[must_use]
    pub fn chat_worker_links(
        &self,
        bot_id: &krill_proto::bots::BotId,
    ) -> Vec<(SessionId, LinkedDispatchRef, u64)> {
        let mut out: Vec<(SessionId, LinkedDispatchRef, u64)> = Vec::new();
        for item in self
            .items
            .items
            .iter()
            .filter(|i| &i.owner_bot_id == bot_id)
        {
            for e in item
                .events
                .iter()
                .filter(|e| e.kind == BotItemEventKind::Linked)
            {
                let (
                    BotItemActor::Bot {
                        session_id: Some(chat),
                        ..
                    },
                    Some(raw),
                ) = (&e.actor, e.text.strip_prefix("dispatch:"))
                else {
                    continue;
                };
                let raw = raw.trim();
                if raw.is_empty() {
                    continue;
                }
                let target = raw.parse::<u64>().map_or_else(
                    |_| LinkedDispatchRef::Dispatch(raw.to_owned()),
                    |n| LinkedDispatchRef::Session(SessionId(n)),
                );
                if let Some((_, _, prev_at)) =
                    out.iter_mut().find(|(c, r, _)| *c == *chat && *r == target)
                {
                    *prev_at = (*prev_at).max(e.created_at_ms);
                } else {
                    out.push((*chat, target, e.created_at_ms));
                }
            }
        }
        out.sort_by_key(|(_, _, at)| std::cmp::Reverse(*at));
        out
    }

    /// Where Home's bot keeps its files on this computer, for @ mentions and attachments.
    pub(super) fn bot_home_folder(&self, bot: &BotConfig) -> Option<PathBuf> {
        if !self.host.is_local() {
            return None;
        }
        if let Some(ws) = bot.workspace.as_deref().filter(|w| !w.trim().is_empty()) {
            return Some(PathBuf::from(ws));
        }
        krill_proto::daemon::state_dir().map(|s| s.join("bots").join(&bot.folder))
    }

    /// The bot folder's files matching an @ query (Orca's comment mentions), up to eight.
    /// Scans off the UI thread and caches per `(host, bot, root)` so switching workspace/host/bot
    /// never blocks rendering or serves stale paths.
    pub(super) fn mention_matches(
        &mut self,
        bot: &BotConfig,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        let Some(root) = self.bot_home_folder(bot) else {
            return Vec::new();
        };
        let key = (self.host.clone(), bot.id.clone(), root.clone());
        if let Some(files) = self.items.mention_files.get(&key) {
            let q = query.to_lowercase();
            return files
                .iter()
                .filter(|f| q.is_empty() || f.to_lowercase().contains(&q))
                .take(8)
                .cloned()
                .collect();
        }
        if self.items.mention_scanning.as_ref() != Some(&key) {
            self.items.mention_scanning = Some(key.clone());
            self.items.mention_scan_task = Some(cx.spawn(async move |this, cx| {
                let scanned = cx
                    .background_executor()
                    .spawn(async move { list_files(&root, MAX_MENTION_FILES) })
                    .await;
                this.update(cx, |this, cx| {
                    if this.items.mention_scanning.as_ref() == Some(&key) {
                        this.items.mention_scanning = None;
                    }
                    if this.host == key.0 {
                        this.items
                            .mention_files
                            .retain(|(h, b, _), _| !(h == &key.0 && b == &key.1));
                        this.items.mention_files.insert(key, scanned);
                        cx.notify();
                    }
                })
                .ok();
            }));
        }
        Vec::new()
    }
}

const MAX_MENTION_FILES: usize = 400;
const MAX_MENTION_DIRS: usize = 512;

/// A dispatch reference recorded on a bot item's `Linked` event (`dispatch:<id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedDispatchRef {
    Session(SessionId),
    Dispatch(String),
}

/// `owner/repo` of a GitHub URL.
fn repo_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    Some(format!("{}/{}", parts.next()?, parts.next()?))
}

/// The files under `root` (relative, hidden ones and symlinks skipped), breadth first, capped
/// by both file count and visited directories so huge trees or symlink cycles never hang.
fn list_files(root: &std::path::Path, cap: usize) -> Vec<String> {
    list_files_bounded(root, cap, MAX_MENTION_DIRS)
}

fn list_files_bounded(root: &std::path::Path, max_files: usize, max_dirs: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    let mut visited_dirs = 0usize;
    while let Some(dir) = queue.pop_front() {
        if visited_dirs >= max_dirs {
            break;
        }
        visited_dirs += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else {
                continue;
            };
            // Never follow symlinked directories or files (avoids symlink loops and escapes).
            if ft.is_symlink() {
                continue;
            }
            let path = e.path();
            if ft.is_dir() {
                queue.push_back(path);
            } else if ft.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(rel.to_string_lossy().into_owned());
                if out.len() >= max_files {
                    return out;
                }
            }
        }
    }
    out
}

/// The `@query` the comment ends with, if any (no whitespace after the `@`).
pub(super) fn trailing_mention(text: &str) -> Option<&str> {
    let at = text.rfind('@')?;
    let before_ok = at == 0
        || text
            .get(..at)
            .is_some_and(|b| b.ends_with(char::is_whitespace));
    let query = text.get(at + 1..)?;
    (before_ok && !query.contains(char::is_whitespace)).then_some(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(q: &str) -> BotItemDecision {
        BotItemDecision {
            question: q.to_owned(),
            options: vec!["Yes".to_owned(), "No".to_owned()],
            recommended: Some(0),
            multi: false,
        }
    }

    #[test]
    fn answers_go_one_line_per_question_and_read_back() {
        let ds = [d("Ship it?"), d("Tell the team?\nThey asked.")];
        let text = answers_text(
            &ds,
            &[Some("Yes".to_owned()), Some("No  thanks".to_owned())],
        );
        assert_eq!(text, "Ship it? → Yes\nTell the team? → No thanks");
        assert_eq!(
            named_answers(&ds, &text),
            [Some("Yes".to_owned()), Some("No thanks".to_owned())]
        );
        assert_eq!(
            named_answers(&ds, "Ship it? → Later"),
            [Some("Later".to_owned()), None]
        );
    }

    #[test]
    fn mention_is_the_trailing_at_word() {
        assert_eq!(trailing_mention("see @src/ma"), Some("src/ma"));
        assert_eq!(trailing_mention("@"), Some(""));
        assert_eq!(trailing_mention("mail a@b.c"), None);
        assert_eq!(trailing_mention("@done here"), None);
    }

    #[test]
    fn edit_fields_read_from_the_event() {
        let e = BotItemEvent {
            id: 1,
            item_uid: "u".to_owned(),
            kind: BotItemEventKind::Edited,
            actor: BotItemActor::Owner { device: None },
            text: r#"{"fields":{"title":{"from":"A","to":"B"}}}"#.to_owned(),
            decisions: vec![],
            created_at_ms: 0,
        };
        assert_eq!(
            edited_fields(&e),
            [("title".to_owned(), "A".to_owned(), "B".to_owned())]
        );
        let old = BotItemEvent {
            text: "updated item".to_owned(),
            ..e
        };
        assert!(edited_fields(&old).is_empty());
    }

    #[test]
    fn question_line_strips_markdown_marks() {
        assert_eq!(question_line("## **Ship it?**\nDetails"), "Ship it?");
        assert_eq!(question_line("- Run `cargo test`?"), "Run cargo test?");
    }

    #[test]
    fn named_answers_matches_both_normalized_and_raw_markdown_prefixes() {
        let ds = vec![d("## **Target env?**\nExtra"), d("- Run `cargo test`?")];
        // Normalized prefixes (Home / new daemon)
        assert_eq!(
            named_answers(&ds, "Target env? → Staging\nRun cargo test? → Yes"),
            [Some("Staging".to_owned()), Some("Yes".to_owned())]
        );
        // Raw Markdown prefixes (historical / CLI / Modal)
        assert_eq!(
            named_answers(&ds, "## **Target env?** → Prod\n- Run `cargo test`? → Skip"),
            [Some("Prod".to_owned()), Some("Skip".to_owned())]
        );
    }

    #[test]
    fn question_line_and_named_answers_strip_interleaved_markdown_prefixes() {
        assert_eq!(question_line("> - Target env?"), "Target env?");
        assert_eq!(question_line("> ## Target env?"), "Target env?");
        assert_eq!(question_line("## *Target* env?"), "Target* env?");
        assert_eq!(
            question_line("  >  -  **Deploy** now?\nDetails"),
            "Deploy now?"
        );

        let ds = vec![d("> - Target env?"), d("> ## Rollback plan?")];
        assert_eq!(
            answers_text(&ds, &[Some("Staging".to_owned()), None]),
            "Target env? → Staging"
        );
        assert_eq!(
            named_answers(&ds, "Target env? → Staging\nRollback plan? → Revert"),
            [Some("Staging".to_owned()), Some("Revert".to_owned())]
        );
        assert_eq!(
            named_answers(&ds, "> - Target env? → Prod\n> ## Rollback plan? → None"),
            [Some("Prod".to_owned()), Some("None".to_owned())]
        );
    }

    #[test]
    fn waiting_question_stays_open_across_partial_multi_question_replies() {
        use krill_proto::bots::{BotId, BotItemStatus};

        let ask = BotItemEvent {
            id: 10,
            item_uid: "u1".to_owned(),
            kind: BotItemEventKind::Commented,
            actor: BotItemActor::Bot {
                bot_id: BotId::new("bot-1").unwrap(),
                session_id: None,
            },
            text: "Audit done.".to_owned(),
            decisions: vec![d("Q1?"), d("Q2?"), d("Q3?")],
            created_at_ms: 100,
        };
        let mut item = BotItem {
            uid: "u1".to_owned(),
            number: 1,
            owner_bot_id: BotId::new("bot-1").unwrap(),
            title: "Multi ask".to_owned(),
            description: None,
            done_means: None,
            status: BotItemStatus::NeedsYou,
            close_note: None,
            links: vec![],
            events: vec![ask.clone()],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: 100,
            created_at_ms: 100,
            updated_at_ms: 100,
            closed_at_ms: None,
        };
        assert_eq!(
            super::super::home_item::waiting_question(&item).map(|e| e.id),
            Some(10)
        );
        assert_eq!(question_step_state(&item, &ask, None).current, 0);

        // First line answered via free-form text: krilld moves status to Working,
        // yet Q2 and Q3 remain open and waiting_question stays active on Q2.
        item.status = BotItemStatus::Working;
        item.events.push(BotItemEvent {
            id: 11,
            item_uid: "u1".to_owned(),
            kind: BotItemEventKind::Commented,
            actor: BotItemActor::Owner { device: None },
            text: "Q1? → Custom answer".to_owned(),
            decisions: vec![],
            created_at_ms: 200,
        });
        item.events.push(BotItemEvent {
            id: 12,
            item_uid: "u1".to_owned(),
            kind: BotItemEventKind::Status,
            actor: BotItemActor::Owner { device: None },
            text: "status -> working".to_owned(),
            decisions: vec![],
            created_at_ms: 200,
        });
        assert_eq!(
            super::super::home_item::waiting_question(&item).map(|e| e.id),
            Some(10)
        );
        let state = question_step_state(&item, &ask, None);
        assert_eq!(state.lines, vec![1, 2]);
        assert_eq!(state.current, 1);

        // Answering Q2 and Q3 closes the waiting ask completely.
        item.events.push(BotItemEvent {
            id: 13,
            item_uid: "u1".to_owned(),
            kind: BotItemEventKind::Commented,
            actor: BotItemActor::Owner { device: None },
            text: "Q2? → Yes\nQ3? → Later".to_owned(),
            decisions: vec![],
            created_at_ms: 300,
        });
        assert!(super::super::home_item::waiting_question(&item).is_none());
    }

    #[gpui::test]
    fn multi_question_free_form_reply_sends_immediately_and_advances(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::{Arc, Mutex};

        use gpui::{Modifiers, px};
        use krill_proto::bots::{BotConfig, BotId, BotItemStatus, BotRpcRequest, BotRpcResponse};

        struct State {
            item: BotItem,
            answers: Vec<String>,
            comments: Vec<String>,
            next_event_id: u64,
        }

        let bot_id = BotId::new("bot-1").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Helper", "agy", 1_700_000_000_000).unwrap();
        let initial_item = BotItem {
            uid: "multi-1".to_owned(),
            number: 42,
            owner_bot_id: bot_id.clone(),
            title: "Per-chat layouts".to_owned(),
            description: None,
            done_means: None,
            status: BotItemStatus::NeedsYou,
            close_note: None,
            links: vec![],
            events: vec![BotItemEvent {
                id: 61,
                item_uid: "multi-1".to_owned(),
                kind: BotItemEventKind::Commented,
                actor: BotItemActor::Bot {
                    bot_id: bot_id.clone(),
                    session_id: None,
                },
                text: "Three quick calls.".to_owned(),
                decisions: vec![
                    BotItemDecision {
                        question: "Where should the switcher live?".to_owned(),
                        options: vec!["Top strip".to_owned(), "Right panel".to_owned()],
                        recommended: Some(0),
                        multi: false,
                    },
                    BotItemDecision {
                        question: "Keep sidebar chat rows?".to_owned(),
                        options: vec!["No".to_owned(), "Yes".to_owned()],
                        recommended: Some(0),
                        multi: false,
                    },
                    BotItemDecision {
                        question: "Shortcut to switch chats?".to_owned(),
                        options: vec!["Cmd+1-9".to_owned(), "Ctrl+1-9".to_owned()],
                        recommended: Some(0),
                        multi: false,
                    },
                ],
                created_at_ms: 1_700_000_000_000,
            }],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: 1_700_000_000_000,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_000,
            closed_at_ms: None,
        };

        let state = Arc::new(Mutex::new(State {
            item: initial_item,
            answers: Vec::new(),
            comments: Vec::new(),
            next_event_id: 62,
        }));
        let bot_for_rpc = bot.clone();
        let state_for_rpc = state.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_for_rpc.clone()])),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(vec![
                state_for_rpc.lock().unwrap().item.clone(),
            ])),
            BotRpcRequest::AnswerBotItem {
                answer_text: Some(text),
                ..
            } => {
                let mut st = state_for_rpc.lock().unwrap();
                st.answers.push(text.clone());
                let id = st.next_event_id;
                st.next_event_id += 2;
                let item_uid = st.item.uid.clone();
                st.item.status = BotItemStatus::Working;
                st.item.version += 1;
                st.item.events.push(BotItemEvent {
                    id,
                    item_uid: item_uid.clone(),
                    kind: BotItemEventKind::Commented,
                    actor: BotItemActor::Owner { device: None },
                    text: text.clone(),
                    decisions: vec![],
                    created_at_ms: 1_700_000_001_000,
                });
                st.item.events.push(BotItemEvent {
                    id: id + 1,
                    item_uid,
                    kind: BotItemEventKind::Status,
                    actor: BotItemActor::Owner { device: None },
                    text: "status -> working".to_owned(),
                    decisions: vec![],
                    created_at_ms: 1_700_000_001_000,
                });
                Ok(BotRpcResponse::Item(Box::new(st.item.clone())))
            }
            BotRpcRequest::AddItemComment { text, .. } => {
                let mut st = state_for_rpc.lock().unwrap();
                st.comments.push(text.clone());
                let id = st.next_event_id;
                st.next_event_id += 1;
                let item_uid = st.item.uid.clone();
                st.item.events.push(BotItemEvent {
                    id,
                    item_uid,
                    kind: BotItemEventKind::Commented,
                    actor: BotItemActor::Owner { device: None },
                    text: text.clone(),
                    decisions: vec![],
                    created_at_ms: 1_700_000_002_000,
                });
                Ok(BotRpcResponse::Item(Box::new(st.item.clone())))
            }
            BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(vec![])),
            BotRpcRequest::ConnectorStatus { .. } => Ok(BotRpcResponse::ConnectorStatuses(vec![])),
            BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(vec![])),
            BotRpcRequest::GetBotUpcoming { .. } => Ok(BotRpcResponse::Upcoming(
                krill_proto::bots::BotUpcoming::default(),
            )),
            BotRpcRequest::GetSoul { .. } | BotRpcRequest::GetMemory { .. } => {
                Ok(BotRpcResponse::Text(String::new()))
            }
            BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(vec![])),
            BotRpcRequest::ListSuggestions { .. } => Ok(BotRpcResponse::Suggestions(vec![])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(420.), px(900.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Items, cx);
                v.open_home_item("multi-1".to_owned(), cx);
            });
        });
        vcx.run_until_parked();

        // Step card has NO "Type your answer" chip and NO "Send now" before anything is held.
        assert!(vcx.debug_bounds("bot-item-step-reply-0-0").is_some());
        assert!(vcx.debug_bounds("bot-item-type-answer-61-0").is_none());
        assert!(vcx.debug_bounds("bot-item-send-held").is_none());

        // 1) Typing a free-form answer for Q1 sends IMMEDIATELY (without files or waiting for Q2/Q3).
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.send_item_comment(
                    "multi-1".to_owned(),
                    "Top strip with a review pass",
                    None,
                    cx,
                );
            });
        });
        vcx.run_until_parked();
        assert_eq!(
            state.lock().unwrap().answers,
            vec!["Where should the switcher live? → Top strip with a review pass"]
        );

        // After host sets status=Working + records Q1 answer, Q1 folds above and step advances to Q2!
        assert!(vcx.debug_bounds("bot-item-answer-line-0").is_some());
        assert!(vcx.debug_bounds("bot-item-step-reply-1-0").is_some());

        // 2) Pick Q2 via quick reply (held), then click "Send now" — wait: test held + typed on Q3!
        let q2_chip = vcx
            .debug_bounds("bot-item-step-reply-1-0")
            .expect("Q2 quick reply");
        vcx.simulate_click(q2_chip.center(), Modifiers::default());
        vcx.run_until_parked();
        // Held on device, not sent yet; "Send now" appears and step moves to Q3.
        assert_eq!(state.lock().unwrap().answers.len(), 1);
        assert!(vcx.debug_bounds("bot-item-send-held").is_some());
        assert!(vcx.debug_bounds("bot-item-step-reply-2-0").is_some());

        // 3) Free-form reply on Q3 sends BOTH held Q2 pick and typed Q3 reply at once!
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.send_item_comment("multi-1".to_owned(), "Use Alt+1-9", None, cx);
            });
        });
        vcx.run_until_parked();
        assert_eq!(
            state.lock().unwrap().answers,
            vec![
                "Where should the switcher live? → Top strip with a review pass",
                "Keep sidebar chat rows? → No\nShortcut to switch chats? → Use Alt+1-9",
            ]
        );
        // All 3 questions answered: stepper & Send now disappear.
        assert!(vcx.debug_bounds("bot-item-send-held").is_none());
        assert!(vcx.debug_bounds("bot-item-step-reply-2-0").is_none());
    }

    #[gpui::test]
    fn multi_question_send_now_and_dismissed_replying_to_work(cx: &mut gpui::TestAppContext) {
        use std::sync::{Arc, Mutex};

        use gpui::{Modifiers, px};
        use krill_proto::bots::{BotConfig, BotId, BotItemStatus, BotRpcRequest, BotRpcResponse};

        let bot_id = BotId::new("bot-2").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Helper", "agy", 1_700_000_000_000).unwrap();
        let item = BotItem {
            uid: "multi-2".to_owned(),
            number: 43,
            owner_bot_id: bot_id.clone(),
            title: "Send now test".to_owned(),
            description: None,
            done_means: None,
            status: BotItemStatus::NeedsYou,
            close_note: None,
            links: vec![],
            events: vec![BotItemEvent {
                id: 71,
                item_uid: "multi-2".to_owned(),
                kind: BotItemEventKind::Commented,
                actor: BotItemActor::Bot {
                    bot_id: bot_id.clone(),
                    session_id: None,
                },
                text: "Pick".to_owned(),
                decisions: vec![d("First?"), d("Second?"), d("Third?")],
                created_at_ms: 1_700_000_000_000,
            }],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: 1_700_000_000_000,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_000,
            closed_at_ms: None,
        };
        let answers = Arc::new(Mutex::new(Vec::<String>::new()));
        let comments = Arc::new(Mutex::new(Vec::<String>::new()));
        let item_state = Arc::new(Mutex::new(item));
        let bot_for_rpc = bot.clone();
        let ans_rpc = answers.clone();
        let com_rpc = comments.clone();
        let it_rpc = item_state.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_for_rpc.clone()])),
            BotRpcRequest::ListItems { .. } => {
                Ok(BotRpcResponse::Items(vec![it_rpc.lock().unwrap().clone()]))
            }
            BotRpcRequest::AnswerBotItem {
                answer_text: Some(text),
                ..
            } => {
                ans_rpc.lock().unwrap().push(text.clone());
                let mut it = it_rpc.lock().unwrap();
                let ev_id = 72 + u64::try_from(it.events.len()).unwrap_or(0);
                let item_uid = it.uid.clone();
                it.status = BotItemStatus::Working;
                it.events.push(BotItemEvent {
                    id: ev_id,
                    item_uid,
                    kind: BotItemEventKind::Commented,
                    actor: BotItemActor::Owner { device: None },
                    text: text.clone(),
                    decisions: vec![],
                    created_at_ms: 1_700_000_001_000,
                });
                Ok(BotRpcResponse::Item(Box::new(it.clone())))
            }
            BotRpcRequest::AddItemComment { text, .. } => {
                com_rpc.lock().unwrap().push(text.clone());
                Ok(BotRpcResponse::Item(Box::new(
                    it_rpc.lock().unwrap().clone(),
                )))
            }
            BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(vec![])),
            BotRpcRequest::ConnectorStatus { .. } => Ok(BotRpcResponse::ConnectorStatuses(vec![])),
            BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(vec![])),
            BotRpcRequest::GetBotUpcoming { .. } => Ok(BotRpcResponse::Upcoming(
                krill_proto::bots::BotUpcoming::default(),
            )),
            BotRpcRequest::GetSoul { .. } | BotRpcRequest::GetMemory { .. } => {
                Ok(BotRpcResponse::Text(String::new()))
            }
            BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(vec![])),
            BotRpcRequest::ListSuggestions { .. } => Ok(BotRpcResponse::Suggestions(vec![])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(420.), px(900.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Items, cx);
                v.open_home_item("multi-2".to_owned(), cx);
            });
        });
        vcx.run_until_parked();

        // Pick Q1, then click Send now: posts only Q1 immediately.
        let q1 = vcx.debug_bounds("bot-item-step-reply-0-0").expect("Q1");
        vcx.simulate_click(q1.center(), Modifiers::default());
        vcx.run_until_parked();
        let send_now = vcx.debug_bounds("bot-item-send-held").expect("Send now");
        vcx.simulate_click(send_now.center(), Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(answers.lock().unwrap().as_slice(), ["First? → Yes"]);

        // Now hide "Replying to" with no held picks: typed text sends as a plain comment.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.items.reply_hidden.insert("multi-2".to_owned(), 71);
                v.send_item_comment("multi-2".to_owned(), "Side note only", None, cx);
            });
        });
        vcx.run_until_parked();
        assert_eq!(comments.lock().unwrap().as_slice(), ["Side note only"]);
        assert_eq!(answers.lock().unwrap().len(), 1);
    }

    #[test]
    fn domain_c_waiting_note_and_skipped_ask_helpers() {
        use std::collections::BTreeSet;

        use krill_proto::bots::{BotId, BotItemStatus};

        let encoded = super::super::home::encode_waiting_note("CI build");
        assert_eq!(encoded, "waiting:CI build");
        let bot_id = BotId::new("bot-w").unwrap();
        let ask = BotItemEvent {
            id: 50,
            item_uid: "w1".to_owned(),
            kind: BotItemEventKind::Commented,
            actor: BotItemActor::Bot {
                bot_id: bot_id.clone(),
                session_id: None,
            },
            text: "Need a call.".to_owned(),
            decisions: vec![d("First?"), d("Second?")],
            created_at_ms: 1_700_000_000_000,
        };
        let item = BotItem {
            uid: "w1".to_owned(),
            number: 7,
            owner_bot_id: bot_id,
            title: "Wait test".to_owned(),
            description: None,
            done_means: None,
            status: BotItemStatus::Planned,
            close_note: Some(encoded),
            links: vec![],
            events: vec![ask],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: 1_700_000_000_000,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_000,
            closed_at_ms: None,
        };
        assert_eq!(super::super::home::waiting_note(&item), Some("CI build"));

        // Orca `#47904`: skipping an ask closes waiting_question_with_skipped immediately.
        let mut needs_item = item.clone();
        needs_item.status = BotItemStatus::NeedsYou;
        let empty = BTreeSet::new();
        assert_eq!(
            super::super::home_item::waiting_question_with_skipped(&needs_item, &empty)
                .map(|e| e.id),
            Some(50)
        );
        let skipped = BTreeSet::from([("w1".to_owned(), 50)]);
        assert!(
            super::super::home_item::waiting_question_with_skipped(&needs_item, &skipped).is_none()
        );
    }

    #[gpui::test]
    fn domain_c_item_detail_wait_card_failed_comment_superseded_and_webhooks(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::Arc;

        use gpui::px;
        use krill_proto::bots::{
            BotConfig, BotId, BotItemStatus, BotRpcRequest, BotRpcResponse, BotScheduleConfig,
        };

        let bot_id = BotId::new("bot-c").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "KrillBot", "agy", 1_700_000_000_000).unwrap();
        bot.schedules.push(BotScheduleConfig {
            id: "webhook-deploy".to_owned(),
            name: "Deploy webhook".to_owned(),
            cron: "0 * * * *".to_owned(),
            prompt: "Handle deploy webhook".to_owned(),
            enabled: true,
            timezone: None,
        });
        let item = BotItem {
            uid: "item-c".to_owned(),
            number: 27,
            owner_bot_id: bot_id.clone(),
            title: "Domain C parity check".to_owned(),
            description: Some("Full markdown description.".to_owned()),
            done_means: None,
            status: BotItemStatus::NeedsYou,
            close_note: Some("waiting:Waiting on reviewer".to_owned()),
            links: vec![BotItemLink::Pr(
                "https://github.com/craftowen/krill/pull/212".to_owned(),
            )],
            events: vec![
                BotItemEvent {
                    id: 1,
                    item_uid: "item-c".to_owned(),
                    kind: BotItemEventKind::Commented,
                    actor: BotItemActor::Bot {
                        bot_id: bot_id.clone(),
                        session_id: None,
                    },
                    text: "Old question".to_owned(),
                    decisions: vec![d("Old Q?")],
                    created_at_ms: 1_700_000_000_000,
                },
                BotItemEvent {
                    id: 2,
                    item_uid: "item-c".to_owned(),
                    kind: BotItemEventKind::Commented,
                    actor: BotItemActor::Bot {
                        bot_id: bot_id.clone(),
                        session_id: None,
                    },
                    text: "Newer question".to_owned(),
                    decisions: vec![d("New Q?")],
                    created_at_ms: 1_700_000_005_000,
                },
            ],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: 1_700_000_005_000,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_005_000,
            closed_at_ms: None,
        };
        let bot_rpc = bot.clone();
        let item_rpc = item.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_rpc.clone()])),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(vec![item_rpc.clone()])),
            BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(vec![])),
            BotRpcRequest::ConnectorStatus { .. } => Ok(BotRpcResponse::ConnectorStatuses(vec![])),
            BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(vec![])),
            BotRpcRequest::GetBotUpcoming { .. } => Ok(BotRpcResponse::Upcoming(
                krill_proto::bots::BotUpcoming::default(),
            )),
            BotRpcRequest::GetSoul { .. } | BotRpcRequest::GetMemory { .. } => {
                Ok(BotRpcResponse::Text(String::new()))
            }
            BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(vec![])),
            BotRpcRequest::ListSuggestions { .. } => Ok(BotRpcResponse::Suggestions(vec![])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(380.), px(960.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Items, cx);
                v.open_home_item("item-c".to_owned(), cx);
                v.record_failed_comment(
                    "item-c",
                    "Refused answer draft".to_owned(),
                    false,
                    None,
                    None,
                    cx,
                );
                cx.notify();
            });
        });
        vcx.run_until_parked();

        // Superseded older ask (#47869), Wait card (#47308/#47309), failed comment (#47485),
        // full-width link chips, and bottom dictate/send buttons all render cleanly.
        assert!(vcx.debug_bounds("bot-item-ask-replaced-1").is_some());
        assert!(vcx.debug_bounds("bot-item-wait-card").is_some());
        assert!(vcx.debug_bounds("bot-item-failed-comment-1").is_some());
        assert!(vcx.debug_bounds("bot-item-link-chips").is_some());
        assert!(vcx.debug_bounds("bot-item-comment-dictate").is_some());
        assert!(vcx.debug_bounds("bot-item-comment-send").is_some());

        // Connectors tab shows Webhooks card with Copy URL (#47479).
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Connectors, cx);
            });
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("bot-webhooks-card").is_some());
        assert!(vcx.debug_bounds("bot-webhook-copy-0").is_some());
    }

    #[test]
    fn list_files_skips_symlink_loops_and_enforces_dir_cap() {
        let base = std::env::temp_dir().join(format!(
            "krill-mention-symlink-test-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("readme.md"), "hi").unwrap();
        std::fs::write(sub.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(base.join(".hidden"), "secret").unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("..", sub.join("loop")).unwrap();
            std::os::unix::fs::symlink(&base, base.join("self_loop")).unwrap();
            std::os::unix::fs::symlink(base.join("readme.md"), base.join("link.md")).unwrap();
        }

        let files = list_files(&base, MAX_MENTION_FILES);
        assert!(files.contains(&"readme.md".to_owned()), "{files:?}");
        assert!(
            files.contains(&format!("sub{}main.rs", std::path::MAIN_SEPARATOR)),
            "{files:?}"
        );
        assert!(!files.iter().any(|f| f.contains("loop")), "{files:?}");
        assert!(!files.iter().any(|f| f.contains("link.md")), "{files:?}");
        assert!(!files.iter().any(|f| f.contains(".hidden")), "{files:?}");

        // Verify directory visit cap terminates early on wide trees.
        for i in 0..12 {
            let d = base.join(format!("dir-{i:02}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("f.txt"), "x").unwrap();
        }
        let capped = list_files_bounded(&base, 400, 3);
        assert!(
            capped.len() < 12,
            "dir cap must stop walk early: {capped:?}"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[gpui::test]
    fn mention_matches_scans_in_background_and_invalidates_on_workspace_change(
        cx: &mut gpui::TestAppContext,
    ) {
        use krill_proto::bots::{BotConfig, BotId, BotRpcRequest, BotRpcResponse};
        use std::sync::Arc;

        let base = std::env::temp_dir().join(format!(
            "krill-mention-ws-test-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let ws1 = base.join("ws1");
        let ws2 = base.join("ws2");
        std::fs::create_dir_all(&ws1).unwrap();
        std::fs::create_dir_all(&ws2).unwrap();
        std::fs::write(ws1.join("alpha.rs"), "").unwrap();
        std::fs::write(ws2.join("beta.rs"), "").unwrap();

        let bot_id = BotId::new("mention-bot").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Helper", "agy", 1_700_000_000_000).unwrap();
        bot.workspace = Some(ws1.to_string_lossy().into_owned());
        let bot_rpc = bot.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_rpc.clone()])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.run_until_parked();

        // First call schedules background scan and returns empty synchronously.
        let initial = vcx.update(|_, cx| view.update(cx, |v, cx| v.mention_matches(&bot, "", cx)));
        assert!(initial.is_empty());
        vcx.run_until_parked();
        let ws1_matches =
            vcx.update(|_, cx| view.update(cx, |v, cx| v.mention_matches(&bot, "", cx)));
        assert_eq!(ws1_matches, vec!["alpha.rs"]);

        // Switching bot.workspace invalidates the cache key and scans ws2 instead of serving ws1.
        bot.workspace = Some(ws2.to_string_lossy().into_owned());
        let after_switch =
            vcx.update(|_, cx| view.update(cx, |v, cx| v.mention_matches(&bot, "", cx)));
        assert!(after_switch.is_empty());
        vcx.run_until_parked();
        let ws2_matches =
            vcx.update(|_, cx| view.update(cx, |v, cx| v.mention_matches(&bot, "", cx)));
        assert_eq!(ws2_matches, vec!["beta.rs"]);

        std::fs::remove_dir_all(&base).ok();
    }

    #[gpui::test]
    fn alt_left_respects_input_focus_and_control_modifier(cx: &mut gpui::TestAppContext) {
        use gpui::{KeyDownEvent, Keystroke, Modifiers};
        use krill_proto::bots::{BotConfig, BotId, BotItemStatus, BotRpcRequest, BotRpcResponse};
        use std::sync::Arc;

        let now = now_unix_ms();
        let bot_id = BotId::new("key-bot").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Key Bot", "agy", now).unwrap();
        let item = BotItem {
            uid: "k-1".to_owned(),
            number: 1,
            owner_bot_id: bot_id.clone(),
            title: "Task".to_owned(),
            description: None,
            done_means: None,
            status: BotItemStatus::Planned,
            close_note: None,
            links: vec![],
            events: vec![],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: now,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        let bot_rpc = bot.clone();
        let item_rpc = item.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_rpc.clone()])),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(vec![item_rpc.clone()])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(400.), px(700.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Items, cx);
                v.open_home_item("k-1".to_owned(), cx);
            });
        });
        vcx.run_until_parked();

        // Focus comment input: Option+Left must NOT close the item pane.
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                let inp = v.items.comment_inputs.get("k-1").unwrap().clone();
                inp.update(cx, |s, cx| s.focus(window, cx));
                let ev = KeyDownEvent {
                    keystroke: Keystroke {
                        modifiers: Modifiers::alt(),
                        key: "left".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                };
                v.item_key(&ev, window, cx);
                assert_eq!(v.items.open_uid.as_deref(), Some("k-1"));
            });
        });

        // Unfocus input: Ctrl+Option+Left must NOT close; plain Option+Left closes.
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                window.focus(&v.focus, cx);
                let ctrl_alt_left = KeyDownEvent {
                    keystroke: Keystroke {
                        modifiers: Modifiers {
                            control: true,
                            alt: true,
                            ..Modifiers::default()
                        },
                        key: "left".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                };
                v.item_key(&ctrl_alt_left, window, cx);
                assert_eq!(v.items.open_uid.as_deref(), Some("k-1"));

                let alt_left = KeyDownEvent {
                    keystroke: Keystroke {
                        modifiers: Modifiers::alt(),
                        key: "left".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                };
                v.item_key(&alt_left, window, cx);
                assert_eq!(v.items.open_uid, None);
            });
        });
    }

    #[gpui::test]
    fn stepper_handles_filtered_out_open_item_and_group_panel_filters(
        cx: &mut gpui::TestAppContext,
    ) {
        use krill_proto::bots::{BotConfig, BotId, BotItemStatus, BotRpcRequest, BotRpcResponse};
        use std::sync::Arc;

        let now = now_unix_ms();
        let b1 = BotId::new("step-b1").unwrap();
        let b2 = BotId::new("step-b2").unwrap();
        let cfg1 = BotConfig::new(b1.clone(), "Alpha", "agy", now).unwrap();
        let cfg2 = BotConfig::new(b2.clone(), "Beta", "agy", now).unwrap();
        let mk = |bot: &BotId, uid: &str, n: u64, status: BotItemStatus, desc: &str| BotItem {
            uid: uid.to_owned(),
            number: n,
            owner_bot_id: bot.clone(),
            title: format!("Item {uid}"),
            description: Some(desc.to_owned()),
            done_means: None,
            status,
            close_note: None,
            links: vec![],
            events: vec![],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: now - n * 1_000,
            created_at_ms: now - n * 1_000,
            updated_at_ms: now - n * 1_000,
            closed_at_ms: None,
        };
        let items = vec![
            mk(&b1, "u-work", 1, BotItemStatus::Working, "grp-1"),
            mk(&b1, "u-need-1", 2, BotItemStatus::NeedsYou, "grp-1"),
            mk(&b2, "u-need-2", 3, BotItemStatus::NeedsYou, "grp-1"),
        ];
        let bots_rpc = vec![cfg1, cfg2];
        let items_rpc = items.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(bots_rpc.clone())),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(items_rpc.clone())),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.run_until_parked();

        // Single-bot mode: open item is Working while `needs_you_only` filters it out ->
        // step_item(1) advances into order[0] ("u-need-1") instead of no-oping.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&b1, super::super::DetailTab::Items, cx);
                v.items.set_items(items.clone());
                v.items.needs_you_only = true;
                v.open_home_item("u-work".to_owned(), cx);
                assert_eq!(v.walk_order(), vec!["u-need-1"]);
                v.step_item(-1, cx);
                assert_eq!(v.items.open_uid.as_deref(), Some("u-work"));
                v.step_item(1, cx);
                assert_eq!(v.items.open_uid.as_deref(), Some("u-need-1"));
            });
        });

        // Group panel mode: walk_order respects group_panel.filter (bot_ids & needs_you_only).
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_group_in_panel("grp-1", &[b1.clone(), b2.clone()], &[], cx);
                v.items.set_items(items.clone());
                v.toggle_group_filter_bot(b2.clone(), cx);
                assert_eq!(v.walk_order(), vec!["u-need-2"]);
            });
        });
    }
}
