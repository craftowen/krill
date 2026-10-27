//! Orca's dedicated reset-credit GET supplies expiry when /wham/usage omits grants.
use krill_proto::usage::{AccountMeters, ProviderKind};
use zeroize::Zeroizing;

pub(super) fn supplement(
    meters: &mut AccountMeters,
    token: &str,
    account: Option<&str>,
    send: impl FnOnce(&str) -> Option<Zeroizing<Vec<u8>>>,
) {
    let parser = super::super::meters::codex_reset::read;
    if !super::super::meters::codex_reset::needs_supplement(meters) {
        return;
    }
    let Some(config) = super::request_config(ProviderKind::Codex, token, account) else {
        return;
    };
    let config =
        Zeroizing::new(config.replacen("/wham/usage", "/wham/rate-limit-reset-credits", 1));
    let Some(body) = send(&config) else {
        return;
    };
    if u64::try_from(body.len())
        .ok()
        .is_none_or(|len| len > super::MAX_BYTES)
    {
        return;
    }
    let Ok(json) = serde_json::from_slice(&body) else {
        return;
    };
    parser(meters, &json);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplements_missing_expiry_with_same_account_get_and_preserves_usage() {
        let mut meters = super::super::super::meters::parse(
            ProviderKind::Codex,
            r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000}},"rate_limit_reset_credits":{"available_count":1}}"#,
        );
        let window = meters.meters[0].clone();
        supplement(&mut meters, "fixture-token", Some("account-1"), |config| {
            assert!(config.contains(
                "url = \"https://chatgpt.com/backend-api/wham/rate-limit-reset-credits\""
            ));
            assert!(config.contains("request = \"GET\""));
            assert!(config.contains("ChatGPT-Account-Id: account-1"));
            assert!(!config.contains("consume"));
            Some(br#"{"available_count":1,"credits":[{"status":"available","expires_at":"2099-01-01T00:00:00Z"}]}"#.to_vec().into())
        });
        assert_eq!(meters.meters[0], window);
        assert_eq!(meters.meters[2].value.as_deref(), Some("4070908800"));
        supplement(&mut meters, "fixture-token", None, |_| {
            panic!("complete grants must not refetch");
        });
    }
    #[test]
    fn failures_keep_the_last_reported_credit_count_and_unknown_expiry() {
        for body in [
            None,
            Some(b"bad json".to_vec()),
            Some(br#"{"error":"unauthorized"}"#.to_vec()),
        ] {
            let mut meters = super::super::super::meters::parse(
                ProviderKind::Codex,
                r#"{"rate_limit_reset_credits":{"available_count":2}}"#,
            );
            let before = meters.clone();
            supplement(&mut meters, "fixture-token", None, |_| {
                body.map(Zeroizing::new)
            });
            assert_eq!(meters, before);
        }
    }
}
