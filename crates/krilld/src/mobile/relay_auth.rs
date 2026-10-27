//! Hosted relay account sessions. Secrets stay in owner-only storage and never enter IPC replies.
use super::devices::{read_private, write_private};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

const FILE: &str = "mobile-relay-session.json";

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Session {
    pub token: String,
    pub expires_at_ms: u64,
    pub email: String,
    pub origin: String,
}
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}
impl Session {
    pub fn valid(&self, origin: &str, now: u64) -> bool {
        self.origin == origin
            && self.expires_at_ms > now
            && !self.token.is_empty()
            && self.token.len() <= 4096
            && self.token.bytes().all(|b| b.is_ascii_graphic())
    }
    pub fn load(dir: Option<&Path>, origin: &str, now: u64) -> io::Result<Option<Self>> {
        let Some(dir) = dir else { return Ok(None) };
        let Some(bytes) = read_private(&dir.join(FILE))? else {
            return Ok(None);
        };
        let session: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if session.valid(origin, now) {
            Ok(Some(session))
        } else {
            Self::remove(Some(dir))?;
            Ok(None)
        }
    }
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_private(
            &dir.join(FILE),
            &serde_json::to_vec(self).map_err(io::Error::other)?,
        )
    }
    pub fn remove(dir: Option<&Path>) -> io::Result<()> {
        let Some(path) = dir.map(|d| d.join(FILE)) else {
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

#[derive(Default)]
pub(super) struct Account {
    pub session: Option<Session>,
    pub pending: Option<Pending>,
    pub dir: Option<PathBuf>,
    pub generation: u64,
}
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("session", &self.session)
            .field("pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}
#[derive(Clone)]
pub(super) struct Pending {
    pub code: String,
    pub verifier: String,
    pub next_poll_at_ms: u64,
    pub expires_at_ms: u64,
}

/// Curl's config arrives over stdin; neither account secrets nor PKCE verifiers appear in ps.
pub(super) fn post(
    origin: &str,
    path: &str,
    body: &serde_json::Value,
    token: Option<&str>,
) -> io::Result<(u16, serde_json::Value)> {
    use std::fmt::Write as _;
    use std::io::{Read as _, Write as _};
    use std::process::{Command, Stdio};
    let url = format!("{origin}{path}");
    let parsed = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://127.0.0.1:"))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "relay origin must use HTTPS")
        })?;
    if parsed.is_empty() || origin.bytes().any(|b| b.is_ascii_control()) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let quote = |s: &str| serde_json::to_string(s).map_err(io::Error::other);
    let mut config = format!(
        "url = {}\nrequest = \"POST\"\nheader = \"Content-Type: application/json\"\ndata = {}\n",
        quote(&url)?,
        quote(&body.to_string())?
    );
    if let Some(token) = token {
        writeln!(
            config,
            "header = {}",
            quote(&format!("x-krill-session: {token}"))?
        )
        .map_err(io::Error::other)?;
    }
    let mut command = Command::new("curl");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--max-filesize",
            "65536",
            "--write-out",
            "\n%{http_code}",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let sent = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("relay request unavailable"))?
        .write_all(config.as_bytes());
    if let Err(error) = sent {
        child.kill().ok();
        child.wait().ok();
        return Err(error);
    }
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("relay response unavailable"))?
        .take(65537)
        .read_to_end(&mut bytes);
    if bytes.len() > 65536 || read.is_err() {
        child.kill().ok();
        child.wait().ok();
        return Err(io::Error::other("invalid relay response"));
    }
    if !child.wait()?.success() {
        return Err(io::Error::other("relay request failed"));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| io::Error::other("invalid relay response"))?;
    let (json, status) = text
        .rsplit_once('\n')
        .ok_or_else(|| io::Error::other("invalid relay response"))?;
    let status = status
        .parse()
        .map_err(|_| io::Error::other("invalid relay status"))?;
    let value = if json.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(json).map_err(|_| io::Error::other("invalid relay JSON"))?
    };
    Ok((status, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_is_bound_to_origin_and_expiry_and_redacts_secret() {
        let s = Session {
            token: "opaque-secret".into(),
            expires_at_ms: 100,
            email: "a@example.test".into(),
            origin: "https://relay.test".into(),
        };
        assert!(s.valid(&s.origin, 99));
        assert!(!s.valid(&s.origin, 100));
        assert!(!s.valid("https://other.test", 0));
        assert!(!format!("{s:?}").contains(&s.token));
        assert!(
            !Session {
                token: "bad\r\nheader".into(),
                ..s
            }
            .valid("https://relay.test", 0)
        );
    }
    #[test]
    fn persisted_session_is_private_and_logout_removes_it() {
        let dir = std::env::temp_dir().join(format!(
            "krill-relay-auth-{}",
            super::super::devices::hex(&super::super::e2ee::random().unwrap())
        ));
        let s = Session {
            token: "opaque".into(),
            expires_at_ms: u64::MAX,
            email: String::new(),
            origin: "https://relay.test".into(),
        };
        s.save(&dir).unwrap();
        assert!(Session::load(Some(&dir), &s.origin, 0).unwrap().is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.join(FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        Session::remove(Some(&dir)).unwrap();
        assert!(Session::load(Some(&dir), &s.origin, 0).unwrap().is_none());
        std::fs::remove_dir(dir).unwrap();
    }
}
