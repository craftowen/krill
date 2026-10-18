//! MK-086 endpoint settings and transport shared with the session daemon (MK-126).
#[path = "../../krilld/src/webhook/transport.rs"]
mod transport;
use transport::validated_url;
pub use transport::{Endpoint, deliver, deliver_transition};

impl Endpoint {
    #[must_use]
    pub fn provider_badge(&self) -> &'static str {
        match self {
            Self::Discord { .. } => "Discord",
            Self::Telegram { .. } => "Telegram",
            Self::Ntfy { url } => {
                let lower = url.to_ascii_lowercase();
                if lower.contains("hooks.slack.com") {
                    "Slack"
                } else if lower.contains("discord.com") || lower.contains("discordapp.com") {
                    "Discord"
                } else if lower.contains("ntfy") {
                    "ntfy"
                } else {
                    "Webhook"
                }
            }
        }
    }

    pub fn from_url_input(raw: &str) -> std::io::Result<Self> {
        let trimmed = raw.trim();
        let valid = validated_url(trimmed)?.to_owned();
        let lower = valid.to_ascii_lowercase();
        if lower.contains("discord.com/api/webhooks")
            || lower.contains("discordapp.com/api/webhooks")
        {
            Ok(Self::Discord { url: valid })
        } else {
            Ok(Self::Ntfy { url: valid })
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
