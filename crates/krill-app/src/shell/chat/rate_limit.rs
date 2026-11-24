// UI interaction patterns referenced from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
//! Rate-limit banner details (which window, when it resets) and auto-continue after the reset.

use std::time::Duration;

use gpui::{Context, Task};
use krill_app::accounts::display;
use krill_app::i18n::catalog::chat as ctext;
use krill_proto::control::AgentState;
use krill_proto::usage::{AccountStatus, AccountsSnapshot};

use super::{ChatKey, QueuedChatPrompt, Shell, build_submitted_chat_payload};

/// Resets further away than this get no clock in the banner and no auto-continue.
pub(super) const RESET_HORIZON_SECS: u64 = 24 * 3600;
/// Usage files lag the provider by a few seconds; resume a little after the reset.
const RESUME_GRACE_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LimitKind {
    FiveHour,
    Weekly,
    Model,
    Extra,
    Generic,
}

impl LimitKind {
    fn of_meter(key: &str) -> Self {
        match key {
            "five_hour" | "5h" => Self::FiveHour,
            "seven_day" | "7d" => Self::Weekly,
            k if k.contains("extra") => Self::Extra,
            k if k.contains("weekly") => Self::Weekly,
            k if k.starts_with("seven_day_") => Self::Model,
            _ => Self::Generic,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LimitInfo {
    pub kind: LimitKind,
    /// Epoch seconds, always after `now`.
    pub reset_at: u64,
    /// The window is at 100%, so it is what blocks the account.
    pub full: bool,
}

/// The window limiting `account_id` and its next reset.
///
/// Full windows win, and among them the latest reset, because the account stays
/// blocked until every full window has reset. Without a full window the most used
/// one is shown. Windows without a future reset are ignored.
pub(super) fn limit_info(snap: &AccountsSnapshot, account_id: &str, now: u64) -> Option<LimitInfo> {
    let future = |raw: Option<&str>| {
        raw.and_then(krill_proto::time::parse_reset_epoch)
            .and_then(|at| u64::try_from(at).ok())
            .filter(|at| *at > now)
    };
    let mut windows: Vec<(LimitKind, u8, u64)> = Vec::new();
    if let Some(info) = snap.accounts.iter().find(|a| a.id == account_id) {
        for (kind, window) in [
            (LimitKind::FiveHour, &info.usage.five_hour),
            (LimitKind::Weekly, &info.usage.seven_day),
        ] {
            if let Some(w) = window
                && let Some(at) = future(w.resets_at.as_deref())
            {
                windows.push((kind, w.percent, at));
            }
        }
    }
    for model in &snap.advanced.model_windows {
        if model.account_id == account_id
            && let Some(at) = future(model.seven_day.resets_at.as_deref())
        {
            windows.push((LimitKind::Model, model.seven_day.percent, at));
        }
    }
    for meter in snap.meters_for(account_id).map_or(&[][..], |m| &m.meters) {
        if let Some(w) = &meter.window
            && let Some(at) = future(w.resets_at.as_deref())
        {
            windows.push((LimitKind::of_meter(&meter.key), w.percent, at));
        }
    }
    let full = windows.iter().filter(|w| w.1 >= 100).max_by_key(|w| w.2);
    let best = full.or_else(|| {
        windows
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)))
    });
    if let Some(&(kind, percent, reset_at)) = best {
        return Some(LimitInfo {
            kind,
            reset_at,
            full: percent >= 100,
        });
    }
    snap.advanced
        .rate_limits
        .iter()
        .filter(|r| r.account_id == account_id && r.clears_at > now)
        .map(|r| r.clears_at)
        .min()
        .map(|reset_at| LimitInfo {
            kind: LimitKind::Generic,
            reset_at,
            full: true,
        })
}

/// Banner sentence: the limit kind and its local reset clock, or the generic text.
/// Orca's `NativeChatUsageLimitRow` words: "{agent} 주간 한도에 도달했습니다" and "{time}에 계속 가능"
/// ("{time}에 계속" once it will continue by itself).
pub(super) fn banner_words(
    info: Option<LimitInfo>,
    agent: &str,
    scheduled: bool,
) -> (String, String) {
    let agent = krill_proto::agent_history::VaultAgent::parse(agent)
        .map_or_else(|| ctext::LIMIT_AGENT.get(), |a| a.label());
    let title = match info.map(|i| i.kind) {
        Some(LimitKind::Weekly) => &ctext::LIMIT_TITLE_WEEKLY,
        Some(LimitKind::Extra) => &ctext::LIMIT_TITLE_EXTRA,
        _ => &ctext::LIMIT_TITLE_USAGE,
    }
    .format(&[("agent", agent)]);
    let status = info.and_then(|i| reset_clock(i.reset_at)).map_or_else(
        || ctext::LIMIT_RESET_UNKNOWN.get().to_owned(),
        |time| {
            if scheduled {
                ctext::LIMIT_CONTINUES_AT.format(&[("time", &time)])
            } else {
                ctext::LIMIT_CAN_CONTINUE_AT.format(&[("time", &time)])
            }
        },
    );
    (title, status)
}

/// The reset in the reader's clock, as Orca's message times read (`오후 6:18`).
pub(super) fn reset_clock(epoch: u64) -> Option<String> {
    let epoch = i64::try_from(epoch).ok()?;
    let now = i64::try_from(display::now()).ok()?;
    krill_app::chat_message_time(epoch, now).map(|(short, _)| short)
}

/// A scheduled "continue" prompt for one chat after its limit resets.
/// Dropping the entry drops the timer task, which cancels it.
pub(super) struct AutoContinue {
    pub reset_at: u64,
    account_id: String,
    _task: Task<()>,
}

/// Why auto-arming stays off for a chat until its rate limit clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Declined {
    /// Cancelled or answered by hand for the cycle ending at this reset.
    Cycle(u64),
    /// Already resumed once; another limit needs the user's decision.
    Episode,
}

impl Shell {
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "tests override the clock on self")
    )]
    pub(super) fn chat_now_secs(&self) -> u64 {
        #[cfg(test)]
        if let Some(now) = self.chat.now_override {
            return now;
        }
        display::now()
    }

    fn chat_account_id(&self, key: &ChatKey, cx: &gpui::App) -> Option<String> {
        self.sidebar
            .read(cx)
            .session_on(&key.0, key.1)?
            .account
            .as_ref()
            .map(|a| a.account_id.clone())
    }

    /// A chat stopped on its usage limit, for the sidebar's gauge (Orca `UsageLimitStop`):
    /// whether Krill continues it at the reset, and the reset's local clock. Only chats whose
    /// log has been read know.
    pub(in crate::shell) fn chat_usage_stop(
        &self,
        key: &ChatKey,
        cx: &gpui::App,
    ) -> Option<(bool, Option<String>)> {
        if !self.chat.logs.get(key)?.meta.rate_limited {
            return None;
        }
        let auto = self.chat.auto_continue.get(key).map(|e| e.reset_at);
        let reset = auto.or_else(|| self.chat_limit_info(key, cx).map(|i| i.reset_at));
        Some((auto.is_some(), reset.and_then(reset_clock)))
    }

    /// The limit blocking this chat's current account, from the host's account snapshot.
    pub(super) fn chat_limit_info(&self, key: &ChatKey, cx: &gpui::App) -> Option<LimitInfo> {
        let account = self.chat_account_id(key, cx)?;
        limit_info(
            &self.accounts_for_host(&key.0),
            &account,
            self.chat_now_secs(),
        )
    }

    /// Schedules the continue prompt for the next reset. Returns whether one is scheduled.
    pub(super) fn arm_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) -> bool {
        let now = self.chat_now_secs();
        let Some(account_id) = self.chat_account_id(key, cx) else {
            return false;
        };
        let Some(info) = self.chat_limit_info(key, cx) else {
            return false;
        };
        if info.reset_at.saturating_sub(now) > RESET_HORIZON_SECS {
            return false;
        }
        if self
            .chat
            .auto_continue
            .get(key)
            .is_some_and(|e| e.reset_at == info.reset_at && e.account_id == account_id)
        {
            return true;
        }
        let wait = Duration::from_secs(info.reset_at - now + RESUME_GRACE_SECS);
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| this.fire_auto_continue(&task_key, cx))
                .ok();
        });
        self.chat.auto_continue.insert(
            key.clone(),
            AutoContinue {
                reset_at: info.reset_at,
                account_id,
                _task: task,
            },
        );
        cx.notify();
        true
    }

    /// Banner Cancel, a hand-sent prompt or an account switch: drop the timer for this cycle.
    pub(super) fn cancel_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        if let Some(entry) = self.chat.auto_continue.remove(key) {
            self.chat
                .auto_continue_declined
                .insert(key.clone(), Declined::Cycle(entry.reset_at));
            cx.notify();
        }
    }

    /// Re-evaluates the schedule after the composer status (and `rate_limited`) refreshed.
    pub(super) fn sync_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        let rate_limited = self
            .chat
            .logs
            .get(key)
            .is_some_and(|log| log.meta.rate_limited);
        let current_account = self.chat_account_id(key, cx);
        if let Some(log) = self.chat.logs.get_mut(key) {
            if rate_limited {
                if log.rate_limited_account.is_none() {
                    log.rate_limited_account.clone_from(&current_account);
                }
            } else {
                log.rate_limited_account = None;
                log.rate_limit_account_switched = false;
            }
        }
        let now = self.chat_now_secs();
        if let Some(entry) = self.chat.auto_continue.get(key) {
            // Cleared before the reset means someone resolved it; after the reset the timer is due.
            if current_account.as_deref() != Some(entry.account_id.as_str())
                || (!rate_limited && now < entry.reset_at)
            {
                self.chat.auto_continue.remove(key);
                cx.notify();
            }
            return;
        }
        if !rate_limited {
            self.chat.auto_continue_declined.remove(key);
            return;
        }
        if !krill_app::settings::chat_auto_continue_after_rate_limit(cx) {
            return;
        }
        let declined = self.chat.auto_continue_declined.get(key).copied();
        let reset_at = self.chat_limit_info(key, cx).map(|i| i.reset_at);
        if declined == Some(Declined::Episode)
            || reset_at.is_some_and(|at| declined == Some(Declined::Cycle(at)))
        {
            return;
        }
        self.arm_auto_continue(key, cx);
    }

    /// Orca `#47079`: switching accounts on a usage-limited chat makes it resumable immediately
    /// even when the original provider reset time is unknown.
    pub(super) fn rate_limit_ready_after_switch(&self, key: &ChatKey, cx: &gpui::App) -> bool {
        let Some(log) = self.chat.logs.get(key) else {
            return false;
        };
        if !log.meta.rate_limited {
            return false;
        }
        if log.rate_limit_account_switched {
            return true;
        }
        let current = self.chat_account_id(key, cx);
        matches!(
            (&log.rate_limited_account, current),
            (Some(prev), Some(curr)) if prev != &curr
        )
    }

    /// Sends the manual continue prompt after an account switch or reset (`#47079`).
    pub(super) fn continue_after_rate_limit(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        self.cancel_auto_continue(key, cx);
        let text = ctext::AUTO_CONTINUE_PROMPT.get().to_owned();
        let Some(submitted_payload) = build_submitted_chat_payload(&text, &[]) else {
            return;
        };
        self.chat.next_pending_id = self.chat.next_pending_id.wrapping_add(1);
        let id = self.chat.next_pending_id;
        let log = self.chat.logs.entry(key.clone()).or_default();
        log.meta.rate_limited = false;
        log.rate_limited_account = None;
        log.rate_limit_account_switched = false;
        log.queued_prompts.push_front(QueuedChatPrompt {
            id,
            display_text: text,
            submitted_payload,
            attachments: Vec::new(),
        });
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        self.flush_queued_chat_prompt_if_ready(key, cx);
        cx.notify();
    }

    fn fire_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        let Some(entry) = self.chat.auto_continue.remove(key) else {
            return;
        };
        cx.notify();
        let Some(pane) = self.chat_pane(key) else {
            return;
        };
        if !self.is_pane_chat_view(pane, cx)
            || self
                .live
                .get(&pane)
                .is_some_and(|live| live.pane.account_switching())
            || self.session_agent_state(&key.0, key.1, cx) == AgentState::Exited
            || self.chat_account_id(key, cx).as_deref() != Some(entry.account_id.as_str())
        {
            return;
        }
        let snap = self.accounts_for_host(&key.0);
        if snap
            .status_for(&entry.account_id)
            .is_some_and(|s| s.status == AccountStatus::NeedsLogin)
        {
            return;
        }
        // Another full window (say weekly behind a 5h reset) still blocks: wait for that one.
        if limit_info(&snap, &entry.account_id, self.chat_now_secs()).is_some_and(|i| i.full) {
            self.arm_auto_continue(key, cx);
            return;
        }
        let text = ctext::AUTO_CONTINUE_PROMPT.get().to_owned();
        let Some(submitted_payload) = build_submitted_chat_payload(&text, &[]) else {
            return;
        };
        self.chat.next_pending_id = self.chat.next_pending_id.wrapping_add(1);
        let id = self.chat.next_pending_id;
        self.chat
            .logs
            .entry(key.clone())
            .or_default()
            .queued_prompts
            .push_front(QueuedChatPrompt {
                id,
                display_text: text,
                submitted_payload,
                attachments: Vec::new(),
            });
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        self.chat
            .auto_continue_declined
            .insert(key.clone(), Declined::Episode);
        self.flush_queued_chat_prompt_if_ready(key, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::usage::{
        AccountInfo, AccountMeters, ProviderKind, RateLimitExpiry, Snapshot, UsageMeter, Window,
    };

    const NOW: u64 = 1_900_000_000;

    fn win(percent: u8, at: u64) -> Window {
        Window {
            percent,
            resets_at: Some(at.to_string()),
        }
    }

    fn snap(five: Option<Window>, seven: Option<Window>) -> AccountsSnapshot {
        AccountsSnapshot {
            accounts: vec![AccountInfo {
                id: "main".into(),
                provider: ProviderKind::Claude,
                label: "Main".into(),
                email: None,
                profile_dir: "/synthetic/main".into(),
                active: true,
                host_default: false,
                stale: false,
                usage: Snapshot {
                    five_hour: five,
                    seven_day: seven,
                },
            }],
            ..AccountsSnapshot::default()
        }
    }

    #[test]
    fn full_windows_pick_the_latest_reset_and_others_the_most_used() {
        let five_full = snap(Some(win(100, NOW + 600)), Some(win(40, NOW + 86_000)));
        assert_eq!(
            limit_info(&five_full, "main", NOW),
            Some(LimitInfo {
                kind: LimitKind::FiveHour,
                reset_at: NOW + 600,
                full: true
            })
        );
        let both_full = snap(Some(win(100, NOW + 600)), Some(win(100, NOW + 7_200)));
        let info = limit_info(&both_full, "main", NOW).unwrap();
        assert_eq!((info.kind, info.reset_at), (LimitKind::Weekly, NOW + 7_200));
        let none_full = snap(Some(win(30, NOW + 600)), Some(win(90, NOW + 7_200)));
        let info = limit_info(&none_full, "main", NOW).unwrap();
        assert_eq!((info.kind, info.full), (LimitKind::Weekly, false));
        // Past resets do not count; unknown accounts have nothing.
        let past = snap(Some(win(100, NOW - 1)), None);
        assert_eq!(limit_info(&past, "main", NOW), None);
        assert_eq!(limit_info(&five_full, "other", NOW), None);
    }

    #[test]
    fn meters_and_daemon_expiry_fill_in_when_windows_are_missing() {
        let mut s = snap(None, None);
        s.details.accounts.push(AccountMeters {
            account_id: "main".into(),
            plan: None,
            meters: vec![UsageMeter {
                key: "extra_usage".into(),
                label: "Extra".into(),
                window: Some(win(100, NOW + 900)),
                value: None,
            }],
        });
        assert_eq!(
            limit_info(&s, "main", NOW).map(|i| i.kind),
            Some(LimitKind::Extra)
        );
        let mut s = snap(None, None);
        s.advanced.rate_limits.push(RateLimitExpiry {
            account_id: "main".into(),
            clears_at: NOW + 300,
        });
        assert_eq!(
            limit_info(&s, "main", NOW),
            Some(LimitInfo {
                kind: LimitKind::Generic,
                reset_at: NOW + 300,
                full: true
            })
        );
    }

    #[test]
    fn banner_names_the_agent_limit_and_when_it_can_continue() {
        let reset_at = NOW + 3_600;
        let info = limit_info(&snap(None, Some(win(100, reset_at))), "main", NOW);
        let clock = reset_clock(reset_at).unwrap();
        assert_eq!(
            banner_words(info, "claude", false),
            (
                "Claude 주간 한도에 도달했습니다".to_owned(),
                format!("{clock}에 계속 가능")
            )
        );
        assert_eq!(
            banner_words(info, "claude", true).1,
            format!("{clock}에 계속")
        );
        assert_eq!(
            banner_words(None, "mystery", false),
            (
                "에이전트 사용량 한도에 도달했습니다".to_owned(),
                "초기화 시각 알 수 없음".to_owned()
            )
        );
    }
}
