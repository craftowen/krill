//! 계정 사용량 주기 틱. 앱이 꺼져 있어도 데몬이 [`TICK`]마다 백그라운드에서 사용량을 수집한다.

use std::path::Path;
use std::time::Duration;

use super::{Registry, lock};
use crate::usage::accounts;

pub const TICK: Duration = Duration::from_secs(60);

/// 한 번의 틱: 등록 계정의 직접 사용량 캐시를 백그라운드에서 갱신한다.
pub fn tick(home: &Path, state_dir: Option<&Path>) {
    crate::usage::direct::refresh(home, state_dir);
}

impl Registry {
    /// 주기 스레드 본체 한 바퀴(main이 [`TICK`]마다, `catch_unwind` 안에서 부른다).
    /// 업그레이드 인계 중에는 두 데몬이 동시에 상태를 건드리지 않게 건너뛴다.
    pub fn tick_accounts(&self) {
        if *lock(&self.upgrading) {
            return;
        }
        let home = accounts::default_home();
        tick(&home, self.state_dir().as_deref());
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
    fn no_accounts_tick_does_not_create_accounts_file() {
        let (home, state) = (dir("empty-home"), dir("empty-state"));
        tick(&home, Some(&state));
        assert!(!accounts::config_path(&home, Some(&state)).exists());
        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn tick_never_auto_switches_even_with_legacy_auto_switch_config() {
        let (home, state) = (dir("legacy-home"), dir("legacy-state"));
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
        fs::write(
            &config,
            format!("[auto_switch]\nenabled = true\nthreshold_percent = 50\nswitch_on_rate_limit = true\n\n{raw}"),
        )
        .unwrap();
        let host = home.join(".claude");
        fs::create_dir_all(&host).unwrap();
        fs::write(
            host.join(CLAUDE_CACHE),
            r#"{"five_hour":{"utilization":99}}"#,
        )
        .unwrap();

        tick(&home, Some(&state));
        let snap = accounts::refresh_accounts(&home, Some(&state));
        assert!(snap.switched.is_empty());
        assert!(
            snap.accounts
                .iter()
                .any(|a| a.id == "claude-host" && a.active),
            "tick must never auto-switch away from active account"
        );
        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&state).ok();
    }
}
