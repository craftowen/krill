// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Structured reasons why a bot's chat session could not start (`b10792b6`, `e1779f39`, `5fba3d89`).
//!
//! Kept in `krill-proto` so `krilld`, `krill-app`, `krill` CLI, and the mobile PWA all share one
//! refusal contract and word each refusal from the reader's side (naming the bot's host instead of
//! calling a paired computer "this computer").

use serde::{Deserialize, Serialize};

/// Machine-readable reason why a bot chat session was refused or could not start.
/// Variants must only be appended at the end for postcard compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRefusalCode {
    CliNotInstalled,
    CliTooOld,
    BotPaused,
    UnknownAgent,
    InvalidLaunchConfig,
    HostUnsupported,
    AuthRequired,
    QuotaExceeded,
    SpawnFailed,
}

/// Structured refusal attached to a failed bot chat start or automation run card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSessionRefusal {
    pub code: BotRefusalCode,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub minimum_version: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Human-readable agent name matching Orca's `tuiAgentDisplayName` / Krill's bot UI.
#[must_use]
pub fn bot_agent_display_name(agent: &str) -> String {
    match crate::agent::canonical_id(agent) {
        "claude" => "Claude Code".to_owned(),
        "codex" => "Codex".to_owned(),
        "agy" => "Antigravity".to_owned(),
        "opencode" | "opencode2" => "OpenCode".to_owned(),
        "cursor" => "Cursor".to_owned(),
        "pi" => "Pi".to_owned(),
        "grok" => "Grok".to_owned(),
        "omp" => "OMP".to_owned(),
        "hermes" => "Hermes".to_owned(),
        "gemini" => "Gemini".to_owned(),
        other if !other.is_empty() => other.to_owned(),
        _ => "Agent".to_owned(),
    }
}

impl BotSessionRefusal {
    #[must_use]
    pub fn cli_not_installed(agent: &str) -> Self {
        Self {
            code: BotRefusalCode::CliNotInstalled,
            agent: Some(crate::agent::canonical_id(agent).to_owned()),
            installed_version: None,
            minimum_version: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn cli_too_old(
        agent: &str,
        installed_version: impl Into<String>,
        minimum_version: impl Into<String>,
    ) -> Self {
        Self {
            code: BotRefusalCode::CliTooOld,
            agent: Some(crate::agent::canonical_id(agent).to_owned()),
            installed_version: Some(installed_version.into()),
            minimum_version: Some(minimum_version.into()),
            detail: None,
        }
    }

    #[must_use]
    pub fn bot_paused(bot_name: impl Into<String>) -> Self {
        Self {
            code: BotRefusalCode::BotPaused,
            agent: None,
            installed_version: None,
            minimum_version: None,
            detail: Some(bot_name.into()),
        }
    }

    #[must_use]
    pub fn unknown_agent(agent: impl Into<String>) -> Self {
        Self {
            code: BotRefusalCode::UnknownAgent,
            agent: Some(agent.into()),
            installed_version: None,
            minimum_version: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn invalid_launch_config(agent: &str, detail: impl Into<String>) -> Self {
        Self {
            code: BotRefusalCode::InvalidLaunchConfig,
            agent: Some(crate::agent::canonical_id(agent).to_owned()),
            installed_version: None,
            minimum_version: None,
            detail: Some(detail.into()),
        }
    }

    #[must_use]
    pub fn host_unsupported(detail: impl Into<String>) -> Self {
        Self {
            code: BotRefusalCode::HostUnsupported,
            agent: None,
            installed_version: None,
            minimum_version: None,
            detail: Some(detail.into()),
        }
    }

    #[must_use]
    pub fn auth_required(agent: &str) -> Self {
        Self {
            code: BotRefusalCode::AuthRequired,
            agent: Some(crate::agent::canonical_id(agent).to_owned()),
            installed_version: None,
            minimum_version: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn quota_exceeded(agent: &str, detail: impl Into<String>) -> Self {
        Self {
            code: BotRefusalCode::QuotaExceeded,
            agent: Some(crate::agent::canonical_id(agent).to_owned()),
            installed_version: None,
            minimum_version: None,
            detail: Some(detail.into()),
        }
    }

    #[must_use]
    pub fn spawn_failed(agent: &str, detail: impl Into<String>) -> Self {
        Self {
            code: BotRefusalCode::SpawnFailed,
            agent: Some(crate::agent::canonical_id(agent).to_owned()),
            installed_version: None,
            minimum_version: None,
            detail: Some(detail.into()),
        }
    }

    /// Formats this refusal in English from the reader's perspective.
    ///
    /// Pass `host_name = Some("this computer")` for local desktop / host-side sentences,
    /// `Some("<paired server name>")` for a named remote host, or `None` when the reader
    /// (e.g. mobile phone or unnamed paired host) must not call a remote computer "this computer".
    #[must_use]
    pub fn message_en(&self, host_name: Option<&str>) -> String {
        let agent = self
            .agent
            .as_deref()
            .map_or_else(|| "Agent".to_owned(), bot_agent_display_name);
        let on_host = host_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|h| format!(" on {h}"))
            .unwrap_or_default();
        match self.code {
            BotRefusalCode::CliTooOld => match (&self.installed_version, &self.minimum_version) {
                (Some(installed), Some(min)) => {
                    format!("Your {agent} CLI{on_host} is {installed}. Chats need {min} or newer.")
                }
                (_, Some(min)) => {
                    format!("Your {agent} CLI{on_host} is too old. Chats need {min} or newer.")
                }
                _ => format!("Your {agent} CLI{on_host} is older than chats support."),
            },
            BotRefusalCode::CliNotInstalled => {
                format!("{agent} CLI is not installed{on_host}.")
            }
            BotRefusalCode::BotPaused => match self.detail.as_deref().filter(|s| !s.is_empty()) {
                Some(name) => format!("Bot '{name}' is paused. Resume it to start a chat."),
                None => "This bot is paused. Resume it to start a chat.".to_owned(),
            },
            BotRefusalCode::UnknownAgent => {
                format!("Agent '{agent}' is not available{on_host}.")
            }
            BotRefusalCode::InvalidLaunchConfig => {
                match self.detail.as_deref().filter(|s| !s.is_empty()) {
                    Some(detail) => format!("Couldn't start {agent} chat: {detail}"),
                    None => format!("{agent} launch configuration is invalid."),
                }
            }
            BotRefusalCode::HostUnsupported => {
                match self.detail.as_deref().filter(|s| !s.is_empty()) {
                    Some(detail) => detail.to_owned(),
                    None => format!("Chats are not supported{on_host}."),
                }
            }
            BotRefusalCode::AuthRequired => {
                format!("Sign in to {agent}{on_host} to start a chat.")
            }
            BotRefusalCode::QuotaExceeded => {
                match self.detail.as_deref().filter(|s| !s.is_empty()) {
                    Some(detail) => format!("{agent} quota reached: {detail}"),
                    None => format!("{agent} usage quota has been reached."),
                }
            }
            BotRefusalCode::SpawnFailed => match self.detail.as_deref().filter(|s| !s.is_empty()) {
                Some(detail) => format!("Couldn't start {agent} chat{on_host}: {detail}"),
                None => format!("Couldn't start {agent} chat{on_host}."),
            },
        }
    }
}
