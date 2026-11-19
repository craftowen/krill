//! 절대 시각은 앱을 보고 있는 기기의 시스템 시간대로 표시한다.

const SECONDS_PER_DAY: i64 = 86_400;
const MIN_YEAR_EPOCH: i64 = -62_135_596_800; // 0001-01-01T00:00:00Z
const MAX_YEAR_EPOCH: i64 = 253_402_300_799; // 9999-12-31T23:59:59Z

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CivilTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Howard Hinnant의 `civil_from_days`(1970-01-01 기준 일수)로 UTC 달력 시각을 구한다.
pub(crate) fn civil_from_epoch(epoch: i64) -> Option<CivilTime> {
    let days = epoch.div_euclid(SECONDS_PER_DAY);
    let rem = epoch.rem_euclid(SECONDS_PER_DAY);
    let shifted = days.checked_add(719_468)?;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year_base = year_of_era.checked_add(era.checked_mul(400)?)?;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 {
        year_base.checked_add(1)?
    } else {
        year_base
    };
    Some(CivilTime {
        year: i32::try_from(year).ok()?,
        month: u8::try_from(month).ok()?,
        day: u8::try_from(day).ok()?,
        hour: u8::try_from(rem / 3_600).ok()?,
        minute: u8::try_from((rem % 3_600) / 60).ok()?,
        second: u8::try_from(rem % 60).ok()?,
    })
}

/// Howard Hinnant의 `days_from_civil`(1970-01-01 기준 일수)로 epoch 초를 구한다.
#[cfg(any(windows, test))]
pub(crate) fn epoch_from_civil(time: CivilTime) -> Option<i64> {
    let year = i64::from(time.year);
    let month = i64::from(time.month);
    let day = i64::from(time.day);
    let hour = i64::from(time.hour);
    let minute = i64::from(time.minute);
    let second = i64::from(time.second);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    let adjusted = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = adjusted.div_euclid(400);
    let year_of_era = adjusted.rem_euclid(400);
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era
        .checked_mul(146_097)?
        .checked_add(day_of_era)?
        .checked_sub(719_468)?;
    days.checked_mul(SECONDS_PER_DAY)?
        .checked_add(hour * 3_600)?
        .checked_add(minute * 60)?
        .checked_add(second)
}

/// 해당 순간의 현지 오프셋을 적용한다. 현재 오프셋을 더하면 DST 경계에서 틀린다.
pub(crate) fn format_timestamp(epoch: i64) -> Option<String> {
    if !(MIN_YEAR_EPOCH..=MAX_YEAR_EPOCH).contains(&epoch) {
        return None;
    }
    let utc = civil_from_epoch(epoch)?;
    if !(1..=9999).contains(&utc.year) {
        return None;
    }
    let offset_secs = crate::platform::local_utc_offset_secs(epoch, utc)?;
    format_with_offset(epoch, offset_secs)
}

fn format_with_offset(epoch: i64, offset_secs: i32) -> Option<String> {
    if !(-86_400..=86_400).contains(&offset_secs) {
        return None;
    }
    let shifted = epoch.checked_add(i64::from(offset_secs))?;
    let local = civil_from_epoch(shifted)?;
    if !(1..=9999).contains(&local.year) {
        return None;
    }
    let date = format!("{:04}-{:02}-{:02}", local.year, local.month, local.day);
    let clock = format!("{:02}:{:02}", local.hour, local.minute);
    let timezone = if offset_secs == 0 {
        "UTC".into()
    } else {
        let sign = if offset_secs < 0 { '-' } else { '+' };
        let abs = offset_secs.unsigned_abs();
        let hours = abs / 3_600;
        let minutes = (abs % 3_600) / 60;
        format!("UTC{sign}{hours:02}:{minutes:02}")
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
        for (seconds, expected) in [
            (32_400, "2030-01-02 03:30 (UTC+09:00)"),
            (-28_800, "2030-01-01 10:30 (UTC-08:00)"),
            (20_700, "2030-01-02 00:15 (UTC+05:45)"),
            (0, "2030-01-01 18:30 (UTC)"),
        ] {
            assert_eq!(format_with_offset(epoch, seconds).unwrap(), expected);
        }
        assert!(format_timestamp(i64::MAX).is_none());
        assert!(format_timestamp(i64::MIN).is_none());
        assert!(format_timestamp(MIN_YEAR_EPOCH - 1).is_none());
        assert!(format_timestamp(MAX_YEAR_EPOCH + 1).is_none());
        assert!(format_with_offset(MAX_YEAR_EPOCH, 3_600).is_none());
        assert!(format_with_offset(MIN_YEAR_EPOCH, -3_600).is_none());
    }

    proptest::proptest! {
        #[test]
        fn civil_calendar_math_handles_arbitrary_and_supported_epochs(
            any_epoch in proptest::prelude::any::<i64>(),
            epoch in MIN_YEAR_EPOCH..=MAX_YEAR_EPOCH,
        ) {
            core::hint::black_box(civil_from_epoch(any_epoch));
            let civil = civil_from_epoch(epoch).unwrap();
            assert!((1..=9999).contains(&civil.year));
            assert_eq!(epoch_from_civil(civil), Some(epoch));
        }
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
