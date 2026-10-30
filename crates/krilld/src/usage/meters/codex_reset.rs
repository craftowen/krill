//! Codex grants: Orca codex-reset-credit-client.ts (Lovecast Inc., MIT) protocol reference.
use std::collections::BTreeMap;

use krill_proto::usage::{
    AccountMeters, RESET_CREDITS_EXPIRY_METER_PREFIX, RESET_CREDITS_METER_KEY,
    RESET_CREDITS_METER_LABEL, reset_credit_expiry_meter,
};
use serde_json::Value;

fn expiry(value: &Value) -> Option<i64> {
    let text = super::super::resets(value)?;
    // Only numeric timestamps use the seconds/milliseconds convention.
    let at = if let Ok(number) = text.trim().parse::<i64>() {
        if number >= 10_000_000_000 {
            number / 1000
        } else {
            number
        }
    } else {
        krill_proto::time::parse_reset_epoch(&text)?
    };
    (at > 0).then_some(at)
}

pub(in crate::usage) fn needs_supplement(meters: &AccountMeters) -> bool {
    let count = meters
        .meters
        .iter()
        .find(|m| m.key == RESET_CREDITS_METER_KEY)
        .and_then(|m| m.value.as_deref())
        .and_then(|v| v.parse::<u64>().ok());
    if count == Some(0) {
        return false;
    }
    let expiries: Vec<_> = meters
        .meters
        .iter()
        .filter(|m| m.key.starts_with(RESET_CREDITS_EXPIRY_METER_PREFIX))
        .collect();
    count.is_none()
        || expiries.is_empty()
        || expiries.iter().any(|m| {
            m.value
                .as_deref()
                .is_none_or(|v| krill_proto::time::parse_reset_epoch(v).is_none())
        })
}

/// Replace reset metadata only after a valid response; usage windows stay intact.
pub(in crate::usage) fn read(out: &mut AccountMeters, raw: &Value) -> bool {
    let credits = raw.get("credits").and_then(Value::as_array);
    let available = |credit: &&Value| {
        credit
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status.eq_ignore_ascii_case("available"))
    };
    let count = raw
        .get("available_count")
        .and_then(Value::as_u64)
        .or_else(|| {
            credits.map(|rows| {
                u64::try_from(rows.iter().filter(available).count()).unwrap_or(u64::MAX)
            })
        });
    let Some(count) = count else {
        return false;
    };
    let fallback = raw
        .get("expires_at")
        .or_else(|| raw.get("ends_at"))
        .and_then(expiry);
    let mut groups = BTreeMap::<i64, u64>::new();
    let mut unknown = 0_u64;
    let mut remaining = count;
    if let Some(credits) = credits {
        for credit in credits.iter().filter(available) {
            if remaining == 0 {
                break;
            }
            remaining -= 1;
            if let Some(at) = credit.get("expires_at").and_then(expiry) {
                *groups.entry(at).or_default() += 1;
            } else {
                unknown += 1;
            }
        }
    }
    if remaining > 0 {
        if let Some(at) = fallback {
            *groups.entry(at).or_default() += remaining;
        } else {
            unknown = unknown.saturating_add(remaining);
        }
    }
    out.meters.retain(|m| {
        m.key != RESET_CREDITS_METER_KEY && !m.key.starts_with(RESET_CREDITS_EXPIRY_METER_PREFIX)
    });
    super::value_meter(
        out,
        RESET_CREDITS_METER_KEY,
        RESET_CREDITS_METER_LABEL,
        count.to_string(),
    );
    let unknown_index = groups.len();
    for (index, (at, number)) in groups.into_iter().enumerate() {
        out.meters
            .push(reset_credit_expiry_meter(index, number, Some(at)));
    }
    if unknown > 0 {
        out.meters
            .push(reset_credit_expiry_meter(unknown_index, unknown, None));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn available_grants_are_grouped_by_expiry_and_other_statuses_are_ignored() {
        let mut meters = AccountMeters::default();
        let raw = serde_json::json!({"available_count":4,"credits":[
            {"status":"consumed","expires_at":"2030-01-01T00:00:00Z"},
            {"status":"AVAILABLE","expires_at":"2099-01-01T00:00:00.943169Z"},
            {"status":"available","expires_at":4_070_908_800_000_u64},
            {"status":"expired","expires_at":"2020-01-01T00:00:00Z"},
            {"status":"available","expires_at":"4102444800"}
        ]});
        assert!(read(&mut meters, &raw));
        assert_eq!(meters.meters[0].value.as_deref(), Some("4"));
        assert_eq!(meters.meters[1].label, "Reset credit expiry · 2");
        assert_eq!(meters.meters[1].value.as_deref(), Some("4070908800"));
        assert_eq!(meters.meters[2].value.as_deref(), Some("4102444800"));
        assert_eq!(meters.meters[3].value.as_deref(), Some("unknown"));
        assert!(needs_supplement(&meters));
        assert_eq!(meters.effective_max(1), None);
    }

    #[test]
    fn count_can_come_from_available_grants_and_invalid_responses_preserve_previous_data() {
        let mut meters = AccountMeters::default();
        assert!(read(
            &mut meters,
            &serde_json::json!({"credits":[
                {"status":"available","expires_at":"2099-01-01T00:00:00Z"},
                {"status":"used","expires_at":"2100-01-01T00:00:00Z"}
            ]})
        ));
        assert_eq!(meters.meters[0].value.as_deref(), Some("1"));
        assert!(!needs_supplement(&meters));
        let before = meters.clone();
        assert!(!read(
            &mut meters,
            &serde_json::json!({"error":"unavailable"})
        ));
        assert_eq!(meters, before);
        assert!(read(&mut meters, &serde_json::json!({"available_count":0})));
        assert_eq!(meters.meters.len(), 1);
        assert!(!needs_supplement(&meters));
    }
}
