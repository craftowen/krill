//! OWE-712: 계정별 표본·전환 이력은 고정 상한, 쿨다운은 재시작 후에도 유지한다.
use krill_proto::time::parse_reset_epoch;
use krill_proto::usage::{
    AccountInfo, AccountPolicyState, AdvancedPolicy, AutoSwitchEvent, AutoSwitchMetric,
    ModelWindow, ProviderCooldown, RateLimitExpiry, Snapshot, SwitchRecord, UsageForecast,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

const MAX_ACCOUNTS: usize = 256;
const MAX_SAMPLES: usize = 16;
const MAX_HISTORY: usize = 128;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// 리셋을 알 수 없는 한도 감지의 재평가 대기 시간. 영구 차단하지 않는다.
pub(super) const UNKNOWN_LIMIT_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Sample {
    at: u64,
    usage: Snapshot,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Runtime {
    samples: BTreeMap<String, VecDeque<Sample>>,
    cooldowns: BTreeMap<String, u64>,
    history: VecDeque<SwitchRecord>,
    sequence: u64,
}

fn path(home: &Path, state: Option<&Path>) -> PathBuf {
    super::config_path(home, state).with_file_name("account-policy-state.json")
}

/// Older daemons used a PID-only name; the shared atomic writer adds a sequence.
fn temp_owner(name: &str) -> Option<u32> {
    let pid = if let Some(rest) = name.strip_prefix("account-policy-state.") {
        rest.strip_suffix(".tmp")?
    } else {
        let rest = name.strip_prefix(".account-policy-state.json.tmp.")?;
        let (pid, sequence) = rest.split_once('.')?;
        if sequence.is_empty() || !sequence.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        pid
    };
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok().filter(|pid| *pid > 1)
}

fn cleanup_orphan_temps(path: &Path, live_pids: &[u32]) {
    // An empty/incomplete snapshot that cannot see this process is not proof of death.
    if !live_pids.contains(&std::process::id()) {
        return;
    }
    let Some(parent) = path.parent() else { return };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(temp_owner) else {
            continue;
        };
        if live_pids.contains(&pid) {
            continue;
        }
        // Do not touch symlinks, directories, recent writes, or uncertain metadata.
        let old_file = entry.file_type().is_ok_and(|kind| kind.is_file())
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age >= std::time::Duration::from_secs(3600));
        if old_file {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

impl Runtime {
    pub fn load(home: &Path, state: Option<&Path>) -> Self {
        let path = path(home, state);
        // If process enumeration fails, preserve every temp rather than guessing ownership.
        if let Ok(pids) = crate::sys::all_pids() {
            cleanup_orphan_temps(&path, &pids);
        }
        let mut bytes = Vec::new();
        let Ok(file) = std::fs::File::open(path) else {
            return Self::default();
        };
        if file
            .take(MAX_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES
        {
            return Self::default();
        }
        let mut value: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        while value.samples.len() > MAX_ACCOUNTS {
            value.samples.pop_last();
        }
        for ring in value.samples.values_mut() {
            while ring.len() > MAX_SAMPLES {
                ring.pop_front();
            }
        }
        while value.history.len() > MAX_HISTORY {
            value.history.pop_front();
        }
        value
            .cooldowns
            .retain(|key, _| krill_proto::usage::ProviderKind::parse(key).is_some());
        value
    }

    pub fn save(&self, home: &Path, state: Option<&Path>) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        // Unique names avoid concurrent saves truncating each other's pending file;
        // the shared writer removes its temp on write, sync, and rename errors.
        super::write_atomic_0600(&path(home, state), &bytes)
    }

    pub fn observe(&mut self, accounts: &[AccountInfo], stamps: &BTreeMap<String, u64>) {
        self.samples
            .retain(|id, _| accounts.iter().any(|a| a.id == *id));
        for a in accounts
            .iter()
            .filter(|a| !a.stale && !a.usage.is_empty())
            .take(MAX_ACCOUNTS)
        {
            let Some(&at) = stamps.get(&a.id) else {
                continue;
            };
            let ring = self.samples.entry(a.id.clone()).or_default();
            if ring.back().is_some_and(|s| s.at >= at) {
                continue;
            }
            ring.push_back(Sample {
                at,
                usage: a.usage.clone(),
            });
            while ring.len() > MAX_SAMPLES {
                ring.pop_front();
            }
        }
        while self.samples.len() > MAX_ACCOUNTS {
            self.samples.pop_last();
        }
    }

    pub fn forecasts(&self, now: u64) -> Vec<UsageForecast> {
        let mut out = Vec::new();
        for (id, ring) in &self.samples {
            let Some(last) = ring.back() else { continue };
            // 오래된 캐시로 미래를 단정하지 않는다.
            if now.saturating_sub(last.at) > 30 * 60 {
                continue;
            }
            for name in ["5h", "7d"] {
                let window = |s: &Sample| {
                    if name == "5h" {
                        s.usage.five_hour.clone()
                    } else {
                        s.usage.seven_day.clone()
                    }
                };
                let Some(end) = window(last) else { continue };
                let Some(reset) = end
                    .resets_at
                    .as_deref()
                    .and_then(parse_reset_epoch)
                    .and_then(|e| u64::try_from(e).ok())
                    .filter(|e| *e > now)
                else {
                    continue;
                };
                // 감소·리셋 경계를 넘지 않은 연속 구간에서만 증가 속도를 구한다.
                let mut first = last;
                let mut previous = end.percent;
                for sample in ring.iter().rev().skip(1) {
                    let Some(w) = window(sample) else { break };
                    if w.resets_at != end.resets_at || w.percent > previous {
                        break;
                    }
                    first = sample;
                    previous = w.percent;
                }
                let elapsed = last.at.saturating_sub(first.at);
                let Some(start) = window(first) else { continue };
                let increase = u64::from(end.percent.saturating_sub(start.percent));
                if elapsed < 30 || increase == 0 {
                    continue;
                }
                let remaining = u64::from(100_u8.saturating_sub(end.percent));
                let exhausts_at = last
                    .at
                    .saturating_add(remaining.saturating_mul(elapsed).div_ceil(increase));
                out.push(UsageForecast {
                    account_id: id.clone(),
                    window: name.into(),
                    percent_per_hour: increase.saturating_mul(3600).div_ceil(elapsed),
                    exhausts_at,
                    resets_at: reset,
                });
            }
        }
        out
    }

    pub fn cooling(&self, provider: krill_proto::usage::ProviderKind, now: u64) -> bool {
        self.cooldowns
            .get(provider.as_str())
            .is_some_and(|until| now < *until)
    }

    pub fn record(
        &mut self,
        event: AutoSwitchEvent,
        now: u64,
        policy: &AdvancedPolicy,
        predicted: bool,
    ) {
        self.sequence = self.sequence.saturating_add(1);
        self.cooldowns.insert(
            event.provider.as_str().into(),
            now.saturating_add(policy.cooldown_secs),
        );
        self.history.push_back(SwitchRecord {
            sequence: self.sequence,
            at: now,
            event,
            predicted,
        });
        while self.history.len() > MAX_HISTORY {
            self.history.pop_front();
        }
    }

    pub fn snapshot(
        &self,
        policy: AdvancedPolicy,
        now: u64,
        model_windows: Vec<ModelWindow>,
        rate_limits: Vec<RateLimitExpiry>,
    ) -> AccountPolicyState {
        AccountPolicyState {
            supported: true,
            policy,
            cooldowns: self
                .cooldowns
                .iter()
                .filter_map(|(key, &until)| {
                    Some(ProviderCooldown {
                        provider: krill_proto::usage::ProviderKind::parse(key)?,
                        cooldown_until: until,
                    })
                })
                .collect(),
            history: self.history.iter().cloned().collect(),
            forecasts: self.forecasts(now),
            model_windows,
            rate_limits,
        }
    }
}

pub(super) fn predictive_trigger(
    forecasts: &[UsageForecast],
    id: &str,
    metric: AutoSwitchMetric,
    now: u64,
) -> bool {
    let matches_window = |window: &str| match metric {
        AutoSwitchMetric::Max | AutoSwitchMetric::AntigravityAllMax => true,
        AutoSwitchMetric::FiveHour => window == "5h",
        AutoSwitchMetric::SevenDay => window == "7d",
        AutoSwitchMetric::ClaudeSonnetSevenDay
        | AutoSwitchMetric::ClaudeOpusSevenDay
        | AutoSwitchMetric::CodexCodeReview
        | AutoSwitchMetric::AntigravityClaudeGptMax
        | AutoSwitchMetric::AntigravityClaudeGptFiveHour
        | AutoSwitchMetric::AntigravityClaudeGptSevenDay
        | AutoSwitchMetric::OpenCodeThirtyDay => metric.short_code() == window,
    };
    forecasts.iter().any(|f| {
        f.account_id == id
            && matches_window(&f.window)
            && f.exhausts_at < f.resets_at
            && f.exhausts_at <= now.saturating_add(5 * 60)
    })
}

pub(super) fn reset_key(account: &AccountInfo, now: i64) -> i64 {
    [&account.usage.five_hour, &account.usage.seven_day]
        .into_iter()
        .flatten()
        .filter_map(|w| w.resets_at.as_deref().and_then(parse_reset_epoch))
        .filter(|reset| *reset > now)
        .min()
        .unwrap_or(i64::MAX)
}

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

    #[test]
    fn failed_publish_removes_temp_and_keeps_previous_destination() {
        let root =
            std::env::temp_dir().join(format!("policy-publish-error-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let destination = path(&root, Some(&root));
        std::fs::create_dir_all(&destination).unwrap(); // force rename failure
        assert!(runtime().save(&root, Some(&root)).is_err());
        assert!(destination.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reload_cleans_only_old_dead_owner_temps_in_both_formats() {
        let root = std::env::temp_dir().join(format!("policy-orphans-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let live = std::process::id();
        let dead = (2..u32::MAX)
            .find(|pid| !crate::sys::all_pids().unwrap().contains(pid))
            .unwrap();
        let names = [
            format!("account-policy-state.{dead}.tmp"),
            format!(".account-policy-state.json.tmp.{dead}.42"),
            format!("account-policy-state.{live}.tmp"),
            format!(".account-policy-state.json.tmp.{live}.43"),
            format!("account-policy-state.{dead}.tmp.extra"),
            "account-policy-state.invalid.tmp".into(),
            "account-policy-state.0.tmp".into(),
        ];
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        for name in &names {
            let file = std::fs::File::create(root.join(name)).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        let recent = root.join(format!(".account-policy-state.json.tmp.{dead}.44"));
        std::fs::write(&recent, b"pending").unwrap();
        let directory = root.join(format!(".account-policy-state.json.tmp.{dead}.45"));
        std::fs::create_dir(&directory).unwrap();
        cleanup_orphan_temps(&path(&root, Some(&root)), &[]);
        assert!(root.join(&names[0]).is_file());
        assert!(root.join(&names[1]).is_file());
        let expected = runtime();
        expected.save(&root, Some(&root)).unwrap();
        assert_eq!(Runtime::load(&root, Some(&root)), expected);
        for (i, name) in names.iter().enumerate() {
            assert_eq!(root.join(name).exists(), i >= 2, "{name}");
        }
        assert!(recent.is_file());
        assert!(directory.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_saves_publish_complete_snapshots_without_temp_collisions() {
        let root = std::env::temp_dir().join(format!("policy-concurrent-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    for _ in 0..16 {
                        runtime().save(&root, Some(&root)).unwrap();
                    }
                });
            }
        });
        assert_eq!(Runtime::load(&root, Some(&root)), runtime());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn usage(percent: u8, reset: u64) -> Snapshot {
        Snapshot {
            five_hour: Some(Window {
                percent,
                resets_at: Some(reset.to_string()),
            }),
            seven_day: None,
        }
    }
    fn event() -> AutoSwitchEvent {
        AutoSwitchEvent {
            provider: ProviderKind::Claude,
            from_id: "a".into(),
            from_label: "A".into(),
            to_id: "b".into(),
            to_label: "B".into(),
            reason_percent: 80,
        }
    }
    fn runtime() -> Runtime {
        let mut r = Runtime::default();
        r.samples.insert(
            "a".into(),
            VecDeque::from([
                Sample {
                    at: 1000,
                    usage: usage(40, 5000),
                },
                Sample {
                    at: 1300,
                    usage: usage(70, 5000),
                },
            ]),
        );
        r
    }

    #[test]
    fn prediction_uses_increase_and_matching_window_before_reset() {
        let r = runtime();
        let forecasts = r.forecasts(1300);
        assert_eq!(forecasts.len(), 1);
        assert_eq!(forecasts[0].exhausts_at, 1600);
        assert_eq!(forecasts[0].percent_per_hour, 360);
        assert!(predictive_trigger(
            &forecasts,
            "a",
            AutoSwitchMetric::FiveHour,
            1300
        ));
        assert!(!predictive_trigger(
            &forecasts,
            "a",
            AutoSwitchMetric::SevenDay,
            1300
        ));
        let mut after_reset = forecasts.clone();
        after_reset[0].resets_at = 1500;
        assert!(!predictive_trigger(
            &after_reset,
            "a",
            AutoSwitchMetric::Max,
            1300
        ));
    }

    #[test]
    fn prediction_rejects_decrease_reset_single_and_stale_samples() {
        for values in [
            vec![(1000, 40, 5000)],
            vec![(1000, 80, 5000), (1300, 20, 5000)],
            vec![(1000, 40, 4000), (1300, 70, 5000)],
        ] {
            let mut r = Runtime::default();
            r.samples.insert(
                "a".into(),
                values
                    .into_iter()
                    .map(|(at, pct, reset)| Sample {
                        at,
                        usage: usage(pct, reset),
                    })
                    .collect(),
            );
            assert!(r.forecasts(1300).is_empty());
        }
        assert!(runtime().forecasts(4000).is_empty());
    }

    #[test]
    fn cooldown_survives_reload_and_history_is_bounded() {
        let root = std::env::temp_dir().join(format!("owe712-runtime-{}", std::process::id()));
        let policy = AdvancedPolicy {
            cooldown_secs: 300,
            ..Default::default()
        };
        let mut r = Runtime::default();
        for at in 0..200 {
            r.record(event(), at, &policy, false);
        }
        assert!(r.cooling(ProviderKind::Claude, 498));
        assert!(!r.cooling(ProviderKind::Claude, 499));
        assert_eq!(r.history.len(), MAX_HISTORY);
        r.save(&root, Some(&root)).unwrap();
        let reloaded = Runtime::load(&root, Some(&root));
        assert_eq!(r, reloaded);
        assert!(reloaded.cooling(ProviderKind::Claude, 200));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn default_policy_is_equivalent_and_limit_expiry_uses_exhausted_windows() {
        let p = AdvancedPolicy::default();
        assert!(!p.predict_exhaustion);
        assert!(!p.prefer_soonest_reset);
        assert_eq!(p.cooldown_secs, 0);
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
    fn reset_priority_orders_soonest_valid_epoch_and_ignores_expired() {
        let account = |reset| AccountInfo {
            id: "a".into(),
            provider: ProviderKind::Claude,
            label: "A".into(),
            email: None,
            profile_dir: "/tmp".into(),
            active: false,
            host_default: false,
            stale: false,
            usage: usage(20, reset),
        };
        assert!(reset_key(&account(2000), 1000) < reset_key(&account(3000), 1000));
        assert_eq!(reset_key(&account(900), 1000), i64::MAX);
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
        std::fs::write(home.join(".claude").join(super::super::ORCA_CACHE), r#"{"accounts":[{"host":true,"seven_day_fable":{"utilization":42,"resets_at":"5000"}}]}"#).unwrap();
        let out = model_windows(&home, &[a]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].model, "fable");
        assert_eq!(out[0].seven_day.percent, 42);
        std::fs::remove_dir_all(home).ok();
    }
}

#[cfg(test)]
mod engine_tests {
    use super::super::*;
    use krill_proto::usage::AdvancedPolicy;

    fn fixture(tag: &str, active: u8) -> (PathBuf, PathBuf, AccountsFile) {
        let home = std::env::temp_dir().join(format!("owe712-engine-{tag}-{}", std::process::id()));
        let state = home.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let mut file = AccountsFile::default();
        file.auto_switch.enabled = true;
        for (id, pct, delta) in [
            ("active", active, 4000),
            ("low", 10, 5000),
            ("soon", 30, 600),
        ] {
            let dir = home.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            file.accounts.push(StoredAccount {
                id: id.into(),
                provider: "claude".into(),
                label: id.into(),
                email: None,
                profile_dir: dir.display().to_string(),
            });
            cache(&home, id, pct, delta);
        }
        file.active.insert("claude".into(), "active".into());
        save_file(&home, Some(&state), &file).unwrap();
        (home, state, file)
    }
    fn cache(home: &Path, id: &str, pct: u8, delta: i64) {
        std::fs::write(
            home.join(id).join(CLAUDE_CACHE),
            serde_json::json!({"five_hour":{"utilization":pct,"resets_at":now_epoch()+delta}})
                .to_string(),
        )
        .unwrap();
    }
    fn refresh(home: &Path, state: &Path) -> AccountsSnapshot {
        refresh_accounts_authenticated(
            home,
            Some(state),
            RefreshTrigger::Automatic,
            &BTreeMap::new(),
        )
    }
    #[test]
    fn reset_priority_changes_candidate_only_when_enabled() {
        for prefer in [false, true] {
            let (home, state, mut file) =
                fixture(if prefer { "reset-on" } else { "reset-off" }, 95);
            file.advanced_policy.prefer_soonest_reset = prefer;
            save_file(&home, Some(&state), &file).unwrap();
            let snap = refresh(&home, &state);
            assert_eq!(snap.switched[0].to_id, if prefer { "soon" } else { "low" });
            std::fs::remove_dir_all(home).ok();
        }
    }
    #[test]
    fn forecast_switches_below_threshold_and_respects_disabled_rule() {
        for disabled in [false, true] {
            let (home, state, mut file) = fixture(
                if disabled {
                    "predict-disabled"
                } else {
                    "predict"
                },
                80,
            );
            file.advanced_policy.predict_exhaustion = true;
            if disabled {
                file.auto_switch.rules.push(AutoSwitchRule {
                    provider: ProviderKind::Claude,
                    from_account_id: "active".into(),
                    enabled: false,
                    threshold_percent: 90,
                    metric: AutoSwitchMetric::Max,
                    target_account_id: None,
                });
            }
            save_file(&home, Some(&state), &file).unwrap();
            let mut runtime = policy::Runtime::default();
            let now = now_epoch();
            // Use the same reset window as the fixture, even if its file writes
            // crossed a second boundary before the historical sample is made.
            let cached: serde_json::Value = serde_json::from_slice(
                &std::fs::read(home.join("active").join(CLAUDE_CACHE)).unwrap(),
            )
            .unwrap();
            let reset = cached["five_hour"]["resets_at"].as_i64().unwrap();
            let past = AccountInfo {
                id: "active".into(),
                provider: ProviderKind::Claude,
                label: "active".into(),
                email: None,
                profile_dir: home.join("active").display().to_string(),
                active: true,
                host_default: false,
                stale: false,
                usage: Snapshot {
                    five_hour: Some(ProtoWindow {
                        percent: 20,
                        resets_at: Some(reset.to_string()),
                    }),
                    seven_day: None,
                },
            };
            runtime.observe(
                &[past],
                &BTreeMap::from([("active".into(), u64::try_from(now - 300).unwrap())]),
            );
            runtime.save(&home, Some(&state)).unwrap();
            let snap = refresh(&home, &state);
            assert_eq!(snap.switched.len(), usize::from(!disabled), "{snap:?}");
            if !disabled {
                assert!(snap.advanced.history[0].predicted);
            }
            std::fs::remove_dir_all(home).ok();
        }
    }
    /// 리셋 우선이어도 임계치 이상 후보로는 옮기지 않는다(다음 틱에 또 전환되는 연쇄 방지).
    #[test]
    fn reset_priority_never_picks_candidate_over_threshold() {
        let (home, state, mut file) = fixture("reset-over", 95);
        cache(&home, "soon", 90, 600);
        file.advanced_policy.prefer_soonest_reset = true;
        save_file(&home, Some(&state), &file).unwrap();
        let snap = refresh(&home, &state);
        assert_eq!(snap.switched[0].to_id, "low", "{snap:?}");
        std::fs::remove_dir_all(home).ok();
    }
    /// 쿨다운은 사용률 왕복을 막을 뿐, 실제 한도(StopFailure)에 걸린 계정에 묶어 두지 않는다.
    #[test]
    fn cooldown_does_not_hold_a_rate_limited_account() {
        let (home, state, mut file) = fixture("cooldown-limit", 95);
        file.advanced_policy = AdvancedPolicy {
            cooldown_secs: 900,
            ..AdvancedPolicy::default()
        };
        save_file(&home, Some(&state), &file).unwrap();
        assert_eq!(refresh(&home, &state).switched[0].to_id, "low");
        record_rate_limited(ProviderKind::Claude, "low");
        let snap = refresh_accounts_authenticated(
            &home,
            Some(&state),
            RefreshTrigger::RateLimit,
            &BTreeMap::new(),
        );
        clear_rate_limited(ProviderKind::Claude, "low");
        assert_eq!(
            snap.switched.first().map(|e| e.to_id.as_str()),
            Some("soon"),
            "{snap:?}"
        );
        std::fs::remove_dir_all(home).ok();
    }
    #[test]
    fn cooldown_blocks_second_switch_and_survives_refresh() {
        let (home, state, mut file) = fixture("cooldown", 95);
        file.advanced_policy = AdvancedPolicy {
            cooldown_secs: 900,
            ..AdvancedPolicy::default()
        };
        save_file(&home, Some(&state), &file).unwrap();
        let first = refresh(&home, &state);
        assert_eq!(first.switched[0].to_id, "low");
        cache(&home, "low", 95, 5000);
        cache(&home, "active", 5, 4000);
        let second = refresh(&home, &state);
        assert!(second.switched.is_empty());
        assert_eq!(second.advanced.history.len(), 1);
        assert!(second.accounts.iter().any(|a| a.id == "low" && a.active));
        assert!(
            second
                .advanced
                .cooldowns
                .iter()
                .any(|c| c.cooldown_until > u64::try_from(now_epoch()).unwrap())
        );
        std::fs::remove_dir_all(home).ok();
    }
}
