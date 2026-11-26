//! Upgrade-resilient macOS Keychain storage for 32-byte Krill master keys.
//!
//! Why this exists:
//! - Ad-hoc signed macOS builds (`codesign -s -`, no Developer ID) change their `CDHash` on every
//!   release. Items created directly via `SecKeychain::add_generic_password` bind their ACL to the
//!   creator's `CDHash`, so the next version immediately fails with `errSecInteractionNotAllowed`
//!   (`-25308`) when `SecKeychain::disable_user_interaction()` is active.
//! - Storing master keys through Apple's platform-signed `/usr/bin/security` tool (`-i` stdin,
//!   `-T "/usr/bin/security"`, never passing secrets in argv) binds the item's ACL to
//!   `/usr/bin/security` (`identifier "com.apple.security" and anchor apple`), whose signature does
//!   not change across Krill upgrades.
//! - Existing legacy `SecKeychain`-owned items are NEVER queried via `/usr/bin/security` (which
//!   could raise a macOS GUI prompt when `/usr/bin/security` is not in the legacy ACL). Instead,
//!   they are read exclusively via `SecKeychain` with user interaction disabled:
//!   - If readable (same binary or Developer ID build), the 32-byte key is transparently migrated
//!     into the new upgrade-resilient service so future ad-hoc upgrades succeed without friction.
//!   - If blocked by legacy ACL (`-25308`), the error is logged with the exact `OSStatus` (never
//!     secrets) and returned as [`KeyLoadErrorKind::LegacyAclLocked`] so callers can surface a safe
//!     recovery path without blocking unrelated functionality or prompting the user.

use std::fmt::{self, Write as _};
use std::io;
use std::path::Path;
use std::sync::Mutex;

use ring::rand::{SecureRandom as _, SystemRandom};
use security_framework::os::macos::keychain::SecKeychain;
use zeroize::Zeroizing;

use crate::session::lock;
use crate::usage::accounts::agy_native::{
    KEYCHAIN_TIMEOUT, MAX_SECURITY_INTERACTIVE_LINE, run_command_with_timeout,
};

const SECURITY_BIN: &str = "/usr/bin/security";
/// `/usr/bin/security` exit code for `errSecItemNotFound` (`-25300`).
const SECURITY_EXIT_NOT_FOUND: i32 = 44;
/// `/usr/bin/security` exit code for `errSecDuplicateItem` (`-25299`).
const SECURITY_EXIT_DUPLICATE: i32 = 45;

/// `errSecItemNotFound`.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
/// `errSecInteractionNotAllowed` (locked keychain or ACL mismatch with UI disabled).
pub(crate) const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
/// `errSecUserCanceled`.
const ERR_SEC_USER_CANCELED: i32 = -128;
/// `errSecAuthFailed`.
const ERR_SEC_AUTH_FAILED: i32 = -25293;

/// Maximum stdout bytes expected from `security find-generic-password -w` for a 32-byte key
/// (64 hex chars + trailing newline, with generous headroom for unexpected padding).
const MAX_KEY_STDOUT_BYTES: u64 = 256;

/// Process-global serialization for `SecKeychain::disable_user_interaction()` and Keychain ops.
static KEYCHAIN_GATE: Mutex<()> = Mutex::new(());

/// Classification of a macOS Keychain master key load/create failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyLoadErrorKind {
    /// Keychain itself is unlocked, but the legacy `SecKeychain`-owned item blocked this binary's
    /// `CDHash` (`-25308` / `-128` / `-25293`).
    LegacyAclLocked(i32),
    /// The login keychain itself is locked or cannot be opened without user interaction.
    KeychainLocked(i32),
    /// No key exists in either the new or legacy service and `allow_create` was `false`.
    NotFound,
    /// Any other keychain/CLI/format/timeout error.
    Unavailable,
}

/// Structured error returned by [`load_or_create_default`] and [`load_or_create_in_keychain`].
#[derive(Debug, Clone)]
pub(crate) struct KeyLoadError {
    pub(crate) kind: KeyLoadErrorKind,
    pub(crate) message: String,
}

impl KeyLoadError {
    fn legacy_acl_locked(status: i32) -> Self {
        Self {
            kind: KeyLoadErrorKind::LegacyAclLocked(status),
            message: format!("legacy macOS keychain ACL blocked access (keychain status {status})"),
        }
    }

    fn keychain_locked(status: i32) -> Self {
        Self {
            kind: KeyLoadErrorKind::KeychainLocked(status),
            message: format!("macOS keychain is locked or unavailable (keychain status {status})"),
        }
    }

    fn not_found() -> Self {
        Self {
            kind: KeyLoadErrorKind::NotFound,
            message: format!("keychain item not found (keychain status {ERR_SEC_ITEM_NOT_FOUND})"),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: KeyLoadErrorKind::Unavailable,
            message: message.into(),
        }
    }

    /// Returns a static reason label suitable for redacted tracing logs.
    pub(crate) fn log_kind(&self) -> &'static str {
        match self.kind {
            KeyLoadErrorKind::LegacyAclLocked(_) => "legacy_acl_locked",
            KeyLoadErrorKind::KeychainLocked(_) => "keychain_locked",
            KeyLoadErrorKind::NotFound => "not_found",
            KeyLoadErrorKind::Unavailable => "unavailable",
        }
    }

    /// Returns the underlying macOS `OSStatus` when available (never contains secrets).
    pub(crate) fn os_status(&self) -> Option<i32> {
        match self.kind {
            KeyLoadErrorKind::LegacyAclLocked(status)
            | KeyLoadErrorKind::KeychainLocked(status) => Some(status),
            KeyLoadErrorKind::NotFound => Some(ERR_SEC_ITEM_NOT_FOUND),
            KeyLoadErrorKind::Unavailable => None,
        }
    }

    /// Converts into an [`io::Error`] safe for logging and caller propagation (never contains secrets).
    pub(crate) fn into_io_error(self, label: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{label} ({})", self.message),
        )
    }
}

impl fmt::Display for KeyLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Parameters for loading or creating a 32-byte Krill master key in macOS Keychain.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeychainSpec<'a> {
    /// New upgrade-resilient service name owned by `/usr/bin/security`.
    pub(crate) service: &'a str,
    /// Optional legacy `SecKeychain`-owned service name for read-only fallback + auto-migration.
    pub(crate) legacy_service: Option<&'a str>,
    /// Account identifier (typically 64-char SHA-256 hex of canonical state path).
    pub(crate) account: &'a str,
    /// Whether to generate and persist a fresh 32-byte key when none is found.
    pub(crate) allow_create: bool,
    /// Whether to consult `legacy_service` when `service` is not found.
    /// Set to `false` when no encrypted data exists on disk or during explicit locked-vault reset.
    pub(crate) check_legacy: bool,
}

/// Loads or creates a 32-byte master key in the user's default macOS login keychain.
pub(crate) fn load_or_create_default(
    spec: KeychainSpec<'_>,
) -> Result<Zeroizing<[u8; 32]>, KeyLoadError> {
    let _gate = lock(&KEYCHAIN_GATE);
    let _ui = disable_ui_guard()?;
    let keychain = SecKeychain::default().map_err(|err| {
        tracing::warn!(
            service = spec.service,
            status = err.code(),
            "failed to open default macOS Keychain"
        );
        KeyLoadError::keychain_locked(err.code())
    })?;
    load_or_create_locked(spec, &keychain, None)
}

/// Loads or creates a 32-byte master key in an explicit macOS [`SecKeychain`] (used by tests).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by isolated keychain tests in store")
)]
pub(crate) fn load_or_create_in_keychain(
    spec: KeychainSpec<'_>,
    keychain: &SecKeychain,
    keychain_path: Option<&Path>,
) -> Result<Zeroizing<[u8; 32]>, KeyLoadError> {
    let _gate = lock(&KEYCHAIN_GATE);
    let _ui = disable_ui_guard()?;
    load_or_create_locked(spec, keychain, keychain_path)
}

fn disable_ui_guard() -> Result<
    Option<security_framework::os::macos::keychain::KeychainUserInteractionLock>,
    KeyLoadError,
> {
    if SecKeychain::user_interaction_allowed()
        .map_err(|err| KeyLoadError::keychain_locked(err.code()))?
    {
        Ok(Some(SecKeychain::disable_user_interaction().map_err(
            |err| KeyLoadError::keychain_locked(err.code()),
        )?))
    } else {
        Ok(None)
    }
}

/// Runs with `KEYCHAIN_GATE` held and `SecKeychain::disable_user_interaction()` active.
fn load_or_create_locked(
    spec: KeychainSpec<'_>,
    keychain: &SecKeychain,
    keychain_path: Option<&Path>,
) -> Result<Zeroizing<[u8; 32]>, KeyLoadError> {
    // Step 0: Verify the target keychain is unlocked BEFORE spawning `/usr/bin/security`,
    // so `/usr/bin/security` can never trigger a macOS unlock dialog on a locked keychain.
    let mut kc_check = keychain.clone();
    if let Err(err) = kc_check.unlock(None) {
        tracing::warn!(
            service = spec.service,
            status = err.code(),
            "macOS Keychain is locked; refusing interactive unlock"
        );
        return Err(KeyLoadError::keychain_locked(err.code()));
    }

    // Step 1: Try the upgrade-resilient `/usr/bin/security`-owned service first.
    match read_via_security_cli(spec.service, spec.account, keychain_path) {
        Ok(Some(key)) => return Ok(key),
        Ok(None) => {}
        Err(err) => return Err(err),
    }

    // Step 2: If requested, check the legacy `SecKeychain`-owned service (UI strictly disabled).
    // Never invoke `/usr/bin/security` against `legacy_service` because its ACL may trust only an
    // older ad-hoc `CDHash`, which would cause `/usr/bin/security` to pop up a GUI prompt.
    if spec.check_legacy
        && let Some(legacy_service) = spec.legacy_service
    {
        match keychain.find_generic_password(legacy_service, spec.account) {
            Ok((bytes, _)) => {
                let key = decode_raw_32(bytes.as_ref()).ok_or_else(|| {
                    KeyLoadError::unavailable("legacy keychain item has invalid key length")
                })?;
                // Best-effort transparent migration to the upgrade-resilient service so the next
                // ad-hoc binary upgrade does not lose access.
                match write_via_security_cli(spec.service, spec.account, &key, keychain_path) {
                    Ok(()) => {
                        tracing::info!(
                            service = spec.service,
                            legacy_service,
                            "migrated legacy macOS Keychain master key to upgrade-resilient ACL"
                        );
                    }
                    Err(WriteCliError::Duplicate) => {
                        tracing::debug!(
                            service = spec.service,
                            legacy_service,
                            "upgrade-resilient macOS Keychain master key already populated concurrently"
                        );
                    }
                    Err(WriteCliError::Failed(err)) => {
                        tracing::warn!(
                            service = spec.service,
                            legacy_service,
                            reason = err.log_kind(),
                            status = ?err.os_status(),
                            "failed to auto-migrate legacy macOS Keychain key; continuing with legacy key"
                        );
                    }
                }
                return Ok(key);
            }
            Err(err) if is_acl_denial(err.code()) => {
                tracing::warn!(
                    service = legacy_service,
                    status = err.code(),
                    "legacy macOS Keychain master key blocked by ACL (ad-hoc upgrade CDHash mismatch)"
                );
                return Err(KeyLoadError::legacy_acl_locked(err.code()));
            }
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => {}
            Err(err) => {
                tracing::warn!(
                    service = legacy_service,
                    status = err.code(),
                    "legacy macOS Keychain lookup failed"
                );
                return Err(KeyLoadError::unavailable(format!(
                    "legacy keychain lookup failed (keychain status {})",
                    err.code()
                )));
            }
        }
    }

    // Step 3: Refuse creation when caller requested read-only lookup.
    if !spec.allow_create {
        return Err(KeyLoadError::not_found());
    }

    // Step 4: Generate a fresh 32-byte key and persist it atomically (no `-U`) via `/usr/bin/security -i`.
    let mut fresh = Zeroizing::new([0u8; 32]);
    SystemRandom::new()
        .fill(fresh.as_mut())
        .map_err(|_| KeyLoadError::unavailable("master key RNG failure"))?;

    match write_via_security_cli(spec.service, spec.account, &fresh, keychain_path) {
        Ok(()) => Ok(fresh),
        Err(WriteCliError::Duplicate) => {
            // Another process created the key concurrently; read back the winner.
            read_via_security_cli(spec.service, spec.account, keychain_path)?.ok_or_else(|| {
                KeyLoadError::unavailable("keychain item disappeared after duplicate race")
            })
        }
        Err(WriteCliError::Failed(err)) => Err(err),
    }
}

fn is_acl_denial(code: i32) -> bool {
    matches!(
        code,
        ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED
    )
}

fn read_via_security_cli(
    service: &str,
    account: &str,
    keychain_path: Option<&Path>,
) -> Result<Option<Zeroizing<[u8; 32]>>, KeyLoadError> {
    let mut cmd = krill_proto::os::background_command(SECURITY_BIN);
    cmd.args(["find-generic-password", "-s", service, "-a", account, "-w"]);
    if let Some(path) = keychain_path {
        cmd.arg(path);
    }
    let Some((status, stdout)) =
        run_command_with_timeout(&mut cmd, None, true, MAX_KEY_STDOUT_BYTES, KEYCHAIN_TIMEOUT)
    else {
        tracing::warn!(service, "macOS security CLI read failed or timed out");
        return Err(KeyLoadError::unavailable(
            "macOS security CLI read failed or timed out",
        ));
    };
    match status.code() {
        Some(0) => decode_key_output(&stdout).map(Some).ok_or_else(|| {
            tracing::warn!(service, "macOS Keychain master key has unexpected format");
            KeyLoadError::unavailable("macOS Keychain master key has unexpected format")
        }),
        Some(SECURITY_EXIT_NOT_FOUND) => Ok(None),
        code => {
            tracing::warn!(
                service,
                exit_code = ?code,
                "macOS security CLI read returned non-zero status"
            );
            Err(KeyLoadError::unavailable(format!(
                "macOS security CLI read failed (exit {code:?})"
            )))
        }
    }
}

enum WriteCliError {
    Duplicate,
    Failed(KeyLoadError),
}

fn write_via_security_cli(
    service: &str,
    account: &str,
    key: &[u8; 32],
    keychain_path: Option<&Path>,
) -> Result<(), WriteCliError> {
    let path_str = match keychain_path {
        Some(path) => {
            let s = path.to_str().ok_or_else(|| {
                WriteCliError::Failed(KeyLoadError::unavailable(
                    "non-UTF-8 keychain path cannot be passed to security -i",
                ))
            })?;
            Some(s)
        }
        None => None,
    };
    let script = build_add_command_line(service, account, key, path_str).ok_or_else(|| {
        WriteCliError::Failed(KeyLoadError::unavailable(
            "invalid or oversized macOS security -i command",
        ))
    })?;
    let mut cmd = krill_proto::os::background_command(SECURITY_BIN);
    cmd.arg("-i");
    let Some((status, _)) = run_command_with_timeout(
        &mut cmd,
        Some(script.as_bytes()),
        false,
        0,
        KEYCHAIN_TIMEOUT,
    ) else {
        tracing::warn!(service, "macOS security CLI write failed or timed out");
        return Err(WriteCliError::Failed(KeyLoadError::unavailable(
            "macOS security CLI write failed or timed out",
        )));
    };
    match status.code() {
        Some(0) => Ok(()),
        Some(SECURITY_EXIT_DUPLICATE) => Err(WriteCliError::Duplicate),
        code => {
            tracing::warn!(
                service,
                exit_code = ?code,
                "macOS security CLI write returned non-zero status"
            );
            Err(WriteCliError::Failed(KeyLoadError::unavailable(format!(
                "macOS security CLI write failed (exit {code:?})"
            ))))
        }
    }
}

fn safe_security_arg(s: &str) -> bool {
    !s.is_empty()
        && !s
            .bytes()
            .any(|b| matches!(b, b'"' | b'\\' | b'\r' | b'\n' | b'\0'))
}

/// Builds a single `/usr/bin/security -i` stdin command line that atomically inserts a new
/// 32-byte master key encoded as 64 lowercase ASCII hex characters (pure printable ASCII so
/// `find-generic-password -w` round-trips unambiguously).
///
/// Deliberately omits `-U` so concurrent initializers get `errSecDuplicateItem` (exit 45) rather
/// than overwriting an existing master key. Explicitly sets `-T "/usr/bin/security"` so every
/// future ad-hoc `krilld` build can read the key via Apple-signed `/usr/bin/security`.
fn build_add_command_line(
    service: &str,
    account: &str,
    key: &[u8; 32],
    keychain_path: Option<&str>,
) -> Option<Zeroizing<String>> {
    if !safe_security_arg(service) || !safe_security_arg(account) {
        return None;
    }
    if let Some(kc) = keychain_path
        && !safe_security_arg(kc)
    {
        return None;
    }
    let mut hex_key = Zeroizing::new(String::with_capacity(64));
    for byte in key {
        write!(hex_key, "{byte:02x}").ok();
    }
    let mut line = Zeroizing::new(String::with_capacity(256));
    line.push_str("add-generic-password -a \"");
    line.push_str(account);
    line.push_str("\" -s \"");
    line.push_str(service);
    line.push_str("\" -T \"");
    line.push_str(SECURITY_BIN);
    line.push_str("\" -w \"");
    line.push_str(&hex_key);
    line.push('"');
    if let Some(kc) = keychain_path {
        line.push_str(" \"");
        line.push_str(kc);
        line.push('"');
    }
    line.push('\n');
    (line.len() <= MAX_SECURITY_INTERACTIVE_LINE).then_some(line)
}

fn decode_raw_32(bytes: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Zeroizing::new(arr))
}

fn decode_key_output(raw: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    let trimmed = raw
        .strip_suffix(b"\n")
        .unwrap_or(raw)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| raw.strip_suffix(b"\n").unwrap_or(raw));
    if trimmed.len() == 64 && trimmed.iter().all(u8::is_ascii_hexdigit) {
        let mut out = Zeroizing::new([0u8; 32]);
        for (dst, pair) in out.iter_mut().zip(trimmed.as_chunks::<2>().0) {
            let text = std::str::from_utf8(pair).ok()?;
            *dst = u8::from_str_radix(text, 16).ok()?;
        }
        return Some(out);
    }
    decode_raw_32(trimmed)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use security_framework::os::macos::keychain::CreateOptions;

    #[test]
    fn build_add_command_line_formats_safely_without_update_flag() {
        let key = [0xab; 32];
        let line = build_add_command_line(
            "com.krill.antigravity-accounts-key.v2",
            "deadbeef",
            &key,
            None,
        )
        .unwrap();
        assert!(line.starts_with("add-generic-password -a \"deadbeef\" "));
        assert!(!line.contains(" -U "), "must not overwrite existing key");
        assert!(line.contains("-T \"/usr/bin/security\""));
        assert!(line.contains(&format!("-w \"{}\"", "ab".repeat(32))));
        assert!(line.ends_with('\n'));

        assert!(build_add_command_line("bad\"svc", "acct", &key, None).is_none());
        assert!(build_add_command_line("svc", "bad\nacct", &key, None).is_none());
        assert!(build_add_command_line("svc", "acct", &key, Some("bad\\kc")).is_none());
    }

    #[test]
    fn decode_key_output_parses_hex_and_raw_and_rejects_invalid() {
        let expected: [u8; 32] = core::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
        let mut hex = String::new();
        for b in expected {
            write!(hex, "{b:02x}").unwrap();
        }
        hex.push('\n');
        assert_eq!(*decode_key_output(hex.as_bytes()).unwrap(), expected);
        assert_eq!(*decode_key_output(&expected).unwrap(), expected);
        assert!(decode_key_output(b"short\n").is_none());
        assert!(decode_key_output(b"zzzz").is_none());
    }

    #[test]
    fn temp_keychain_creates_migrates_and_rejects_locked_without_prompt() {
        let dir = std::env::temp_dir().join(format!("krilld-kc-key-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let kc_path = dir.join("isolated.keychain-db");

        let _ui = SecKeychain::disable_user_interaction().unwrap();
        let ephemeral_pass = format!("ephemeral-{}", std::process::id());
        let kc = CreateOptions::new()
            .password(&ephemeral_pass)
            .create(&kc_path)
            .unwrap();

        let svc_new = "com.krill.unit-test-key.v2";
        let svc_old = "com.krill.unit-test-key.v1";

        // 1. Fresh creation via `/usr/bin/security -i` + subsequent read.
        let k1 = load_or_create_in_keychain(
            KeychainSpec {
                service: svc_new,
                legacy_service: Some(svc_old),
                account: "acct-fresh",
                allow_create: true,
                check_legacy: true,
            },
            &kc,
            Some(&kc_path),
        )
        .unwrap();
        let k2 = load_or_create_in_keychain(
            KeychainSpec {
                service: svc_new,
                legacy_service: Some(svc_old),
                account: "acct-fresh",
                allow_create: false,
                check_legacy: true,
            },
            &kc,
            Some(&kc_path),
        )
        .unwrap();
        assert_eq!(*k1, *k2);

        // 2. Legacy SecKeychain item transparently auto-migrates into the new service.
        let legacy_bytes = [0x5a_u8; 32];
        kc.add_generic_password(svc_old, "acct-legacy", &legacy_bytes)
            .unwrap();
        let migrated = load_or_create_in_keychain(
            KeychainSpec {
                service: svc_new,
                legacy_service: Some(svc_old),
                account: "acct-legacy",
                allow_create: false,
                check_legacy: true,
            },
            &kc,
            Some(&kc_path),
        )
        .unwrap();
        assert_eq!(*migrated, legacy_bytes);
        // Must now exist directly in `svc_new` without consulting legacy!
        let from_new_only = load_or_create_in_keychain(
            KeychainSpec {
                service: svc_new,
                legacy_service: None,
                account: "acct-legacy",
                allow_create: false,
                check_legacy: false,
            },
            &kc,
            Some(&kc_path),
        )
        .unwrap();
        assert_eq!(*from_new_only, legacy_bytes);

        // 3. Locked keychain returns KeychainLocked(-25308) immediately without prompting.
        assert!(
            Command::new(SECURITY_BIN)
                .args(["lock-keychain"])
                .arg(&kc_path)
                .status()
                .unwrap()
                .success()
        );
        let locked_err = load_or_create_in_keychain(
            KeychainSpec {
                service: svc_new,
                legacy_service: Some(svc_old),
                account: "acct-fresh",
                allow_create: false,
                check_legacy: true,
            },
            &kc,
            Some(&kc_path),
        )
        .unwrap_err();
        assert!(
            matches!(
                locked_err.kind,
                KeyLoadErrorKind::KeychainLocked(
                    ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_AUTH_FAILED
                )
            ),
            "expected KeychainLocked(-25308 | -25293), got {:?}",
            locked_err.kind
        );

        drop(kc);
        Command::new(SECURITY_BIN)
            .args(["delete-keychain"])
            .arg(&kc_path)
            .status()
            .ok();
        std::fs::remove_dir_all(&dir).ok();
    }
}
