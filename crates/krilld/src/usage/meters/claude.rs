//! Reference: Orca claude-oauth-usage-request.ts and claude-usage-credits.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

use super::{AccountMeters, UsageMeter, Value, Window, number, pct, push, value_meter};

pub(super) fn read(out: &mut AccountMeters, json: &Value) {
    let body = json.get("rate_limits").unwrap_or(json);
    for (key, label) in [
        ("five_hour", "5h"),
        ("seven_day", "7d"),
        ("seven_day_sonnet", "Sonnet · 7d"),
        ("seven_day_opus", "Opus · 7d"),
        ("seven_day_oauth_apps", "OAuth apps · 7d"),
    ] {
        push(
            out,
            body.get(key),
            key,
            label,
            &["utilization", "used_percentage", "usedPercent"],
        );
    }
    for (index, limit) in body
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(32)
        .enumerate()
    {
        if limit.get("kind").and_then(Value::as_str) != Some("weekly_scoped")
            || limit.get("is_active").and_then(Value::as_bool) == Some(false)
        {
            continue;
        }
        if let Some(name) = limit
            .pointer("/scope/model/display_name")
            .and_then(Value::as_str)
        {
            push(
                out,
                Some(limit),
                &format!("scoped_{index}"),
                &format!("{name} · 7d"),
                &["percent"],
            );
        }
    }
    if !out
        .meters
        .iter()
        .any(|m| m.label.eq_ignore_ascii_case("Fable · 7d"))
    {
        for key in ["fable_weekly", "fable_seven_day", "seven_day_fable"] {
            let count = out.meters.len();
            push(
                out,
                body.get(key),
                key,
                "Fable · 7d",
                &["utilization", "used_percentage", "usedPercent"],
            );
            if out.meters.len() > count {
                break;
            }
        }
    }
    if !spend(out, body.get("spend")) {
        legacy_spend(out, body.get("extra_usage"));
    }
}

fn money(raw: Option<&Value>) -> Option<f64> {
    let raw = raw?;
    if let Some(value) = number(Some(raw)) {
        return Some(value);
    }
    let exponent = raw.get("exponent").and_then(Value::as_i64).unwrap_or(2);
    let exponent = i32::try_from(exponent)
        .ok()
        .filter(|e| (0..=6).contains(e))?;
    number(raw.get("amount_minor")).map(|n| n / 10_f64.powi(exponent))
}

fn currency(raw: Option<&Value>) -> &str {
    raw.and_then(Value::as_str)
        .filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase()))
        .unwrap_or("USD")
}

fn formatted(value: f64, code: &str) -> String {
    if code == "USD" {
        format!("${value:.2}")
    } else {
        format!("{code} {value:.2}")
    }
}

fn spend(out: &mut AccountMeters, raw: Option<&Value>) -> bool {
    let Some(raw) = raw else { return false };
    let used = money(raw.get("used"));
    let limit = money(raw.get("limit")).or_else(|| money(raw.pointer("/cap/money")));
    let balance = money(raw.get("balance"));
    if limit.is_none() && balance.is_none() {
        return false;
    }
    let code = currency(
        raw.pointer("/used/currency")
            .or_else(|| raw.pointer("/limit/currency"))
            .or_else(|| raw.pointer("/cap/money/currency"))
            .or_else(|| raw.pointer("/balance/currency")),
    );
    let percent =
        number(raw.get("percent")).or_else(|| Some(used? / limit.filter(|n| *n > 0.0)? * 100.0));
    record(
        out,
        used,
        limit,
        percent.filter(|_| raw.get("enabled").and_then(Value::as_bool) == Some(true)),
        code,
    );
    if let Some(balance) = balance {
        value_meter(out, "credits", "Credits", formatted(balance, code));
    }
    true
}

fn legacy_spend(out: &mut AccountMeters, raw: Option<&Value>) {
    let Some(raw) = raw else { return };
    let exponent = raw
        .get("decimal_places")
        .and_then(Value::as_i64)
        .unwrap_or(2);
    let Some(exponent) = i32::try_from(exponent).ok().filter(|e| (0..=6).contains(e)) else {
        return;
    };
    let factor = 10_f64.powi(exponent);
    let used = number(raw.get("used_credits")).map(|n| n / factor);
    let limit = number(raw.get("monthly_limit")).map(|n| n / factor);
    let percent = number(raw.get("utilization"))
        .or_else(|| Some(used? / limit.filter(|n| *n > 0.0)? * 100.0));
    record(
        out,
        used,
        limit,
        percent.filter(|_| raw.get("is_enabled").and_then(Value::as_bool) == Some(true)),
        currency(raw.get("currency")),
    );
}

fn record(
    out: &mut AccountMeters,
    used: Option<f64>,
    limit: Option<f64>,
    percent: Option<f64>,
    code: &str,
) {
    if let Some(percent) = percent {
        out.meters.push(UsageMeter {
            key: "extra_usage".into(),
            label: "Extra usage".into(),
            window: Some(Window {
                percent: pct(percent),
                resets_at: None,
            }),
            value: None,
        });
    }
    if let Some(used) = used {
        let value = limit.map_or_else(
            || formatted(used, code),
            |limit| format!("{} / {}", formatted(used, code), formatted(limit, code)),
        );
        value_meter(out, "extra_spend", "Extra usage", value);
    }
}
