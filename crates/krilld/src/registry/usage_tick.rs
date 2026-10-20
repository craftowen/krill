//! OWE-702: 계정 사용량·자동전환 주기 틱. 앱이 꺼져 있어도 데몬이 [`TICK`]마다 자동전환을 평가한다.
//!
//! - 등록 계정이 없거나 자동전환이 전부 꺼져 있으면 아무것도 읽지 않는다(후보·판정이 없다).
//! - 사용량 캐시 파일의 (수정 시각, 크기)가 지난 틱과 같으면 건너뛴다. 바뀌었을 때만
//!   [`accounts::refresh_accounts`](판정·저장, `SAVE_LOCK` 안)를 부른다. 지문은 잠금 밖에서 `stat`만 한다.
//! - 판정 로직은 공개 함수 그대로라 IPC `ListAccounts`·에이전트 실행과 같은 결과를 낸다.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use krill_proto::usage::{AccountsSnapshot, AutoSwitchPolicy, ProviderKind};

use super::{Registry, lock};
use crate::usage::{CLAUDE_CACHE, ORCA_CACHE, accounts, latest_rollout};

pub const TICK: Duration = Duration::from_secs(60);

/// `read_profile_usage`가 프로필 폴더에서 읽는 캐시 파일 이름의 합집합.
const CACHE_NAMES: [&str; 7] = [
    CLAUDE_CACHE,
    "usage_cache.json",
    ORCA_CACHE,
    "usage.json",
    "quota.json",
    "stats-cache.json",
    "rate_limits.json",
];

/// 틱 사이에 비교하는 입력 파일 지문. 없는 파일은 `None`.
pub type Fingerprint = Vec<(PathBuf, Option<(SystemTime, u64)>)>;

fn stamp(path: PathBuf) -> (PathBuf, Option<(SystemTime, u64)>) {
    let meta = std::fs::metadata(&path)
        .ok()
        .and_then(|m| Some((m.modified().ok()?, m.len())));
    (path, meta)
}

fn fingerprint(
    home: &Path,
    state_dir: Option<&Path>,
    file: &accounts::AccountsFile,
) -> Fingerprint {
    let mut out = vec![
        stamp(accounts::config_path(home, state_dir)),
        stamp(home.join(".claude").join(ORCA_CACHE)),
    ];
    let host = ProviderKind::ALL
        .into_iter()
        .map(|p| (p, accounts::host_default_dir(home, p)));
    let stored = file.accounts.iter().filter_map(|a| {
        Some((
            ProviderKind::parse(&a.provider)?,
            PathBuf::from(&a.profile_dir),
        ))
    });
    for (provider, dir) in host.chain(stored) {
        if provider == ProviderKind::Codex {
            for root in [dir.join("sessions"), dir.clone()] {
                out.extend(latest_rollout(&root).map(stamp));
            }
        }
        out.extend(CACHE_NAMES.iter().map(|name| stamp(dir.join(name))));
    }
    out
}

/// 한 번의 틱. 판정했으면 스냅샷, 건너뛰었으면 `None`. `seen`은 지난 판정 때의 지문이다.
pub fn tick(
    home: &Path,
    state_dir: Option<&Path>,
    seen: &mut Option<Fingerprint>,
) -> Option<AccountsSnapshot> {
    crate::usage::direct::refresh(home, state_dir);
    let file = accounts::load_file(home, state_dir);
    let policy = AutoSwitchPolicy::from(file.auto_switch.clone());
    if file.accounts.is_empty() || !(policy.enabled || policy.rules.iter().any(|r| r.enabled)) {
        *seen = None; // 다시 켜지면 첫 틱에서 바로 판정한다.
        return None;
    }
    let now = fingerprint(home, state_dir, &file);
    // 시간만 지나도 예측·쿨다운·한도 해제가 달라진다. 고급 정책 또는 대기 중
    // 한도 기록이 있으면 파일 지문이 같아도 평가한다.
    if seen.as_ref() == Some(&now)
        && !file.direct_usage // 직접 조회 캐시는 디스크 지문 밖에서 바뀐다.
        && !file.advanced_policy.predict_exhaustion
        && file.advanced_policy.cooldown_secs == 0
        && !accounts::has_rate_limits()
    {
        return None;
    }
    *seen = Some(now);
    Some(accounts::refresh_accounts(home, state_dir))
}

impl Registry {
    /// 주기 스레드 본체 한 바퀴(main이 [`TICK`]마다, `catch_unwind` 안에서 부른다).
    /// 업그레이드 인계 중에는 두 데몬이 동시에 `accounts.toml`을 쓰지 않게 건너뛴다.
    pub fn tick_accounts(&self, seen: &mut Option<Fingerprint>) {
        if *lock(&self.upgrading) {
            return;
        }
        let home = accounts::default_home();
        let Some(snap) = tick(&home, self.state_dir().as_deref(), seen) else {
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
        let mut seen = None;
        assert!(tick(&home, Some(&state), &mut seen).is_none());
        assert!(seen.is_none());
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
        let mut seen = None;
        assert!(tick(&home, Some(&state), &mut seen).is_some());
        assert!(
            tick(&home, Some(&state), &mut seen).is_some(),
            "직접 조회 캐시는 파일 변경 없이 갱신된다"
        );
        fs::remove_dir_all(home).ok();
        fs::remove_dir_all(state).ok();
    }

    #[test]
    fn evaluates_only_when_cache_files_change_and_skips_when_disabled() {
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
        accounts::set_direct_usage(&home, Some(&state), false).unwrap();
        let host = home.join(".claude");
        fs::create_dir_all(&host).unwrap();
        fs::write(
            host.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":5}}"#,
        )
        .unwrap();

        let mut seen = None;
        let first = tick(&home, Some(&state), &mut seen).expect("first tick evaluates");
        assert!(first.switched.is_empty(), "{first:?}");
        assert!(
            tick(&home, Some(&state), &mut seen).is_none(),
            "unchanged files are not re-read"
        );

        // 길이가 달라 수정 시각 해상도와 무관하게 지문이 바뀐다.
        fs::write(
            host.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":95}}"#,
        )
        .unwrap();
        let snap = tick(&home, Some(&state), &mut seen).expect("changed cache evaluates");
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
        assert!(
            tick(&home, Some(&state), &mut seen).is_none(),
            "auto-switch off skips"
        );
        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&state).ok();
    }
}
