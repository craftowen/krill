//! Lightweight bot display/view types shared between settings, sidebar, and bot views.

use krill_proto::bots::{BotConfig, BotId};
use serde::{Deserialize, Serialize};

use crate::i18n::catalog::bots as text;
use crate::tab_color::TabColor;

/// Orca `BOT_SIDEBAR_SORT_CHOICES`: ids and meanings match the projects' Sort by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotSidebarSort {
    /// The saved order. New bots go on top.
    #[default]
    Manual,
    /// "Agent Activity": bots that need you, then most recent activity.
    Smart,
    Recent,
    Name,
}

impl BotSidebarSort {
    pub const ALL: [Self; 4] = [Self::Manual, Self::Smart, Self::Recent, Self::Name];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => text::SBO_SORT_MANUAL.get(),
            Self::Smart => text::SBO_SORT_SMART.get(),
            Self::Recent => text::SBO_SORT_RECENT.get(),
            Self::Name => text::SBO_SORT_NAME.get(),
        }
    }

    #[must_use]
    pub fn description(self) -> Option<&'static str> {
        match self {
            Self::Manual => Some(text::SBO_SORT_MANUAL_DESC.get()),
            Self::Smart => Some(text::SBO_SORT_SMART_DESC.get()),
            Self::Recent | Self::Name => None,
        }
    }
}

/// Orca `BotSidebarView`: how this computer's bots list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BotSidebarView {
    pub sort: BotSidebarSort,
    pub hide_paused: bool,
    /// A bot's worker lines with nothing running drop off, as the projects' Hide sleeping.
    pub hide_sleeping_workers: bool,
}

impl BotSidebarView {
    /// How many options differ from the default, for the button's badge.
    #[must_use]
    pub fn active_filters(self) -> usize {
        usize::from(self.hide_paused) + usize::from(self.hide_sleeping_workers)
    }
}

/// Maps a bot's saved avatar color hex or name to a semantic [`TabColor`].
#[must_use]
pub fn parse_avatar_color(raw: Option<&str>) -> Option<TabColor> {
    let trimmed = raw?.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.as_str() {
        "purple" | "#a855f7" | "#8b5cf6" => Some(TabColor::Purple),
        "pink" | "#ec4899" => Some(TabColor::Pink),
        "red" | "#ef4444" => Some(TabColor::Red),
        "orange" | "#f97316" => Some(TabColor::Orange),
        "yellow" | "#eab308" => Some(TabColor::Yellow),
        "green" | "#22c55e" => Some(TabColor::Green),
        "teal" | "#14b8a6" | "#06b6d4" => Some(TabColor::Teal),
        "gray" | "grey" | "#9ca3af" | "#6b7280" => Some(TabColor::Gray),
        _ => Some(TabColor::Blue),
    }
}

/// Lightweight UI identity cached for sessions owned by a bot (shown in tabs & sidebar rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotSessionMeta {
    pub bot_id: BotId,
    pub bot_name: String,
    pub emoji: Option<String>,
    pub avatar_color: Option<String>,
}

impl BotSessionMeta {
    /// Builds display metadata from a [`BotConfig`].
    #[must_use]
    pub fn from_bot(bot: &BotConfig) -> Self {
        Self {
            bot_id: bot.id.clone(),
            bot_name: bot.name.clone(),
            emoji: bot
                .emoji
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            avatar_color: bot
                .avatar_color
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        }
    }

    /// Formats `<emoji> <bot_name>` when emoji is set, or `<bot_name>` otherwise.
    #[must_use]
    pub fn display_prefix(&self) -> String {
        match self
            .emoji
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(emoji) => format!("{emoji} {}", self.bot_name),
            None => self.bot_name.clone(),
        }
    }

    /// Resolves the mapped semantic [`TabColor`] if a known hex/name is set.
    #[must_use]
    pub fn tab_color(&self) -> Option<TabColor> {
        parse_avatar_color(self.avatar_color.as_deref())
    }
}
