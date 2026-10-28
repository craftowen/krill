//! 로컬 탐색기 이동·가져오기. 덮어쓰기·원본 심볼릭 링크 추적·실패 시 원본 삭제를 하지 않는다.
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use super::{Explorer, Operation};

#[derive(Clone)]
pub(super) struct Drag {
    pub repo: PathBuf,
    pub paths: Vec<PathBuf>,
}

impl gpui::Render for Drag {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        let p = crate::ui::palette(cx);
        crate::ui::primitives::card(p)
            .font_family(crate::ui::FONT_MONO)
            .text_size(crate::ui::TEXT_SM)
            .text_color(p.foreground)
            .child(
                self.paths
                    .iter()
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
    }
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "invalid Explorer transfer path",
    )
}

fn within(root: &Path, path: &Path) -> io::Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| denied())?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(name) if name != ".git"))
    {
        return Err(denied());
    }
    if !path.canonicalize()?.starts_with(root.canonicalize()?) {
        return Err(denied());
    }
    Ok(())
}

/// 독점 생성으로 복사하며 기존 대상과 원본을 보존한다.
/// 부분 가져오기는 남겨 두고 오류에 대상 경로를 알린다.
fn copy_new(source: &Path, destination: &Path, budget: &mut usize, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other("Explorer import depth limit exceeded"));
    }
    *budget = budget
        .checked_sub(1)
        .ok_or_else(|| io::Error::other("Explorer import entry limit exceeded"))?;
    let meta = source.symlink_metadata()?;
    if meta.is_symlink() {
        return Err(denied());
    }
    if meta.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                return Err(denied());
            }
            copy_new(
                &entry.path(),
                &destination.join(entry.file_name()),
                budget,
                depth + 1,
            )?;
        }
    } else if meta.is_file() {
        let mut input = fs::File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        io::copy(&mut input, &mut output)?;
    } else {
        return Err(denied());
    }
    Ok(())
}

impl Explorer {
    pub(super) fn transfer_path(
        &mut self,
        source: &Path,
        directory: &Path,
        import: bool,
    ) -> io::Result<PathBuf> {
        if self.remote_target().is_some() {
            return Err(io::ErrorKind::Unsupported.into());
        }
        within(&self.repo, directory)?;
        if !directory.is_dir() {
            return Err(denied());
        }
        let name = source
            .file_name()
            .filter(|name| *name != ".git")
            .ok_or_else(denied)?;
        let destination = directory.join(name);
        // OS 드롭을 자신의 자식에 복사하면 새 대상을 다시 읽으며 재귀하므로 거부한다.
        if directory
            .canonicalize()?
            .starts_with(source.canonicalize()?)
        {
            return Err(denied());
        }
        if !import && destination == source {
            return Ok(destination);
        }
        if destination.symlink_metadata().is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if import {
            let mut budget = super::MAX_FILTER_ENTRIES;
            copy_new(source, &destination, &mut budget, 0).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "import failed: {error}; partial destination may remain at {}",
                        destination.display()
                    ),
                )
            })?;
        } else {
            within(&self.repo, source)?;
            if source == self.repo || source.symlink_metadata()?.is_symlink() {
                return Err(denied());
            }
            // 다른 볼륨으로 rename이 실패해도 원본을 보존한다. 복사 후 삭제하지 않는다.
            fs::rename(source, &destination)?;
            self.expanded.retain(|path| !path.starts_with(source));
        }
        self.expanded.insert(directory.to_path_buf());
        self.refresh();
        self.selected = Some(destination.clone());
        Ok(destination)
    }
}

impl super::Panel {
    pub(super) fn queue_transfer(
        &mut self,
        paths: &[PathBuf],
        directory: &Path,
        import: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.explorer.remote_target().is_some() {
            return;
        }
        self.queue_explorer_operation(
            Operation::Transfer {
                sources: paths.to_vec(),
                directory: directory.to_path_buf(),
                import,
            },
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "krill-transfer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }
    #[test]
    fn transfer_moves_without_overwrite_and_import_preserves_source() {
        let dir = Temp::new();
        let root = dir.path();
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(root.join("a"), "original").unwrap();
        let mut explorer = Explorer::new(root.to_path_buf());
        explorer
            .transfer_path(&root.join("a"), &root.join("folder"), false)
            .unwrap();
        assert!(!root.join("a").exists());
        assert_eq!(
            fs::read_to_string(root.join("folder/a")).unwrap(),
            "original"
        );
        fs::write(root.join("a"), "other").unwrap();
        assert_eq!(
            explorer
                .transfer_path(&root.join("a"), &root.join("folder"), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "other");
        let external = Temp::new();
        fs::write(external.path().join("new"), "imported").unwrap();
        explorer
            .transfer_path(&external.path().join("new"), root, true)
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("new")).unwrap(), "imported");
        assert!(external.path().join("new").exists());
        assert!(
            explorer
                .transfer_path(&root.join("folder"), &root.join("folder"), false)
                .is_err()
        );
        assert!(
            explorer
                .transfer_path(&root.join("a"), external.path(), false)
                .is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn transfer_rejects_symlink_escape_and_remote_owner() {
        let root = Temp::new();
        let external = Temp::new();
        std::os::unix::fs::symlink(external.path(), root.path().join("escape")).unwrap();
        fs::write(root.path().join("a"), "safe").unwrap();
        let mut explorer = Explorer::new(root.path().to_path_buf());
        assert!(
            explorer
                .transfer_path(&root.path().join("a"), &root.path().join("escape"), false)
                .is_err()
        );
        explorer.connect(crate::hosts::HostTarget::remote(
            "remote",
            root.path().join("sock"),
        ));
        assert_eq!(
            explorer
                .transfer_path(&root.path().join("a"), root.path(), true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(root.path().join("a").exists());
    }
}
