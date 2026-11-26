// Adapted from Orca:
// - `src/main/antigravity/native-credential-backend.ts`
// - `src/main/antigravity/native-file-credentials.ts`
// - `src/main/antigravity/native-macos-credentials.ts`
// - `src/main/antigravity/native-linux-credentials.ts` (including e71aed39a KDE fallback)
// - `src/main/antigravity/native-windows-credentials.ts`
// - `src/main/antigravity/agy-credential-presence.ts`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Native OS credential backends for Antigravity (`agy`).

use std::collections::BTreeMap;
use std::io;
#[cfg(any(target_os = "macos", target_os = "linux", all(test, unix)))]
use std::io::{Read as _, Write as _};
#[cfg(all(target_os = "macos", not(test)))]
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "linux", all(test, unix)))]
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
#[cfg(any(target_os = "macos", target_os = "linux", all(test, unix)))]
use std::time::Instant;

use zeroize::Zeroizing;

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    all(target_os = "windows", not(test))
))]
use super::codec::decode_antigravity_keychain_value;
#[cfg(target_os = "macos")]
use super::codec::encode_antigravity_keychain_value;
use super::codec::{
    AntigravityNativeCredential, MAX_CREDENTIAL_BYTES, parse_antigravity_native_credential,
};
use crate::usage::accounts::write_atomic_0600;

pub(crate) const CONFLICT_MSG: &str =
    "The native Antigravity credential changed during selection; refresh before retrying.";
const KEYRING_FALLBACK_MSG: &str = "Antigravity has a keyring fallback marker; Krill cannot verify which credential store agy will use.";
const FILE_READ_ERR: &str =
    "The Antigravity execution-host credential file could not be read safely.";

#[cfg(target_os = "macos")]
const MACOS_SECURITY_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(target_os = "linux")]
const LINUX_KEYRING_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(target_os = "linux")]
const LINUX_MAX_SECRET_BYTES: usize = 8191;

/// Serializes file credential reads/writes in the daemon process.
static FILE_IO_LOCK: Mutex<()> = Mutex::new(());

/// Trait mirroring Orca's `AntigravityCredentialBackend`.
pub(crate) trait AntigravityCredentialBackend: Send + Sync {
    fn read(&self) -> Result<Option<AntigravityNativeCredential>, String>;
    fn write(&self, contents: &str, expected: Option<&str>) -> Result<(), String>;
}

/// Whether `agy` may hold a login here, read without raising a keyring unlock dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgyCredentialPresence {
    Present,
    Absent,
    Locked,
}

/// Failure reason from [`preflight_auth_with_backoff_using`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgyAuthPreflightError {
    Absent,
    Locked,
}

impl AgyAuthPreflightError {
    #[must_use]
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Absent => {
                "Antigravity is not signed in (auth_required). Sign in with `agy` in a terminal first."
            }
            Self::Locked => {
                "Antigravity credentials are temporarily unavailable because the OS keyring is locked (auth_required)."
            }
        }
    }
}

/// Default bounded backoff when the OS keyring is locked right after reboot (~1.7s total).
#[cfg_attr(
    test,
    expect(dead_code, reason = "used by non-test background launch preflight")
)]
pub(crate) const REBOOT_KEYCHAIN_BACKOFF: &[Duration] = &[
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_millis(1000),
];

/// `agy` writes this after a keyring timeout and may read its file store for a while after.
pub(crate) fn antigravity_keyring_fallback_marker(home: &Path) -> PathBuf {
    home.join(".gemini")
        .join("antigravity-cli")
        .join("cache")
        .join("antigravity-keyring-unavailable")
}

pub(crate) fn antigravity_token_file_path(home: &Path) -> PathBuf {
    home.join(".gemini")
        .join("antigravity-cli")
        .join("antigravity-oauth-token")
}

/// `agy 1.5.16+` forces file-based token storage when `AGY_CLI_FORCE_FILE_TOKEN_STORAGE`
/// is `"1"` or `"true"` (case-insensitive).
pub(crate) fn force_file_token_storage_env(env: &BTreeMap<String, String>) -> bool {
    let val = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("AGY_CLI_FORCE_FILE_TOKEN_STORAGE"))
        .map(|(_, v)| v.trim());
    val.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Mirrors Orca `forcesAntigravityFileStore(env)` plus `agy 1.5.16` `AGY_CLI_FORCE_FILE_TOKEN_STORAGE`.
pub(crate) fn forces_antigravity_file_store(env: &BTreeMap<String, String>) -> bool {
    const VARS: &[&str] = &[
        "SSH_TTY",
        "SSH_CLIENT",
        "SSH_CONNECTION",
        "WSL_DISTRO_NAME",
        "WSL_INTEROP",
    ];
    if force_file_token_storage_env(env) {
        return true;
    }
    VARS.iter().any(|name| {
        env.iter()
            .any(|(k, v)| k.eq_ignore_ascii_case(name) && !v.is_empty())
    })
}

/// Mirrors Orca `isAntigravityFileStorageHost(env)`.
pub(crate) fn is_antigravity_file_storage_host(env: &BTreeMap<String, String>) -> bool {
    if forces_antigravity_file_store(env) {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        if lacks_linux_session_bus(env) || is_linux_container_or_wsl() {
            return true;
        }
    }
    false
}

pub(crate) fn current_process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

#[cfg(target_os = "linux")]
fn lacks_linux_session_bus(env: &BTreeMap<String, String>) -> bool {
    use std::os::unix::fs::FileTypeExt as _;
    let addr = env
        .get("DBUS_SESSION_BUS_ADDRESS")
        .map(String::as_str)
        .unwrap_or("");
    if !matches!(addr, "" | "none:" | "disabled:") {
        return false;
    }
    let uid = rustix::process::getuid().as_raw();
    let sock = format!("/run/user/{uid}/bus");
    std::fs::metadata(sock).map_or(true, |m| !m.file_type().is_socket())
}

#[cfg(target_os = "linux")]
fn is_linux_container_or_wsl() -> bool {
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
        let lower = cgroup.to_ascii_lowercase();
        if lower.contains("docker") || lower.contains("containerd") || lower.contains("kubepods") {
            return true;
        }
    }
    if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        let lower = release.to_ascii_lowercase();
        if lower.contains("microsoft") || lower.contains("wsl") {
            return true;
        }
    }
    false
}

pub(crate) fn is_real_home(home: &Path) -> bool {
    if cfg!(test) {
        return false;
    }
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    let Some(real_home) = krill_proto::os::home_dir() else {
        return false;
    };
    home == real_home
        || home
            .canonicalize()
            .ok()
            .zip(real_home.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

/// True when `path` resides under the real OS user's home directory (as opposed to an isolated
/// `HOME=$(mktemp -d)` or offline test sandbox). Used on macOS to avoid polluting the user's
/// real Keychain with temporary vault keys during isolated E2E runs.
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) fn is_real_home_for_state_path(path: &Path) -> bool {
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    // Check whether the current process HOME matches the real OS user home from passwd.
    let Some(env_home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    let passwd_home = crate::sys::user()
        .ok()
        .filter(|u| !u.home.is_empty())
        .map(|u| PathBuf::from(std::ffi::OsStr::from_bytes(&u.home)));
    let Some(real_home) = passwd_home else {
        return false;
    };
    let env_canon = env_home.canonicalize().unwrap_or(env_home);
    let real_canon = real_home.canonicalize().unwrap_or(real_home);
    if env_canon != real_canon {
        return false;
    }
    let path_canon = path
        .canonicalize()
        .or_else(|_| {
            path.parent()
                .and_then(|p| p.canonicalize().ok())
                .map(|p| p.join(path.file_name().unwrap_or_default()))
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        })
        .unwrap_or_else(|_| path.to_path_buf());
    path_canon.starts_with(&real_canon)
}

pub(crate) fn read_antigravity_credential_file(
    path: &Path,
) -> Result<Option<Zeroizing<String>>, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(FILE_READ_ERR.to_owned()),
    };
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() > u64::try_from(MAX_CREDENTIAL_BYTES).unwrap_or(u64::MAX)
    {
        return Err(FILE_READ_ERR.to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(FILE_READ_ERR.to_owned());
        }
    }
    let bytes = Zeroizing::new(std::fs::read(path).map_err(|_| FILE_READ_ERR.to_owned())?);
    let text = String::from_utf8(bytes.to_vec()).map_err(|_| FILE_READ_ERR.to_owned())?;
    Ok(Some(Zeroizing::new(text)))
}

/// File-backed credential store (`~/.gemini/antigravity-cli/antigravity-oauth-token`).
pub(crate) struct FileCredentialBackend {
    path: PathBuf,
}

impl FileCredentialBackend {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn read_unlocked(&self) -> Result<Option<AntigravityNativeCredential>, String> {
        let Some(contents) = read_antigravity_credential_file(&self.path)? else {
            return Ok(None);
        };
        parse_antigravity_native_credential(&contents)
            .map(Some)
            .map_err(|_| FILE_READ_ERR.to_owned())
    }
}

impl AntigravityCredentialBackend for FileCredentialBackend {
    fn read(&self) -> Result<Option<AntigravityNativeCredential>, String> {
        let _guard = crate::session::lock(&FILE_IO_LOCK);
        self.read_unlocked()
    }

    fn write(&self, contents: &str, expected: Option<&str>) -> Result<(), String> {
        let _guard = crate::session::lock(&FILE_IO_LOCK);
        parse_antigravity_native_credential(contents)?;
        let current = self.read_unlocked()?;
        if current.as_ref().map(|c| c.contents.as_str()) != expected {
            return Err(CONFLICT_MSG.to_owned());
        }
        if let Some(parent) = self.path.parent() {
            #[cfg(unix)]
            {
                use krill_proto::os::fs::DirBuilderExt as _;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(|_| {
                        "Antigravity could not update its credential file privately.".to_owned()
                    })?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(parent).map_err(|_| {
                "Antigravity could not update its credential file privately. Check the file permissions and try again.".to_owned()
            })?;
        }
        write_atomic_0600(&self.path, contents.as_bytes()).map_err(|_| {
            "Antigravity could not update its credential file privately.".to_owned()
        })?;
        if self.read_unlocked()?.as_ref().map(|c| c.contents.as_str()) != Some(contents) {
            return Err(CONFLICT_MSG.to_owned());
        }
        Ok(())
    }
}

/// Host credential backend matching Orca's `createAntigravityHostCredentialBackend`.
/// On macOS/Windows/Linux desktop: reads keyring first, falls back to file; checks fallback marker;
/// writes to whichever store currently holds the native login (CAS + readback verified).
/// In tests or non-real-home directories or when `AGY_CLI_FORCE_FILE_TOKEN_STORAGE` / SSH / WSL is set:
/// uses the file backend exclusively so real OS keyrings are never touched in tests.
pub(crate) struct HostCredentialBackend {
    home: PathBuf,
    file: FileCredentialBackend,
    force_file_only: bool,
}

impl HostCredentialBackend {
    pub(crate) fn new(home: &Path) -> Self {
        let env = current_process_env();
        let force_file_only = !is_real_home(home) || is_antigravity_file_storage_host(&env);
        Self {
            home: home.to_path_buf(),
            file: FileCredentialBackend::new(antigravity_token_file_path(home)),
            force_file_only,
        }
    }

    fn marker_path(&self) -> PathBuf {
        antigravity_keyring_fallback_marker(&self.home)
    }

    fn read_keyring() -> Result<Option<AntigravityNativeCredential>, String> {
        #[cfg(target_os = "macos")]
        {
            read_macos_credential()
        }
        #[cfg(target_os = "linux")]
        {
            read_linux_credential()
        }
        #[cfg(target_os = "windows")]
        {
            read_windows_credential()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err("Native Antigravity account switching is not supported on this host.".to_owned())
        }
    }

    fn write_keyring(contents: &str) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            write_macos_credential(contents)
        }
        #[cfg(target_os = "linux")]
        {
            write_linux_credential(contents)
        }
        #[cfg(target_os = "windows")]
        {
            write_windows_credential(contents)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = contents;
            Err("Native Antigravity account switching is not supported on this host.".to_owned())
        }
    }
}

impl AntigravityCredentialBackend for HostCredentialBackend {
    fn read(&self) -> Result<Option<AntigravityNativeCredential>, String> {
        if self.force_file_only {
            return self.file.read();
        }
        if self.marker_path().exists() {
            return Err(KEYRING_FALLBACK_MSG.to_owned());
        }
        match Self::read_keyring() {
            Ok(Some(cred)) => Ok(Some(cred)),
            Ok(None) => self.file.read(),
            Err(keyring_err) => {
                // On Windows (native-windows-credentials.ts:821), fall back to file if present.
                #[cfg(target_os = "windows")]
                if let Ok(Some(cred)) = self.file.read() {
                    return Ok(Some(cred));
                }
                Err(keyring_err)
            }
        }
    }

    fn write(&self, contents: &str, expected: Option<&str>) -> Result<(), String> {
        parse_antigravity_native_credential(contents)?;
        if self.force_file_only {
            return self.file.write(contents, expected);
        }
        if self.read()?.as_ref().map(|c| c.contents.as_str()) != expected {
            return Err(CONFLICT_MSG.to_owned());
        }
        let native = Self::read_keyring()?;
        if self.marker_path().exists() {
            return Err(CONFLICT_MSG.to_owned());
        }
        if let Some(native_cred) = native {
            if Some(native_cred.contents.as_str()) != expected {
                return Err(CONFLICT_MSG.to_owned());
            }
            Self::write_keyring(contents)?;
        } else if self.file.read()?.is_some() {
            self.file.write(contents, expected)?;
        } else {
            // Neither keyring nor file currently holds a login (e.g. after external `agy /logout`).
            // On a keyring-capable desktop host, restore into the OS keyring so `agy 1.5.16+`
            // picks up the selected account natively without creating a competing file token.
            if Self::write_keyring(contents).is_err() {
                self.file.write(contents, expected)?;
            }
        }
        if self.read()?.as_ref().map(|c| c.contents.as_str()) != Some(contents) {
            return Err(CONFLICT_MSG.to_owned());
        }
        Ok(())
    }
}

/// Non-prompting check whether `agy` may hold a login on this host (`agy-credential-presence.ts`).
#[cfg(test)]
pub(crate) fn agy_credential_presence(home: &Path) -> AgyCredentialPresence {
    let env = current_process_env();
    agy_credential_presence_with_env(home, &env)
}

/// Non-prompting check using an explicit launch environment (respects
/// `AGY_CLI_FORCE_FILE_TOKEN_STORAGE=1`, `SSH_CONNECTION`, and non-real-home sandboxes).
pub(crate) fn agy_credential_presence_with_env(
    home: &Path,
    env: &BTreeMap<String, String>,
) -> AgyCredentialPresence {
    let token_file_present = || {
        let p = antigravity_token_file_path(home);
        if std::fs::symlink_metadata(&p)
            .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0)
        {
            AgyCredentialPresence::Present
        } else {
            AgyCredentialPresence::Absent
        }
    };
    if !is_real_home(home) || is_antigravity_file_storage_host(env) {
        return token_file_present();
    }
    if antigravity_keyring_fallback_marker(home).exists() {
        return token_file_present();
    }
    #[cfg(target_os = "macos")]
    {
        match has_macos_credential() {
            Ok(true) => AgyCredentialPresence::Present,
            Ok(false) => token_file_present(),
            Err(()) => match token_file_present() {
                AgyCredentialPresence::Present => AgyCredentialPresence::Present,
                _ => AgyCredentialPresence::Locked,
            },
        }
    }
    #[cfg(target_os = "windows")]
    {
        #[cfg(not(test))]
        {
            match crate::sys::generic_credential_result("gemini:antigravity") {
                Ok(Some(_)) => AgyCredentialPresence::Present,
                Ok(None) => token_file_present(),
                Err(_) => match token_file_present() {
                    AgyCredentialPresence::Present => AgyCredentialPresence::Present,
                    _ => AgyCredentialPresence::Locked,
                },
            }
        }
        #[cfg(test)]
        {
            token_file_present()
        }
    }
    #[cfg(target_os = "linux")]
    {
        match read_linux_credential() {
            Ok(Some(_)) => AgyCredentialPresence::Present,
            Ok(None) => token_file_present(),
            Err(_) => match token_file_present() {
                AgyCredentialPresence::Present => AgyCredentialPresence::Present,
                _ => AgyCredentialPresence::Locked,
            },
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        token_file_present()
    }
}

/// Read-only preflight with bounded backoff for locked OS keyrings (e.g. right after reboot).
/// - `Present` -> `Ok(())` immediately.
/// - `Absent` -> `Err(Absent)` immediately (no useless sleep when genuinely logged out).
/// - `Locked` -> retries after each delay in `delays`, returning `Err(Locked)` if still locked.
pub(crate) fn preflight_auth_with_backoff_using(
    delays: &[Duration],
    mut probe: impl FnMut() -> AgyCredentialPresence,
) -> Result<(), AgyAuthPreflightError> {
    let mut attempt = 0usize;
    loop {
        match probe() {
            AgyCredentialPresence::Present => return Ok(()),
            AgyCredentialPresence::Absent => return Err(AgyAuthPreflightError::Absent),
            AgyCredentialPresence::Locked => {
                let Some(&delay) = delays.get(attempt) else {
                    return Err(AgyAuthPreflightError::Locked);
                };
                attempt = attempt.saturating_add(1);
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
            }
        }
    }
}

#[cfg_attr(
    test,
    expect(dead_code, reason = "used by non-test background launch preflight")
)]
pub(crate) fn preflight_auth_for_host(
    home: &Path,
    launch_env: &BTreeMap<String, String>,
    delays: &[Duration],
) -> Result<(), AgyAuthPreflightError> {
    preflight_auth_with_backoff_using(delays, || {
        agy_credential_presence_with_env(home, launch_env)
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", all(test, unix)))]
pub(crate) struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: Zeroizing<Vec<u8>>,
    #[cfg(target_os = "linux")]
    pub stderr: String,
    pub timed_out: bool,
    pub truncated: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux", all(test, unix)))]
pub(crate) fn run_helper_with_timeout(
    cmd: &mut Command,
    stdin_bytes: Option<&[u8]>,
    capture_stdout: bool,
    capture_stderr: bool,
    max_output: usize,
    timeout: Duration,
) -> Option<ProcessOutput> {
    cmd.stdin(if stdin_bytes.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stderr(if capture_stderr {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd.spawn().ok()?;
    let limit = u64::try_from(max_output)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let stdout_reader = if capture_stdout {
        let stdout = child.stdout.take()?;
        let handle = std::thread::Builder::new()
            .name("agy-cred-stdout".into())
            .spawn(move || {
                let mut buf = Vec::new();
                stdout.take(limit).read_to_end(&mut buf).ok()?;
                Some(Zeroizing::new(buf))
            })
            .ok();
        let Some(handle) = handle else {
            child.kill().ok();
            child.wait().ok();
            return None;
        };
        Some(handle)
    } else {
        None
    };
    let stderr_reader = if capture_stderr {
        let stderr = child.stderr.take()?;
        let handle = std::thread::Builder::new()
            .name("agy-cred-stderr".into())
            .spawn(move || {
                let mut buf = Vec::new();
                stderr.take(16 * 1024).read_to_end(&mut buf).ok()?;
                Some(String::from_utf8_lossy(&buf).into_owned())
            })
            .ok();
        let Some(handle) = handle else {
            child.kill().ok();
            child.wait().ok();
            if let Some(h) = stdout_reader {
                drop(h.join());
            }
            return None;
        };
        Some(handle)
    } else {
        None
    };
    if let Some(bytes) = stdin_bytes {
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(bytes).is_ok());
        if !wrote {
            child.kill().ok();
            child.wait().ok();
            if let Some(h) = stdout_reader {
                drop(h.join());
            }
            if let Some(h) = stderr_reader {
                drop(h.join());
            }
            return None;
        }
    }
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                child.kill().ok();
                child.wait().ok();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
        }
    };
    let stdout = match stdout_reader {
        Some(h) => h.join().ok().flatten().unwrap_or_default(),
        None => Zeroizing::new(Vec::new()),
    };
    #[cfg(target_os = "linux")]
    let stderr = match stderr_reader {
        Some(h) => h.join().ok().flatten().unwrap_or_default(),
        None => String::new(),
    };
    #[cfg(not(target_os = "linux"))]
    if let Some(h) = stderr_reader {
        drop(h.join());
    }
    let truncated = stdout.len() > max_output;
    Some(ProcessOutput {
        code: status.and_then(|s| s.code()),
        stdout,
        #[cfg(target_os = "linux")]
        stderr,
        timed_out,
        truncated,
    })
}

// ---------------------------------------------------------------------------
// macOS Keychain (`native-macos-credentials.ts`)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn has_macos_credential() -> Result<bool, ()> {
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args(["find-generic-password", "-s", "gemini", "-a", "antigravity"]);
    let out = run_helper_with_timeout(
        &mut cmd,
        None,
        false,
        false,
        MAX_CREDENTIAL_BYTES,
        MACOS_SECURITY_TIMEOUT,
    )
    .ok_or(())?;
    if !out.timed_out && matches!(out.code, Some(0 | 44)) {
        return Ok(out.code == Some(0));
    }
    Err(())
}

#[cfg(target_os = "macos")]
pub(crate) fn read_macos_credential() -> Result<Option<AntigravityNativeCredential>, String> {
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args([
        "find-generic-password",
        "-s",
        "gemini",
        "-a",
        "antigravity",
        "-w",
    ]);
    let out = run_helper_with_timeout(
        &mut cmd,
        None,
        true,
        false,
        MAX_CREDENTIAL_BYTES,
        MACOS_SECURITY_TIMEOUT,
    )
    .ok_or_else(|| "The Antigravity macOS credential store could not be accessed.".to_owned())?;
    if out.timed_out || out.truncated {
        return Err("The Antigravity macOS credential store could not be read.".to_owned());
    }
    match out.code {
        Some(44) => Ok(None),
        Some(0) => {
            let text = Zeroizing::new(
                String::from_utf8(out.stdout.to_vec())
                    .map_err(|_| "Antigravity credentials could not be decoded.".to_owned())?,
            );
            let decoded = decode_antigravity_keychain_value(&text)?;
            parse_antigravity_native_credential(&decoded).map(Some)
        }
        _ => Err("The Antigravity macOS credential store could not be read.".to_owned()),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn write_macos_credential(contents: &str) -> Result<(), String> {
    parse_antigravity_native_credential(contents)?;
    let encoded = encode_antigravity_keychain_value(contents)?;
    let input = Zeroizing::new(format!(
        "add-generic-password -U -s \"gemini\" -a \"antigravity\" -w {}\n",
        encoded.as_str()
    ));
    // Avoid truncating credentials in security's limited interactive command buffer.
    if input.len() > 4096 {
        return Err(
            "Antigravity credentials exceed the macOS credential command limit.".to_owned(),
        );
    }
    let mut cmd = Command::new("/usr/bin/security");
    cmd.arg("-i");
    let out = run_helper_with_timeout(
        &mut cmd,
        Some(input.as_bytes()),
        false,
        false,
        0,
        MACOS_SECURITY_TIMEOUT,
    )
    .ok_or_else(|| "The Antigravity macOS credential store could not be updated.".to_owned())?;
    if out.timed_out || out.code != Some(0) {
        return Err("The Antigravity macOS credential store could not be updated.".to_owned());
    }
    let actual = read_macos_credential()?;
    if actual.as_ref().map(|c| c.contents.as_str()) != Some(contents) {
        return Err("The Antigravity macOS credential update could not be verified.".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Linux Secret Service (`native-linux-credentials.ts` + e71aed39a KDE fallback)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn run_linux_tool(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<ProcessOutput, String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    match run_helper_with_timeout(
        &mut cmd,
        input,
        true,
        true,
        MAX_CREDENTIAL_BYTES,
        LINUX_KEYRING_TIMEOUT,
    ) {
        Some(out) => Ok(out),
        None => {
            if program == "gdbus" {
                Err("Antigravity account switching on Linux needs gdbus (GLib) to reach the keyring. Install it and try again.".to_owned())
            } else {
                Err("Antigravity account switching on Linux needs secret-tool. Install libsecret-tools (or your distribution's equivalent) and try again.".to_owned())
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn get_secret_service_property(
    object_path: &str,
    iface: &str,
    property: &str,
) -> Result<Option<String>, String> {
    let out = run_linux_tool(
        "gdbus",
        &[
            "call",
            "--session",
            "--timeout",
            "5",
            "--dest",
            "org.freedesktop.secrets",
            "--object-path",
            object_path,
            "--method",
            "org.freedesktop.DBus.Properties.Get",
            iface,
            property,
        ],
        None,
    )?;
    if out.timed_out
        || out.truncated
        || out.stderr.contains("Timeout was reached")
        || out.stderr.contains("DBus.Error.NoReply")
        || out.stderr.contains("DBus.Error.Timeout")
        || out.stderr.contains("DBus.Error.TimedOut")
    {
        return Err("The Antigravity Linux keyring did not respond.".to_owned());
    }
    if out.code == Some(0) {
        return Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()));
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
fn resolve_agy_linux_collection() -> Result<Option<&'static str>, String> {
    const LOGIN_COLLECTION: &str = "/org/freedesktop/secrets/collection/login";
    const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";
    let Some(collections) = get_secret_service_property(
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
        "Collections",
    )?
    else {
        return Ok(None);
    };
    let path = if collections.contains(&format!("'{LOGIN_COLLECTION}'")) {
        LOGIN_COLLECTION
    } else {
        DEFAULT_COLLECTION
    };
    let Some(locked) =
        get_secret_service_property(path, "org.freedesktop.Secret.Collection", "Locked")?
    else {
        return Ok(None);
    };
    if locked.contains("<true>") {
        return Err("Your Linux keyring is locked, so Krill cannot read or switch the Antigravity account. Unlock the keyring (for example in Passwords and Keys) and try again.".to_owned());
    }
    if !locked.contains("<false>") {
        return Err("The Antigravity Linux keyring could not be accessed.".to_owned());
    }
    Ok(Some(path))
}

#[cfg(target_os = "linux")]
fn linux_secret_search() -> Result<AntigravityNativeCredential, String> {
    const START: &str = "\nsecret = ";
    const END: &str = "\ncreated = ";
    let out = run_linux_tool(
        "secret-tool",
        &["search", "service", "gemini", "username", "antigravity"],
        None,
    )?;
    if out.timed_out || out.truncated || out.code != Some(0) {
        return Err("The Antigravity Linux keyring could not be read.".to_owned());
    }
    let stdout = Zeroizing::new(
        String::from_utf8(out.stdout.to_vec())
            .map_err(|_| "The Antigravity Linux keyring could not be read.".to_owned())?,
    );
    let start = stdout
        .find(START)
        .ok_or_else(|| "The Antigravity Linux keyring could not be read.".to_owned())?;
    let end = stdout
        .rfind(END)
        .ok_or_else(|| "The Antigravity Linux keyring could not be read.".to_owned())?;
    if end < start + START.len() {
        return Err("The Antigravity Linux keyring could not be read.".to_owned());
    }
    let secret = stdout
        .get(start + START.len()..end)
        .ok_or_else(|| "The Antigravity Linux keyring could not be read.".to_owned())?;
    let decoded = decode_antigravity_keychain_value(secret)?;
    parse_antigravity_native_credential(&decoded)
}

#[cfg(target_os = "linux")]
fn linux_secret_lookup() -> Result<Option<AntigravityNativeCredential>, String> {
    let out = run_linux_tool(
        "secret-tool",
        &["lookup", "service", "gemini", "username", "antigravity"],
        None,
    )?;
    if !out.timed_out && !out.truncated {
        if out.code == Some(0) && !out.stdout.is_empty() {
            let text = Zeroizing::new(
                String::from_utf8(out.stdout.to_vec())
                    .map_err(|_| "Antigravity credentials could not be decoded.".to_owned())?,
            );
            let decoded = decode_antigravity_keychain_value(&text)?;
            return parse_antigravity_native_credential(&decoded).map(Some);
        }
        if out.code == Some(1) && out.stdout.is_empty() && out.stderr.trim().is_empty() {
            return Ok(None);
        }
        if out.code == Some(1)
            && out
                .stderr
                .contains("secret does not contain a textual password")
        {
            return linux_secret_search().map(Some);
        }
    }
    Err("The Antigravity Linux keyring could not be read.".to_owned())
}

#[cfg(target_os = "linux")]
fn read_linux_credential() -> Result<Option<AntigravityNativeCredential>, String> {
    if resolve_agy_linux_collection()?.is_some() {
        linux_secret_lookup()
    } else {
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn write_linux_credential(contents: &str) -> Result<(), String> {
    parse_antigravity_native_credential(contents)?;
    if contents.len() > LINUX_MAX_SECRET_BYTES {
        return Err("Antigravity credentials exceed the Linux keyring command limit.".to_owned());
    }
    let collection = resolve_agy_linux_collection()?
        .ok_or_else(|| "The Antigravity Linux keyring is unavailable.".to_owned())?;
    let col_arg = format!("--collection={collection}");
    let out = run_linux_tool(
        "secret-tool",
        &[
            "store",
            &col_arg,
            "--label=Password for 'antigravity' on 'gemini'",
            "service",
            "gemini",
            "username",
            "antigravity",
        ],
        Some(contents.as_bytes()),
    )?;
    if out.timed_out || out.truncated || out.code != Some(0) {
        return Err("The Antigravity Linux keyring could not be updated.".to_owned());
    }
    if linux_secret_lookup()?.as_ref().map(|c| c.contents.as_str()) != Some(contents) {
        return Err("The Antigravity Linux keyring update could not be verified.".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows Credential Manager (`native-windows-credentials.ts`)
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
#[cfg_attr(
    test,
    expect(
        clippy::unnecessary_wraps,
        reason = "matches non-test Result signature"
    )
)]
fn read_windows_credential() -> Result<Option<AntigravityNativeCredential>, String> {
    #[cfg(test)]
    {
        Ok(None)
    }
    #[cfg(not(test))]
    {
        let Some(bytes) =
            crate::sys::generic_credential_result("gemini:antigravity").map_err(|_| {
                "The Antigravity Windows credential store could not be read.".to_owned()
            })?
        else {
            return Ok(None);
        };
        if bytes.len() > 2560 {
            return Err("The Antigravity Windows credential store could not be read.".to_owned());
        }
        let text = Zeroizing::new(String::from_utf8(bytes.to_vec()).map_err(|_| {
            "The Antigravity Windows credential store could not be read.".to_owned()
        })?);
        let decoded = decode_antigravity_keychain_value(&text)?;
        parse_antigravity_native_credential(&decoded).map(Some)
    }
}

#[cfg(target_os = "windows")]
fn write_windows_credential(contents: &str) -> Result<(), String> {
    parse_antigravity_native_credential(contents)?;
    #[cfg(test)]
    {
        let _ = contents;
        Ok(())
    }
    #[cfg(not(test))]
    {
        crate::sys::write_generic_credential(
            "gemini:antigravity",
            "antigravity",
            contents.as_bytes(),
        )
        .map_err(|_| "The Antigravity Windows credential store could not be updated.".to_owned())?;
        if read_windows_credential()?
            .as_ref()
            .map(|c| c.contents.as_str())
            != Some(contents)
        {
            return Err(
                "The Antigravity Windows credential update could not be verified.".to_owned(),
            );
        }
        Ok(())
    }
}
