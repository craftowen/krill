//! `log` 크레이트 경고를 운영 로그로 넘긴다. gpui는 디스플레이 링크 생성 실패·드로어블 획득 실패 같은 렌더 경로 오류를
//! `log::warn!`/`log::error!`로만 남겨서, 이 다리가 없으면 화면 멈춤의 단서가 어디에도 기록되지 않았다.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 같은 오류가 매 프레임 반복돼도 상한 회전 로그가 밀려나지 않게 초당 이 수만 남긴다.
const PER_SECOND: u64 = 20;

struct Forward {
    // 상위 32비트: 유닉스 초, 하위 32비트: 그 초에 남긴 수.
    window: AtomicU64,
}

static FORWARD: Forward = Forward {
    window: AtomicU64::new(0),
};

impl Forward {
    fn admit(&self, second: u64) -> bool {
        let used = |packed: u64| {
            if packed >> 32 == second {
                packed & 0xffff_ffff
            } else {
                0
            }
        };
        let previous = self
            .window
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |packed| {
                Some((second << 32) | (used(packed) + 1).min(PER_SECOND))
            })
            .unwrap_or_else(|packed| packed);
        used(previous) < PER_SECOND
    }
}

impl log::Log for Forward {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        let second = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() & 0xffff_ffff);
        if !self.enabled(record.metadata()) || !self.admit(second) {
            return;
        }
        let source = record.target();
        let message = record.args();
        if record.level() == log::Level::Error {
            tracing::error!(source, "{message}");
        } else {
            tracing::warn!(source, "{message}");
        }
    }

    fn flush(&self) {}
}

/// `log` 크레이트의 경고·오류를 `tracing`(운영 로그)으로 넘긴다. 이미 다른 로거가 있으면 아무것도 하지 않는다.
pub fn forward_log_warnings() {
    if log::set_logger(&FORWARD).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_records_per_second_and_resets_next_second() {
        let forward = Forward {
            window: AtomicU64::new(0),
        };
        assert!((0..PER_SECOND).all(|_| forward.admit(7)));
        assert!(!forward.admit(7), "a flood within one second is capped");
        assert!(forward.admit(8), "the next second starts a fresh budget");
    }
}
