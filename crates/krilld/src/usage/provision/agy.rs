//! agy 대화·요약 인덱스만 기본 저장소로 공유한다. 인증 전환은 별도 경로다.
//! 설계 참고: Orca antigravity/native-account-launch.ts·native-account-service.ts
//! (MIT, Copyright (c) 2026 Lovecast Inc.). 코드 복사 없음.
//! 요약 DB는 conversations에서 재구성되므로 행 병합 없이 원본을 백업한다.

use std::io;
use std::path::Path;

use super::Report;

#[cfg(unix)]
#[path = "agy_history.rs"]
mod history;

#[cfg(unix)]
pub(super) fn needs_merge(home: &Path, profile: &Path) -> bool {
    history::needs_merge(home, profile)
}

pub(super) fn provision(home: &Path, profile: &Path) -> io::Result<Report> {
    #[cfg(unix)]
    {
        history::provision(home, profile, &|| {
            let local = profile.join(".gemini/antigravity-cli");
            let host = home.join(".gemini/antigravity-cli");
            super::history_probe::quiescent_at(&local, profile, "ANTIGRAVITY_CLI_HOME")
                && super::history_probe::quiescent_at(&host, home, "ANTIGRAVITY_CLI_HOME")
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (home, profile);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "agy native credential switching requires an explicit owner decision",
        ))
    }
}

#[cfg(test)]
#[cfg(unix)]
#[path = "agy_tests.rs"]
mod tests;
