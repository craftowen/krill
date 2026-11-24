//! 탭 9색 색상 태그(MK-180, Orca 9색 패리티).

use serde::{Deserialize, Serialize};

/// 탭 색상 태그(MK-180, Orca 9색 패리티).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TabColor {
    Blue,
    Purple,
    Pink,
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Gray,
}

impl TabColor {
    pub const ALL: [Self; 9] = [
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Teal,
        Self::Gray,
    ];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Gray => "gray",
        }
    }
}
