//! Provider-specific meters and running-session account handoffs. Optional IPC tail.

use serde::{Deserialize, Serialize};

use super::Window;
use crate::SessionId;

/// A meter actually reported by a provider. Missing meters are never invented as 0%.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMeter {
    pub key: String,
    pub label: String,
    pub window: Option<Window>,
    /// Non-percentage values, for example a credit balance. Contains no credentials.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountMeters {
    pub account_id: String,
    pub plan: Option<String>,
    pub meters: Vec<UsageMeter>,
}

impl AccountMeters {
    /// Most constrained reported pool, excluding reset windows and non-percentage balances.
    #[must_use]
    pub fn effective_max(&self, now_secs: i64) -> Option<u8> {
        self.meters
            .iter()
            .filter_map(|meter| {
                let window = meter.window.as_ref()?;
                let expired = window
                    .resets_at
                    .as_deref()
                    .and_then(crate::time::parse_reset_epoch)
                    .is_some_and(|at| now_secs >= at);
                (!expired).then_some(window.percent.min(100))
            })
            .max()
    }
}

/// The old terminal remains available until its replacement has been created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandoff {
    pub from: SessionId,
    pub to: Option<SessionId>,
    pub account_id: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDetails {
    pub supported: bool,
    pub accounts: Vec<AccountMeters>,
    pub handoffs: Vec<SessionHandoff>,
}
