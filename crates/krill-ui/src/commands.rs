//! Quick Commands 설정 스키마(MK-092). 실행은 앱 셸의 기존 붙여넣기 경로에만 맡긴다.
use serde::{Deserialize, Serialize};

/// `settings.toml`의 `[[quick_commands]]`. 자동 Enter 없이 명령·프롬프트를 주입한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickCommand {
    pub name: String,
    pub text: String,
}

/// Copy Context의 앱 로컬 상한.
pub const MAX_CONTEXT_LINES: u16 = 1_000;

/// 사용자에게 보이는 ko/en 문자열 키(MK-094).
pub use crate::i18n::catalog::commands as text;
