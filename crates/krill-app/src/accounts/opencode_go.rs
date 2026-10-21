//! `OpenCode` Go API key override (Orca opencode-go-api-key-store reference, MIT).
//!
//! Priority: settings override > `/connect` auth.json > `OPENCODE_API_KEY`.
//! Krill stores the override in a 0600 file under state dir or home config.

use std::io;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "opencode-go-api-key";
pub const MAX_BYTES: u64 = 4096;

#[must_use]
pub fn path(home: &Path, state_dir: Option<&Path>) -> PathBuf {
    if let Some(state) = state_dir {
        state.join(FILE_NAME)
    } else {
        home.join(".config").join("minikrill").join(FILE_NAME)
    }
}

fn trimmed(raw: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(raw).ok()?.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

#[must_use]
pub fn read_override(home: &Path, state_dir: Option<&Path>) -> Option<String> {
    let path = path(home, state_dir);
    let file = std::fs::File::open(&path).ok()?;
    if file.metadata().ok()?.len() > MAX_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    file.take(MAX_BYTES.saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    trimmed(&buf)
}

#[must_use]
pub fn has_override(home: &Path, state_dir: Option<&Path>) -> bool {
    read_override(home, state_dir).is_some()
}

pub fn save_override(home: &Path, state_dir: Option<&Path>, key: &str) -> io::Result<()> {
    let k = key.trim();
    if k.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "api key required",
        ));
    }
    let path = path(home, state_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        opts.open(&path)?.write_all(k.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::File::create(&path)?.write_all(k.as_bytes())?;
    }
    Ok(())
}

pub fn clear_override(home: &Path, state_dir: Option<&Path>) -> io::Result<()> {
    match std::fs::remove_file(path(home, state_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
