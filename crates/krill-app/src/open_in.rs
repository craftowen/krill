//! MK-055: 폴더를 외부 앱에 연다. 셸·에디터 CLI 설치에 의존하지 않는다 — macOS는 Launch Services(`open -b`),
//! Windows는 `ShellExecuteW`로 탐색기나 에디터 URL 처리기(`vscode://file/…`)를 부른다(OWE-834).

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
            Self::Finder if cfg!(windows) => "File Explorer",
            Self::Finder => "Finder",
        }
    }

    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "macOS Launch Services 전용")
    )]
    fn bundle(self) -> &'static str {
        match self {
            Self::VsCode => "com.microsoft.VSCode",
            Self::Cursor => "com.todesktop.230313mzl4w4u92",
            Self::Zed => "dev.zed.Zed",
            Self::Finder => "com.apple.finder",
        }
    }

    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "macOS Launch Services 전용")
    )]
    fn command(self, folder: &Path) -> Command {
        let mut cmd = crate::platform::background_command("/usr/bin/open");
        cmd.args(["-b", self.bundle(), "--"]).arg(folder);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        cmd
    }

    /// 백그라운드에서 호출한다. 없는 앱·폴더와 Launch Services 오류를 호출자에게 돌려준다.
    pub fn open(self, folder: &Path) -> io::Result<()> {
        if !std::fs::canonicalize(folder)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                crate::i18n::catalog::open_in::FOLDER.get(),
            ));
        }
        self.open_path(folder)
    }

    /// 폴더나 파일을 연다(파일 탐색기 우클릭 메뉴). 백그라운드에서 호출한다.
    pub fn open_path(self, path: &Path) -> io::Result<()> {
        let path = std::fs::canonicalize(path)?;
        #[cfg(windows)]
        return crate::platform::shell_open(self.windows_target(&path).as_ref())
            .map_err(|e| self.failed(&e.to_string()));
        #[cfg(not(windows))]
        {
            if !cfg!(target_os = "macos") {
                return Err(io::ErrorKind::Unsupported.into());
            }
            let out = self.command(&path).output()?;
            if out.status.success() {
                Ok(())
            } else {
                Err(self.failed(String::from_utf8_lossy(&out.stderr).trim()))
            }
        }
    }

    fn failed(self, error: &str) -> io::Error {
        io::Error::other(
            crate::i18n::catalog::open_in::FAILED
                .format(&[("app", self.label()), ("error", error)]),
        )
    }

    /// `ShellExecuteW`에 넘길 대상. 탐색기는 경로 그대로, 에디터는 각자 등록한 URL 처리기(`<scheme>://file/<경로>`)다.
    #[cfg(windows)]
    fn windows_target(self, path: &Path) -> String {
        let path = path.to_string_lossy();
        // `canonicalize`가 붙이는 `\\?\` 접두사는 탐색기·에디터가 읽지 못한다.
        let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
        match self {
            Self::VsCode => editor_url("vscode", path),
            Self::Cursor => editor_url("cursor", path),
            Self::Zed => editor_url("zed", path),
            Self::Finder => path.to_owned(),
        }
    }
}

/// 에디터 URL(`vscode://file/C:/a%20b`). 경로 구분자는 `/`로, 예약 문자·공백·비 ASCII는 퍼센트 인코딩한다.
#[cfg(any(windows, test))]
fn editor_url(scheme: &str, path: &str) -> String {
    use std::fmt::Write as _;
    let mut url = format!("{scheme}://file/");
    for b in path.replace('\\', "/").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            url.push(char::from(b));
        } else {
            write!(url, "%{b:02X}").ok();
        }
    }
    url
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
    fn editor_urls_use_forward_slashes_and_escape_everything_else() {
        assert_eq!(
            editor_url("vscode", r"C:\Users\오웬\my proj#1"),
            "vscode://file/C:/Users/%EC%98%A4%EC%9B%AC/my%20proj%231"
        );
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
