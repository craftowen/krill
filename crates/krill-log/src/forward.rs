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
        if !self.enabled(record.metadata()) || expected_font_skip(record) || !self.admit(second) {
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

/// gpui가 글꼴 패밀리를 읽다 'm' 글리프가 없는 얼굴(Apple Braille·Al Bayan 등 시스템 글꼴)을 건너뛸 때 내는
/// 경고. 설정 화면이 고정폭 목록을 만들려고 모든 패밀리를 해석할 때마다 20여 줄씩 나오는 정상 건너뜀이다.
/// 사용자가 지정한 터미널 글꼴을 못 쓰는 경우는 krill-app이 따로 경고한다(`settings::terminal_font_family`).
fn expected_font_skip(record: &log::Record) -> bool {
    record.target() == "gpui_apple::text_system"
        && record
            .args()
            .to_string()
            .ends_with("has no 'm' character and was not loaded")
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

    #[test]
    fn font_enumeration_skips_are_dropped_but_other_text_warnings_pass() {
        fn record(target: &str, args: std::fmt::Arguments<'_>) -> bool {
            expected_font_skip(
                &log::Record::builder()
                    .level(log::Level::Warn)
                    .target(target)
                    .args(args)
                    .build(),
            )
        }
        assert!(record(
            "gpui_apple::text_system",
            format_args!(
                "font '{}' has no 'm' character and was not loaded",
                "Apple Braille"
            ),
        ));
        assert!(!record(
            "gpui_apple::text_system",
            format_args!("Failed to read traits for font \"X\""),
        ));
        assert!(!record(
            "gpui_macos::window",
            format_args!("font 'X' has no 'm' character and was not loaded"),
        ));
    }

    #[test]
    fn forwarded_warning_reaches_the_operating_log_with_its_source() {
        let dir = std::env::temp_dir().join(format!("krill-log-forward-{}", std::process::id()));
        let writer = crate::RollingLog::open(&dir, crate::Component::App).unwrap();
        let forward = Forward {
            window: AtomicU64::new(0),
        };
        tracing::subscriber::with_default(crate::subscriber::Logger::new(writer), || {
            log::Log::log(
                &forward,
                &log::Record::builder()
                    .level(log::Level::Warn)
                    .target("gpui_macos::window")
                    .args(format_args!("nextDrawable waited"))
                    .build(),
            );
        });
        let text = std::fs::read_to_string(dir.join("krill-app.log")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(text.contains("WARN"), "{text}");
        assert!(text.contains(r#"source="gpui_macos::window""#), "{text}");
        assert!(text.contains("nextDrawable waited"), "{text}");
    }
}
