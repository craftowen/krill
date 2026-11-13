//! `log` 크레이트 경고를 운영 로그로 넘긴다. gpui는 디스플레이 링크 생성 실패·드로어블 획득 실패 같은 렌더 경로 오류를
//! `log::warn!`/`log::error!`로만 남겨서, 이 다리가 없으면 화면 멈춤의 단서가 어디에도 기록되지 않았다.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 같은 오류가 매 프레임 반복돼도 상한 회전 로그가 밀려나지 않게 초당 이 수만 남긴다.
const PER_SECOND: u64 = 20;

/// 디스플레이 잠자기·헤드리스 상태에서 30초마다 반복되는 display-link 오류·경고를 묶어 남기는 주기(15분).
const DISPLAY_LINK_COOLDOWN_SECS: u64 = 900;

struct Forward {
    // 상위 32비트: 유닉스 초, 하위 32비트: 그 초에 남긴 수.
    window: AtomicU64,
    // 디스플레이 잠자기·헤드리스 중 반복되는 display-link 생성 실패(-6661 등) 쿨다운 상태.
    display_link_create: AtomicU64,
    // 디스플레이 링크 정지(subscribed: false) 재시작 경고 쿨다운 상태.
    display_link_stall: AtomicU64,
}

static FORWARD: Forward = Forward::new();

impl Forward {
    const fn new() -> Self {
        Self {
            window: AtomicU64::new(0),
            display_link_create: AtomicU64::new(0),
            display_link_stall: AtomicU64::new(0),
        }
    }

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

    fn coalesce_display_link(&self, message: &str, second: u64) -> Option<u32> {
        let slot = if message.starts_with("could not create display link, code: ") {
            &self.display_link_create
        } else if message.starts_with("display link stalled ")
            && message.contains("subscribed: false")
        {
            &self.display_link_stall
        } else {
            return Some(0);
        };
        admit_cooldown(slot, second, DISPLAY_LINK_COOLDOWN_SECS)
    }

    fn log_at(&self, record: &log::Record, second: u64) {
        if !log::Log::enabled(self, record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        if is_expected_font_skip(record.target(), &message) {
            return;
        }
        let Some(coalesced) = self.coalesce_display_link(&message, second) else {
            return;
        };
        if !self.admit(second) {
            return;
        }
        let source = resolve_source(record);
        let source = source.as_ref();
        match (record.level() == log::Level::Error, coalesced) {
            (true, 0) => tracing::error!(source, "{message}"),
            (true, count) => tracing::error!(source, coalesced = count, "{message}"),
            (false, 0) => tracing::warn!(source, "{message}"),
            (false, count) => tracing::warn!(source, coalesced = count, "{message}"),
        }
    }
}

fn admit_cooldown(slot: &AtomicU64, second: u64, cooldown_secs: u64) -> Option<u32> {
    const INIT_BIT: u64 = 1 << 31;
    const SUPPRESSED_MASK: u64 = 0x7fff_ffff;

    let sec32 = (second & 0xffff_ffff) as u32;
    let fresh = (u64::from(sec32) << 32) | INIT_BIT;
    let mut cur = slot.load(Ordering::Acquire);
    loop {
        if cur == 0 {
            match slot.compare_exchange_weak(0, fresh, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(0),
                Err(actual) => {
                    cur = actual;
                    continue;
                }
            }
        }
        let last_sec = (cur >> 32) as u32;
        if u64::from(sec32.wrapping_sub(last_sec)) < cooldown_secs {
            let next_suppressed = (cur & SUPPRESSED_MASK)
                .saturating_add(1)
                .min(SUPPRESSED_MASK);
            let next = (cur & !SUPPRESSED_MASK) | next_suppressed;
            match slot.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return None,
                Err(actual) => cur = actual,
            }
        } else {
            let hidden = (cur & SUPPRESSED_MASK) as u32;
            match slot.compare_exchange_weak(cur, fresh, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(hidden),
                Err(actual) => cur = actual,
            }
        }
    }
}

/// `gpui-pre-util`의 `log_err()` 등이 레지스트리 크레이트에서 `target("")`을 넘길 때도
/// 파일 시스템 전체 경로를 노출하지 않고 `<crate>::<module>` 수준의 출처만 복구한다.
fn resolve_source<'a>(record: &'a log::Record<'_>) -> Cow<'a, str> {
    if !record.target().is_empty() {
        return Cow::Borrowed(record.target());
    }
    if let Some(module) = record.module_path().filter(|module| !module.is_empty()) {
        return Cow::Borrowed(module);
    }
    if let Some(source) = record.file().and_then(module_from_source_path) {
        return Cow::Owned(source);
    }
    Cow::Borrowed("unknown")
}

fn module_from_source_path(path: &str) -> Option<String> {
    let normalized = if path.contains('\\') {
        Cow::Owned(path.replace('\\', "/"))
    } else {
        Cow::Borrowed(path)
    };
    let (prefix, rest) = normalized.rsplit_once("/src/")?;
    let crate_dir = prefix.rsplit('/').next().filter(|seg| !seg.is_empty())?;
    let crate_name = strip_crate_version(crate_dir).replace('-', "_");
    if crate_name.is_empty() || matches!(crate_name.as_str(), "." | "..") {
        return None;
    }

    let rel = rest.strip_suffix(".rs").unwrap_or(rest);
    let mut parts = vec![crate_name];
    for seg in rel.split('/').filter(|seg| !seg.is_empty()) {
        if matches!(seg, "." | "..") {
            return None;
        }
        parts.push(seg.to_owned());
    }
    if parts.len() > 1
        && matches!(
            parts.last().map(String::as_str),
            Some("lib" | "main" | "mod")
        )
    {
        parts.pop();
    }
    Some(parts.join("::"))
}

fn strip_crate_version(dir: &str) -> &str {
    for (idx, _) in dir.match_indices('-') {
        if idx == 0 {
            continue;
        }
        let (Some(head), Some(ver)) = (dir.get(..idx), dir.get(idx + 1..)) else {
            continue;
        };
        let mut parts = ver.split('.');
        let major_is_digits = parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        let has_minor_digit = parts
            .next()
            .is_some_and(|p| p.bytes().next().is_some_and(|b| b.is_ascii_digit()));
        if major_is_digits && has_minor_digit {
            return head;
        }
    }
    dir
}

impl log::Log for Forward {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        let second = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() & 0xffff_ffff);
        self.log_at(record, second);
    }

    fn flush(&self) {}
}

/// gpui가 글꼴 패밀리를 읽다 'm' 글리프가 없는 얼굴(Apple Braille·Al Bayan 등 시스템 글꼴)을 건너뛸 때 내는
/// 경고. 설정 화면이 고정폭 목록을 만들려고 모든 패밀리를 해석할 때마다 20여 줄씩 나오는 정상 건너뜀이다.
/// 사용자가 지정한 터미널 글꼴을 못 쓰는 경우는 krill-app이 따로 경고한다(`settings::terminal_font_family`).
fn is_expected_font_skip(target: &str, message: &str) -> bool {
    target == "gpui_apple::text_system"
        && message.ends_with("has no 'm' character and was not loaded")
}

#[cfg(test)]
fn expected_font_skip(record: &log::Record) -> bool {
    is_expected_font_skip(record.target(), &record.args().to_string())
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
        let forward = Forward::new();
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
        let forward = Forward::new();
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

    #[test]
    fn empty_target_resolves_to_crate_module_without_leaking_paths() {
        let registry = log::Record::builder()
            .level(log::Level::Error)
            .target("")
            .module_path(None)
            .file(Some(
                "/Users/macmini/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-macos-0.3.9/src/display_link.rs",
            ))
            .args(format_args!("could not create display link, code: -6661"))
            .build();
        let source = resolve_source(&registry);
        assert_eq!(source, "gpui_pre_macos::display_link");
        assert!(!source.contains("macmini"));
        assert!(!source.contains(".cargo"));

        assert_eq!(
            module_from_source_path(r"C:\Users\alice\vendor\gpui-kit-assets\src\icons\mod.rs")
                .as_deref(),
            Some("gpui_kit_assets::icons")
        );
        assert_eq!(
            module_from_source_path("crates/krill-app/src/shell.rs").as_deref(),
            Some("krill_app::shell")
        );
        assert_eq!(
            module_from_source_path("/tmp/gpui-0.3.9-rc.1/src/lib.rs").as_deref(),
            Some("gpui")
        );

        let no_src = log::Record::builder()
            .level(log::Level::Error)
            .target("")
            .module_path(None)
            .file(Some("/Users/alice/private/secret.rs"))
            .args(format_args!("boom"))
            .build();
        assert_eq!(resolve_source(&no_src), "unknown");
    }

    #[test]
    fn display_link_sleep_floods_are_coalesced_and_active_stalls_pass() {
        let dir =
            std::env::temp_dir().join(format!("krill-log-display-link-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let writer = crate::RollingLog::open(&dir, crate::Component::App).unwrap();
        let forward = Forward::new();
        let registry_file = "/Users/macmini/.cargo/registry/src/index.crates.io-abc/gpui-pre-macos-0.3.9/src/display_link.rs";

        tracing::subscriber::with_default(crate::subscriber::Logger::new(writer), || {
            for step in 0..=30 {
                forward.log_at(
                    &log::Record::builder()
                        .level(log::Level::Error)
                        .target("")
                        .file(Some(registry_file))
                        .args(format_args!("could not create display link, code: -6661"))
                        .build(),
                    step * 30,
                );
            }
            for step in 0..=3 {
                forward.log_at(
                    &log::Record::builder()
                        .level(log::Level::Warn)
                        .target("gpui_macos::window")
                        .args(format_args!(
                            "display link stalled for 4500ms (stall #{step}, subscribed: false, display: Some(1)); restarting"
                        ))
                        .build(),
                    100 + step * 30,
                );
            }
            forward.log_at(
                &log::Record::builder()
                    .level(log::Level::Warn)
                    .target("gpui_macos::window")
                    .args(format_args!(
                        "display link stalled for 250ms (stall #1, subscribed: true, display: Some(1)); restarting"
                    ))
                    .build(),
                130,
            );
            forward.log_at(
                &log::Record::builder()
                    .level(log::Level::Error)
                    .target("gpui::app")
                    .args(format_args!("unrelated error stays visible"))
                    .build(),
                130,
            );
        });

        let text = std::fs::read_to_string(dir.join("krill-app.log")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let create_lines: Vec<&&str> = lines
            .iter()
            .filter(|line| line.contains("could not create display link, code: -6661"))
            .collect();
        assert_eq!(create_lines.len(), 2, "{text}");
        assert!(
            create_lines[0].contains(r#"source="gpui_pre_macos::display_link""#),
            "{text}"
        );
        assert!(create_lines[1].contains("coalesced=29"), "{text}");
        assert!(!text.contains("macmini"), "{text}");

        let idle_stalls = lines
            .iter()
            .filter(|line| line.contains("subscribed: false"))
            .count();
        assert_eq!(idle_stalls, 1, "{text}");
        assert!(text.contains("subscribed: true"), "{text}");
        assert!(text.contains("unrelated error stays visible"), "{text}");
    }
}
