//! RFC 3339·epoch 초 시각 파싱 공용 모듈(OWE-709).
//!
//! `resets_at`(RFC 3339 또는 epoch 초 문자열)을 krilld 사용량 창 판정과
//! 앱 리셋 분류가 같은 순간으로 풀게 한 곳에 둔다. 순수 계산이라
//! 와이어 형식(N-1 호환)에 영향을 주지 않는다.
//!
//! 허용하는 RFC 3339 부분집합: `YYYY-MM-DD` + `T`/`t`/공백 +
//! `HH:MM:SS[.분수]` + `Z`/`z`|`±HH:MM`|`±HHMM`|`±HH`.
//! 초는 윤초 `60`까지, 분수는 `.`·`,` 뒤 숫자만 받는다.
//! 연도는 `0..=9999`로 제한해 달력 계산이 넘치지 않게 한다.
//!
//! OWE-697(앱 `settings.rs` 교체) 참고 — 앱 기존 구현과의 차이:
//! 앱은 윤초 `60`·`,` 소수·연도 `0`을 거부하고 `T`/`t`/공백·`±HHMM`을
//! 받는다. 합칠 때 테스트(`settings/tests.rs` 분류 케이스)로 확인한다.

/// `resets_at` 원문(epoch 초 문자열 또는 RFC 3339)을 epoch 초로 푼다.
/// 공백은 무시하고, 빈 문자열·형식을 모르는 값은 `None`이다.
#[must_use]
pub fn parse_reset_epoch(s: &str) -> Option<i64> {
    let text = s.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(epoch) = text.parse::<i64>() {
        return Some(epoch);
    }
    parse_rfc3339_epoch(text)
}

/// RFC 3339 부분집합을 epoch 초로 푼다. 형식을 모르면 `None`.
#[must_use]
pub fn parse_rfc3339_epoch(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let mut fields = date.split('-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: i64 = fields.next()?.parse().ok()?;
    let day: i64 = fields.next()?.parse().ok()?;
    // RFC 3339는 네 자리 연도다. 산술 전에 범위를 제한해 극단 연도로
    // 달력 계산이 넘치지 않게 한다.
    if fields.next().is_some() || !(0..=9999).contains(&year) {
        return None;
    }
    let (clock, offset_secs) =
        if let Some(clock) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
            (clock, 0)
        } else {
            let at = rest.rfind(['+', '-'])?;
            let (clock, zone) = rest.split_at(at);
            (clock, parse_zone_offset_secs(zone)?)
        };
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let sec_part = clock.next()?;
    if clock.next().is_some() {
        return None;
    }
    let mut sec_split = sec_part.split(['.', ',']);
    let second: i64 = sec_split.next()?.parse().ok()?;
    if let Some(frac) = sec_split.next()
        && (frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    if sec_split.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    days.checked_mul(86_400)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?
        .checked_sub(offset_secs)
}

/// `±HH:MM`·`±HHMM`·`±HH`를 초 오프셋으로 푼다.
fn parse_zone_offset_secs(zone: &str) -> Option<i64> {
    let (sign, digits) = match zone.strip_prefix('+') {
        Some(digits) => (1_i64, digits),
        None => (-1_i64, zone.strip_prefix('-')?),
    };
    let (hours, minutes) = match digits.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if digits.len() == 4 => (digits.get(..2)?, digits.get(2..)?),
        None if digits.len() == 2 => (digits, "00"),
        None => return None,
    };
    if hours.len() != 2
        || minutes.len() != 2
        || !hours
            .bytes()
            .chain(minutes.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let parsed_hours: i64 = hours.parse().ok()?;
    let parsed_minutes: i64 = minutes.parse().ok()?;
    if parsed_hours > 23 || parsed_minutes > 59 {
        return None;
    }
    parsed_hours
        .checked_mul(3_600)?
        .checked_add(parsed_minutes.checked_mul(60)?)
        .and_then(|offset| sign.checked_mul(offset))
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            if leap { 29 } else { 28 }
        }
    }
}

/// Howard Hinnant의 `days_from_civil`(1970-01-01 기준 일수). checked 연산만 쓴다.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let adjusted = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = if adjusted >= 0 {
        adjusted
    } else {
        adjusted.checked_sub(399)?
    } / 400;
    let year_of_era = adjusted.checked_sub(era.checked_mul(400)?)?;
    let month_prime = month.checked_add(9)? % 12;
    let day_of_year = 153_i64
        .checked_mul(month_prime)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(day.checked_sub(1)?)?;
    let day_of_era = year_of_era
        .checked_mul(365)?
        .checked_add(year_of_era.checked_div(4)?)?
        .checked_sub(year_of_era.checked_div(100)?)?
        .checked_add(day_of_year)?;
    era.checked_mul(146_097)?
        .checked_add(day_of_era)?
        .checked_sub(719_468)
}

#[cfg(test)]
mod tests {
    use super::{parse_reset_epoch, parse_rfc3339_epoch};

    proptest::proptest! {
        /// 파서는 임의 입력에 패닉하지 않는다.
        #[test]
        fn arbitrary_strings_do_not_panic(s in proptest::prelude::any::<String>()) {
            core::hint::black_box(parse_rfc3339_epoch(&s));
            core::hint::black_box(parse_reset_epoch(&s));
        }
    }

    /// krilld 기존 케이스(그대로 풀려야 한다).
    #[test]
    fn parses_epoch_seconds_and_known_rfc3339_instants() {
        assert_eq!(parse_reset_epoch("1792123200"), Some(1_792_123_200));
        assert_eq!(parse_rfc3339_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_epoch("2001-01-01T00:00:00Z"),
            Some(978_307_200)
        );
        assert_eq!(
            parse_reset_epoch("2026-10-15T04:30:00.000Z"),
            Some(1_792_038_600)
        );
        assert_eq!(
            parse_reset_epoch("2099-01-01T00:00:00Z"),
            parse_reset_epoch("2099-01-01T09:00:00+09:00")
        );
        assert!(
            parse_reset_epoch("2099-01-01T00:00:00Z").unwrap()
                > parse_reset_epoch("2001-01-01T00:00:00Z").unwrap()
        );
    }

    /// 앱 기존 케이스(같은 순간은 같은 epoch).
    #[test]
    fn same_instant_in_z_offset_and_fractional_forms() {
        let forms = [
            "2030-01-01T00:00:00Z",
            "2030-01-01T09:00:00+09:00",
            "2030-01-01T00:00:00.428638+00:00",
        ];
        for form in forms {
            assert_eq!(parse_reset_epoch(form), Some(1_893_456_000), "{form}");
        }
        for past in [
            "2026-10-16T10:30:00.000Z",
            "2026-10-05T22:59:59.914509+00:00",
            "2026-10-03T05:30:00.428638+00:00",
            "1000000000",
        ] {
            assert!(parse_reset_epoch(past).unwrap() < 1_900_000_000, "{past}");
        }
    }

    #[test]
    fn rejects_unknown_or_zone_less_forms() {
        for bad in [
            "",
            "  ",
            "not-a-date",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-10-16T25:00:00Z",
            "2026-10-16",
            "2030-01-01",
            "12:00:00",
            "2026-10-16 10:30:00",
        ] {
            assert_eq!(parse_rfc3339_epoch(bad), None, "{bad:?}");
            assert_eq!(parse_reset_epoch(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn malformed_unicode_and_signed_offsets_are_rejected_without_panicking() {
        for zone in ["+€a", "+a€", "+-1:00", "+00:-1"] {
            let timestamp = format!("2030-01-01T00:00:00{zone}");
            assert_eq!(parse_rfc3339_epoch(&timestamp), None, "{timestamp}");
        }
    }

    /// 합친 뒤 새로 받아들이는 표기(양쪽 기존 구현 중 하나는 거부).
    #[test]
    fn accepts_unified_spellings() {
        let zulu = parse_rfc3339_epoch("2026-10-15T04:30:00.000Z").unwrap();
        // 소문자 구분자·접미사, 공백 구분자, 붙인 오프셋.
        assert_eq!(parse_rfc3339_epoch("2026-10-15t04:30:00.000z"), Some(zulu));
        assert_eq!(parse_rfc3339_epoch("2026-10-15 04:30:00+00:00"), Some(zulu));
        assert_eq!(parse_rfc3339_epoch("2026-10-15T13:30:00+0900"), Some(zulu));
        assert_eq!(parse_rfc3339_epoch("2026-10-15T04:30:00+00"), Some(zulu));
        // `,` 소수·윤초 60(다음 분 0초와 같은 epoch).
        assert_eq!(parse_rfc3339_epoch("2026-10-15T04:30:00,000Z"), Some(zulu));
        assert_eq!(parse_rfc3339_epoch("2026-10-15T04:29:60Z"), Some(zulu));
    }
}
