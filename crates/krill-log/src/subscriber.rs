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
            self.seen.clear();
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
        if hidden > 0 {
            write!(line, " suppressed_repeats={hidden}").ok();
        }
        self.writer.append(&line.0).ok();
    }
}

struct Line(String);

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
}
