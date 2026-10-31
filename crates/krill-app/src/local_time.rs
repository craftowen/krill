//! 절대 시각은 앱을 보고 있는 기기의 시스템 시간대로 표시한다.

use chrono::{DateTime, Datelike, FixedOffset, Local};

/// 해당 순간의 현지 오프셋을 적용한다. 현재 오프셋을 더하면 DST 경계에서 틀린다.
pub(crate) fn format_timestamp(epoch: i64) -> Option<String> {
    let utc = DateTime::from_timestamp(epoch, 0)?;
    if !(1..=9999).contains(&utc.year()) {
        return None;
    }
    format_datetime(utc.with_timezone(&Local).fixed_offset())
}

fn format_datetime(time: DateTime<FixedOffset>) -> Option<String> {
    if !(1..=9999).contains(&time.year()) {
        return None;
    }
    let date = time.format("%Y-%m-%d").to_string();
    let clock = time.format("%H:%M").to_string();
    let timezone = if time.offset().local_minus_utc() == 0 {
        "UTC".into()
    } else {
        format!("UTC{}", time.format("%:z"))
    };
    Some(crate::i18n::catalog::settings::SCHEDULE_RUN_TIME.format(&[
        ("date", &date),
        ("time", &clock),
        ("timezone", &timezone),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_dates_cross_midnight_and_support_fractional_offsets() {
        let epoch = krill_proto::time::parse_reset_epoch("2030-01-01T18:30:00Z").unwrap();
        let utc = DateTime::from_timestamp(epoch, 0).unwrap();
        for (seconds, expected) in [
            (32_400, "2030-01-02 03:30 (UTC+09:00)"),
            (-28_800, "2030-01-01 10:30 (UTC-08:00)"),
            (20_700, "2030-01-02 00:15 (UTC+05:45)"),
            (0, "2030-01-01 18:30 (UTC)"),
        ] {
            let zone = FixedOffset::east_opt(seconds).unwrap();
            assert_eq!(format_datetime(utc.with_timezone(&zone)).unwrap(), expected);
        }
        assert!(format_timestamp(i64::MAX).is_none());
        assert!(format_timestamp(i64::MIN).is_none());
        assert!(format_timestamp(DateTime::<chrono::Utc>::MAX_UTC.timestamp()).is_none());
        assert!(format_timestamp(DateTime::<chrono::Utc>::MIN_UTC.timestamp()).is_none());
    }

    // 자식 프로세스마다 TZ를 지정해 병렬 테스트의 전역 환경을 건드리지 않는다.
    #[cfg(unix)]
    #[test]
    fn system_timezone_includes_korea_and_dst_at_expiry() {
        for (zone, winter, summer) in [
            (
                "Asia/Seoul",
                "2030-01-01 09:00 (UTC+09:00)",
                "2030-07-01 09:00 (UTC+09:00)",
            ),
            (
                "America/New_York",
                "2029-12-31 19:00 (UTC-05:00)",
                "2030-06-30 20:00 (UTC-04:00)",
            ),
            ("UTC", "2030-01-01 00:00 (UTC)", "2030-07-01 00:00 (UTC)"),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "local_time::tests::timezone_probe",
                    "--nocapture",
                ])
                .env("TZ", zone)
                .env("KRILL_TEST_LOCAL_TIMES", format!("{winter}|{summer}"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{zone}: {output:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn timezone_probe() {
        let Ok(expected) = std::env::var("KRILL_TEST_LOCAL_TIMES") else {
            return;
        };
        for (input, expected) in ["2030-01-01T00:00:00Z", "2030-07-01T00:00:00Z"]
            .into_iter()
            .zip(expected.split('|'))
        {
            let epoch = krill_proto::time::parse_reset_epoch(input).unwrap();
            assert_eq!(format_timestamp(epoch).unwrap(), expected);
        }
    }
}
