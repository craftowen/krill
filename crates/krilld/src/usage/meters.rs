//! Provider-specific usage mapping. Reference: Orca src/main/rate-limits/{antigravity-usage-response,
//! opencode-go-status-parsing,codex-backend-usage-client}.ts, Copyright (c) 2026 Lovecast Inc., MIT.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use krill_proto::usage::{AccountInfo, AccountMeters, ProviderKind, UsageMeter, Window};
use serde_json::Value;

use crate::session::lock;

mod claude;
pub(super) mod codex_reset;

type Key = (PathBuf, ProviderKind, PathBuf);
static CACHE: Mutex<BTreeMap<Key, (Instant, AccountMeters)>> = Mutex::new(BTreeMap::new());
const CACHE_TTL: Duration = Duration::from_mins(30);

pub(super) fn remember(home: &Path, provider: ProviderKind, dir: &Path, reading: AccountMeters) {
    let mut cache = lock(&CACHE);
    cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
    cache.insert(
        (home.into(), provider, dir.into()),
        (Instant::now(), reading),
    );
}

pub(super) fn has_cached(home: &Path, provider: ProviderKind, dir: &Path) -> bool {
    lock(&CACHE)
        .get(&(home.into(), provider, dir.into()))
        .is_some_and(|(at, _)| at.elapsed() < CACHE_TTL)
}

pub(super) fn evict(home: &Path) {
    lock(&CACHE).retain(|(h, _, _), _| h != home);
}

pub(super) fn evict_profile(home: &Path, provider: ProviderKind, dir: &Path) {
    lock(&CACHE).remove(&(home.into(), provider, dir.into()));
}

pub(super) fn read(home: &Path, account: &AccountInfo) -> AccountMeters {
    let dir = Path::new(&account.profile_dir);
    let cached = lock(&CACHE)
        .get(&(home.into(), account.provider, dir.into()))
        .filter(|(at, _)| at.elapsed() < CACHE_TTL)
        .map(|(_, reading)| reading.clone());
    let from_cache = cached.is_some();
    let mut reading = cached.unwrap_or_else(|| read_local(home, account));
    reading.account_id.clone_from(&account.id);
    if from_cache && account.provider == ProviderKind::Claude {
        for (key, window) in [
            ("five_hour", &account.usage.five_hour),
            ("seven_day", &account.usage.seven_day),
        ] {
            if let Some(window) = window
                && let Some(meter) = reading.meters.iter_mut().find(|m| m.key == key)
            {
                meter.window = Some(window.clone());
            }
        }
    }
    if reading.meters.is_empty() {
        for (key, label, window) in [
            ("five_hour", "5h", &account.usage.five_hour),
            ("seven_day", "7d", &account.usage.seven_day),
        ] {
            if let Some(window) = window {
                reading.meters.push(UsageMeter {
                    key: key.into(),
                    label: label.into(),
                    window: Some(window.clone()),
                    value: None,
                });
            }
        }
    }
    reading
}

fn read_local(home: &Path, account: &AccountInfo) -> AccountMeters {
    let dir = Path::new(&account.profile_dir);
    for name in [
        "statusline_cache.json",
        "usage_cache.json",
        "usage.json",
        "quota.json",
        "rate_limits.json",
    ] {
        if let Some(raw) = super::direct::read_file(&dir.join(name))
            && let Ok(text) = std::str::from_utf8(&raw)
        {
            let reading = parse(account.provider, text);
            if !reading.meters.is_empty() {
                return reading;
            }
        }
    }
    if account.provider == ProviderKind::Codex
        && let Some(path) = super::latest_rollout(&dir.join("sessions"))
        && let Ok(file) = std::fs::File::open(path)
    {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::End(
            -i64::try_from(file.metadata().map_or(0, |m| m.len()).min(super::TAIL_MAX))
                .unwrap_or(0),
        ))
        .ok();
        let mut bytes = Vec::new();
        file.take(super::TAIL_MAX).read_to_end(&mut bytes).ok();
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines().rev() {
            let reading = parse(account.provider, line);
            if !reading.meters.is_empty() {
                return reading;
            }
        }
    }
    if account.provider == ProviderKind::Claude
        && let Some(raw) = super::direct::read_file(&home.join(".claude").join(super::ORCA_CACHE))
        && let Ok(json) = serde_json::from_slice::<Value>(&raw)
        && let Some(rows) = json.get("accounts").and_then(Value::as_array)
    {
        let uuid = dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str());
        if let Some(row) = rows.iter().find(|row| {
            if account.host_default {
                row.get("host").and_then(Value::as_bool) == Some(true)
            } else {
                uuid.is_some() && row.get("id").and_then(Value::as_str) == uuid
            }
        }) {
            return parse(account.provider, &row.to_string());
        }
    }
    AccountMeters::default()
}

pub(super) fn parse(provider: ProviderKind, text: &str) -> AccountMeters {
    if text.len() > 1024 * 1024 {
        return AccountMeters::default();
    }
    let json = serde_json::from_str::<Value>(text).ok().or_else(|| {
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|v| v.pointer("/command/name").and_then(Value::as_str) == Some("usage"))
    });
    let Some(json) = json else {
        return AccountMeters::default();
    };
    let mut out = AccountMeters {
        plan: json
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..AccountMeters::default()
    };
    match provider {
        ProviderKind::Claude => claude::read(&mut out, &json),
        ProviderKind::Codex => {
            let rl = json
                .get("rate_limit")
                .or_else(|| json.get("rate_limits"))
                .or_else(|| json.pointer("/payload/rate_limits"));
            if let Some(rl) = rl {
                for key in ["primary_window", "secondary_window", "primary", "secondary"] {
                    if let Some(raw) = rl.get(key) {
                        let minutes =
                            raw.get("window_minutes")
                                .and_then(Value::as_u64)
                                .or_else(|| {
                                    raw.get("limit_window_seconds")
                                        .and_then(Value::as_u64)
                                        .map(|s| s / 60)
                                });
                        let label = match minutes {
                            Some(300) => "5h".into(),
                            Some(10_080) => "7d".into(),
                            Some(n) => format!("{n}m"),
                            None => continue,
                        };
                        push(
                            &mut out,
                            Some(raw),
                            key,
                            &label,
                            &["used_percent", "usedPercent"],
                        );
                    }
                }
            }
            if let Some(credits) = json.get("credits") {
                if credits.get("unlimited").and_then(Value::as_bool) == Some(true) {
                    value_meter(&mut out, "credits", "Credits", "∞".into());
                } else if credits
                    .get("has_credits")
                    .or_else(|| credits.get("hasCredits"))
                    .and_then(Value::as_bool)
                    == Some(true)
                    && let Some(balance) = number(credits.get("balance"))
                {
                    value_meter(&mut out, "credits", "Credits", format!("{balance}"));
                }
            }
            if let Some(credits) = json.get("rate_limit_reset_credits") {
                codex_reset::read(&mut out, credits);
            }
            if let Some(review) = json.get("code_review_rate_limit") {
                for key in ["primary_window", "secondary_window"] {
                    push(
                        &mut out,
                        review.get(key),
                        &format!("review_{key}"),
                        "Code review",
                        &["used_percent"],
                    );
                }
            }
        }
        ProviderKind::Antigravity => {
            if json
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| s != "SUCCESS")
            {
                return out;
            }
            if json
                .pointer("/command/name")
                .and_then(Value::as_str)
                .is_some_and(|s| s != "usage")
            {
                return out;
            }
            let groups = json
                .pointer("/command/data/groups")
                .or_else(|| json.pointer("/data/groups"))
                .or_else(|| json.get("groups"));
            for group in groups
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(32)
            {
                let name = group
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Models");
                for raw in group
                    .get("buckets")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(16)
                {
                    if raw.get("disabled").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let Some(remaining) = number(raw.get("remaining_fraction")) else {
                        continue;
                    };
                    let window = raw.get("window").and_then(Value::as_str).unwrap_or("");
                    let window = match window {
                        "weekly" => "7d",
                        other => other,
                    };
                    let key = raw.get("id").and_then(Value::as_str).unwrap_or(window);
                    out.meters.push(UsageMeter {
                        key: key.into(),
                        label: format!("{name} · {window}"),
                        window: Some(Window {
                            percent: pct((1.0 - remaining.clamp(0.0, 1.0)) * 100.0),
                            resets_at: reset(raw),
                        }),
                        value: None,
                    });
                }
            }
        }
        ProviderKind::OpenCode => {
            for (key, legacy, label) in [
                ("rolling", "fiveHour", "5h"),
                ("weekly", "week", "7d"),
                ("monthly", "month", "30d"),
            ] {
                if let Some(raw) = json.get("usage").and_then(|v| v.get(key)) {
                    push(&mut out, Some(raw), key, label, &["percent"]);
                } else if let Some(raw) = json.pointer("/access/meters").and_then(|v| v.get(legacy))
                    && let (Some(used), Some(limit)) = (
                        number(raw.get("usedMicroCents")),
                        number(raw.get("limitMicroCents")),
                    )
                    && limit > 0.0
                {
                    out.meters.push(UsageMeter {
                        key: key.into(),
                        label: label.into(),
                        window: Some(Window {
                            percent: pct(used / limit * 100.0),
                            resets_at: reset(raw),
                        }),
                        value: None,
                    });
                }
            }
            // Legacy Krill caches may expose the same two windows as Claude.
            if out.meters.is_empty() {
                for (key, label) in [("five_hour", "5h"), ("seven_day", "7d")] {
                    push(
                        &mut out,
                        json.get(key),
                        key,
                        label,
                        &["utilization", "used_percent"],
                    );
                }
            }
        }
        ProviderKind::Gemini => {}
    }
    out.plan = out.plan.map(|s| s.chars().take(64).collect());
    out.meters.truncate(64);
    for meter in &mut out.meters {
        meter.key = meter.key.chars().take(128).collect();
        meter.label = meter
            .label
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect();
    }
    out
}

fn number(raw: Option<&Value>) -> Option<f64> {
    let raw = raw?;
    raw.as_f64()
        .or_else(|| raw.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite, rounded and clamped to 0..=100"
)]
fn pct(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

fn reset(raw: &Value) -> Option<String> {
    ["resets_at", "reset_at", "reset_time", "resetsAt"]
        .into_iter()
        .find_map(|key| raw.get(key).and_then(super::resets))
        .map(|s| s.chars().take(128).collect())
}

fn push(out: &mut AccountMeters, raw: Option<&Value>, key: &str, label: &str, keys: &[&str]) {
    let Some(raw) = raw else {
        return;
    };
    let Some(percent) = keys.iter().find_map(|key| number(raw.get(*key))) else {
        return;
    };
    out.meters.push(UsageMeter {
        key: key.into(),
        label: label.into(),
        window: Some(Window {
            percent: pct(percent),
            resets_at: reset(raw),
        }),
        value: None,
    });
}

fn value_meter(out: &mut AccountMeters, key: &str, label: &str, value: String) {
    out.meters.push(UsageMeter {
        key: key.into(),
        label: label.into(),
        window: None,
        value: Some(value),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_reset_credit_expiry_is_metadata_not_a_usage_pool() {
        for (raw, expected) in [
            (r#","expires_at":"2099-01-01T00:00:00Z""#, "4070908800"),
            ("", "unknown"),
        ] {
            let meters = parse(
                ProviderKind::Codex,
                &format!(r#"{{"rate_limit_reset_credits":{{"available_count":2{raw}}}}}"#),
            );
            assert_eq!(meters.meters[1].value.as_deref(), Some(expected));
            assert_eq!(meters.effective_max(1), None);
        }
    }
    #[test]
    fn provider_meters_preserve_pools_credits_and_monthly_limits() {
        let agy = parse(
            ProviderKind::Antigravity,
            r#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini","buckets":[{"id":"g","window":"weekly","remaining_fraction":0.2},{"id":"disabled","window":"5h","disabled":true,"remaining_fraction":0}]},{"name":"Claude and GPT","buckets":[{"id":"c","window":"weekly","remaining_fraction":0.8}]}]}}}"#,
        );
        assert_eq!(agy.meters.len(), 2);
        assert_eq!(agy.meters[0].window.as_ref().unwrap().percent, 80);
        assert_eq!(agy.meters[1].label, "Claude and GPT · 7d");
        let go = parse(
            ProviderKind::OpenCode,
            r#"{"usage":{"rolling":{"percent":12},"weekly":{"percent":34},"monthly":{"percent":56}}}"#,
        );
        assert_eq!(
            go.meters
                .iter()
                .map(|m| m.label.as_str())
                .collect::<Vec<_>>(),
            ["5h", "7d", "30d"]
        );
        let codex = parse(
            ProviderKind::Codex,
            r#"{"plan_type":"pro","rate_limit":{"primary_window":{"used_percent":7,"limit_window_seconds":3600}},"credits":{"has_credits":true,"balance":"42"},"rate_limit_reset_credits":{"available_count":2}}"#,
        );
        assert_eq!(codex.plan.as_deref(), Some("pro"));
        assert_eq!(codex.meters[0].label, "60m");
        assert_eq!(codex.meters[1].value.as_deref(), Some("42"));
        assert_eq!(codex.meters[2].value.as_deref(), Some("2"));
        let claude = parse(
            ProviderKind::Claude,
            r#"{"seven_day_sonnet":{"utilization":18},"seven_day_opus":null,"extra_usage":{"is_enabled":true,"utilization":10,"used_credits":123,"monthly_limit":2000}}"#,
        );
        assert_eq!(claude.meters[0].label, "Sonnet · 7d");
        assert_eq!(
            claude.meters.last().unwrap().value.as_deref(),
            Some("$1.23 / $20.00")
        );
        assert!(parse(ProviderKind::OpenCode, "{}").meters.is_empty());
    }

    #[test]
    fn claude_scoped_limits_and_spend_keep_currency_exponents_and_real_balance() {
        let reading = parse(
            ProviderKind::Claude,
            r#"{
          "limits":[{"kind":"weekly_scoped","percent":27,"scope":{"model":{"display_name":"Fable"}}}],
          "fable_weekly":{"utilization":99},
          "spend":{"enabled":true,"used":{"amount_minor":5000,"currency":"EUR","exponent":2},
            "cap":{"money":{"amount_minor":200000,"currency":"EUR","exponent":2}},
            "balance":{"amount_minor":1234,"currency":"EUR","exponent":2}}
        }"#,
        );
        assert_eq!(
            reading
                .meters
                .iter()
                .filter(|m| m.label == "Fable · 7d")
                .count(),
            1
        );
        assert_eq!(reading.meters[0].window.as_ref().unwrap().percent, 27);
        assert_eq!(reading.meters[1].window.as_ref().unwrap().percent, 3);
        assert_eq!(
            reading.meters[2].value.as_deref(),
            Some("EUR 50.00 / EUR 2000.00")
        );
        assert_eq!(reading.meters[3].value.as_deref(), Some("EUR 12.34"));
        let legacy = parse(
            ProviderKind::Claude,
            r#"{"extra_usage":{"is_enabled":false,"currency":"JPY","decimal_places":0,"used_credits":50,"monthly_limit":200}}"#,
        );
        assert!(
            legacy
                .meters
                .iter()
                .all(|m| m.window.is_none() && m.key != "credits")
        );
        assert_eq!(
            legacy.meters[0].value.as_deref(),
            Some("JPY 50.00 / JPY 200.00")
        );
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_provider_payloads_are_bounded_and_do_not_panic(text in ".{0,4096}") {
            for provider in ProviderKind::ALL {
                let reading = parse(provider, &text);
                proptest::prop_assert!(reading.meters.len() <= 64);
                for meter in reading.meters {
                    proptest::prop_assert!(meter.label.chars().count() <= 120);
                    proptest::prop_assert!(meter.window.is_none_or(|w| w.percent <= 100));
                }
            }
        }

        #[test]
        fn all_reported_go_windows_clamp_percent_without_inventing_missing_windows(percent in -1000_i32..1000) {
            let reading = parse(ProviderKind::OpenCode, &serde_json::json!({"usage":{"monthly":{"percent":percent}}}).to_string());
            proptest::prop_assert_eq!(reading.meters.len(), 1);
            proptest::prop_assert_eq!(&reading.meters[0].label, "30d");
            proptest::prop_assert_eq!(i32::from(reading.meters[0].window.as_ref().unwrap().percent), percent.clamp(0, 100));
        }
    }
}
