//! ⌘P 빠른 열기의 파일 목록. git 저장소면 `git ls-files`(추적 + .gitignore를 존중한 미추적), 아니면 폴더를 훑는다.
//! 검색·팔레트는 Jump 팔레트([`crate::jump`])를 그대로 쓴다.

use krill_proto::os::ffi::OsStrExt as _;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use krill_proto::SessionId;

use crate::jump::Item;

/// 목록 상한. 거대한 저장소에서도 팔레트 메모리·검색 시간이 묶이게 한다.
pub const MAX_FILES: usize = 50_000;

const SCAN_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_LIST_BYTES: u64 = 8 * 1024 * 1024;
static SCANNING: AtomicBool = AtomicBool::new(false);
struct ScanPermit;
impl Drop for ScanPermit {
    fn drop(&mut self) {
        SCANNING.store(false, Ordering::Release);
    }
}

/// `root` 아래 파일의 상대 경로. Concurrent callers never queue blocking workers.
pub fn list(root: &Path) -> Vec<PathBuf> {
    let deadline = Instant::now() + SCAN_TIMEOUT;
    loop {
        if let Some(files) = try_list(root) {
            return files;
        }
        if Instant::now() >= deadline {
            return Vec::new();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// None means an existing scan still owns admission; the UI retries asynchronously.
pub fn try_list(root: &Path) -> Option<Vec<PathBuf>> {
    SCANNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    let _permit = ScanPermit;
    let deadline = Instant::now() + SCAN_TIMEOUT;
    Some(git_files(root, deadline).unwrap_or_else(|| walk(root, deadline)))
}

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn command_output(mut command: Command, deadline: Instant) -> std::io::Result<Vec<u8>> {
    let mut child = Reap(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("missing git stdout"))?;
    let reader = crate::process_io::read(stdout, MAX_LIST_BYTES + 1, deadline)?;
    loop {
        if Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                return Err(std::io::Error::other("git ls-files failed"));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = reader.finish(deadline)?;
    if output.timed_out {
        return Err(std::io::ErrorKind::TimedOut.into());
    }
    if u64::try_from(output.bytes.len()).unwrap_or(u64::MAX) > MAX_LIST_BYTES {
        return Err(std::io::Error::other("file list too large"));
    }
    Ok(output.bytes)
}

fn git_files(root: &Path, deadline: Instant) -> Option<Vec<PathBuf>> {
    let mut command = crate::platform::background_command("git");
    command
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]);
    let output = command_output(command, deadline).ok()?;
    let mut files = output
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .take(MAX_FILES)
        .map(|path| PathBuf::from(OsStr::from_bytes(path)))
        .collect::<Vec<_>>();
    files.dedup();
    files.retain(|path| Instant::now() < deadline && root.join(path).is_file());
    Some(files)
}

/// git이 아닌 폴더: 숨김 항목과 흔한 빌드 산출물 폴더를 건너뛰고 너비 우선으로 훑는다.
// ponytail: .gitignore 없는 폴더라 무시 규칙은 이름 목록뿐이다. 필요하면 .ignore 파일 해석을 더한다.
fn walk(root: &Path, deadline: Instant) -> Vec<PathBuf> {
    const SKIP: [&str; 3] = ["node_modules", "target", "dist"];
    let mut files = Vec::new();
    let mut queue = std::collections::VecDeque::from([PathBuf::new()]);
    while let Some(rel) = queue.pop_front() {
        if Instant::now() >= deadline {
            break;
        }
        let Ok(entries) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for entry in entries.flatten() {
            if Instant::now() >= deadline {
                break;
            }
            let name = entry.file_name();
            if name.as_bytes().starts_with(b".") || SKIP.iter().any(|s| name == **s) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = rel.join(&name);
            if kind.is_dir() && queue.len() < MAX_FILES {
                queue.push_back(path);
            } else if kind.is_file() {
                files.push(path);
                if files.len() >= MAX_FILES {
                    return files;
                }
            }
        }
    }
    files.sort();
    files
}

/// Jump 팔레트 항목. 항목 id는 `files`의 인덱스다.
pub fn items(files: &[PathBuf]) -> Vec<Item> {
    files
        .iter()
        .zip(0u64..)
        .map(|(p, i)| {
            let mut item = Item::new(SessionId(i), p.display().to_string(), false);
            item.is_file = true;
            item
        })
        .collect()
}

/// 팔레트가 고른 항목의 파일.
pub fn chosen(files: &[PathBuf], id: SessionId) -> Option<&PathBuf> {
    usize::try_from(id.0).ok().and_then(|i| files.get(i))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn stalled_git_and_inherited_pipe_have_deadlines() {
        for script in ["sleep 1", "(sleep 1) & exit 0"] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let started = Instant::now();
            let error = command_output(command, started + Duration::from_millis(100)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
    #[test]
    fn busy_scan_does_not_start_another_process() {
        while SCANNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let _permit = ScanPermit;
        assert!(try_list(Path::new("/does-not-exist")).is_none());
    }
}
