//! Windows 설치 폴더의 파일 단위 이름 바꾸기 교체(OWE-837).
//!
//! 실행 중인 exe와 로드된 DLL은 지울 수도 쓰기로 열 수도 없다. 하지만 같은 볼륨 안에서 이름을 바꾸거나
//! 다른 폴더로 옮기는 것은 된다(VM 실측, `docs/research/windows-spike-OWE-837.md` §2). 그래서 업데이트는
//! 설치 폴더의 파일을 하나씩 `<설치>.prev`로 옮기고 새 파일을 그 자리에 놓는다. 폴더째 이름을 바꾸지 않는
//! 이유는 그 폴더가 실행 중 프로세스의 작업 디렉터리면 폴더 이름 바꾸기가 실패하기 때문이다(실측).
//! 지울 수 없는 파일(아직 도는 구 holder·krilld·OpenConsole 이미지)은 `<설치>.trash`로 옮겨 두고 다음 업데이트 때 다시 지운다.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

/// 업데이트가 설치 폴더 최상위에서 옮겨도 되는 이름. 패키지에서 빠진 파일도 지우지 말고 남겨 둔다.
/// 이 목록에도 새 패키지에도 없는 항목이 있으면 사용자 파일일 수 있어 교체를 거부한다([`check_install_entries`]).
const KNOWN_ENTRIES: [&str; 9] = [
    "Krill.json",
    "krill-app.exe",
    "krill.exe",
    "krilld.exe",
    "krill-holder.exe",
    "krill-link.exe",
    "conpty.dll",
    "x64",
    "THIRD_PARTY_NOTICES.txt",
];

/// 설치 폴더 최상위 항목이 모두 Krill 파일(알려진 이름이거나 새 패키지에 있는 이름)인지 확인한다.
/// 사용자가 exe를 다른 파일과 같은 폴더(예: 바탕 화면)에 두었을 때 그 파일들을 `.prev`로 옮기지 않기 위함이다.
pub fn check_install_entries(install: &Path, staging: &Path) -> io::Result<()> {
    for entry in fs::read_dir(install)? {
        let name = entry?.file_name();
        let known = KNOWN_ENTRIES.iter().any(|k| OsStr::new(k) == name);
        if !known && !staging.join(&name).exists() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unexpected entry in install dir: {}",
                    Path::new(&name).display()
                ),
            ));
        }
    }
    Ok(())
}

/// `src` 아래 모든 파일을 `dst`의 같은 상대 경로로 옮긴다. `dst`에 같은 파일이 있으면 먼저 치운다.
/// 비게 된 하위 폴더는 지우되(실패 무시) `src` 자체는 남긴다(작업 디렉터리일 수 있다).
pub fn move_tree(src: &Path, dst: &Path, trash: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            move_tree(&from, &to, trash)?;
            fs::remove_dir(&from).ok();
        } else {
            if to.symlink_metadata().is_ok() {
                discard(&to, trash)?;
            }
            fs::rename(&from, &to)?;
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
