//! 낡은 로그인 세션에 갇힌 holder 교체. macOS에서 loginwindow가 다시 떠 `gui/<uid>` 도메인이 새로 생기면, 세션 생존
//! 설계로 계속 사는 옛 holder와 그 자식(krilld·세션 셸)은 죽은 bootstrap 포트를 쥔 채 남는다. 그 안에서는
//! `trustd` 같은 시스템 서비스에 닿지 못해 Go 바이너리(`gh`·계정 로그인 등)가 `x509: OSStatus -26276`으로 TLS 검증에
//! 실패한다(2026-11-20, curl은 정상). 앱 재시작으로는 holder가 안 바뀌므로 풀리지 않는다.
//!
//! krilld는 holder의 자식이라 같은 포트를 물려받는다. `launchctl managername`이 거절되면 포트가 죽은 것이다.
//! 두 번 연속 거절되면 holder를 죽여 "holder만 크래시" 경로(ARCHITECTURE §4.1)로 보낸다: 세션은 krilld가 쥐고 살고,
//! 클라이언트가 현재 로그인 세션에서 새 holder를 띄워 재결합한다. 새 세션부터는 정상 컨텍스트에서 spawn된다
//! (이미 떠 있는 세션 셸 안의 프로세스는 옛 컨텍스트 그대로다). 재기동 루프를 막으려 5분 쿨다운을 둔다.

use std::sync::Arc;

use crate::registry::Registry;

#[cfg(target_os = "macos")]
pub fn start(registry: &Arc<Registry>) {
    use std::time::Duration;

    if registry.holder().is_none() || krill_proto::daemon::state_dir_isolated() {
        return;
    }
    let (Some(dir), Some(holder)) = (
        krill_proto::daemon::state_dir(),
        crate::rejoin::holder_pid(),
    ) else {
        return;
    };
    let registry = Arc::downgrade(registry);
    std::thread::Builder::new()
        .name("login-guard".to_owned())
        .stack_size(256 * 1024)
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(10));
                let holder_alive = || {
                    registry.upgrade().is_some_and(|reg| {
                        reg.holder()
                            .is_some_and(crate::holder::Holder::is_connected)
                    }) && crate::rejoin::holder_pid() == Some(holder)
                };
                if replace_stale_holder_with(
                    &dir,
                    holder,
                    unix_seconds(),
                    Duration::from_secs(2),
                    holder_alive,
                    krill_proto::os::bootstrap_dead,
                    kill,
                ) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(50));
            }
        })
        .ok();
}

#[cfg(not(target_os = "macos"))]
pub fn start(_registry: &Arc<Registry>) {}

#[cfg(any(target_os = "macos", test))]
const COOLDOWN: u64 = 5 * 60;

/// 포트가 죽었음이 두 번 연속 확인되고 쿨다운이 지났으면 holder를 죽이고 `true`.
/// holder가 이미 떠났거나 연결이 끊겼으면 죽이지 않고 감시를 끝낸다(`true`).
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "production calls replace_stale_holder_with")
)]
fn replace_stale_holder(
    dir: &std::path::Path,
    holder: u32,
    now: u64,
    holder_alive: impl FnMut() -> bool,
    dead: impl FnMut() -> bool,
    kill: impl FnOnce(u32),
) -> bool {
    replace_stale_holder_with(
        dir,
        holder,
        now,
        std::time::Duration::ZERO,
        holder_alive,
        dead,
        kill,
    )
}

#[cfg(any(target_os = "macos", test))]
fn replace_stale_holder_with(
    dir: &std::path::Path,
    holder: u32,
    now: u64,
    recheck_delay: std::time::Duration,
    mut holder_alive: impl FnMut() -> bool,
    mut dead: impl FnMut() -> bool,
    kill: impl FnOnce(u32),
) -> bool {
    if !holder_alive() {
        return true;
    }
    if !dead() {
        return false;
    }
    if !recheck_delay.is_zero() {
        std::thread::sleep(recheck_delay);
    }
    if !dead() {
        return false;
    }
    if !holder_alive() {
        return true;
    }
    let path = dir.join("login-guard-restart");
    let last = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok());
    // 시계가 뒤로 움직여도 루프를 막는다. 기록에 실패하면 죽이지 않는다(기록 없이는 쿨다운을 못 지킨다).
    if last.is_some_and(|l| now.saturating_sub(l) < COOLDOWN)
        || krill_proto::os::write_atomic_0600(&path, now.to_string().as_bytes()).is_err()
    {
        return false;
    }
    if !holder_alive() {
        return true;
    }
    tracing::warn!(holder, "stale login session; replacing holder");
    kill(holder);
    true
}

#[cfg(target_os = "macos")]
fn kill(pid: u32) {
    if let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        rustix::process::kill_process(pid, rustix::process::Signal::KILL).ok();
    }
}

#[cfg(target_os = "macos")]
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("login-guard-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn healthy_or_single_failure_never_kills() {
        let d = dir("healthy");
        let mut killed = None;
        assert!(!replace_stale_holder(
            &d,
            7,
            1,
            || true,
            || false,
            |p| killed = Some(p)
        ));
        let mut calls = 0;
        // 첫 판정만 거절이고 재확인은 통과: 일시 오류로 보고 죽이지 않는다.
        assert!(!replace_stale_holder(
            &d,
            7,
            1,
            || true,
            || {
                calls += 1;
                calls == 1
            },
            |p| killed = Some(p)
        ));
        assert_eq!(killed, None);
        assert!(!d.join("login-guard-restart").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn dead_port_kills_once_per_cooldown() {
        let d = dir("dead");
        let mut killed = Vec::new();
        assert!(replace_stale_holder(
            &d,
            7,
            10_000,
            || true,
            || true,
            |p| killed.push(p)
        ));
        // 새 holder의 krilld도 거절돼도 쿨다운 안에서는 다시 죽이지 않는다(재기동 루프 방지).
        assert!(!replace_stale_holder(
            &d,
            8,
            10_001,
            || true,
            || true,
            |p| killed.push(p)
        ));
        assert!(!replace_stale_holder(
            &d,
            8,
            9_000,
            || true,
            || true,
            |p| killed.push(p)
        ));
        assert!(replace_stale_holder(
            &d,
            8,
            10_000 + COOLDOWN,
            || true,
            || true,
            |p| killed.push(p)
        ));
        assert_eq!(killed, [7, 8]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn gone_holder_exits_without_killing_stale_pid() {
        let d = dir("gone");
        let mut killed = Vec::new();
        let mut dead_calls = 0;
        // Holder already gone before bootstrap check: stop immediately without probing or killing.
        assert!(replace_stale_holder(
            &d,
            9,
            10_000,
            || false,
            || {
                dead_calls += 1;
                true
            },
            |p| killed.push(p)
        ));
        assert_eq!(dead_calls, 0);
        assert!(killed.is_empty());
        assert!(!d.join("login-guard-restart").exists());

        // Holder disappears while bootstrap_dead is running: stop without cooldown write or kill.
        let mut checks = 0;
        assert!(replace_stale_holder(
            &d,
            9,
            10_000,
            || {
                checks += 1;
                checks == 1
            },
            || true,
            |p| killed.push(p)
        ));
        assert!(killed.is_empty());
        assert!(!d.join("login-guard-restart").exists());

        // Holder disappears right before kill: stop without sending SIGKILL to a recycled PID.
        let mut checks = 0;
        assert!(replace_stale_holder(
            &d,
            9,
            10_000,
            || {
                checks += 1;
                checks <= 2
            },
            || true,
            |p| killed.push(p)
        ));
        assert!(killed.is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }
}
