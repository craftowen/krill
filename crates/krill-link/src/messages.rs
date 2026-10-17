//! Korean/English CLI messages introduced by resumable link settings.
pub const VALUE_REQUIRED: &str = "값이 필요합니다 (value required)";
pub const INVALID_GRACE: &str = "유예 시간은 밀리초 숫자로 입력하세요 (invalid grace period)";
pub const INVALID_BUFFER: &str = "보관 용량은 바이트 숫자로 입력하세요 (invalid buffer limit)";
pub const INVALID_RESUME_SETTINGS: &str = "재연결 유예는 1~600000밀리초, 보관 용량은 1~4194304바이트로 설정하세요 (resume grace must be 1..=600000 ms and buffer 1..=4194304 bytes)";
