//! 온보딩 동의 판정·기록(OWE-690, MK-129). 화면은 [`view`]에 있다.
//!
//! 첫 실행에 데이터·hook·네트워크·백그라운드 고지를 보여주고 명시적 동의를 받는다.
//! 동의하면 `<상태 디렉터리>/consent.toml`에 `{ version, accepted_at, app_version }`을
//! 남기고, 고지 내용이 실질적으로 바뀌면 [`CONSENT_VERSION`]을 올려 다시 묻는다.
//!
//! 버전 규칙: 사용자에게 실질적으로 알릴 내용(수집 항목·저장 위치·기본값·되돌리기 방법)이
//! 바뀔 때만 올린다. 문구 다듬기·오타 수정·번역 손질은 올리지 않는다.
//!
//! - [`needs_consent`]는 순수 판정이다. 파일 없음·파싱 실패·구버전·깨진 값이면 `true`(재동의)
//!   이고, 그 경우에도 파일을 지우거나 고치지 않는다(덮어쓰기는 동의 때만).
//! - [`record_consent`]는 0600·임시 파일+rename 원자 쓰기다. 쓰기가 중단되면 기존 파일이
//!   그대로 남는다. `KRILL_STATE_DIR` 격리 상태에서는 호출자가 넘긴 격리 디렉터리에만 쓴다.
//! - 거절은 앱 프로세스만 끝낸다. 데몬·세션은 건드리지 않으므로 이 모듈에 데몬을 멈추는
//!   코드는 없다(다음 실행에 다시 묻는다).

pub mod view;

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 고지 내용이 실질적으로 바뀔 때만 올리는 동의 문서 버전(위의 버전 규칙 참고).
pub const CONSENT_VERSION: u32 = 1;
/// 상태 디렉터리 안의 동의 기록 파일 이름.
pub const CONSENT_FILE: &str = "consent.toml";

/// `consent.toml` 한 줄 기록. 비밀은 없고 버전·시각·앱 버전만 둔다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Consent {
    version: u32,
    accepted_at: String,
    app_version: String,
}

/// 상태 디렉터리의 동의 기록 경로.
#[must_use]
pub fn consent_path(state: &Path) -> PathBuf {
    state.join(CONSENT_FILE)
}

/// 동의가 필요하면 `true`. 파일 없음·파싱 실패·구버전·깨진 값(빈 앱 버전·잘못된 시각)이면
/// 재동의를 묻고, 파일을 지우거나 고치지는 않는다. 기존 사용자(`onboarding.done` 있음,
/// `consent.toml` 없음)도 파일이 없으므로 1회 동의를 받는다.
#[must_use]
pub fn needs_consent(state: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(consent_path(state)) else {
        return true;
    };
    let Ok(record) = toml::from_str::<Consent>(&text) else {
        return true;
    };
    if record.version < CONSENT_VERSION {
        return true;
    }
    if record.app_version.trim().is_empty() || !valid_timestamp(&record.accepted_at) {
        return true;
    }
    false
}

/// 동의를 기록한다. 시각은 RFC 3339 UTC(`accepted_at`), 앱 버전은 빌드 버전을 쓴다.
pub fn record_consent(state: &Path, now: SystemTime) -> io::Result<()> {
    let record = Consent {
        version: CONSENT_VERSION,
        accepted_at: format_rfc3339_utc(now)?,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let body = toml::to_string(&record).map_err(io::Error::other)?;
    atomic_write_0600(&consent_path(state), &body)
}

/// `settings.rs`의 `write_settings`와 같은 원자 교체다. 그 헬퍼는 settings 전용이라
/// 여기서 필요한 최소 형태(0600·고유 임시 파일·fsync·rename·실패 시 임시 파일 정리)만 둔다.
/// 상태기계(`state.rs`)의 `onboarding.toml` 저장도 쓴다.
pub(crate) fn atomic_write_0600(path: &Path, body: &str) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let result = (|| {
        use std::io::Write as _;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

/// `SystemTime`을 RFC 3339 UTC(`YYYY-MM-DDTHH:MM:SSZ`)로 만든다. 시간대 변환 없이
/// UNIX 시각을 양력으로 풀 뿐이라 윤초는 담지 않는다(기록용 시각이라 허용한다).
fn format_rfc3339_utc(now: SystemTime) -> io::Result<String> {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    Ok(format_unix_rfc3339(secs))
}

fn format_unix_rfc3339(secs: u64) -> String {
    let days = secs / 86_400;
    let rest = secs - days * 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = rest / 3_600;
    let minute = rest / 60 - hour * 60;
    let second = rest - hour * 3_600 - minute * 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// 일 수를 양력 연·월·일로 푼다(Howard Hinnant의 `civil_from_days`, u64 연산이라 넘치지 않는다).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
    }
}

fn digits(text: &str, len: usize) -> Option<u32> {
    if text.len() == len && text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse::<u32>().ok()
    } else {
        None
    }
}

/// 기록한 시각 모양(`YYYY-MM-DDTHH:MM:SS[.소수]Z`)인지 본다. 손으로 고쳐 깨진 값이면
/// 재동의를 묻기 위한 검사라 달력상 존재하는 날짜까지만 확인한다.
fn valid_timestamp(text: &str) -> bool {
    let Some(stamp) = text.strip_suffix('Z') else {
        return false;
    };
    let Some((date, time)) = stamp.split_once('T') else {
        return false;
    };
    let mut date_parts = date.split('-');
    let (Some(y), Some(mo), Some(d), None) = (
        date_parts.next(),
        date_parts.next(),
        date_parts.next(),
        date_parts.next(),
    ) else {
        return false;
    };
    let (Some(year), Some(month), Some(day)) = (digits(y, 4), digits(mo, 2), digits(d, 2)) else {
        return false;
    };
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return false;
    }
    let (hms, frac_ok) = match time.split_once('.') {
        Some((hms, frac)) => (
            hms,
            !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()),
        ),
        None => (time, true),
    };
    if !frac_ok {
        return false;
    }
    let mut time_parts = hms.split(':');
    let (Some(h), Some(mi), Some(s), None) = (
        time_parts.next(),
        time_parts.next(),
        time_parts.next(),
        time_parts.next(),
    ) else {
        return false;
    };
    let (Some(hour), Some(min), Some(sec)) = (digits(h, 2), digits(mi, 2), digits(s, 2)) else {
        return false;
    };
    // 윤초(60초)는 기록 시각에 둘 수 있게 열어 둔다.
    hour <= 23 && min <= 59 && sec <= 60
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, format_unix_rfc3339, valid_timestamp};

    #[test]
    fn unix_epoch_formats_to_1970() {
        assert_eq!(format_unix_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn known_timestamps_roundtrip() {
        // 2023-11-14T22:13:20Z와 윤년 2024-02-29를 달력으로 확인한 고정값이다.
        assert_eq!(format_unix_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert!(valid_timestamp("2023-11-14T22:13:20Z"));
        assert!(valid_timestamp("2024-02-29T00:00:00.123Z"));
    }

    #[test]
    fn broken_timestamps_are_rejected() {
        for bad in [
            "",
            "어제",
            "2023-11-14 22:13:20",
            "2023-11-14T22:13:20",
            "2023-13-01T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2023-11-14T24:00:00Z",
            "2023-11-14T22:13:61Z",
            "2023-11-14T22:13:20.Z",
            "2023-11-14T22:13Z",
        ] {
            assert!(!valid_timestamp(bad), "{bad}");
        }
    }
}
