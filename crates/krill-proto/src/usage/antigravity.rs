//! Native Antigravity (`agy`) account vault DTOs and RPC operations.
//!
//! 1:1 port of Orca's `src/shared/agent-accounts/antigravity/antigravity-account-types.ts`,
//! `src/main/antigravity/antigravity-sign-in-url.ts`,
//! `src/main/antigravity/antigravity-sign-in-failure.ts`,
//! and `src/shared/rpc-contract/accounts/antigravity-accounts-params.ts`.
//! Credentials never cross IPC; only non-secret account summaries, sign-in flow status, and cached usage readings do.

use serde::{Deserialize, Serialize};

use super::{Snapshot, UsageMeter};

const MAX_SIGN_IN_CHARS: usize = 16_384;
const MAX_STATE_CHARS: usize = 512;
const AGY_LOGIN_ORIGIN: &str = "https://accounts.google.com";
const AGY_LOGIN_REDIRECT: &str = "https://antigravity.google/oauth-callback";

pub const ANTIGRAVITY_SUBSCRIPTION_REQUIRED: &str =
    "Google requires an eligible Antigravity subscription for this account.";
pub const ANTIGRAVITY_ACCOUNT_INELIGIBLE: &str =
    "This Google account can't use Antigravity. Sign in with a different account.";
pub const ANTIGRAVITY_INVALID_GRANT: &str =
    "Google didn't accept that code. Sign in again to get a new one.";
pub const ANTIGRAVITY_ACCESS_DENIED: &str =
    "Google sign-in for Antigravity was not approved. Start sign-in again.";
pub const ANTIGRAVITY_SIGN_IN_FAILED: &str =
    "Google sign-in for Antigravity failed. Start sign-in again.";

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

/// Initiating action for an in-app `agy acp --login` sign-in flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntigravitySignInIntent {
    SignIn,
    AddAccount,
}

/// Lifecycle phase of an in-app `agy acp --login` sign-in flow on the execution host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntigravitySignInPhase {
    Starting,
    Waiting,
    Verifying,
    SignedIn,
    Failed,
    Cancelled,
    Expired,
}

impl AntigravitySignInPhase {
    #[must_use]
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Starting | Self::Waiting | Self::Verifying)
    }
}

/// Non-secret status of an execution-host `agy acp --login` flow.
/// Mirrors Orca `AntigravityChatSignInState`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravitySignInState {
    /// Non-`None` only for the caller that started the flow (capability token).
    pub flow_id: Option<String>,
    pub phase: AntigravitySignInPhase,
    pub intent: AntigravitySignInIntent,
    /// Validated Google OAuth URL (`https://accounts.google.com/o/oauth2/...`) while waiting.
    /// Hidden (`None`) from other clients.
    pub url: Option<String>,
    /// Safe user-facing failure reason when `phase == Failed`.
    pub error: Option<String>,
}

impl std::fmt::Debug for AntigravitySignInState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravitySignInState")
            .field("flow_id", &self.flow_id.as_ref().map(|_| "[REDACTED]"))
            .field("phase", &self.phase)
            .field("intent", &self.intent)
            .field("url", &self.url.as_ref().map(|_| "[REDACTED]"))
            .field("error", &self.error)
            .finish()
    }
}

/// Full non-secret state returned by native Antigravity account operations.
/// Mirrors Orca `AntigravityAccountState`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravityAccountState {
    pub accounts: Vec<AntigravityAccountSummary>,
    pub active_account_id: Option<String>,
    pub current_account: Option<AntigravityCurrentAccount>,
    pub selected_account_id: Option<String>,
    /// True when an existing vault on disk cannot be decrypted because a previous
    /// ad-hoc build's macOS Keychain ACL locked the legacy master key (`-25308`).
    /// Host `current_account` is still reported so the user can recover in one click.
    #[serde(default)]
    pub vault_locked: bool,
    /// New `agy` launches put `selected_account_id` back when another saved account took `agy`'s
    /// native slot (Orca `restoresSelectionAtLaunch`). False on Windows and older daemons.
    #[serde(default)]
    pub restores_selection_at_launch: bool,
    /// True when the execution-host daemon supports in-app `agy acp --login` (`SignIn*` ops).
    /// Defaults to `false` when decoding replies from N-1 daemons so clients hide unsupported UI.
    #[serde(default)]
    pub sign_in_supported: bool,
    /// In-app `agy acp --login` flow state on the execution host (`None` when idle).
    #[serde(default)]
    pub sign_in: Option<AntigravitySignInState>,
}

impl AntigravityAccountState {
    /// True when a saved account was selected, but the host's native `agy` login no longer matches it.
    #[must_use]
    pub fn native_account_changed(&self) -> bool {
        self.selected_account_id.is_some() && self.active_account_id != self.selected_account_id
    }

    /// When a running `agy` on another saved account refreshed over the native slot and new
    /// launches will restore `selected_account_id`, returns that selected account id.
    #[must_use]
    pub fn launches_use_account(&self) -> Option<&str> {
        if !self.restores_selection_at_launch {
            return None;
        }
        let active = self.active_account_id.as_deref()?;
        let selected = self.selected_account_id.as_deref()?;
        if selected == active {
            return None;
        }
        self.accounts
            .iter()
            .any(|account| account.id == selected)
            .then_some(selected)
    }
}

/// Native Antigravity account RPC action (`accounts.antigravity*`).
/// Variants must only be appended at the end for postcard wire compatibility.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntigravityAccountOp {
    List,
    AddCurrent,
    Select {
        account_id: String,
    },
    Remove {
        account_id: String,
    },
    /// Back up an ACL-locked legacy vault, reset it with a fresh upgrade-resilient key,
    /// and re-save the current host `agy` login if signed in.
    ResetLockedVault,
    /// Start an execution-host `agy acp --login` flow. Saves any currently
    /// signed-in unsaved account to the vault first so adding another account cannot lose it.
    SignInStart {
        intent: AntigravitySignInIntent,
    },
    /// Deliver the pasted authorization code or callback URL to the owning `agy acp --login` flow.
    SignInComplete {
        flow_id: String,
        answer: String,
    },
    /// Cancel and terminate the owning `agy acp --login` flow.
    SignInCancel {
        flow_id: String,
    },
    /// Read-only poll of the execution-host sign-in flow and latest account state.
    SignInStatus {
        flow_id: Option<String>,
    },
}

impl std::fmt::Debug for AntigravityAccountOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::List => f.write_str("List"),
            Self::AddCurrent => f.write_str("AddCurrent"),
            Self::Select { account_id } => f
                .debug_struct("Select")
                .field("account_id", account_id)
                .finish(),
            Self::Remove { account_id } => f
                .debug_struct("Remove")
                .field("account_id", account_id)
                .finish(),
            Self::ResetLockedVault => f.write_str("ResetLockedVault"),
            Self::SignInStart { intent } => f
                .debug_struct("SignInStart")
                .field("intent", intent)
                .finish(),
            Self::SignInComplete { .. } => f
                .debug_struct("SignInComplete")
                .field("flow_id", &"[REDACTED]")
                .field("answer", &"[REDACTED]")
                .finish(),
            Self::SignInCancel { .. } => f
                .debug_struct("SignInCancel")
                .field("flow_id", &"[REDACTED]")
                .finish(),
            Self::SignInStatus { flow_id } => f
                .debug_struct("SignInStatus")
                .field("flow_id", &flow_id.as_ref().map(|_| "[REDACTED]"))
                .finish(),
        }
    }
}

/// Extract and strictly validate Google's OAuth sign-in link from one line of `agy acp --login` output.
/// Mirrors Orca `agyLoginLink` (`antigravity-sign-in-url.ts`).
#[must_use]
pub fn agy_login_link(line: &str) -> Option<&str> {
    let link = line.trim();
    if link.is_empty()
        || link.len() > MAX_SIGN_IN_CHARS
        || link.chars().any(|c| c.is_whitespace() || c.is_control())
        || link.contains('#')
    {
        return None;
    }
    let after_origin = link.strip_prefix(AGY_LOGIN_ORIGIN)?;
    let (path, query) = after_origin.split_once('?')?;
    if !matches!(path, "/o/oauth2/auth" | "/o/oauth2/v2/auth") {
        return None;
    }
    let response_type = single_query_param(query, "response_type")?;
    if response_type != "code" {
        return None;
    }
    let redirect_uri = single_query_param(query, "redirect_uri")?;
    if redirect_uri != AGY_LOGIN_REDIRECT {
        return None;
    }
    let state = single_query_param(query, "state")?;
    if state.is_empty()
        || state.len() > MAX_STATE_CHARS
        || state.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    Some(link)
}

/// Validate what may be written to `agy acp --login` stdin as a single line:
/// either a bare authorization code or the full `https://antigravity.google/oauth-callback?...` URL.
/// Mirrors Orca `agyLoginAnswer` (`antigravity-sign-in-url.ts`).
#[must_use]
pub fn agy_login_answer(pasted: &str) -> Option<String> {
    let answer = pasted.trim();
    if answer.is_empty() || answer.len() > MAX_SIGN_IN_CHARS || answer.chars().any(char::is_control)
    {
        return None;
    }
    if looks_like_url(answer) {
        if answer.chars().any(char::is_whitespace) {
            return None;
        }
        let no_fragment = answer.split('#').next().unwrap_or(answer);
        let (base, query) = no_fragment.split_once('?')?;
        if !is_agy_login_redirect(base) {
            return None;
        }
        let code = first_query_param(query, "code")?;
        if code.is_empty() || code.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return None;
        }
        return Some(answer.to_owned());
    }
    if answer.chars().any(char::is_whitespace) {
        return None;
    }
    Some(answer.to_owned())
}

/// Map failed `agy acp --login` output into a safe, user-facing explanation.
/// Mirrors Orca `agyLoginFailure` (`antigravity-sign-in-failure.ts`).
#[must_use]
pub fn agy_login_failure(output: &str) -> &'static str {
    if output.contains("SUBSCRIPTION_REQUIRED") {
        return ANTIGRAVITY_SUBSCRIPTION_REQUIRED;
    }
    let lower = output.to_ascii_lowercase();
    if lower.contains("onboarding failed") || lower.contains("ineligible") {
        return ANTIGRAVITY_ACCOUNT_INELIGIBLE;
    }
    if lower.contains("invalid_grant") {
        return ANTIGRAVITY_INVALID_GRANT;
    }
    if lower.contains("access_denied")
        || lower.contains("denied access")
        || lower.contains("cancelled")
    {
        return ANTIGRAVITY_ACCESS_DENIED;
    }
    ANTIGRAVITY_SIGN_IN_FAILED
}

fn looks_like_url(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

fn is_agy_login_redirect(base: &str) -> bool {
    let Some((scheme, rest)) = base.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    path == "oauth-callback"
        && (authority.eq_ignore_ascii_case("antigravity.google")
            || authority.eq_ignore_ascii_case("antigravity.google:443"))
}

fn single_query_param(query: &str, name: &str) -> Option<String> {
    find_query_param(query, name, true)
}

fn first_query_param(query: &str, name: &str) -> Option<String> {
    find_query_param(query, name, false)
}

fn find_query_param(query: &str, name: &str, unique: bool) -> Option<String> {
    let mut found = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_k, raw_v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode_form(raw_k)?;
        if key == name {
            if !unique {
                return percent_decode_form(raw_v);
            }
            if found.is_some() {
                return None;
            }
            found = Some(percent_decode_form(raw_v)?);
        }
    }
    found
}

fn percent_decode_form(input: &str) -> Option<String> {
    let mut bytes = input.bytes();
    let mut out = Vec::with_capacity(input.len());
    while let Some(b) = bytes.next() {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let hi = hex_nibble(bytes.next()?)?;
                let lo = hex_nibble(bytes.next()?)?;
                out.push((hi << 4) | lo);
            }
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}

const fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.saturating_sub(b'0')),
        b'a'..=b'f' => Some(b.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(b.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}
