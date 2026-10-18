#![cfg(target_os = "macos")]
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::agent::hooks::{self, Agent};

/// 임시 `HOME`. 소켓 경로가 104바이트를 넘지 않게 `/tmp` 아래에 둔다(`temp_dir()`은 길다).
struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/mk170-{}-{n}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn lib(&self, rel: &str) -> PathBuf {
        self.0.join("Library").join(rel)
    }

    fn old(&self) -> PathBuf {
        self.lib("Application Support/dev.minikrill.app")
    }

    fn new_dir(&self) -> PathBuf {
        self.lib("Application Support/dev.krill.app")
    }

    /// 구버전이 남긴 사용자 데이터 전부(상태·receipt·캐시·로그·환경설정·창 상태).
    fn seed(&self) {
        fs::create_dir_all(self.old().join("run")).unwrap();
        fs::create_dir_all(self.old().join("hooks")).unwrap();
        fs::write(self.old().join("agents.toml"), "marker").unwrap();
        fs::write(self.old().join("hooks/codex.json"), "{\"agent\":\"codex\"}").unwrap();
        for dir in [
            "Caches/dev.minikrill.app",
            "Logs/Minikrill",
            "Saved Application State/dev.minikrill.app.savedState",
        ] {
            fs::create_dir_all(self.lib(dir)).unwrap();
            fs::write(self.lib(dir).join("f"), dir).unwrap();
        }
        fs::create_dir_all(self.lib("Preferences")).unwrap();
        fs::write(self.lib("Preferences/dev.minikrill.app.plist"), "prefs").unwrap();
    }

    fn run(&self) -> Outcome {
        migrate_in(&self.0, Launchctl::Skip, &mut |_| Ok(())).unwrap()
    }

    /// 완료 상태: 데이터는 새 자리, 옛 자리는 새 이름을 가리키는 링크, 1단계 링크는 없다.
    fn assert_done(&self) {
        assert_eq!(state_dir_in(&self.0), self.new_dir());
        assert_eq!(
            fs::read_to_string(self.new_dir().join("agents.toml")).unwrap(),
            "marker"
        );
        assert_eq!(
            fs::read_to_string(self.old().join("agents.toml")).unwrap(),
            "marker"
        );
        let receipt = hooks::receipt(&state_dir_in(&self.0), Agent::Codex);
        assert!(receipt.is_file(), "hook receipt must follow the state dir");
        for (old, new) in items(&self.0) {
            assert!(!new.is_symlink() && new.exists(), "{}", new.display());
            assert_eq!(
                fs::read_link(&old).unwrap(),
                PathBuf::from(new.file_name().unwrap())
            );
            assert!(!is_link(&link_of(&old)));
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn fresh_home_needs_nothing() {
    let h = Home::new();
    assert_eq!(state_dir_in(&h.0), h.new_dir());
    assert_eq!(h.run(), Outcome::Current);
    assert!(!h.lib("Application Support").exists());
}

#[test]
fn idle_legacy_install_moves_everything_and_leaves_links() {
    let h = Home::new();
    h.seed();
    assert_eq!(state_dir_in(&h.0), h.old());
    assert_eq!(h.run(), Outcome::Migrated);
    h.assert_done();
    assert_eq!(h.run(), Outcome::Current);
    h.assert_done();
}

/// 규칙 1: 옛 holder(소켓을 쥔 쪽)가 살아 있으면 아무것도 옮기지 않고 옛 경로를 계속 쓴다.
#[test]
fn live_legacy_socket_keeps_old_path() {
    let h = Home::new();
    h.seed();
    let _holder = UnixListener::bind(h.old().join("run/krilld.sock")).unwrap();
    assert_eq!(h.run(), Outcome::Legacy);
    assert_eq!(state_dir_in(&h.0), h.old());
    assert!(!h.new_dir().exists());
    assert!(h.lib("Caches/dev.minikrill.app/f").is_file());
    assert!(!h.lib("Caches/dev.krill.app").exists());
}

/// 소켓 파일이 없어도 krilld가 소켓 락을 쥐고 있으면 살아 있는 것으로 본다.
#[test]
fn held_krilld_lock_keeps_old_path() {
    let h = Home::new();
    h.seed();
    let lock = File::create(h.old().join("run/krilld.sock.lock")).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(h.run(), Outcome::Legacy);
    assert_eq!(state_dir_in(&h.0), h.old());
    drop(lock);
    assert_eq!(h.run(), Outcome::Migrated);
    h.assert_done();
}

/// 다른 기동자가 옛 경로에 holder를 띄우는 중(기동 락)이면 옮기지 않는다.
#[test]
fn busy_launch_lock_keeps_old_path() {
    let h = Home::new();
    h.seed();
    let launch = launch_lock(&h.old().join("run/krilld.sock"), Duration::ZERO).unwrap();
    assert_eq!(h.run(), Outcome::Legacy);
    assert!(!h.new_dir().exists());
    drop(launch);
    assert_eq!(h.run(), Outcome::Migrated);
    h.assert_done();
}

/// 규칙 5: 모든 단계 직후에 끊겨도(정리 코드 없이 멈춤 = kill -9) 상태 디렉터리는 늘 한 곳에 온전히 있고,
/// 다음 실행이 끝까지 마친다.
#[test]
fn interruption_after_any_step_resumes() {
    let total = {
        let h = Home::new();
        h.seed();
        let mut n = 0;
        migrate_in(&h.0, Launchctl::Skip, &mut |_| {
            n += 1;
            Ok(())
        })
        .unwrap();
        n
    };
    assert_eq!(total, items(Path::new("/")).len() * 2);
    for kill_at in 0..total {
        let h = Home::new();
        h.seed();
        let mut n = 0;
        let killed = migrate_in(&h.0, Launchctl::Skip, &mut |_| {
            n += 1;
            if n > kill_at {
                Err(io::Error::other("killed"))
            } else {
                Ok(())
            }
        });
        assert!(killed.is_err(), "step {kill_at}");
        let state = state_dir_in(&h.0);
        assert_eq!(
            fs::read_to_string(state.join("agents.toml")).unwrap(),
            "marker",
            "step {kill_at}: {}",
            state.display()
        );
        assert_eq!(h.run(), Outcome::Migrated, "step {kill_at}");
        h.assert_done();
    }
}

/// 같은 이름의 실제 파일은 1단계 링크로 보지 않고 지우지 않는다(옮기지도 않는다).
#[test]
fn foreign_file_at_link_name_is_kept() {
    let h = Home::new();
    h.seed();
    let foreign = link_of(&h.old());
    fs::write(&foreign, "user").unwrap();
    assert!(migrate_in(&h.0, Launchctl::Skip, &mut |_| Ok(())).is_err());
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "user");
    assert_eq!(state_dir_in(&h.0), h.old());
}

/// 둘 다 실제 디렉터리면(사용자가 되살림 등) 어느 쪽도 건드리지 않는다.
#[test]
fn both_real_dirs_are_left_alone() {
    let h = Home::new();
    h.seed();
    fs::create_dir_all(h.new_dir()).unwrap();
    h.run();
    assert!(!h.old().is_symlink());
    assert!(h.old().join("agents.toml").is_file());
    assert_eq!(state_dir_in(&h.0), h.new_dir());
}

/// 옛 link `LaunchAgent`는 새 라벨 plist로 바뀌고 옛 plist는 지워진다.
#[test]
fn legacy_link_agent_plist_is_relabelled() {
    let h = Home::new();
    let agents = h.lib("LaunchAgents");
    fs::create_dir_all(&agents).unwrap();
    let old = agents.join("dev.minikrill.app.link.plist");
    fs::write(
        &old,
        include_str!("../../../../krill-link/src/launchd/link.plist")
            .replace("dev.krill.app", "dev.minikrill.app"),
    )
    .unwrap();
    assert_eq!(h.run(), Outcome::Migrated);
    assert!(!old.exists());
    assert_eq!(
        fs::read_to_string(agents.join("dev.krill.app.link.plist")).unwrap(),
        include_str!("../../../../krill-link/src/launchd/link.plist")
    );
    assert_eq!(h.run(), Outcome::Current);
}
