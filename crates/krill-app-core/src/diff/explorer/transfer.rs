//! 로컬 탐색기 이동·가져오기. 덮어쓰기·원본 심볼릭 링크 추적·실패 시 원본 삭제를 하지 않는다.
#[cfg(test)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
        let destination = crate::platform::explorer_transfer::transfer(
            &self.repo,
            source,
            directory,
            import,
            super::MAX_FILTER_ENTRIES,
        )?;
        if !import {
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
            Self(path.canonicalize().unwrap())
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
