//! CLI 상태 조회는 `SAVE_LOCK` 밖에서 한다. 캐시는 비밀값을 포함하지 않는다.

use krill_proto::os::ffi::OsStrExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use krill_proto::account_login::{AuthStatus, probe_with_environment};
use krill_proto::usage::{ConnectionStatus, ProviderKind};

use super::{host_default_dir, load_file};
use crate::session::lock;

type CacheKey = (ProviderKind, PathBuf);
pub(super) type Statuses = BTreeMap<CacheKey, AuthStatus>;
type Stamp = Vec<Option<SystemTime>>;
type Cached = (Instant, Stamp, Option<AuthStatus>);
static STATES: Mutex<BTreeMap<CacheKey, ConnectionStatus>> = Mutex::new(BTreeMap::new());

pub(super) fn connection(provider: ProviderKind, dir: &Path) -> ConnectionStatus {
    lock(&STATES)
        .get(&(provider, dir.to_path_buf()))
        .copied()
        .unwrap_or_default()
}

static CACHE: Mutex<BTreeMap<CacheKey, Cached>> = Mutex::new(BTreeMap::new());

/// 캐시된 로그인 상태만 돌려준다. CLI 조회(프로필마다 최대 5초)는 백그라운드 스레드가 하므로
/// 실행·재개·hook(레지스트리 잠금 안)·자동전환 경로가 CLI를 기다리지 않는다. 조회 중이면 "모름"(항목 없음)이다.
pub(super) fn statuses(home: &Path, state: Option<&Path>) -> Statuses {
    let agents = crate::agent::control::Control::new(state).agents().ok();
    let mut profiles: Vec<_> = load_file(home, state)
        .accounts
        .into_iter()
        .filter_map(|a| {
            Some((
                ProviderKind::parse(&a.provider)?,
                PathBuf::from(a.profile_dir),
                false,
            ))
        })
        .collect();
    for provider in ProviderKind::ALL {
        profiles.push((provider, host_default_dir(home, provider), true));
    }
    let mut found = BTreeMap::new();
    for (provider, dir, host) in profiles {
        let key = (provider, dir.clone());
        let Some(exe) = executable(provider, agents.as_ref()) else {
            lock(&STATES).insert(key, ConnectionStatus::NotInstalled);
            continue;
        };
        // 삭제된 사용자 프로필을 상태 CLI가 다시 만들지 않게 한다.
        if !host && !dir.is_dir() {
            lock(&STATES).insert(key, ConnectionStatus::CheckFailed);
            continue;
        }
        let mut stamp: Stamp = [
            "antigravity-oauth-token",
            ".claude.json",
            "auth.json",
            ".credentials.json",
            ".krill-login-completed",
            ".gemini/antigravity-cli/antigravity-oauth-token",
            "data/opencode/auth.json",
        ]
        .iter()
        .map(|name| {
            std::fs::metadata(dir.join(name))
                .and_then(|m| m.modified())
                .ok()
        })
        .collect();
        stamp.push(std::fs::metadata(&exe).and_then(|m| m.modified()).ok());
        stamp.push(state.and_then(|s| {
            std::fs::metadata(s.join(krill_proto::agent::FILE_NAME))
                .and_then(|m| m.modified())
                .ok()
        }));
        let cached = lock(&CACHE).get(&key).cloned();
        let status = match cached {
            Some((at, prior, status)) if prior == stamp => {
                if at.elapsed() >= Duration::from_secs(30) {
                    spawn_probe(
                        exe,
                        &key,
                        stamp,
                        host,
                        home.to_path_buf(),
                        state.map(Path::to_path_buf),
                    );
                }
                status
            }
            _ if provider == ProviderKind::Antigravity => {
                let status = super::super::direct::antigravity_auth(&dir, false);
                if !host || status.as_ref().is_some_and(|s| s.logged_in) {
                    lock(&STATES).insert(
                        key.clone(),
                        status.as_ref().map_or(ConnectionStatus::CheckFailed, |s| {
                            if s.logged_in {
                                ConnectionStatus::Connected
                            } else {
                                ConnectionStatus::NeedsLogin
                            }
                        }),
                    );
                    lock(&CACHE).insert(key.clone(), (Instant::now(), stamp, status.clone()));
                    if !host {
                        super::super::provision::provision_if_enabled(
                            home,
                            state,
                            provider,
                            &dir.to_string_lossy(),
                        );
                    }
                    status
                } else {
                    lock(&STATES).insert(key.clone(), ConnectionStatus::Checking);
                    spawn_probe(
                        exe,
                        &key,
                        stamp,
                        host,
                        home.to_path_buf(),
                        state.map(Path::to_path_buf),
                    );
                    None
                }
            }
            // 처음이거나 파일이 바뀌었다(로그인·로그아웃): 새 결과가 올 때까지 모름.
            _ => {
                lock(&STATES).insert(key.clone(), ConnectionStatus::Checking);
                spawn_probe(
                    exe,
                    &key,
                    stamp,
                    host,
                    home.to_path_buf(),
                    state.map(Path::to_path_buf),
                );
                None
            }
        };
        if let Some(status) = status {
            found.insert((provider, dir), status);
        }
    }
    found
}

/// 같은 프로필은 한 번에 하나만 조회한다.
static PROBING: Mutex<BTreeSet<CacheKey>> = Mutex::new(BTreeSet::new());

fn spawn_probe(
    exe: PathBuf,
    key: &CacheKey,
    stamp: Stamp,
    host: bool,
    home: PathBuf,
    state: Option<PathBuf>,
) {
    if !lock(&PROBING).insert(key.clone()) {
        return;
    }
    let job = key.clone();
    let spawned = std::thread::Builder::new()
        .name("account-auth".into())
        .spawn(move || {
            let (provider, dir) = &job;
            // 호스트 기본 계정은 env 없이 묻는다(Claude는 env가 있으면 다른 키체인 항목을 본다).
            let status = std::panic::catch_unwind(|| {
                if *provider == ProviderKind::Antigravity {
                    return super::super::direct::antigravity_auth(dir, host);
                }
                let path = std::ffi::OsStr::from_bytes(crate::shell::login_path());
                let mut status = probe_with_environment(
                    &exe,
                    *provider,
                    (!host).then_some(dir.as_path()),
                    Some(&home),
                    Some(path),
                )?;
                if status.logged_in && *provider == ProviderKind::Codex {
                    status.identity = super::super::direct::read_file(&dir.join("auth.json"))
                        .and_then(|raw| super::identity::from_credentials(*provider, &raw));
                }
                Some(status)
            })
            .ok()
            .flatten();
            lock(&STATES).insert(
                job.clone(),
                status.as_ref().map_or(ConnectionStatus::CheckFailed, |s| {
                    if s.logged_in {
                        ConnectionStatus::Connected
                    } else {
                        ConnectionStatus::NeedsLogin
                    }
                }),
            );
            let mut cache = lock(&CACHE);
            cache.retain(|_, (at, _, _)| at.elapsed() < Duration::from_secs(300));
            cache.insert(job.clone(), (Instant::now(), stamp, status));
            drop(cache);
            // 기록 이행은 별도 작업으로 보낸다. 사용 중이던 프로필도 종료 후 다음 조회에서 재시도한다.
            if !host
                && matches!(
                    provider,
                    ProviderKind::Claude | ProviderKind::Codex | ProviderKind::Antigravity
                )
            {
                super::super::provision::provision_if_enabled(
                    &home,
                    state.as_deref(),
                    *provider,
                    &dir.to_string_lossy(),
                );
            }
            lock(&PROBING).remove(&job);
        });
    if let Err(error) = spawned {
        lock(&STATES).insert(key.clone(), ConnectionStatus::CheckFailed);
        tracing::warn!(%error, "account auth probe thread failed to start");
        lock(&PROBING).remove(key);
    }
}

#[cfg(not(test))]
fn executable(
    provider: ProviderKind,
    agents: Option<&krill_proto::agent::Agents>,
) -> Option<PathBuf> {
    if cfg!(debug_assertions) && std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return None;
    }
    let path = std::ffi::OsStr::from_bytes(crate::shell::login_path());
    let program = agents
        .and_then(|a| a.resolve(provider.as_str()))
        .and_then(|(_, profile)| profile.program())
        .unwrap_or(provider.as_str());
    krill_proto::agent::which(program, path)
}

// 단위 테스트에서는 실제 CLI/네트워크/사용자 자격증명을 조회하지 않는다.
// fake CLI의 명령·환경·응답 검증은 krill_proto::account_login::tests에서 수행한다.
#[cfg(test)]
fn executable(_: ProviderKind, _: Option<&krill_proto::agent::Agents>) -> Option<PathBuf> {
    FAKE_EXE.with(|exe| exe.borrow().clone())
}

#[cfg(test)]
thread_local! {
    /// 테스트가 지정한 가짜 CLI(기본 없음 = 조회하지 않음).
    static FAKE_EXE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[test]
    fn agy_profiles_refresh_native_emails_after_login_without_cross_profile_fallback() {
        use super::super::{
            AccountsFile, RefreshTrigger, StoredAccount, refresh_accounts_authenticated, save_file,
        };
        let root = std::env::temp_dir().join(format!("agy-native-identity-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state = root.join("state");
        let one = home.join("profile-one");
        let two = home.join("profile-two");
        for dir in [&one, &two] {
            std::fs::create_dir_all(dir.join(".gemini/antigravity-cli")).unwrap();
        }
        let file = AccountsFile {
            accounts: [(&one, "agy-one"), (&two, "agy-two")]
                .into_iter()
                .map(|(dir, id)| StoredAccount {
                    id: id.into(),
                    provider: "agy".into(),
                    label: id.into(),
                    email: Some("previous@example.test".into()),
                    profile_dir: dir.display().to_string(),
                })
                .collect(),
            provisioning: false,
            ..AccountsFile::default()
        };
        save_file(&home, Some(&state), &file).unwrap();
        // agy identity reads no CLI. Other providers may fail their isolated fake probes.
        FAKE_EXE.with(|e| *e.borrow_mut() = Some(root.join("unused-cli")));
        let emails = || {
            let auth = statuses(&home, Some(&state));
            let snapshot = refresh_accounts_authenticated(
                &home,
                Some(&state),
                RefreshTrigger::ReadOnly,
                &auth,
            );
            snapshot
                .accounts
                .into_iter()
                .filter(|a| !a.host_default && a.provider == ProviderKind::Antigravity)
                .map(|a| (a.id, a.email))
                .collect::<BTreeMap<_, _>>()
        };
        let token_path = |dir: &Path| dir.join(".gemini/antigravity-cli/antigravity-oauth-token");
        let write_token = |dir: &Path, email: &str| {
            std::fs::write(
                token_path(dir),
                serde_json::to_vec(&serde_json::json!({
                    "auth_method":"consumer", "id_token_email":email,
                    "token":{"access_token":"synthetic-access"}
                }))
                .unwrap(),
            )
            .unwrap();
        };
        // Host credentials must never fill an unauthenticated isolated profile.
        let host = host_default_dir(&home, ProviderKind::Antigravity);
        std::fs::create_dir_all(&host).unwrap();
        std::fs::write(
            host.join("antigravity-oauth-token"),
            br#"{"auth_method":"consumer","token":{"access_token":"host"},"id_token_email":"host@example.test"}"#,
        )
        .unwrap();
        assert!(emails().values().all(Option::is_none));
        write_token(&one, "one@example.test");
        write_token(&two, "two@example.test");
        assert_eq!(
            emails(),
            BTreeMap::from([
                ("agy-one".into(), Some("one@example.test".into())),
                ("agy-two".into(), Some("two@example.test".into()))
            ])
        );
        // A completed login invalidates the cache even if token mtime is preserved.
        let modified = std::fs::metadata(token_path(&one))
            .unwrap()
            .modified()
            .unwrap();
        write_token(&one, "new@example.test");
        std::fs::File::options()
            .write(true)
            .open(token_path(&one))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        std::fs::write(one.join(".krill-login-completed"), "new login").unwrap();
        std::fs::remove_file(token_path(&two)).unwrap();
        assert_eq!(
            emails(),
            BTreeMap::from([
                ("agy-one".into(), Some("new@example.test".into())),
                ("agy-two".into(), None)
            ])
        );
        // A token file alone still indicates login, but cannot revive saved identity.
        std::fs::write(token_path(&one), b"unreadable-identity").unwrap();
        assert!(emails().values().all(Option::is_none));
        FAKE_EXE.with(|e| *e.borrow_mut() = None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_probe_is_check_failed_and_login_marker_invalidates_cache() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("owe719-cache-{}", std::process::id()));
        let home = root.join("home");
        let state = root.join("state");
        let dir = home.join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(statuses(&home, Some(&state)).is_empty());
        assert_eq!(
            connection(ProviderKind::Claude, &dir),
            ConnectionStatus::NotInstalled
        );
        let exe = root.join("fake-cli");
        let write = |script: &str| {
            std::fs::write(&exe, script).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        write("#!/bin/sh\necho 'authentication timed out' >&2\nexit 1\n");
        FAKE_EXE.with(|e| *e.borrow_mut() = Some(exe.clone()));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            statuses(&home, Some(&state));
            if connection(ProviderKind::Claude, &dir) == ConnectionStatus::CheckFailed {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        write("#!/bin/sh\necho '{\"loggedIn\":true}'\n");
        std::fs::write(dir.join(".krill-login-completed"), "new").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let found = statuses(&home, Some(&state));
            if found
                .get(&(ProviderKind::Claude, dir.clone()))
                .is_some_and(|s| s.logged_in)
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        // 마커가 띄운 백그라운드 재조회가 잠깐 Checking으로 보일 수 있다(check.sh flaky, 2026-10-19).
        while connection(ProviderKind::Claude, &dir) != ConnectionStatus::Connected {
            assert!(
                Instant::now() < deadline,
                "connection never became Connected"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // cli.log 등 비자격증명 파일 생성으로 디렉터리 mtime이 바뀌어도 Connected 캐시가 Checking으로 흔들리지 않아야 한다.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("cli.log"), "runtime log churn\n").unwrap();
        let after_log = statuses(&home, Some(&state));
        assert!(
            after_log
                .get(&(ProviderKind::Claude, dir.clone()))
                .is_some_and(|s| s.logged_in)
        );
        assert_eq!(
            connection(ProviderKind::Claude, &dir),
            ConnectionStatus::Connected
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 느린 CLI가 실행·hook 경로를 막지 않고(캐시만 읽음), 호스트 기본 계정은 env 없이 조회한다.
    #[test]
    fn slow_cli_never_blocks_and_host_probe_drops_config_env() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("owe700-bg-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let exe = root.join("claude");
        std::fs::write(
            &exe,
            "#!/bin/sh\nsleep 1\n[ -z \"${CLAUDE_CONFIG_DIR+x}\" ] || { echo '{\"loggedIn\":false}'; exit 0; }\necho '{\"loggedIn\":true}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        FAKE_EXE.with(|e| *e.borrow_mut() = Some(exe));
        let state = root.join("state");
        let started = Instant::now();
        assert!(statuses(&home, Some(&state)).is_empty(), "조회 중에는 모름");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "CLI를 기다리면 안 된다"
        );
        let key = (ProviderKind::Claude, home.join(".claude"));
        let deadline = Instant::now() + Duration::from_secs(10);
        let found = loop {
            let found = statuses(&home, Some(&state));
            if found.contains_key(&key) || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(found.get(&key).is_some_and(|s| s.logged_in), "{found:?}");
        std::fs::remove_dir_all(root).ok();
    }
}
