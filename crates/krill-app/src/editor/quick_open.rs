//! ⌘P 빠른 열기의 파일 목록. git 저장소면 `git ls-files`(추적 + .gitignore를 존중한 미추적), 아니면 폴더를 훑는다.
//! 검색·팔레트는 Jump 팔레트([`crate::jump`])를 그대로 쓴다.

use krill_proto::os::ffi::OsStrExt as _;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use krill_proto::SessionId;

use crate::jump::Item;

/// 목록 상한. 거대한 저장소에서도 팔레트 메모리·검색 시간이 묶이게 한다.
pub const MAX_FILES: usize = 50_000;

/// `root` 아래 파일의 상대 경로.
pub fn list(root: &Path) -> Vec<PathBuf> {
    git_files(root).unwrap_or_else(|| walk(root))
}

fn git_files(root: &Path) -> Option<Vec<PathBuf>> {
    let out = crate::platform::background_command("git")
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let mut files: Vec<PathBuf> = out
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(OsStr::from_bytes(p)))
        .collect();
    // 지운 뒤 아직 커밋하지 않은 추적 파일은 열 수 없으니 뺀다(병합 충돌 파일은 단계마다 한 번씩 나온다).
    files.dedup();
    files.retain(|p| root.join(p).is_file());
    files.truncate(MAX_FILES);
    Some(files)
}

/// git이 아닌 폴더: 숨김 항목과 흔한 빌드 산출물 폴더를 건너뛰고 너비 우선으로 훑는다.
// ponytail: .gitignore 없는 폴더라 무시 규칙은 이름 목록뿐이다. 필요하면 .ignore 파일 해석을 더한다.
fn walk(root: &Path) -> Vec<PathBuf> {
    const SKIP: [&str; 3] = ["node_modules", "target", "dist"];
    let mut files = Vec::new();
    let mut queue = std::collections::VecDeque::from([PathBuf::new()]);
    while let Some(rel) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.as_bytes().starts_with(b".") || SKIP.iter().any(|s| name == **s) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = rel.join(&name);
            if kind.is_dir() {
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
        .map(|(p, i)| Item::new(SessionId(i), p.display().to_string(), false))
        .collect()
}

/// 팔레트가 고른 항목의 파일.
pub fn chosen(files: &[PathBuf], id: SessionId) -> Option<&PathBuf> {
    usize::try_from(id.0).ok().and_then(|i| files.get(i))
}
