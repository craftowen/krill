//! Claude 설정과 확장은 `~/.claude`를 그대로 참조한다. 계정에 남는 것은 로그인과 런타임 상태뿐이다.
//! 구조는 Orca `claude-profile-provisioning.ts`(MIT)를 참고했다(코드 복사 없음). Orca는 파일을 복사하지만
//! Claude Code 2.1.314가 링크를 유지한 채 원본에 저장하는 것을 확인해 파일도 링크로 둔다.
//! 프로필에만 있던 내용은 기본 폴더에 없는 것만 옮기고 원본 트리는 백업에 보존한다.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use krill_proto::hook_install::store;
use serde_json::{Map, Value};

use super::claude_history::BACKUP_NAME;
use super::history_store::{
    archive_root, make_archive, migration_lock, real_dir, replace_with_link, unused_path,
};
use super::{EXCLUDED, HISTORY, MEMORY_IMPORT, Report, publish};

/// 기본 폴더에 없어도 만들어 첫 설치부터 프로필 전용 폴더가 생기지 않게 한다.
const EXTENSIONS: &[&str] = &["skills", "commands", "agents", "plugins", "rules"];
/// 링크로 공유하는 파일과 기본 폴더에 없을 때의 초기 내용.
const FILES: &[(&str, Option<&str>)] =
    &[("settings.json", Some("{}\n")), ("keybindings.json", None)];
/// 프로필에서 켠 플러그인이 공유 뒤에도 켜져 있도록 함께 옮기는 설정 키.
const PLUGIN_SETTINGS: &[&str] = &["enabledPlugins", "extraKnownMarketplaces"];

pub(super) fn share(host: &Path, profile: &Path, report: &mut Report) -> io::Result<()> {
    real_dir(host)?;
    let archive = archive_root(host, profile, BACKUP_NAME);
    let mut first_error = None;
    let mut note = |result: io::Result<()>| {
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    };
    for name in directories(host)? {
        note(share_directory(host, profile, &archive, &name, report));
    }
    for &(name, initial) in FILES {
        note(share_file(host, profile, &archive, name, initial, report));
    }
    note(share_memory(host, profile, &archive, report));
    first_error.map_or(Ok(()), Err)
}

fn directories(host: &Path) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for &name in EXTENSIONS {
        if !host.join(name).is_dir() {
            real_dir(&host.join(name))?;
        }
        names.push(OsString::from(name));
    }
    for entry in fs::read_dir(host)? {
        let entry = entry?;
        let name = entry.file_name();
        if shared_directory(&name) && !names.contains(&name) && entry.path().is_dir() {
            names.push(name);
        }
    }
    Ok(names)
}

fn shared_directory(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        !name.starts_with('.')
            && !name.starts_with("daemon")
            && !name.starts_with("agent-memory")
            && !EXCLUDED.contains(&name)
            && !HISTORY.contains(&name)
    })
}

fn label(name: &OsStr) -> &'static str {
    EXTENSIONS
        .iter()
        .copied()
        .find(|known| OsStr::new(known) == name)
        .unwrap_or("directory")
}

fn same_entry(link: &Path, target: &Path) -> bool {
    fs::read_link(link).is_ok_and(|path| path == target)
        || link
            .canonicalize()
            .ok()
            .zip(target.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

fn share_directory(
    host: &Path,
    profile: &Path,
    archive: &Path,
    name: &OsStr,
    report: &mut Report,
) -> io::Result<()> {
    let source = profile.join(name);
    let target = host.join(name);
    match source.symlink_metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::os::unix::fs::symlink(&target, &source)?;
            report.changed.push(label(name));
        }
        Err(error) => return Err(error),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if !same_entry(&source, &target) {
                report.conflicts.push(label(name));
            }
        }
        Ok(metadata) if metadata.is_dir() => {
            absorb(host, &source, &target, &archive.join(name))?;
            report.changed.push(label(name));
        }
        Ok(_) => report.conflicts.push(label(name)),
    }
    Ok(())
}

/// 없는 항목만 기본 폴더로 게시하고 프로필 폴더를 링크로 교체한다. 사용 중인 세션의 경로도 그대로 풀린다.
fn absorb(host: &Path, source: &Path, target: &Path, backup: &Path) -> io::Result<()> {
    let _lock = migration_lock(host)?;
    let plugins = source.file_name() == Some(OsStr::new("plugins"));
    if plugins {
        import_plugins(source, target)?;
    }
    copy_missing(source, target, Path::new(""), plugins, 0)?;
    make_archive(host, backup)?;
    let original = unused_path(&backup.join("original"));
    replace_with_link(target, source, &original)?;
    copy_missing(&original, target, Path::new(""), plugins, 0)
}

/// 마켓플레이스 저장소와 플러그인 버전은 완결된 트리다. 같은 항목이 있으면 파일별로 섞지 않는다.
fn whole_tree(relative: &Path) -> bool {
    let first = relative.iter().next().and_then(OsStr::to_str);
    let depth = relative.iter().count();
    (first == Some("marketplaces") && depth == 2) || (first == Some("cache") && depth == 4)
}

fn copy_missing(
    source: &Path,
    target: &Path,
    relative: &Path,
    plugins: bool,
    depth: usize,
) -> io::Result<()> {
    if depth > 64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let metadata = source.symlink_metadata()?;
    if metadata.is_dir() {
        if depth > 0 {
            if plugins && whole_tree(relative) && target.symlink_metadata().is_ok() {
                return Ok(());
            }
            match real_dir(target) {
                Ok(()) => {}
                // 기본 폴더의 다른 종류 항목이 우선한다. 프로필 항목은 백업에 남는다.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_missing(
                &entry.path(),
                &target.join(&name),
                &relative.join(&name),
                plugins,
                depth + 1,
            )?;
        }
        return Ok(());
    }
    let result = if metadata.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, target)
    } else if metadata.is_file() {
        match fs::hard_link(source, target) {
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                if target.symlink_metadata().is_ok() {
                    return Ok(());
                }
                fs::copy(source, target).map(drop)
            }
            result => result,
        }
    } else {
        return Ok(());
    };
    match result {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => Err(error),
        _ => Ok(()),
    }
}

/// 프로필에만 설치한 플러그인·마켓플레이스 항목을 기본 등록부에 더한다. 기본 폴더 항목이 우선한다.
fn import_plugins(source: &Path, target: &Path) -> io::Result<()> {
    for (name, section) in [
        ("installed_plugins.json", Some("plugins")),
        ("known_marketplaces.json", None),
    ] {
        import_entries(
            &source.join(name),
            &target.join(name),
            section,
            Some((source, target)),
        )?;
    }
    Ok(())
}

fn read_object(path: &Path) -> io::Result<Option<(String, Map<String, Value>)>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match serde_json::from_str(&text)? {
        Value::Object(object) => Ok(Some((text, object))),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

fn rewrite_paths(value: &mut Value, from: &Path, to: &Path) {
    match value {
        Value::String(text) => {
            if let Ok(rest) = Path::new(text.as_str()).strip_prefix(from) {
                *text = to.join(rest).to_string_lossy().into_owned();
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| rewrite_paths(item, from, to)),
        Value::Object(object) => object
            .values_mut()
            .for_each(|item| rewrite_paths(item, from, to)),
        _ => {}
    }
}

/// `section` 안(없으면 최상위)에서 기본 파일에 없는 키만 더한다.
fn import_entries(
    source: &Path,
    target: &Path,
    section: Option<&str>,
    relocate: Option<(&Path, &Path)>,
) -> io::Result<()> {
    let Some((_, incoming)) = read_object(source)? else {
        return Ok(());
    };
    let current = read_object(target)?;
    let mut root = match &current {
        Some((_, object)) => object.clone(),
        None if section.is_some() => incoming.clone(),
        None => Map::new(),
    };
    let entries = match section {
        Some(name) => match incoming.get(name) {
            Some(Value::Object(entries)) => entries.clone(),
            _ => return Ok(()),
        },
        None => incoming,
    };
    if current.is_none()
        && let Some(name) = section
    {
        root.insert(name.to_owned(), Value::Object(Map::new()));
    }
    let slot = match section {
        Some(name) => {
            match root
                .entry(name)
                .or_insert_with(|| Value::Object(Map::new()))
            {
                Value::Object(object) => object,
                _ => return Err(io::ErrorKind::InvalidData.into()),
            }
        }
        None => &mut root,
    };
    let mut changed = false;
    for (key, mut value) in entries {
        if slot.contains_key(&key) {
            continue;
        }
        if let Some((from, to)) = relocate {
            rewrite_paths(&mut value, from, to);
        }
        slot.insert(key, value);
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    let mut next = serde_json::to_string_pretty(&Value::Object(root))?;
    next.push('\n');
    let path = resolved(target)?;
    store::replace(
        &path,
        current.as_ref().map(|(text, _)| text.as_str()),
        Some(&next),
    )
}

/// 사용자가 링크로 관리하는 설정도 링크를 깨지 않고 실제 파일을 고친다.
fn resolved(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
            Ok(path
                .parent()
                .ok_or(io::ErrorKind::InvalidInput)?
                .canonicalize()?
                .join(name))
        }
        Err(error) => Err(error),
    }
}

fn share_file(
    host: &Path,
    profile: &Path,
    archive: &Path,
    name: &'static str,
    initial: Option<&str>,
    report: &mut Report,
) -> io::Result<()> {
    let source = host.join(name);
    let destination = profile.join(name);
    match destination.symlink_metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !seed(&source, initial)? {
                return Ok(());
            }
            std::os::unix::fs::symlink(&source, &destination)?;
        }
        Err(error) => return Err(error),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if !same_entry(&destination, &source) {
                report.conflicts.push(name);
            }
            return Ok(());
        }
        Ok(metadata) if metadata.is_file() => {
            if source.symlink_metadata().is_ok() {
                if name == "settings.json" {
                    for key in PLUGIN_SETTINGS {
                        import_entries(&destination, &source, Some(key), None)?;
                    }
                }
            } else {
                fs::hard_link(&destination, &source)?;
            }
            preserve(host, archive, name, &destination)?;
            let staged = unused_path(&destination.with_file_name(format!(".{name}.krill-link")));
            std::os::unix::fs::symlink(&source, &staged)?;
            if let Err(error) = fs::rename(&staged, &destination) {
                fs::remove_file(&staged).ok();
                return Err(error);
            }
        }
        Ok(_) => {
            report.conflicts.push(name);
            return Ok(());
        }
    }
    report.changed.push(name);
    Ok(())
}

/// 기본 파일이 있으면 `true`. 없고 초기 내용이 정해져 있으면 만든다.
fn seed(source: &Path, initial: Option<&str>) -> io::Result<bool> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if source.symlink_metadata().is_ok() {
        return Ok(true);
    }
    let Some(initial) = initial else {
        return Ok(false);
    };
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(source)
    {
        Ok(mut file) => file.write_all(initial.as_bytes())?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    Ok(true)
}

fn preserve(host: &Path, archive: &Path, name: &str, file: &Path) -> io::Result<()> {
    let backup = archive.join(name);
    make_archive(host, &backup)?;
    fs::hard_link(file, unused_path(&backup.join("original")))
}

/// 전역 메모리는 사본이 아니라 import로 따라간다. 부모 폴더 메모리와 이중 로드되지 않는다.
fn share_memory(
    host: &Path,
    profile: &Path,
    archive: &Path,
    report: &mut Report,
) -> io::Result<()> {
    if !host.join("CLAUDE.md").is_file() {
        return Ok(());
    }
    let destination = profile.join("CLAUDE.md");
    match destination.symlink_metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => publish(
            &destination,
            |tmp| fs::write(tmp, MEMORY_IMPORT),
            "CLAUDE.md",
            report,
        ),
        Err(error) => Err(error),
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if fs::read(&destination)? == MEMORY_IMPORT.as_bytes() {
                return Ok(());
            }
            preserve(host, archive, "CLAUDE.md", &destination)?;
            let staged = unused_path(&destination.with_file_name(".CLAUDE.md.krill-import"));
            fs::write(&staged, MEMORY_IMPORT)?;
            if let Err(error) = fs::rename(&staged, &destination) {
                fs::remove_file(&staged).ok();
                return Err(error);
            }
            report.changed.push("CLAUDE.md");
            Ok(())
        }
        Ok(_) => {
            report.conflicts.push("CLAUDE.md");
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "claude_config_tests.rs"]
mod tests;
