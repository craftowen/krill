//! Capture the CLI's browser callback without treating the OAuth URL as shell source.

use std::io;
use std::path::{Path, PathBuf};

pub(super) fn write_helper(temp: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    let (name, script) = (
        "browser.cmd",
        // SET writes the value without expanding URL metacharacters a second time.
        // Disable delayed expansion so `!` in OAuth state also survives.
        "@echo off\r\nsetlocal DisableDelayedExpansion\r\nset \"KRILL_LOGIN_CAPTURE_URL=%~1\"\r\n> \"%KRILL_LOGIN_URL_FILE%.tmp\" set KRILL_LOGIN_CAPTURE_URL\r\nmove /y \"%KRILL_LOGIN_URL_FILE%.tmp\" \"%KRILL_LOGIN_URL_FILE%\" >nul\r\n",
    );
    #[cfg(not(windows))]
    let (name, script) = (
        "browser",
        "#!/bin/sh\nfor arg do\ncase \"$arg\" in https://*) printf '%s' \"$arg\" > \"$KRILL_LOGIN_URL_FILE.tmp\" && mv \"$KRILL_LOGIN_URL_FILE.tmp\" \"$KRILL_LOGIN_URL_FILE\";; esac\ndone\n",
    );
    let browser = temp.join(name);
    std::fs::write(&browser, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(browser)
}

pub(super) fn read_link(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let text = text.trim();
    Some(
        text.strip_prefix("KRILL_LOGIN_CAPTURE_URL=")
            .unwrap_or(text)
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_callback_keeps_the_complete_url_in_a_path_with_spaces() {
        let root = std::env::temp_dir().join(format!("krill browser 한글 {}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let helper = write_helper(&root).unwrap();
        let link = root.join("authorize url");
        let url = "https://auth.openai.com/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback&state=a!b%25c&code_challenge=d-e_f";
        let status = crate::platform::background_command(&helper)
            .arg(url)
            .env("KRILL_LOGIN_URL_FILE", &link)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(read_link(&link).as_deref(), Some(url));
        std::fs::remove_dir_all(root).unwrap();
    }
}
