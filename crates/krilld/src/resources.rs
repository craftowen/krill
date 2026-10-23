//! Resource Manager 측정(MK-071, [`krill_proto::resources`]). 메모리는 커널 `phys_footprint`를 그대로 읽는다.
//! 세션 값은 루트와 ppid 계보상 후손의 합이다(요청마다 전체 프로세스를 한 번 훑는다).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::session::lock;

use krill_proto::SessionId;
use krill_proto::resources::{Resources, SessionUsage};

use crate::sys;

/// 상태 디렉터리를 내려가는 최대 깊이(세그먼트는 `sessions/<id>.noindex/` 아래라 얕다).
const MAX_DEPTH: u8 = 8;

/// `sessions`: 범위 안 세션과 돌고 있으면 루트 pid. `holder`: holder에 연결돼 있는지.
pub(crate) fn measure(
    client: Option<u32>,
    holder: bool,
    sessions: &[(SessionId, Option<u32>)],
    state: Option<&Path>,
) -> Resources {
    let me = std::process::id();
    let parents = if sessions.iter().any(|(_, pid)| pid.is_some()) {
        sys::all_pids()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| Some((p, sys::parent_pid(p).ok()?)))
            .collect()
    } else {
        Vec::new()
    };
    let children = children(&parents);
    let sessions = sessions
        .iter()
        .map(|&(session, root)| {
            let tree = root.map(|r| tree(r, &children)).unwrap_or_default();
            let sizes: Vec<u64> = tree
                .iter()
                .filter_map(|&p| sys::footprint(p).ok())
                .collect();
            SessionUsage {
                session,
                footprint: sizes.iter().fold(0, |a, &b| a.saturating_add(b)),
                processes: u32::try_from(sizes.len()).unwrap_or(u32::MAX),
            }
        })
        .collect();
    let holder = holder
        .then(|| sys::parent_pid(me).ok())
        .flatten()
        .filter(|&p| p > 1)
        .and_then(|p| sys::footprint(p).ok());
    Resources {
        client: client.and_then(|p| sys::footprint(p).ok()),
        daemon: sys::footprint(me).unwrap_or(0),
        holder,
        disk: state.map_or(0, cached_disk_usage),
        sessions,
    }
}

/// Disk walks have their own cadence. Each key is a daemon state directory, never a peer scope.
/// Only the disk total is cached; process footprints remain live for every request.
fn cached_disk_usage(dir: &Path) -> u64 {
    type Samples = HashMap<PathBuf, (Instant, u64)>;
    static DISK: OnceLock<Mutex<Samples>> = OnceLock::new();
    let cache = DISK.get_or_init(Mutex::default);
    if let Some((at, bytes)) = lock(cache).get(dir)
        && at.elapsed() < Duration::from_secs(30)
    {
        return *bytes;
    }
    let bytes = disk_usage(dir, MAX_DEPTH);
    let mut samples = lock(cache);
    samples.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(30));
    samples.insert(dir.to_owned(), (Instant::now(), bytes));
    bytes
}

fn children(parents: &[(u32, u32)]) -> HashMap<u32, Vec<u32>> {
    let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in parents {
        if pid != ppid {
            map.entry(ppid).or_default().push(pid);
        }
    }
    map
}

/// `root`와 그 후손. ppid 그래프는 트리지만 PID 재사용 경합에 대비해 방문한 PID는 다시 넣지 않는다.
fn tree(root: u32, children: &HashMap<u32, Vec<u32>>) -> Vec<u32> {
    let mut out = vec![root];
    let mut visited = HashSet::from([root]);
    let mut i = 0;
    while let Some(&pid) = out.get(i) {
        for &c in children.get(&pid).into_iter().flatten() {
            if visited.insert(c) {
                out.push(c);
            }
        }
        i += 1;
    }
    out
}

/// 할당 블록 기준 디스크 사용량. 심볼릭 링크는 따라가지 않는다.
fn disk_usage(dir: &Path, depth: u8) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|e| Some((fs::symlink_metadata(e.path()).ok()?, e.path())))
        .map(|(m, path)| {
            let own = crate::os::allocated(&m);
            if m.is_dir() && depth > 0 {
                own.saturating_add(disk_usage(&path, depth - 1))
            } else {
                own
            }
        })
        .fold(0, u64::saturating_add)
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn tree_follows_ppid_without_cycles() {
        let parents = [(10, 1), (11, 10), (12, 11), (13, 10), (20, 1), (11, 12)];
        let mut t = tree(10, &children(&parents));
        t.sort_unstable();
        assert_eq!(t, vec![10, 11, 12, 13]);
        assert_eq!(tree(99, &children(&parents)), vec![99]);
    }

    #[test]
    fn disk_usage_counts_nested_files() {
        let dir = std::env::temp_dir().join(format!("mk071-disk-{}", std::process::id()));
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("a/b/f"), vec![1u8; 100_000]).unwrap();
        let used = disk_usage(&dir, MAX_DEPTH);
        assert!((100_000..200_000).contains(&used), "{used}");
        assert!(disk_usage(&dir, 0) < 100_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// 완료 조건(표시값이 `footprint(1)` ±10%): 같은 순간 자식 트리를 두 도구로 잰다.
    #[test]
    fn session_footprint_matches_footprint_tool() {
        if !Path::new("/usr/bin/footprint").exists() {
            return;
        }
        // 셸 + 자식 sleep 하나. 셸은 sleep을 기다리며 남는다.
        let mut root = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 30; :"])
            .spawn()
            .unwrap();
        let pid = root.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let r = measure(Some(pid), false, &[(SessionId(1), Some(pid))], None);
        let usage = r.sessions[0];
        assert_eq!(usage.processes, 2, "sh + sleep");
        let mut tool = 0;
        for p in tree(pid, &children(&parents_now())) {
            tool += footprint_tool(p);
            let p = rustix::process::Pid::from_raw(i32::try_from(p).unwrap()).unwrap();
            rustix::process::kill_process(p, rustix::process::Signal::KILL).ok();
        }
        root.wait().ok();
        let diff = usage.footprint.abs_diff(tool);
        assert!(
            diff * 10 <= tool,
            "measured {} vs footprint(1) {tool}",
            usage.footprint
        );
        assert!(r.daemon > 0);
        assert_eq!(r.holder, None);
    }

    fn parents_now() -> Vec<(u32, u32)> {
        sys::all_pids()
            .unwrap()
            .into_iter()
            .filter_map(|p| Some((p, sys::parent_pid(p).ok()?)))
            .collect()
    }

    /// `footprint -f bytes <pid>` 첫 줄의 `Footprint: N B`.
    fn footprint_tool(pid: u32) -> u64 {
        let out = Command::new("/usr/bin/footprint")
            .args(["-f", "bytes", "--noCategories", &pid.to_string()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let n = text
            .split("Footprint:")
            .nth(1)
            .and_then(|t| t.split_whitespace().next())
            .unwrap_or_else(|| panic!("footprint output: {text}"));
        n.parse().unwrap()
    }
}
