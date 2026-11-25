//! Native Antigravity (`agy`) account vault DTOs and RPC operations.
//!
//! 1:1 port of Orca's `src/shared/agent-accounts/antigravity/antigravity-account-types.ts`
//! and `src/shared/rpc-contract/accounts/antigravity-accounts-params.ts`.
//! Credentials never cross IPC; only non-secret account summaries and cached usage readings do.

use serde::{Deserialize, Serialize};

use super::{Snapshot, UsageMeter};

/// Last usage reading verified as belonging to a specific saved Antigravity account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravityStoredUsage {
    pub usage: Snapshot,
    #[serde(default)]
    pub meters: Vec<UsageMeter>,
    /// Unix epoch seconds when the reading was recorded.
    pub updated_at: u64,
}

/// Non-secret summary of one saved Antigravity account snapshot in the encrypted vault.
/// Mirrors Orca `AntigravityAccountSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravityAccountSummary {
    pub id: String,
    pub email: Option<String>,
    pub subject: Option<String>,
    pub auth_method: String,
    /// Unix epoch milliseconds when the snapshot was first saved.
    pub created_at: u64,
    /// Unix epoch milliseconds when the snapshot was last updated.
    pub updated_at: u64,
    /// Last verified usage reading for this account on this host (`None` when never read).
    pub usage: Option<AntigravityStoredUsage>,
}

/// Current native `agy` login observed on the execution host.
/// Mirrors Orca `AntigravityAccountState['currentAccount']`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravityCurrentAccount {
    pub email: Option<String>,
    pub subject: Option<String>,
    pub auth_method: String,
    pub identity_known: bool,
}

/// Full non-secret state returned by native Antigravity account operations.
/// Mirrors Orca `AntigravityAccountState`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravityAccountState {
    pub accounts: Vec<AntigravityAccountSummary>,
    pub active_account_id: Option<String>,
    pub current_account: Option<AntigravityCurrentAccount>,
    pub selected_account_id: Option<String>,
}

impl AntigravityAccountState {
    /// True when a saved account was selected, but the host's native `agy` login no longer matches it.
    #[must_use]
    pub fn native_account_changed(&self) -> bool {
        self.selected_account_id.is_some() && self.active_account_id != self.selected_account_id
    }
}

/// Native Antigravity account RPC action (`accounts.antigravity{List,AddCurrent,Select,Remove}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntigravityAccountOp {
    List,
    AddCurrent,
    Select { account_id: String },
    Remove { account_id: String },
}
