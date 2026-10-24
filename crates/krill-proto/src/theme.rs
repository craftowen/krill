//! 터미널 색 테마(MK-020). UI가 krilld에 푸시하고, krilld는 색 질의(OSC 4·10·11·12)에 이 값으로 답한다.
//! 한 번도 받은 적 없으면 [`DEFAULT`]로 즉시 답한다(MK-008 결정 2). 앱 터미널의 기본 색도 [`DEFAULT`]를 쓴다 —
//! 두 값이 어긋나면 에이전트가 실제와 다른 배경을 보고 색을 고른다.
//!
//! # 디스크 포맷
//!
//! krilld가 마지막 테마를 저장하는 파일은 [`Theme`]의 postcard 인코딩 그대로다(`u8` 57개, 길이 접두 없음).
//! 필드는 끝에만 붙이고, 읽는 쪽은 남은 바이트를 무시한다. `theme/tests.rs`의 고정 바이트로 이전 저장 파일의 호환을 검증한다.

use serde::{Deserialize, Serialize};

use crate::screen::Msg;

/// OWE-590: 앱 테마·셀 픽셀 크기 푸시.
pub const FEAT_THEME: u64 = 1 << 46;
/// 클라이언트 → 데몬. 응답은 없으며 연결 순서대로 적용한다.
pub const THEME: u8 = 0x44;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

/// 마지막 UI 테마는 디스크에 유지하고 셀 크기는 현재 연결에서만 갱신한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Push {
    pub theme: Theme,
    pub cell: Option<CellSize>,
}

impl Msg for Push {
    const KIND: u8 = THEME;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// ANSI 0~15. 16~255는 표준 6×6×6 큐브·회색 단계라 보내지 않는다.
    pub ansi: [Rgb; 16],
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// 내장 기본 테마 `Krill Trench`(해구). 앱 다크 터미널 기본값과 같다.
/// UI가 붙기 전에도 OSC 10·11·12·4 질의에 화면과 같은 색으로 답한다.
pub const DEFAULT: Theme = Theme {
    foreground: rgb(0xdf, 0xed, 0xf2),
    background: rgb(0x06, 0x16, 0x20),
    cursor: rgb(0xd3, 0xe5, 0xee),
    ansi: [
        rgb(0x6f, 0x8b, 0x9c),
        rgb(0xe5, 0x7a, 0x8b),
        rgb(0x8f, 0xc7, 0xac),
        rgb(0xd4, 0xc7, 0x7d),
        rgb(0x82, 0xb6, 0xe1),
        rgb(0xb4, 0x9f, 0xd7),
        rgb(0x83, 0xc3, 0xce),
        rgb(0xc8, 0xdc, 0xe6),
        rgb(0x94, 0xae, 0xbc),
        rgb(0xf7, 0x9a, 0xa8),
        rgb(0xa9, 0xe1, 0xc3),
        rgb(0xed, 0xdf, 0xa0),
        rgb(0xa4, 0xd0, 0xf3),
        rgb(0xcd, 0xb9, 0xed),
        rgb(0xa3, 0xdc, 0xe4),
        rgb(0xed, 0xf6, 0xfa),
    ],
};

impl Theme {
    /// 디스크 포맷으로 인코딩한다.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    /// 디스크 포맷을 읽는다. 끝에 남은 바이트(새 필드)는 무시한다.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

#[cfg(test)]
mod tests;
