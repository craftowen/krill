//! Windows 설치 폴더의 파일 단위 이름 바꾸기 교체(OWE-837).
//!
//! 실행 중인 exe와 로드된 DLL은 지울 수도 쓰기로 열 수도 없다. 하지만 같은 볼륨 안에서 이름을 바꾸거나
//! 다른 폴더로 옮기는 것은 된다(VM 실측, `docs/research/windows-spike-OWE-837.md` §2). 그래서 업데이트는
//! 설치 폴더의 파일을 하나씩 `<설치>.prev`로 옮기고 새 파일을 그 자리에 놓는다. 폴더째 이름을 바꾸지 않는
//! 이유는 그 폴더가 실행 중 프로세스의 작업 디렉터리면 폴더 이름 바꾸기가 실패하기 때문이다(실측).
//! 지울 수 없는 파일(아직 도는 구 holder·krilld·OpenConsole 이미지)은 `<설치>.trash`로 옮겨 두고 다음 업데이트 때 다시 지운다.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::Path;

use super::{append_ext, write_private_file};

/// 업데이트가 설치 폴더 최상위에서 관리하는 Krill 배포 항목(`packaging/windows/bundle.ps1`).
/// 패키지에서 빠진 구 파일도 `.prev`로 치우고, 이 목록에도 새 패키지에도 없는 항목(예: `krill.exe.bak`,
/// 사용자 메모·덤프 파일)은 건드리지 않고 제자리에 남긴다.
pub(super) const KNOWN_ENTRIES: [&str; 10] = [
    "Krill.json",
    "krill-app.exe",
    "krill.exe",
    "krilld.exe",
    "krill-holder.exe",
    "krill-link.exe",
    "conpty.dll",
    "x64",
    "arm64",
    "THIRD_PARTY_NOTICES.txt",
];

/// 스테이징 폴더의 최상위 항목 이름을 읽어 온다.
pub fn list_staged_entries(staging: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(staging)? {
        let name = entry?.file_name().into_string().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "staged entry name is not valid UTF-8",
            )
        })?;
        names.push(name);
    }
    Ok(names)
}

/// 이번 교체가 스테이징에서 들여온 최상위 항목 목록을 기록한다.
/// 롤백 시 `KNOWN_ENTRIES`에 아직 없는 미래 버전의 신규 파일까지 지우되 사용자 파일은 남기기 위함이다.
pub fn save_staged_entries(path: &Path, entries: &[String]) -> io::Result<()> {
    let bytes = serde_json::to_vec(entries).map_err(io::Error::other)?;
    let tmp = append_ext(path, "tmp");
    write_private_file(&tmp, &bytes)?;
    if let Err(err) = fs::rename(&tmp, path) {
        fs::remove_file(&tmp).ok();
        return Err(err);
    }
    Ok(())
}

fn load_staged_entries(path: &Path) -> Vec<String> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn is_managed_name(name: &OsStr, staged: &[String], extra: &[OsString]) -> bool {
    KNOWN_ENTRIES.iter().any(|k| name.eq_ignore_ascii_case(k))
        || staged.iter().any(|s| name.eq_ignore_ascii_case(s))
        || extra.iter().any(|e| name.eq_ignore_ascii_case(e))
}

/// 설치 폴더 최상위에서 Krill 관리 대상(`KNOWN_ENTRIES` ∪ `staged`)만 `prev`로 옮긴다.
/// 백업본(`krill.exe.bak`)이나 사용자 파일은 건드리지 않고 설치 폴더에 그대로 둔다.
pub fn move_managed(
    install: &Path,
    staged: &[String],
    prev: &Path,
    trash: &Path,
) -> io::Result<()> {
    fs::create_dir_all(prev)?;
    for entry in fs::read_dir(install)? {
        let entry = entry?;
        let name = entry.file_name();
        if !is_managed_name(&name, staged, &[]) {
            continue;
        }
        move_entry(&entry, &prev.join(&name), trash)?;
    }
    Ok(())
}

/// `src` 아래 모든 파일을 `dst`의 같은 상대 경로로 옮긴다. `dst`에 같은 파일이 있으면 먼저 치운다.
/// 비게 된 하위 폴더는 지우되(실패 무시) `src` 자체는 남긴다(작업 디렉터리일 수 있다).
pub fn move_tree(src: &Path, dst: &Path, trash: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        move_entry(&entry, &to, trash)?;
    }
    Ok(())
}

fn move_entry(entry: &fs::DirEntry, to: &Path, trash: &Path) -> io::Result<()> {
    let from = entry.path();
    if entry.file_type()?.is_dir() {
        move_tree(&from, to, trash)?;
        fs::remove_dir(&from).ok();
    } else {
        if to.symlink_metadata().is_ok() {
            discard(to, trash)?;
        }
        fs::rename(&from, to)?;
    }
    Ok(())
}

/// 롤백 시 설치 폴더 최상위에서 Krill 관리 대상(`KNOWN_ENTRIES` ∪ `prev` ∪ `staged_entries_file`)만 치운다.
/// 관리 대상 밖의 사용자·백업 파일은 그대로 남긴다.
pub fn clear_managed(
    install: &Path,
    prev: &Path,
    staged_entries_file: &Path,
    trash: &Path,
) -> io::Result<()> {
    let entries = match fs::read_dir(install) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let staged = load_staged_entries(staged_entries_file);
    let prev_names: Vec<OsString> = fs::read_dir(prev)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !is_managed_name(&name, &staged, &prev_names) {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            clear(&path, trash)?;
            fs::remove_dir(&path).ok();
        } else {
            discard(&path, trash)?;
        }
    }
    Ok(())
}

/// `dir` 아래 파일을 모두 치운다(지우거나, 실행 중이면 `trash`로 옮긴다). `dir` 자체는 남긴다.
pub fn clear(dir: &Path, trash: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            clear(&path, trash)?;
            fs::remove_dir(&path).ok();
        } else {
            discard(&path, trash)?;
        }
    }
    Ok(())
}

/// 지울 수 있는 것만 지운다(이전 업데이트의 `trash` 정리). 실패는 무시한다.
pub fn purge(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            purge(&path);
            fs::remove_dir(&path).ok();
        } else {
            fs::remove_file(&path).ok();
        }
    }
    fs::remove_dir(dir).ok();
}

/// 파일을 지우고, 실행 중이라 지울 수 없으면 `trash/<n>-<이름>`으로 옮긴다.
fn discard(path: &Path, trash: &Path) -> io::Result<()> {
    if fs::remove_file(path).is_ok() {
        return Ok(());
    }
    fs::create_dir_all(trash)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    for n in 0..u32::MAX {
        let to = trash.join(format!("{n}-{name}"));
        if to.symlink_metadata().is_err() {
            return fs::rename(path, to);
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}
