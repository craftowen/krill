//! 앱의 마지막 생존 시각을 레이아웃에 주기적으로 저장한다.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 앱이 살아 있는 동안 마지막 생존 시각을 레이아웃에 다시 쓰는 간격.
pub(super) const HEARTBEAT: Duration = Duration::from_mins(1);

pub(super) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}
