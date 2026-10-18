//! agy 계정 격리 보류(OWE-721). 실제 키체인·~/.gemini 어댑터는 만들지 않는다.
//! 설계 참고: Orca antigravity/native-account-launch.ts·native-account-service.ts
//! (MIT, Copyright (c) 2026 Lovecast Inc.). 코드 복사 없음.
//! 네이티브 자격증명 전환은 오너 결정이 필요해 보류 중이다(OWE-721).
//! 프로필 격리 실행(`--gemini_dir`)은 `agent.rs`의 가드·폴백이 맡는다(OWE-901).

use std::io;
use std::path::Path;

use super::Report;

pub(super) fn provision(_home: &Path, _profile: &Path) -> io::Result<Report> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "agy native credential switching requires an explicit owner decision",
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn refuses_native_switching_without_reading_or_writing_any_store() {
        // 존재하지 않는 경로에도 Unsupported. 파일·키체인 저장소 자체를 호출하지 않는다.
        let missing = std::env::temp_dir().join("owe721-missing-agy-home");
        assert_eq!(
            super::provision(&missing, &missing).unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}
