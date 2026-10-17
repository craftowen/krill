//! MK-055: macOS Launch Services로 폴더를 외부 앱에 연다. 셸·에디터 CLI 설치에 의존하지 않는다.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    VsCode,
    Cursor,
    Zed,
    Finder,
}

impl Target {
    pub const ALL: [Self; 4] = [Self::VsCode, Self::Cursor, Self::Zed, Self::Finder];

    pub fn label(self) -> &'static str {
        match self {
            Self::VsCode => "VS Code",
            Self::Cursor => "Cursor",
            Self::Zed => "Zed",
            Self::Finder => "Finder",
        }
    }

    fn bundle(self) -> &'static str {
        match self {
            Self::VsCode => "com.microsoft.VSCode",
            Self::Cursor => "com.todesktop.230313mzl4w4u92",
            Self::Zed => "dev.zed.Zed",
            Self::Finder => "com.apple.finder",
        }
    }

    fn command(self, folder: &Path) -> Command {
        let mut cmd = Command::new("/usr/bin/open");
        cmd.args(["-b", self.bundle(), "--"]).arg(folder);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        cmd
    }

    /// 백그라운드에서 호출한다. 없는 앱·폴더와 Launch Services 오류를 호출자에게 돌려준다.
    pub fn open(self, folder: &Path) -> io::Result<()> {
        let folder = std::fs::canonicalize(folder)?;
        if !folder.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                crate::i18n::catalog::open_in::FOLDER.get(),
            ));
        }
        let out = self.command(&folder).output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                crate::i18n::catalog::open_in::FAILED.format(&[
                    ("app", self.label()),
                    ("error", String::from_utf8_lossy(&out.stderr).trim()),
                ]),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_one_literal_argument_and_targets_are_explicit() {
        let path = Path::new("/tmp/한글 'folder'; $(touch nope) -n");
        for (target, bundle) in Target::ALL.into_iter().zip([
            "com.microsoft.VSCode",
            "com.todesktop.230313mzl4w4u92",
            "dev.zed.Zed",
            "com.apple.finder",
        ]) {
            let cmd = target.command(path);
            assert_eq!(cmd.get_program(), "/usr/bin/open");
            assert_eq!(
                cmd.get_args().collect::<Vec<_>>(),
                [
                    "-b".as_ref(),
                    bundle.as_ref(),
                    "--".as_ref(),
                    path.as_os_str()
                ]
            );
        }
    }

    #[test]
    fn nonexistent_folder_does_not_launch_an_app() {
        assert_eq!(
            Target::Finder
                .open(Path::new("/mk055-does-not-exist/folder"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }
}
