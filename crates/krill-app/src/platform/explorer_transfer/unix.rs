use super::{Component, File, OsStr, OsString, Path, PathBuf, denied, io, relative};
use rustix::fs::{self, Mode, OFlags};
use std::os::unix::fs::MetadataExt as _;

pub(super) struct Directory {
    pub path: PathBuf,
    ancestors: Vec<File>,
    file: File,
}

fn conflict(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("Explorer transfer namespace conflict: {error}"))
}

fn verify_identity(expected: &File, current: &File) -> io::Result<()> {
    let expected = expected.metadata()?;
    let current = current.metadata()?;
    if expected.dev() != current.dev() || expected.ino() != current.ino() {
        return Err(conflict("selected entry changed"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn system_path(path: &Path) -> io::Result<PathBuf> {
    // macOS exposes these immutable OS anchors to Finder and temp_dir. Map
    // only their exact documented link targets, then traverse strictly below.
    for (alias, expected) in [("/var", "private/var"), ("/tmp", "private/tmp")] {
        if let Ok(tail) = path.strip_prefix(alias) {
            if std::fs::read_link(alias)? != Path::new(expected) {
                return Err(denied());
            }
            return Ok(Path::new("/").join(expected).join(tail));
        }
    }
    Ok(path.to_path_buf())
}

impl Directory {
    pub fn absolute(path: &Path) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        let mapped = system_path(path)?;
        #[cfg(target_os = "macos")]
        let path = mapped.as_path();
        if !path.is_absolute() {
            return Err(denied());
        }
        let file = fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        let root = Self {
            path: PathBuf::from("/"),
            ancestors: Vec::new(),
            file,
        };
        // No canonicalize here: every intermediate component must be opened NOFOLLOW.
        let tail: PathBuf = path
            .components()
            .filter(|c| !matches!(c, Component::RootDir))
            .collect();
        root.descend(&relative(&tail)?)
    }
    pub fn descend(&self, parts: &[OsString]) -> io::Result<Self> {
        let mut dir = Self {
            path: self.path.clone(),
            ancestors: self
                .ancestors
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<_>>()?,
            file: self.file.try_clone()?,
        };
        for name in parts {
            let file = fs::openat(
                &dir.file,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into();
            dir.ancestors.push(dir.file);
            dir.file = file;
            dir.path.push(name);
        }
        Ok(dir)
    }
    /// Reopen the expected absolute path component by component, never following links.
    /// Pinned fds retain authority after a rename; they do not prove current location.
    pub fn verify_position(&self) -> io::Result<()> {
        let current = Self::absolute(&self.path).map_err(conflict)?;
        if current.ancestors.len() != self.ancestors.len() {
            return Err(conflict("directory ancestry changed"));
        }
        for (expected, current) in self
            .ancestors
            .iter()
            .chain(std::iter::once(&self.file))
            .zip(
                current
                    .ancestors
                    .iter()
                    .chain(std::iter::once(&current.file)),
            )
        {
            verify_identity(expected, current)?;
        }
        Ok(())
    }
    pub fn verify_node(&self, name: &OsStr, node: &File) -> io::Result<()> {
        self.verify_position()?;
        let current = self.open_node(name, false).map_err(conflict)?;
        verify_identity(node, &current)
    }
    pub fn open_node(&self, name: &OsStr, _: bool) -> io::Result<File> {
        self.verify_position()?;
        // NONBLOCK prevents special files such as FIFOs from hanging the worker.
        let file: File = fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        let meta = file.metadata()?;
        if !meta.is_file() && !meta.is_dir() {
            return Err(denied());
        }
        Ok(file)
    }
    pub fn contains(&self, node: &File, _: &Path) -> io::Result<bool> {
        let source = node.metadata()?;
        for file in self.ancestors.iter().chain(std::iter::once(&self.file)) {
            let meta = file.metadata()?;
            if source.dev() == meta.dev() && source.ino() == meta.ino() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn child(&self, name: &OsStr, file: File) -> io::Result<Self> {
        let mut ancestors = self
            .ancestors
            .iter()
            .map(File::try_clone)
            .collect::<io::Result<Vec<_>>>()?;
        ancestors.push(self.file.try_clone()?);
        Ok(Self {
            path: self.path.join(name),
            ancestors,
            file,
        })
    }
    pub fn names(&self) -> io::Result<impl Iterator<Item = io::Result<OsString>>> {
        use std::os::unix::ffi::OsStrExt as _;
        self.verify_position()?;
        Ok(
            fs::Dir::read_from(&self.file)?.filter_map(|entry| match entry {
                Ok(entry) if matches!(entry.file_name().to_bytes(), b"." | b"..") => None,
                Ok(entry) => Some(Ok(
                    OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string()
                )),
                Err(error) => Some(Err(error.into())),
            }),
        )
    }
    pub fn create_file(&self, name: &OsStr) -> io::Result<File> {
        self.verify_position()?;
        Ok(fs::openat(
            &self.file,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?
        .into())
    }
    pub fn create_dir(&self, name: &OsStr) -> io::Result<Self> {
        self.verify_position()?;
        fs::mkdirat(&self.file, name, Mode::RWXU)?;
        let child = self.descend(&[name.to_os_string()])?;
        child.verify_position()?;
        Ok(child)
    }
    pub fn move_new(
        source: &Self,
        name: &OsStr,
        node: &File,
        target: &Self,
        target_name: &OsStr,
    ) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        let flags = fs::RenameFlags::NOREPLACE;
        #[cfg(target_os = "linux")]
        let flags = fs::RenameFlags::NOREPLACE;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            source.verify_node(name, node)?;
            target.verify_position()?;
            // POSIX cannot bind this name-based syscall to the opened inode or freeze
            // ancestor names. A same-user rename can still race these last checks.
            fs::renameat_with(&source.file, name, &target.file, target_name, flags)?;
            // Never roll back by name: concurrent entries and the saved original must
            // survive. Report a possible completed destination if observation differs.
            source
                .verify_position()
                .and_then(|()| target.verify_node(target_name, node))
                .map_err(|error| {
                    io::Error::other(format!(
                        "{error}; move may have completed at {}; no rollback attempted",
                        target.path.join(target_name).display()
                    ))
                })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (source, name, node, target, target_name);
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn only_exact_system_anchor_components_are_remapped() {
        assert_eq!(
            system_path(Path::new("/var/folders/file")).unwrap(),
            Path::new("/private/var/folders/file")
        );
        assert_eq!(
            system_path(Path::new("/tmp/file")).unwrap(),
            Path::new("/private/tmp/file")
        );
        assert_eq!(
            system_path(Path::new("/variable/file")).unwrap(),
            Path::new("/variable/file")
        );
        assert_eq!(
            system_path(Path::new("/tmp-other/file")).unwrap(),
            Path::new("/tmp-other/file")
        );
    }
}
