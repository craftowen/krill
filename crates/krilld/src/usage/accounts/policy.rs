//! 계정 한도 만료 시각 계산과 Orca 캐시 모델별 주간 창 읽기.
use krill_proto::time::parse_reset_epoch;
use krill_proto::usage::{AccountInfo, ModelWindow, Snapshot};
use std::path::Path;

const MAX_ACCOUNTS: usize = 256;
/// 리셋을 알 수 없는 한도 감지의 재평가 대기 시간. 영구 차단하지 않는다.
pub(super) const UNKNOWN_LIMIT_SECS: u64 = 15 * 60;

pub(super) fn clears_at(usage: &Snapshot, detected: u64) -> u64 {
    let windows: Vec<_> = [&usage.five_hour, &usage.seven_day]
        .into_iter()
        .flatten()
        .collect();
    let exhausted = windows.iter().any(|w| w.percent >= 100);
    // 소진 창이 여러 개면 모두 풀린 뒤 해제한다. 낮은 캐시만 있으면 가장 가까운 리셋.
    let resets = windows
        .iter()
        .filter(|w| !exhausted || w.percent >= 100)
        .filter_map(|w| w.resets_at.as_deref().and_then(parse_reset_epoch))
        .filter_map(|e| u64::try_from(e).ok())
        .filter(|e| *e > detected);
    if exhausted {
        resets.max()
    } else {
        resets.min()
    }
    .unwrap_or_else(|| detected.saturating_add(UNKNOWN_LIMIT_SECS))
}

/// Orca 캐시에 실제 있는 모델별 주간 창만 표시한다. 네트워크 조회·가짜 0%는 없다.
pub(super) fn model_windows(home: &Path, accounts: &[AccountInfo]) -> Vec<ModelWindow> {
    let Ok(text) = std::fs::read_to_string(home.join(".claude").join(super::ORCA_CACHE)) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(rows) = value.get("accounts").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in rows.iter().take(MAX_ACCOUNTS) {
        let id = if row.get("host").and_then(serde_json::Value::as_bool) == Some(true) {
            super::host_default_id(krill_proto::usage::ProviderKind::Claude)
        } else if let Some(id) = row.get("id").and_then(serde_json::Value::as_str) {
            format!("claude-orca-{}", super::slugify(id))
        } else {
            continue;
        };
        if !accounts.iter().any(|a| a.id == id) {
            continue;
        }
        // Orca 모델 이름은 캐시 키를 따른다(예: seven_day_fable).
        for (key, data) in row.as_object().into_iter().flatten() {
            let Some(model) = key.strip_prefix("seven_day_").or_else(|| {
                matches!(
                    key.as_str(),
                    "fable" | "fable_weekly" | "fable_seven_day" | "fableWeekly"
                )
                .then_some("fable")
            }) else {
                continue;
            };
            let data = if let Some(pct) = data.get("usedPercent") {
                serde_json::json!({"utilization":pct,"resets_at":data.get("resetsAt")})
            } else {
                data.clone()
            };
            let wrapper = serde_json::json!({"seven_day": data});
            let usage = super::parse_claude(&wrapper.to_string());
            if let Some(seven_day) = usage.seven_day {
                out.push(ModelWindow {
                    account_id: id.clone(),
                    model: model.into(),
                    seven_day: seven_day.to_proto(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::usage::{ProviderKind, Window};

    fn usage(percent: u8, reset: u64) -> Snapshot {
        Snapshot {
            five_hour: Some(Window {
                percent,
                resets_at: Some(reset.to_string()),
            }),
            seven_day: None,
        }
    }

    #[test]
    fn limit_expiry_uses_exhausted_windows() {
        let mut u = usage(100, 2000);
        u.seven_day = Some(Window {
            percent: 20,
            resets_at: Some("9000".into()),
        });
        assert_eq!(clears_at(&u, 1000), 2000);
        u.seven_day.as_mut().unwrap().percent = 100;
        assert_eq!(clears_at(&u, 1000), 9000);
        assert_eq!(clears_at(&Snapshot::default(), 1000), 1900);
    }

    #[test]
    fn model_windows_are_present_only_when_cached() {
        let home = std::env::temp_dir().join(format!("owe712-model-{}", std::process::id()));
        let a = AccountInfo {
            id: "claude-host".into(),
            provider: ProviderKind::Claude,
            label: "host".into(),
            email: None,
            profile_dir: "/tmp".into(),
            active: true,
            host_default: true,
            stale: false,
            usage: usage(20, 5000),
        };
        assert!(model_windows(&home, std::slice::from_ref(&a)).is_empty());
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join(super::super::ORCA_CACHE),
            r#"{"accounts":[{"host":true,"seven_day_fable":{"utilization":42,"resets_at":"5000"}}]}"#,
        )
        .unwrap();
        let out = model_windows(&home, &[a]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].model, "fable");
        assert_eq!(out[0].seven_day.percent, 42);
        std::fs::remove_dir_all(home).ok();
    }
}
