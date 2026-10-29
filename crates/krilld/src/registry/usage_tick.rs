//! OWE-702: 계정 사용량·자동전환 주기 틱. 앱이 꺼져 있어도 데몬이 [`TICK`]마다 자동전환을 평가한다.
//!
//! - 사용량은 자동전환 활성 여부와 관계없이 백그라운드에서 수집한다.
//! - 자동전환은 등록 계정과 활성 정책이 있을 때만 평가한다.
//! - 직접 조회 캐시는 파일 변경 없이 갱신되므로 파일 지문으로 판정을 생략하지 않는다.

use std::path::Path;
use std::time::Duration;

use krill_proto::usage::{AccountsSnapshot, AutoSwitchPolicy};

use super::{Registry, lock};
use crate::usage::accounts;

pub const TICK: Duration = Duration::from_secs(60);

/// 한 번의 틱. 자동전환 정책을 평가했으면 스냅샷, 비활성이면 `None`.
pub fn tick(home: &Path, state_dir: Option<&Path>) -> Option<AccountsSnapshot> {
    crate::usage::direct::refresh(home, state_dir);
    let file = accounts::load_file(home, state_dir);
    let policy = AutoSwitchPolicy::from(file.auto_switch.clone());
    if file.accounts.is_empty() || !(policy.enabled || policy.rules.iter().any(|r| r.enabled)) {
        return None;
    }
    Some(accounts::refresh_accounts(home, state_dir))
}

impl Registry {
    /// 주기 스레드 본체 한 바퀴(main이 [`TICK`]마다, `catch_unwind` 안에서 부른다).
    /// 업그레이드 인계 중에는 두 데몬이 동시에 `accounts.toml`을 쓰지 않게 건너뛴다.
    pub fn tick_accounts(&self) {
        if *lock(&self.upgrading) {
            return;
        }
        let home = accounts::default_home();
        let Some(snap) = tick(&home, self.state_dir().as_deref()) else {
            return;
        };
        for s in &snap.switched {
            tracing::info!(
                provider = s.provider.as_str(),
                from = %s.from_id,
                to = %s.to_id,
                percent = s.reason_percent,
                "usage tick auto-switched account"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use krill_proto::usage::ProviderKind;

    use crate::usage::CLAUDE_CACHE;

    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("owe702-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn no_accounts_skips_without_reading_or_writing() {
        let (home, state) = (dir("empty-home"), dir("empty-state"));
        assert!(tick(&home, Some(&state)).is_none());
        assert!(!accounts::config_path(&home, Some(&state)).exists());
        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn direct_usage_is_evaluated_even_when_disk_fingerprint_is_unchanged() {
        let (home, state) = (dir("direct-home"), dir("direct-state"));
        accounts::set_auto_switch(
            &home,
            Some(&state),
            &AutoSwitchPolicy {
                enabled: true,
                ..AutoSwitchPolicy::default()
            },
        )
        .unwrap();
        accounts::add_account(&home, Some(&state), ProviderKind::Claude, "Quota", None, "")
            .unwrap();
        assert!(tick(&home, Some(&state)).is_some());
        assert!(
            tick(&home, Some(&state)).is_some(),
            "직접 조회 캐시는 파일 변경 없이 갱신된다"
        );
        fs::remove_dir_all(home).ok();
        fs::remove_dir_all(state).ok();
    }

    #[test]
    fn legacy_disabled_usage_still_evaluates_and_auto_switch_off_skips() {
        let (home, state) = (dir("home"), dir("state"));
        accounts::set_auto_switch(
            &home,
            Some(&state),
            &AutoSwitchPolicy {
                enabled: true,
                ..AutoSwitchPolicy::default()
            },
        )
        .unwrap();
        let backup = home.join("claude-backup");
        accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Claude,
            "Backup",
            None,
            &backup.display().to_string(),
        )
        .unwrap();
        fs::write(
            backup.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":10}}"#,
        )
        .unwrap();
        let config = accounts::config_path(&home, Some(&state));
        let raw = fs::read_to_string(&config).unwrap();
        fs::write(&config, format!("direct_usage = false\n{raw}")).unwrap();
        let host = home.join(".claude");
        fs::create_dir_all(&host).unwrap();
        fs::write(
            host.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":5}}"#,
        )
        .unwrap();

        let first = tick(&home, Some(&state)).expect("first tick evaluates");
        assert!(first.switched.is_empty(), "{first:?}");
        assert!(
            tick(&home, Some(&state)).is_some(),
            "legacy opt-out cannot skip in-memory usage updates"
        );

        // 새 사용량이 소진 임계값을 넘으면 대기 계정으로 전환한다.
        fs::write(
            host.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":95}}"#,
        )
        .unwrap();
        let snap = tick(&home, Some(&state)).expect("changed cache evaluates");
        assert_eq!(snap.switched.len(), 1, "{snap:?}");
        assert_eq!(snap.switched[0].to_id, "claude-backup");

        let off = AutoSwitchPolicy {
            enabled: false,
            ..AutoSwitchPolicy::default()
        };
        accounts::set_auto_switch(&home, Some(&state), &off).unwrap();
        fs::write(
            host.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":99}}"#,
        )
        .unwrap();
        assert!(tick(&home, Some(&state)).is_none(), "auto-switch off skips");
        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&state).ok();
    }
}
