// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! How Home words times: Orca's `upcomingAt`, `ranText`, `lookedText`, `formatWaitTime` and the
//! header's `botQuietLine`, in the local time zone.

use crate::i18n::{Language, Message, catalog::bot_home as text, language};
use crate::local_time::{CivilTime, civil_from_epoch};

use super::home::local_offset_secs;

const DAY_MS: u64 = 86_400_000;
const WEEKDAYS: [Message; 7] = [
    text::WEEKDAY_SUN,
    text::WEEKDAY_MON,
    text::WEEKDAY_TUE,
    text::WEEKDAY_WED,
    text::WEEKDAY_THU,
    text::WEEKDAY_FRI,
    text::WEEKDAY_SAT,
];
const SHORT_MONTHS: [Message; 12] = [
    text::MONTH_SHORT_01,
    text::MONTH_SHORT_02,
    text::MONTH_SHORT_03,
    text::MONTH_SHORT_04,
    text::MONTH_SHORT_05,
    text::MONTH_SHORT_06,
    text::MONTH_SHORT_07,
    text::MONTH_SHORT_08,
    text::MONTH_SHORT_09,
    text::MONTH_SHORT_10,
    text::MONTH_SHORT_11,
    text::MONTH_SHORT_12,
];

/// `ms` in local time: its day number, its fields and its weekday (Sunday 0).
fn local(ms: u64, offset: i64) -> (i64, CivilTime, usize) {
    let secs = i64::try_from(ms / 1000).unwrap_or(0) + offset;
    let day = secs.div_euclid(86_400);
    let civil = civil_from_epoch(secs).unwrap_or(CivilTime {
        year: 1970,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    });
    // 1970-01-01 was a Thursday.
    let weekday = usize::try_from((day + 4).rem_euclid(7)).unwrap_or(0);
    (day, civil, weekday)
}

/// `8:00 PM` (`8 PM` on the hour when `short`); Korean `오후 8:00` / `오후 8시`.
fn clock_hm(hour: u8, minute: u8, short: bool, lang: Language) -> String {
    let h12 = match hour % 12 {
        0 => 12,
        h => h,
    };
    let ampm = if hour >= 12 { text::PM } else { text::AM }.in_language(lang);
    let h_str = h12.to_string();
    if short && minute == 0 {
        text::CLOCK_HOUR.format_in(lang, &[("ampm", ampm), ("hour", &h_str)])
    } else {
        let m_str = format!("{minute:02}");
        text::CLOCK_FULL.format_in(
            lang,
            &[("ampm", ampm), ("hour", &h_str), ("minute", &m_str)],
        )
    }
}

fn clock(c: &CivilTime, short: bool, lang: Language) -> String {
    clock_hm(c.hour, c.minute, short, lang)
}

/// Formats `minutes` since midnight (`9:00 AM` / `오전 9:00`) in the active language.
pub(super) fn clock_minutes(minutes: u32) -> String {
    let hour = u8::try_from((minutes / 60) % 24).unwrap_or(0);
    let minute = u8::try_from(minutes % 60).unwrap_or(0);
    clock_hm(hour, minute, false, language())
}

fn month_day(c: &CivilTime, lang: Language) -> String {
    let month = SHORT_MONTHS[usize::from(c.month.clamp(1, 12)) - 1].in_language(lang);
    let day = c.day.to_string();
    text::MONTH_DAY.format_in(lang, &[("month", month), ("day", &day)])
}

/// Orca's `formatWaitTime`: the time today, the weekday within six days, else the date.
/// Orca's `formatBotItemSnoozeTime`: as `wait_time`, always with minutes ("Thu 9:00 AM").
pub(super) fn snooze_time(at: u64, now: u64) -> String {
    wait_time_in(at, now, local_offset_secs(now), language(), false)
}

fn wait_time_in(at: u64, now: u64, offset: i64, lang: Language, short: bool) -> String {
    let (day, c, weekday) = local(at, offset);
    let (today, ..) = local(now, offset);
    let time = clock(&c, short, lang);
    if day == today {
        return time;
    }
    if at.abs_diff(now) < 6 * DAY_MS {
        let name = WEEKDAYS[weekday].in_language(lang);
        return format!("{name} {time}");
    }
    let date = month_day(&c, lang);
    text::DATE_TIME.format_in(lang, &[("date", &date), ("time", &time)])
}

/// Today, yesterday or earlier, as Orca's `lookedDay` says.
fn day_of(at: u64, now: u64, offset: i64) -> i64 {
    local(now, offset).0 - local(at, offset).0
}

/// Orca's `upcomingAt`: "Today at 4:00 PM", "Tomorrow at 9:00 AM", then the wait time.
pub(super) fn upcoming_at(at: u64, now: u64) -> String {
    let offset = local_offset_secs(now);
    let lang = language();
    let time = clock(&local(at, offset).1, false, lang);
    match day_of(at, now, offset) {
        0 => text::TODAY_AT.format(&[("time", &time)]),
        -1 => text::TOMORROW_AT.format(&[("time", &time)]),
        _ => wait_time_in(at, now, offset, lang, true),
    }
}

/// Orca's `ranText`: "Ran 6:00 PM" today, "Ran yesterday", then "Ran Nov 11".
pub(super) fn ran_text(at: u64, now: u64) -> String {
    let offset = local_offset_secs(now);
    let lang = language();
    let c = local(at, offset).1;
    match day_of(at, now, offset) {
        0 => text::RAN_AT.format(&[("time", &clock(&c, false, lang))]),
        1 => text::RAN_YESTERDAY.get().to_owned(),
        _ => text::RAN_AT.format(&[("time", &month_day(&c, lang))]),
    }
}

/// Orca's `lookedText`: "Looked 4:12 PM", "Looked yesterday 4:12 PM", then "Looked Oct 29".
pub(super) fn looked_text(at: u64, now: u64) -> String {
    let offset = local_offset_secs(now);
    let lang = language();
    let c = local(at, offset).1;
    match day_of(at, now, offset) {
        0 => text::LOOKED.format(&[("time", &clock(&c, false, lang))]),
        1 => text::LOOKED_YESTERDAY.format(&[("time", &clock(&c, false, lang))]),
        _ => text::LOOKED.format(&[("time", &month_day(&c, lang))]),
    }
}

/// Orca's `botQuietLine`: "Running {name}" while an automation runs, else "Idle · next 8:00 PM ·
/// Look around"; the second string is what a narrow panel says instead ("Next 8:00 PM").
pub(super) fn quiet_line(
    running: Option<&str>,
    next: Option<(&str, u64)>,
    now: u64,
) -> (String, Option<String>) {
    if let Some(name) = running {
        return (text::RUNNING_NAMED.format(&[("name", name)]), None);
    }
    let Some((name, at)) = next else {
        return (crate::i18n::catalog::bots::BP_IDLE.get().to_owned(), None);
    };
    let offset = local_offset_secs(now);
    let lang = language();
    let time = clock(&local(at, offset).1, false, lang);
    let when = match day_of(at, now, offset) {
        0 => time,
        -1 => text::TOMORROW_LOWER.format(&[("time", &time)]),
        _ => wait_time_in(at, now, offset, lang, true),
    };
    (
        text::IDLE_NEXT.format(&[("when", &when), ("name", name)]),
        Some(text::NEXT_AT.format(&[("when", &when)])),
    )
}

/// "every 4 hours", "every day": how often an interval-run automation runs.
pub(super) fn every_text(interval_secs: u64) -> String {
    let hours = (interval_secs / 3_600).max(1);
    if hours.is_multiple_of(24) {
        let days = hours / 24;
        if days == 1 {
            text::EVERY_DAY.get().to_owned()
        } else {
            text::EVERY_DAYS.format(&[("n", &days.to_string())])
        }
    } else if hours == 1 {
        text::EVERY_HOUR.get().to_owned()
    } else {
        text::EVERY_HOURS.format(&[("n", &hours.to_string())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-23 is a Monday; 20:00 UTC.
    const MON_8PM: u64 = 1_795_464_000_000;

    #[test]
    fn wait_time_says_the_time_then_the_weekday_then_the_date() {
        let en = Language::En;
        assert_eq!(
            wait_time_in(MON_8PM, MON_8PM - 3_600_000, 0, en, true),
            "8 PM"
        );
        assert_eq!(
            wait_time_in(MON_8PM + 4 * DAY_MS + 1_800_000, MON_8PM, 0, en, true),
            "Fri 8:30 PM"
        );
        assert_eq!(
            wait_time_in(MON_8PM + 10 * DAY_MS, MON_8PM, 0, en, true),
            "Dec 3, 8 PM"
        );
        assert_eq!(
            wait_time_in(MON_8PM + DAY_MS, MON_8PM, 0, Language::Ko, true),
            "화 오후 8시"
        );
        assert_eq!(
            wait_time_in(MON_8PM + 10 * DAY_MS, MON_8PM, 0, Language::Ko, true),
            "12월 3일 오후 8시"
        );
    }

    #[test]
    fn clock_reads_twelve_hours() {
        let at = |hour, minute| CivilTime {
            year: 2026,
            month: 1,
            day: 1,
            hour,
            minute,
            second: 0,
        };
        assert_eq!(clock(&at(0, 5), false, Language::En), "12:05 AM");
        assert_eq!(clock(&at(12, 0), true, Language::En), "12 PM");
        assert_eq!(clock(&at(9, 30), false, Language::Ko), "오전 9:30");
        assert_eq!(clock(&at(12, 0), true, Language::Ko), "오후 12시");
        assert_eq!(clock(&at(20, 0), false, Language::Ko), "오후 8:00");
    }
}
