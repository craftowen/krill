use std::collections::HashMap;
use std::fmt::{self, Write};
use std::hash::{Hash as _, Hasher as _};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::{RECORD_BYTES, RollingLog};

/// 같은 사건(숫자만 다른 반복 포함)은 창마다 이 수만 남기고 나머지는 세어 둔다. 디스플레이 링크 실패·스케줄러 오류가
/// 초당 수 번 반복돼 5개 회전 파일이 몇 시간 만에 밀려나면 진짜 장애의 단서가 사라진다(2026-10-27 실측: 25,433줄).
const REPEATS_PER_WINDOW: u32 = 3;
const WINDOW_MS: u128 = 60_000;
const MAX_KEYS: usize = 256;

#[derive(Debug, Default)]
struct Repeats {
    // 키 → (창 시작 ms, 창 안에서 본 횟수)
    seen: HashMap<u64, (u128, u32)>,
}

impl Repeats {
    /// `Some(n)`이면 기록한다(n = 직전 창에서 숨긴 수). `None`이면 이 사건은 버린다.
    fn admit(&mut self, key: u64, now: u128) -> Option<u32> {
        if self.seen.len() >= MAX_KEYS && !self.seen.contains_key(&key) {
            self.seen.retain(|_, &mut (start, count)| {
                now.saturating_sub(start) < WINDOW_MS || count > REPEATS_PER_WINDOW
            });
            if self.seen.len() >= MAX_KEYS
                && let Some(&oldest_key) = self
                    .seen
                    .iter()
                    .min_by_key(|(_, (start, _))| *start)
                    .map(|(k, _)| k)
            {
                self.seen.remove(&oldest_key);
            }
        }
        let entry = self.seen.entry(key).or_insert((now, 0));
        let mut hidden = 0;
        if now.saturating_sub(entry.0) >= WINDOW_MS {
            hidden = entry.1.saturating_sub(REPEATS_PER_WINDOW);
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        (entry.1 <= REPEATS_PER_WINDOW).then_some(hidden)
    }
}

/// Event-only subscriber: no span trees, buffers, worker threads or shutdown flush to lose on SIGKILL.
#[derive(Debug)]
pub(crate) struct Logger {
    writer: RollingLog,
    next_span: AtomicU64,
    repeats: Mutex<Repeats>,
}

impl Logger {
    pub(crate) fn new(writer: RollingLog) -> Self {
        Self {
            writer,
            next_span: AtomicU64::new(1),
            repeats: Mutex::default(),
        }
    }
}

impl Subscriber for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let target = metadata.target().split("::").next().unwrap_or_default();
        // `krill_log`: forwarded `log` warnings (gpui render path) keep their origin in a `source` field.
        metadata.is_event()
            && *metadata.level() <= Level::INFO
            && matches!(
                target,
                "krill_app" | "krilld" | "krill_holder" | "krill" | "krill_log"
            )
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut line = Line(String::with_capacity(RECORD_BYTES));
        write!(
            line,
            "{millis} {} pid={} {}",
            event.metadata().level(),
            std::process::id(),
            event.metadata().target()
        )
        .ok();
        let body = line.0.len();
        event.record(&mut line);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (event.metadata().level().as_str(), event.metadata().target()).hash(&mut hasher);
        line.0
            .bytes()
            .skip(body)
            .filter(|byte| !byte.is_ascii_digit())
            .for_each(|byte| byte.hash(&mut hasher));
        // 잠금이 오염돼도 기록은 계속한다(fail-open).
        let hidden = match self.repeats.lock() {
            Ok(mut repeats) => match repeats.admit(hasher.finish(), millis) {
                Some(hidden) => hidden,
                None => return,
            },
            Err(_) => 0,
        };
        line.append_suppressed(hidden);
        self.writer.append(&line.0).ok();
    }
}

struct Line(String);

impl Line {
    fn append_suppressed(&mut self, hidden: u32) {
        if hidden == 0 {
            return;
        }
        let suffix = format!(" suppressed_repeats={hidden}");
        let max_body = (RECORD_BYTES - 1).saturating_sub(suffix.len());
        if self.0.len() > max_body {
            self.0.truncate(self.0.floor_char_boundary(max_body));
        }
        self.0.push_str(&suffix);
    }
}

impl Write for Line {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let remaining = (RECORD_BYTES - 1).saturating_sub(self.0.len());
        let end = value.floor_char_boundary(value.len().min(remaining));
        // Keep each event on one line, including fields containing control characters.
        for character in value.get(..end).unwrap_or_default().chars() {
            self.0.push(if character.is_control() {
                ' '
            } else {
                character
            });
        }
        Ok(())
    }
}

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        write!(self, " {}={value:?}", field.name()).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_are_capped_per_window_and_reported_once_the_window_rolls() {
        let mut repeats = Repeats::default();
        let admitted = (0..10).filter(|_| repeats.admit(1, 0).is_some()).count();
        assert_eq!(admitted, REPEATS_PER_WINDOW as usize);
        assert!(repeats.admit(2, 0).is_some(), "other events are unaffected");
        assert_eq!(repeats.admit(1, WINDOW_MS), Some(10 - REPEATS_PER_WINDOW));
    }

    /// 2026-10-27 실측 폭주(25,433줄, 약 10시간)를 같은 형태로 재생해 남는 줄 수를 잰다.
    #[test]
    fn display_link_flood_replay_keeps_only_a_handful_of_lines() {
        let mut repeats = Repeats::default();
        let ten_hours_ms = 10 * 3600 * 1000u128;
        let kept = (0..25_433u128)
            .filter(|n| repeats.admit(7, n * ten_hours_ms / 25_433).is_some())
            .count();
        assert!(kept <= 3 * 601, "kept {kept} of 25433");
    }

    #[test]
    fn max_keys_evicts_expired_or_oldest_without_clearing_active_windows() {
        let mut repeats = Repeats::default();
        for _ in 0..10 {
            repeats.admit(999, 100);
        }
        for k in 1..(MAX_KEYS as u64) {
            assert_eq!(repeats.admit(k, 50), Some(0));
        }
        assert_eq!(repeats.seen.len(), MAX_KEYS);

        // Inserting a new key after the non-suppressed keys expired reaps them without losing key 999.
        assert_eq!(repeats.admit(1_000, 100 + WINDOW_MS), Some(0));
        assert_eq!(
            repeats.admit(999, 100 + WINDOW_MS),
            Some(10 - REPEATS_PER_WINDOW)
        );

        // When all MAX_KEYS entries are still active, only the single oldest entry is evicted.
        let mut full = Repeats::default();
        for i in 0..(MAX_KEYS as u64) {
            assert_eq!(full.admit(i, u128::from(i)), Some(0));
        }
        for _ in 0..5 {
            full.admit(10, 10);
        }
        assert_eq!(full.admit(5_000, 500), Some(0));
        assert_eq!(full.seen.len(), MAX_KEYS);
        assert!(!full.seen.contains_key(&0));
        assert_eq!(full.admit(10, 10), None);
    }

    #[test]
    fn near_capacity_line_preserves_suppressed_repeats_suffix() {
        let mut line = Line(String::with_capacity(RECORD_BYTES));
        write!(line, "{}", "a".repeat(RECORD_BYTES + 32)).ok();
        assert_eq!(line.0.len(), RECORD_BYTES - 1);
        line.append_suppressed(338);
        assert!(line.0.len() < RECORD_BYTES);
        assert!(line.0.ends_with(" suppressed_repeats=338"));
    }
}
