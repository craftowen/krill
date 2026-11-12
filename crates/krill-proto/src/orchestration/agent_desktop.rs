use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{LockGuard, invalid};
#[cfg(unix)]
use crate::os::fs::OpenOptionsExt as _;

pub const AGENT_DESKTOP_VERSION: &str = "1.0.2";
pub const AGENT_DESKTOP_TAG: &str = "v1.0.2";
pub const AGENT_DESKTOP_REPO: &str = "vercel-labs/agent-desktop";
pub const MAX_AGENT_DESKTOP_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;

pub const ENV_BIN_OVERRIDE: &str = "KRILL_AGENT_DESKTOP_BIN";
pub const ENV_ARCHIVE_OVERRIDE: &str = "KRILL_AGENT_DESKTOP_ARCHIVE";
pub const ENV_SHA256_OVERRIDE: &str = "KRILL_AGENT_DESKTOP_SHA256";

const VERSION_STAMP_FILE: &str = ".agent-desktop.version";
const LOCK_FILE: &str = ".krill-agent-desktop.lock";
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDesktopTarget {
    MacosArm64,
    MacosX64,
    LinuxArm64,
    LinuxX64,
    WindowsArm64,
    WindowsX64,
}

impl AgentDesktopTarget {
    #[must_use]
    pub const fn current() -> Option<Self> {
        Self::for_target(std::env::consts::OS, std::env::consts::ARCH)
    }

    #[must_use]
    pub const fn for_target(os: &str, arch: &str) -> Option<Self> {
        match (os.as_bytes(), arch.as_bytes()) {
            (b"macos", b"aarch64") => Some(Self::MacosArm64),
            (b"macos", b"x86_64") => Some(Self::MacosX64),
            (b"linux", b"aarch64") => Some(Self::LinuxArm64),
            (b"linux", b"x86_64") => Some(Self::LinuxX64),
            (b"windows", b"aarch64") => Some(Self::WindowsArm64),
            (b"windows", b"x86_64") => Some(Self::WindowsX64),
            _ => None,
        }
    }

    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::MacosArm64 => "aarch64-apple-darwin",
            Self::MacosX64 => "x86_64-apple-darwin",
            Self::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Self::LinuxX64 => "x86_64-unknown-linux-gnu",
            Self::WindowsArm64 => "aarch64-pc-windows-msvc",
            Self::WindowsX64 => "x86_64-pc-windows-msvc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDesktopAsset {
    pub target: AgentDesktopTarget,
    pub version: &'static str,
    pub archive_name: &'static str,
    pub url: &'static str,
    pub sha256_hex: &'static str,
    pub sha256_bytes: [u8; 32],
    pub main_binary: &'static str,
    pub colocated_helper: Option<&'static str>,
}

pub const PINNED_ASSETS: &[AgentDesktopAsset] = &[
    AgentDesktopAsset {
        target: AgentDesktopTarget::MacosArm64,
        version: AGENT_DESKTOP_VERSION,
        archive_name: "agent-desktop-v1.0.2-aarch64-apple-darwin.tar.gz",
        url: "https://github.com/vercel-labs/agent-desktop/releases/download/v1.0.2/agent-desktop-v1.0.2-aarch64-apple-darwin.tar.gz",
        sha256_hex: "7bee385196d7b478de80cf05ca4304aaa38f0eba593b3406ad3c247c1773be19",
        sha256_bytes: [
            0x7b, 0xee, 0x38, 0x51, 0x96, 0xd7, 0xb4, 0x78, 0xde, 0x80, 0xcf, 0x05, 0xca, 0x43,
            0x04, 0xaa, 0xa3, 0x8f, 0x0e, 0xba, 0x59, 0x3b, 0x34, 0x06, 0xad, 0x3c, 0x24, 0x7c,
            0x17, 0x73, 0xbe, 0x19,
        ],
        main_binary: "agent-desktop",
        colocated_helper: Some("agent-desktop-macos-helper"),
    },
    AgentDesktopAsset {
        target: AgentDesktopTarget::MacosX64,
        version: AGENT_DESKTOP_VERSION,
        archive_name: "agent-desktop-v1.0.2-x86_64-apple-darwin.tar.gz",
        url: "https://github.com/vercel-labs/agent-desktop/releases/download/v1.0.2/agent-desktop-v1.0.2-x86_64-apple-darwin.tar.gz",
        sha256_hex: "8edda6df17eafc042395502a823dbb8415bc8ac14835a6731e4d8becc9cb4121",
        sha256_bytes: [
            0x8e, 0xdd, 0xa6, 0xdf, 0x17, 0xea, 0xfc, 0x04, 0x23, 0x95, 0x50, 0x2a, 0x82, 0x3d,
            0xbb, 0x84, 0x15, 0xbc, 0x8a, 0xc1, 0x48, 0x35, 0xa6, 0x73, 0x1e, 0x4d, 0x8b, 0xec,
            0xc9, 0xcb, 0x41, 0x21,
        ],
        main_binary: "agent-desktop",
        colocated_helper: Some("agent-desktop-macos-helper"),
    },
    AgentDesktopAsset {
        target: AgentDesktopTarget::LinuxArm64,
        version: AGENT_DESKTOP_VERSION,
        archive_name: "agent-desktop-v1.0.2-aarch64-unknown-linux-gnu.tar.gz",
        url: "https://github.com/vercel-labs/agent-desktop/releases/download/v1.0.2/agent-desktop-v1.0.2-aarch64-unknown-linux-gnu.tar.gz",
        sha256_hex: "864cb782e0afc8bf0df19622016b24bdbd1a83fc7749f784ee82aa49a7f845eb",
        sha256_bytes: [
            0x86, 0x4c, 0xb7, 0x82, 0xe0, 0xaf, 0xc8, 0xbf, 0x0d, 0xf1, 0x96, 0x22, 0x01, 0x6b,
            0x24, 0xbd, 0xbd, 0x1a, 0x83, 0xfc, 0x77, 0x49, 0xf7, 0x84, 0xee, 0x82, 0xaa, 0x49,
            0xa7, 0xf8, 0x45, 0xeb,
        ],
        main_binary: "agent-desktop",
        colocated_helper: Some("agent-desktop-linux-clipboard-owner"),
    },
    AgentDesktopAsset {
        target: AgentDesktopTarget::LinuxX64,
        version: AGENT_DESKTOP_VERSION,
        archive_name: "agent-desktop-v1.0.2-x86_64-unknown-linux-gnu.tar.gz",
        url: "https://github.com/vercel-labs/agent-desktop/releases/download/v1.0.2/agent-desktop-v1.0.2-x86_64-unknown-linux-gnu.tar.gz",
        sha256_hex: "ad661b7a3fd8f990a769243f4889a240ca0e2c8dab3ef5bddd6019086c71e0b4",
        sha256_bytes: [
            0xad, 0x66, 0x1b, 0x7a, 0x3f, 0xd8, 0xf9, 0x90, 0xa7, 0x69, 0x24, 0x3f, 0x48, 0x89,
            0xa2, 0x40, 0xca, 0x0e, 0x2c, 0x8d, 0xab, 0x3e, 0xf5, 0xbd, 0xdd, 0x60, 0x19, 0x08,
            0x6c, 0x71, 0xe0, 0xb4,
        ],
        main_binary: "agent-desktop",
        colocated_helper: Some("agent-desktop-linux-clipboard-owner"),
    },
    AgentDesktopAsset {
        target: AgentDesktopTarget::WindowsArm64,
        version: AGENT_DESKTOP_VERSION,
        archive_name: "agent-desktop-v1.0.2-aarch64-pc-windows-msvc.tar.gz",
        url: "https://github.com/vercel-labs/agent-desktop/releases/download/v1.0.2/agent-desktop-v1.0.2-aarch64-pc-windows-msvc.tar.gz",
        sha256_hex: "a0ecd6e17e60a0bcfbb45533a72f2d3a7a73508769588273ef08b0477d2b9687",
        sha256_bytes: [
            0xa0, 0xec, 0xd6, 0xe1, 0x7e, 0x60, 0xa0, 0xbc, 0xfb, 0xb4, 0x55, 0x33, 0xa7, 0x2f,
            0x2d, 0x3a, 0x7a, 0x73, 0x50, 0x87, 0x69, 0x58, 0x82, 0x73, 0xef, 0x08, 0xb0, 0x47,
            0x7d, 0x2b, 0x96, 0x87,
        ],
        main_binary: "agent-desktop.exe",
        colocated_helper: None,
    },
    AgentDesktopAsset {
        target: AgentDesktopTarget::WindowsX64,
        version: AGENT_DESKTOP_VERSION,
        archive_name: "agent-desktop-v1.0.2-x86_64-pc-windows-msvc.tar.gz",
        url: "https://github.com/vercel-labs/agent-desktop/releases/download/v1.0.2/agent-desktop-v1.0.2-x86_64-pc-windows-msvc.tar.gz",
        sha256_hex: "2186c76e7df197c0e67365e45d36d869f74714c56bb8a73817e2a66f3e18e222",
        sha256_bytes: [
            0x21, 0x86, 0xc7, 0x6e, 0x7d, 0xf1, 0x97, 0xc0, 0xe6, 0x73, 0x65, 0xe4, 0x5d, 0x36,
            0xd8, 0x69, 0xf7, 0x47, 0x14, 0xc5, 0x6b, 0xb8, 0xa7, 0x38, 0x17, 0xe2, 0xa6, 0x6f,
            0x3e, 0x18, 0xe2, 0x22,
        ],
        main_binary: "agent-desktop.exe",
        colocated_helper: None,
    },
];

#[must_use]
pub fn current_agent_desktop_asset() -> Option<&'static AgentDesktopAsset> {
    let target = AgentDesktopTarget::current()?;
    PINNED_ASSETS.iter().find(|asset| asset.target == target)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionSource {
    LocalDir(PathBuf),
    LocalArchive {
        path: PathBuf,
        expected_sha256: [u8; 32],
    },
    ColocatedBundle(PathBuf),
    RemoteRelease(&'static AgentDesktopAsset),
    SkippedOffline,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDesktopSource {
    AlreadyStaged,
    ColocatedBundle,
    LocalOverride,
    DownloadedRelease,
    Busy,
    SkippedOffline,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDesktopLinkStatus {
    Linked(PathBuf),
    ForeignPreserved(PathBuf),
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDesktopReport {
    pub executable: PathBuf,
    pub helper: Option<PathBuf>,
    pub source: AgentDesktopSource,
    pub link: AgentDesktopLinkStatus,
}

pub fn parse_sha256_hex(hex: &str) -> io::Result<[u8; 32]> {
    let trimmed = hex.trim();
    if trimmed.len() != 64 || !trimmed.is_ascii() {
        return Err(invalid("SHA-256 hex digest must be 64 hex characters"));
    }
    let mut out = [0u8; 32];
    for (slot, chunk) in out.iter_mut().zip(trimmed.as_bytes().as_chunks::<2>().0) {
        let pair = std::str::from_utf8(chunk).map_err(|_| invalid("invalid hex in SHA-256"))?;
        *slot =
            u8::from_str_radix(pair, 16).map_err(|_| invalid("invalid hex digit in SHA-256"))?;
    }
    Ok(out)
}

pub fn verify_archive_sha256(bytes: &[u8], expected: &[u8; 32]) -> io::Result<()> {
    let actual = ring::digest::digest(&ring::digest::SHA256, bytes);
    if actual.as_ref() == expected.as_slice() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "agent-desktop archive SHA-256 mismatch",
        ))
    }
}

pub fn resolve_provision_source(
    asset: Option<&'static AgentDesktopAsset>,
    colocated_dir: Option<PathBuf>,
    bin_override: Option<&OsStr>,
    archive_override: Option<&OsStr>,
    sha256_override: Option<&str>,
    offline_or_isolated: bool,
) -> io::Result<ProvisionSource> {
    let Some(asset) = asset else {
        return Ok(ProvisionSource::UnsupportedPlatform);
    };
    if let Some(dir) = bin_override.filter(|v| !v.is_empty()) {
        let path = PathBuf::from(dir);
        let resolved = if path.file_name().is_some_and(|n| n == asset.main_binary) {
            path.parent().map_or(path.clone(), Path::to_path_buf)
        } else {
            path
        };
        return Ok(ProvisionSource::LocalDir(resolved));
    }
    if let Some(archive) = archive_override.filter(|v| !v.is_empty()) {
        let expected_sha256 = match sha256_override.map(str::trim).filter(|s| !s.is_empty()) {
            Some(hex) => parse_sha256_hex(hex)?,
            None => asset.sha256_bytes,
        };
        return Ok(ProvisionSource::LocalArchive {
            path: PathBuf::from(archive),
            expected_sha256,
        });
    }
    if let Some(dir) = colocated_dir {
        return Ok(ProvisionSource::ColocatedBundle(dir));
    }
    if offline_or_isolated {
        return Ok(ProvisionSource::SkippedOffline);
    }
    Ok(ProvisionSource::RemoteRelease(asset))
}

pub fn stage_agent_desktop(
    krill_exe: Option<&Path>,
    home: &Path,
) -> io::Result<Option<AgentDesktopReport>> {
    let Some(asset) = current_agent_desktop_asset() else {
        return Ok(None);
    };
    let bin_override = std::env::var_os(ENV_BIN_OVERRIDE);
    let archive_override = std::env::var_os(ENV_ARCHIVE_OVERRIDE);
    let sha256_override = std::env::var(ENV_SHA256_OVERRIDE).ok();
    let has_override = bin_override.as_ref().is_some_and(|v| !v.is_empty())
        || archive_override.as_ref().is_some_and(|v| !v.is_empty());
    let bin_dir = crate::user_dirs::share_dir(home).join("bin");
    if !has_override && is_staged_version(&bin_dir, asset) {
        super::skills::ensure_directory_safe(&bin_dir)?;
        let lock_path = bin_dir.join(LOCK_FILE);
        if !super::skills::regular_or_missing(&lock_path)? {
            return Err(invalid("agent-desktop lock must not be a symlink"));
        }
        let staged_main = bin_dir.join(asset.main_binary);
        let staged_helper = asset.colocated_helper.map(|h| bin_dir.join(h));
        let link = ensure_agent_desktop_cli_link(home, &staged_main, None)?;
        return Ok(Some(AgentDesktopReport {
            executable: staged_main,
            helper: staged_helper,
            source: AgentDesktopSource::AlreadyStaged,
            link,
        }));
    }
    let colocated = find_colocated_dir(krill_exe, asset);
    let source = resolve_provision_source(
        Some(asset),
        colocated,
        bin_override.as_deref(),
        archive_override.as_deref(),
        sha256_override.as_deref(),
        true,
    )?;
    match source {
        ProvisionSource::SkippedOffline
        | ProvisionSource::UnsupportedPlatform
        | ProvisionSource::RemoteRelease(_) => Ok(None),
        ProvisionSource::LocalDir(dir) => {
            provision_from_source(home, asset, ProvisionSource::LocalDir(dir), None).map(Some)
        }
        ProvisionSource::ColocatedBundle(dir) => {
            provision_from_source(home, asset, ProvisionSource::ColocatedBundle(dir), None)
                .map(Some)
        }
        ProvisionSource::LocalArchive {
            path,
            expected_sha256,
        } => provision_from_source(
            home,
            asset,
            ProvisionSource::LocalArchive {
                path,
                expected_sha256,
            },
            None,
        )
        .map(Some),
    }
}

pub fn ensure_agent_desktop_installed(
    home: &Path,
    krill_exe: Option<&Path>,
) -> io::Result<AgentDesktopReport> {
    ensure_agent_desktop_installed_with_path(home, krill_exe, None)
}

pub fn ensure_agent_desktop_installed_with_path(
    home: &Path,
    krill_exe: Option<&Path>,
    search_path: Option<&OsStr>,
) -> io::Result<AgentDesktopReport> {
    let bin_dir = crate::user_dirs::share_dir(home).join("bin");
    let Some(asset) = current_agent_desktop_asset() else {
        return Ok(AgentDesktopReport {
            executable: bin_dir.join("agent-desktop"),
            helper: None,
            source: AgentDesktopSource::UnsupportedPlatform,
            link: AgentDesktopLinkStatus::NotApplicable,
        });
    };
    let bin_override = std::env::var_os(ENV_BIN_OVERRIDE);
    let archive_override = std::env::var_os(ENV_ARCHIVE_OVERRIDE);
    let sha256_override = std::env::var(ENV_SHA256_OVERRIDE).ok();
    let has_override = bin_override.as_ref().is_some_and(|v| !v.is_empty())
        || archive_override.as_ref().is_some_and(|v| !v.is_empty());

    super::skills::ensure_directory_safe(&bin_dir)?;
    fs::create_dir_all(&bin_dir)?;
    let lock_path = bin_dir.join(LOCK_FILE);
    if !super::skills::regular_or_missing(&lock_path)? {
        return Err(invalid("agent-desktop lock must not be a symlink"));
    }

    if !has_override && is_staged_version(&bin_dir, asset) {
        let staged_main = bin_dir.join(asset.main_binary);
        let staged_helper = asset.colocated_helper.map(|h| bin_dir.join(h));
        let link = ensure_agent_desktop_cli_link(home, &staged_main, search_path)?;
        return Ok(AgentDesktopReport {
            executable: staged_main,
            helper: staged_helper,
            source: AgentDesktopSource::AlreadyStaged,
            link,
        });
    }

    let offline_or_isolated = is_isolated_or_offline(home);
    let colocated = find_colocated_dir(krill_exe, asset);
    let source = resolve_provision_source(
        Some(asset),
        colocated,
        bin_override.as_deref(),
        archive_override.as_deref(),
        sha256_override.as_deref(),
        offline_or_isolated,
    )?;
    provision_from_source(home, asset, source, search_path)
}

fn is_isolated_or_offline(home: &Path) -> bool {
    if cfg!(test) || std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return true;
    }
    if let Some(override_dir) = crate::daemon::state_dir_override()
        && crate::daemon::default_state_dir().as_deref() != Some(Path::new(&override_dir))
    {
        return true;
    }
    crate::os::home_dir().as_deref() != Some(home)
}

fn provision_from_source(
    home: &Path,
    asset: &'static AgentDesktopAsset,
    source: ProvisionSource,
    search_path: Option<&OsStr>,
) -> io::Result<AgentDesktopReport> {
    let bin_dir = crate::user_dirs::share_dir(home).join("bin");
    super::skills::ensure_directory_safe(&bin_dir)?;
    fs::create_dir_all(&bin_dir)?;
    let staged_main = bin_dir.join(asset.main_binary);
    let staged_helper = asset.colocated_helper.map(|h| bin_dir.join(h));

    match source {
        ProvisionSource::SkippedOffline => Ok(AgentDesktopReport {
            executable: staged_main,
            helper: staged_helper,
            source: AgentDesktopSource::SkippedOffline,
            link: AgentDesktopLinkStatus::NotApplicable,
        }),
        ProvisionSource::UnsupportedPlatform => Ok(AgentDesktopReport {
            executable: staged_main,
            helper: staged_helper,
            source: AgentDesktopSource::UnsupportedPlatform,
            link: AgentDesktopLinkStatus::NotApplicable,
        }),
        ProvisionSource::LocalDir(dir) => {
            let Some(_lock) = try_acquire_lock(&bin_dir)? else {
                return Ok(busy_report(staged_main, staged_helper));
            };
            stage_pair_from_dir(&dir, &bin_dir, asset, asset.version)?;
            let link = ensure_agent_desktop_cli_link(home, &staged_main, search_path)?;
            Ok(AgentDesktopReport {
                executable: staged_main,
                helper: staged_helper,
                source: AgentDesktopSource::LocalOverride,
                link,
            })
        }
        ProvisionSource::ColocatedBundle(dir) => {
            let Some(_lock) = try_acquire_lock(&bin_dir)? else {
                return Ok(busy_report(staged_main, staged_helper));
            };
            stage_pair_from_dir(&dir, &bin_dir, asset, asset.version)?;
            let link = ensure_agent_desktop_cli_link(home, &staged_main, search_path)?;
            Ok(AgentDesktopReport {
                executable: staged_main,
                helper: staged_helper,
                source: AgentDesktopSource::ColocatedBundle,
                link,
            })
        }
        ProvisionSource::LocalArchive {
            path,
            expected_sha256,
        } => {
            let bytes = read_bounded_archive(&path, MAX_AGENT_DESKTOP_ARCHIVE_BYTES)?;
            verify_archive_sha256(&bytes, &expected_sha256)?;
            let Some(_lock) = try_acquire_lock(&bin_dir)? else {
                return Ok(busy_report(staged_main, staged_helper));
            };
            extract_and_stage_archive(&bytes, &bin_dir, asset, asset.archive_name, asset.version)?;
            let link = ensure_agent_desktop_cli_link(home, &staged_main, search_path)?;
            Ok(AgentDesktopReport {
                executable: staged_main,
                helper: staged_helper,
                source: AgentDesktopSource::LocalOverride,
                link,
            })
        }
        ProvisionSource::RemoteRelease(pinned) => {
            let Some(_lock) = try_acquire_lock(&bin_dir)? else {
                return Ok(busy_report(staged_main, staged_helper));
            };
            if !is_staged_version(&bin_dir, pinned) {
                let bytes = download_release_archive(&bin_dir, pinned)?;
                verify_archive_sha256(&bytes, &pinned.sha256_bytes)?;
                extract_and_stage_archive(
                    &bytes,
                    &bin_dir,
                    pinned,
                    pinned.archive_name,
                    pinned.version,
                )?;
            }
            let link = ensure_agent_desktop_cli_link(home, &staged_main, search_path)?;
            Ok(AgentDesktopReport {
                executable: staged_main,
                helper: staged_helper,
                source: AgentDesktopSource::DownloadedRelease,
                link,
            })
        }
    }
}

fn busy_report(executable: PathBuf, helper: Option<PathBuf>) -> AgentDesktopReport {
    AgentDesktopReport {
        executable,
        helper,
        source: AgentDesktopSource::Busy,
        link: AgentDesktopLinkStatus::NotApplicable,
    }
}

fn try_acquire_lock(bin_dir: &Path) -> io::Result<Option<LockGuard>> {
    let lock_path = bin_dir.join(LOCK_FILE);
    if !super::skills::regular_or_missing(&lock_path)? {
        return Err(invalid("agent-desktop lock must not be a symlink"));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(&lock_path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(LockGuard { _file: file })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

fn is_staged_version(bin_dir: &Path, asset: &AgentDesktopAsset) -> bool {
    fs::read_to_string(bin_dir.join(VERSION_STAMP_FILE))
        .is_ok_and(|version| version.trim() == asset.version)
        && has_pair_in_dir(bin_dir, asset)
}

fn is_regular_executable(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find_colocated_dir(krill_exe: Option<&Path>, asset: &AgentDesktopAsset) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(exe) = krill_exe
        && let Some(parent) = exe.parent()
    {
        candidates.push(parent.to_path_buf());
        if let Some(contents) = parent.parent() {
            candidates.push(contents.join("Helpers"));
        }
    }
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        candidates.push(parent.to_path_buf());
        if let Some(contents) = parent.parent() {
            candidates.push(contents.join("Helpers"));
        }
    }
    candidates
        .into_iter()
        .find(|dir| has_pair_in_dir(dir, asset))
}

fn has_pair_in_dir(dir: &Path, asset: &AgentDesktopAsset) -> bool {
    is_regular_executable(&dir.join(asset.main_binary))
        && asset
            .colocated_helper
            .is_none_or(|helper| is_regular_executable(&dir.join(helper)))
}

fn stage_pair_from_dir(
    src_dir: &Path,
    bin_dir: &Path,
    asset: &AgentDesktopAsset,
    version: &str,
) -> io::Result<()> {
    let main_src = src_dir.join(asset.main_binary);
    if !is_regular_executable(&main_src) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing {}", main_src.display()),
        ));
    }
    if let Some(helper) = asset.colocated_helper {
        let helper_src = src_dir.join(helper);
        if !is_regular_executable(&helper_src) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("missing colocated helper {}", helper_src.display()),
            ));
        }
        atomic_copy_executable(&helper_src, &bin_dir.join(helper))?;
    }
    atomic_copy_executable(&main_src, &bin_dir.join(asset.main_binary))?;
    write_version_stamp(bin_dir, version)
}

fn atomic_copy_executable(src: &Path, dst: &Path) -> io::Result<()> {
    if !super::skills::regular_or_missing(dst)? {
        return Err(invalid("staged binary destination must not be a symlink"));
    }
    if fs::canonicalize(src).ok() == fs::canonicalize(dst).ok() && is_regular_executable(dst) {
        return Ok(());
    }
    let parent = dst
        .parent()
        .ok_or_else(|| invalid("staged binary has no parent directory"))?;
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".krill-ad-{}-{seq}.tmp", std::process::id()));
    fs::copy(src, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    }
    let result = fs::rename(&tmp, dst);
    if result.is_err() {
        fs::remove_file(&tmp).ok();
    }
    result
}

fn write_version_stamp(bin_dir: &Path, version: &str) -> io::Result<()> {
    let stamp = bin_dir.join(VERSION_STAMP_FILE);
    if !super::skills::regular_or_missing(&stamp)? {
        return Err(invalid("version stamp must not be a symlink"));
    }
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = bin_dir.join(format!(".krill-ad-ver-{}-{seq}.tmp", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let write_res = file
        .write_all(format!("{version}\n").as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    let rename_res = write_res.and_then(|()| fs::rename(&tmp, &stamp));
    if rename_res.is_err() {
        fs::remove_file(&tmp).ok();
    }
    rename_res
}

fn read_bounded_archive(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(invalid("agent-desktop download exceeds size cap"));
    }
    Ok(bytes)
}

fn download_release_archive(bin_dir: &Path, asset: &AgentDesktopAsset) -> io::Result<Vec<u8>> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_archive = bin_dir.join(format!(".krill-ad-dl-{}-{seq}.tar.gz", std::process::id()));
    let curl_status = crate::os::background_command("curl")
        .args([
            "-fsSL",
            "--retry",
            "2",
            "--connect-timeout",
            "10",
            "--max-time",
            "60",
            "-o",
        ])
        .arg(&tmp_archive)
        .arg(asset.url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    let downloaded = match curl_status {
        Ok(status) if status.success() && tmp_archive.is_file() => true,
        _ => {
            fs::remove_file(&tmp_archive).ok();
            crate::os::background_command("gh")
                .args([
                    "release",
                    "download",
                    AGENT_DESKTOP_TAG,
                    "--repo",
                    AGENT_DESKTOP_REPO,
                    "--pattern",
                    asset.archive_name,
                    "--output",
                ])
                .arg(&tmp_archive)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success() && tmp_archive.is_file())
        }
    };
    if !downloaded {
        fs::remove_file(&tmp_archive).ok();
        return Err(io::Error::other(format!(
            "failed to download {}",
            asset.url
        )));
    }
    let result = read_bounded_archive(&tmp_archive, MAX_AGENT_DESKTOP_ARCHIVE_BYTES);
    fs::remove_file(&tmp_archive).ok();
    result
}

fn extract_and_stage_archive(
    archive_bytes: &[u8],
    bin_dir: &Path,
    asset: &AgentDesktopAsset,
    archive_name: &str,
    version: &str,
) -> io::Result<()> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let work_dir = bin_dir.join(format!(".krill-ad-extract-{}-{seq}", std::process::id()));
    fs::create_dir_all(&work_dir)?;
    let archive_path = work_dir.join(archive_name);
    let result = (|| {
        fs::write(&archive_path, archive_bytes)?;
        let mut tar = crate::os::background_command("tar");
        if cfg!(windows)
            && archive_path.parent() == Some(work_dir.as_path())
            && archive_path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(str::is_ascii)
        {
            tar.current_dir(&work_dir)
                .arg("-xzf")
                .arg(archive_path.file_name().unwrap_or_default())
                .arg("-C")
                .arg(".");
        } else {
            tar.arg("-xzf").arg(&archive_path).arg("-C").arg(&work_dir);
        }
        let out = tar.stdin(Stdio::null()).output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "tar extraction failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        stage_pair_from_dir(&work_dir, bin_dir, asset, version)
    })();
    fs::remove_dir_all(&work_dir).ok();
    result
}

pub fn ensure_agent_desktop_cli_link(
    home: &Path,
    staged_main: &Path,
    search_path: Option<&OsStr>,
) -> io::Result<AgentDesktopLinkStatus> {
    let default_path;
    let path_env = if let Some(p) = search_path {
        p
    } else {
        default_path = std::env::var_os("PATH").unwrap_or_default();
        &default_path
    };
    let bin_name = staged_main
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("agent-desktop");
    let local_bin = home.join(".local/bin");
    let link_path = local_bin.join(bin_name);
    let staged_canon = fs::canonicalize(staged_main).ok();

    if let Some(found) = crate::agent::which("agent-desktop", path_env) {
        let found_canon = fs::canonicalize(&found).ok();
        let is_ours = (staged_canon.is_some() && found_canon == staged_canon) || found == link_path;
        if !is_ours {
            return Ok(AgentDesktopLinkStatus::ForeignPreserved(found));
        }
    }

    super::skills::ensure_directory_safe(&local_bin)?;
    fs::create_dir_all(&local_bin)?;

    #[cfg(unix)]
    {
        match fs::symlink_metadata(&link_path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let raw = fs::read_link(&link_path)?;
                let target = if raw.is_absolute() {
                    raw
                } else {
                    local_bin.join(raw)
                };
                let target_canon = fs::canonicalize(&target).ok();
                let points_to_staged = target == staged_main
                    || (staged_canon.is_some() && target_canon == staged_canon);
                if points_to_staged && link_path.is_file() {
                    return Ok(AgentDesktopLinkStatus::Linked(link_path));
                }
                if target.starts_with(crate::user_dirs::share_dir(home))
                    || fs::metadata(&link_path).is_err()
                {
                    fs::remove_file(&link_path)?;
                } else {
                    return Ok(AgentDesktopLinkStatus::ForeignPreserved(link_path));
                }
            }
            Ok(_) => return Ok(AgentDesktopLinkStatus::ForeignPreserved(link_path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::os::unix::fs::symlink(staged_main, &link_path)?;
        Ok(AgentDesktopLinkStatus::Linked(link_path))
    }

    #[cfg(not(unix))]
    {
        if let Ok(link_meta) = fs::metadata(&link_path) {
            let existing_matches = fs::metadata(staged_main)
                .ok()
                .filter(|staged_meta| staged_meta.len() == link_meta.len())
                .and_then(|_| fs::read(&link_path).ok().zip(fs::read(staged_main).ok()))
                .is_some_and(|(a, b)| a == b);
            if !existing_matches {
                return Ok(AgentDesktopLinkStatus::ForeignPreserved(link_path));
            }
            return Ok(AgentDesktopLinkStatus::Linked(link_path));
        }
        atomic_copy_executable(staged_main, &link_path)?;
        Ok(AgentDesktopLinkStatus::Linked(link_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("krill-ad-test-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pinned_assets_carry_their_version_and_hashes() {
        assert_eq!(PINNED_ASSETS.len(), 6);
        for asset in PINNED_ASSETS {
            let parsed = parse_sha256_hex(asset.sha256_hex).unwrap();
            assert_eq!(parsed, asset.sha256_bytes, "{}", asset.archive_name);
            assert!(asset.url.ends_with(asset.archive_name));
            assert!(
                asset
                    .url
                    .contains(&format!("/releases/download/{AGENT_DESKTOP_TAG}/")),
                "{}",
                asset.url
            );
            assert_eq!(asset.version, AGENT_DESKTOP_VERSION);
        }
    }

    #[test]
    #[cfg(unix)]
    fn only_the_pinned_version_counts_as_installed() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = temp_home("pinned-only");
        let asset = current_agent_desktop_asset().unwrap();
        let bin_dir = crate::user_dirs::share_dir(&home).join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        for name in std::iter::once(asset.main_binary).chain(asset.colocated_helper) {
            let path = bin_dir.join(name);
            fs::write(&path, b"#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(bin_dir.join(".agent-desktop.checked"), "0\n").unwrap();

        fs::write(bin_dir.join(VERSION_STAMP_FILE), "9.9.9\n").unwrap();
        let report =
            ensure_agent_desktop_installed_with_path(&home, None, Some(OsStr::new(""))).unwrap();
        assert_ne!(
            report.source,
            AgentDesktopSource::AlreadyStaged,
            "a version not pinned in source must not be trusted"
        );

        fs::write(
            bin_dir.join(VERSION_STAMP_FILE),
            format!("{AGENT_DESKTOP_VERSION}\n"),
        )
        .unwrap();
        let report =
            ensure_agent_desktop_installed_with_path(&home, None, Some(OsStr::new(""))).unwrap();
        assert_eq!(report.source, AgentDesktopSource::AlreadyStaged);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn verify_archive_sha256_accepts_matching_and_rejects_tampered_bytes() {
        let payload = b"agent-desktop-test-archive-payload";
        let digest = ring::digest::digest(&ring::digest::SHA256, payload);
        let mut expected = [0u8; 32];
        expected.copy_from_slice(digest.as_ref());
        verify_archive_sha256(payload, &expected).unwrap();

        let mut tampered = expected;
        *tampered.first_mut().unwrap() ^= 0xff;
        let err = verify_archive_sha256(payload, &tampered).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn resolve_provision_source_honors_override_precedence_and_offline_gate() {
        let asset = PINNED_ASSETS.first().unwrap();
        let src = resolve_provision_source(
            Some(asset),
            Some(PathBuf::from("/bundle/Helpers")),
            Some(OsStr::new("/custom/bin/agent-desktop")),
            Some(OsStr::new("/custom/archive.tar.gz")),
            None,
            true,
        )
        .unwrap();
        assert_eq!(src, ProvisionSource::LocalDir(PathBuf::from("/custom/bin")));

        let archive_src = resolve_provision_source(
            Some(asset),
            Some(PathBuf::from("/bundle/Helpers")),
            None,
            Some(OsStr::new("/custom/archive.tar.gz")),
            None,
            true,
        )
        .unwrap();
        assert_eq!(
            archive_src,
            ProvisionSource::LocalArchive {
                path: PathBuf::from("/custom/archive.tar.gz"),
                expected_sha256: asset.sha256_bytes,
            }
        );

        let bundle_src = resolve_provision_source(
            Some(asset),
            Some(PathBuf::from("/bundle/Helpers")),
            None,
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(
            bundle_src,
            ProvisionSource::ColocatedBundle(PathBuf::from("/bundle/Helpers"))
        );

        let offline = resolve_provision_source(Some(asset), None, None, None, None, true).unwrap();
        assert_eq!(offline, ProvisionSource::SkippedOffline);

        let remote = resolve_provision_source(Some(asset), None, None, None, None, false).unwrap();
        assert_eq!(remote, ProvisionSource::RemoteRelease(asset));
    }

    #[test]
    #[cfg(unix)]
    fn archive_provisioning_stages_colocated_helper_links_path_and_preserves_foreign_cli() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;

        let root = temp_home("archive-stage");
        let home = root.join("home");
        let src_tar_dir = root.join("tar-src");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&src_tar_dir).unwrap();

        let asset = current_agent_desktop_asset().unwrap();
        let main_path = src_tar_dir.join(asset.main_binary);
        fs::write(&main_path, b"#!/bin/sh\necho 1.0.2\n").unwrap();
        fs::set_permissions(&main_path, fs::Permissions::from_mode(0o755)).unwrap();

        if let Some(helper) = asset.colocated_helper {
            let helper_path = src_tar_dir.join(helper);
            fs::write(&helper_path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let archive_path = root.join(asset.archive_name);
        let mut tar_cmd = Command::new("tar");
        tar_cmd
            .arg("-czf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&src_tar_dir)
            .arg(asset.main_binary);
        if let Some(helper) = asset.colocated_helper {
            tar_cmd.arg(helper);
        }
        assert!(tar_cmd.status().unwrap().success());

        let archive_bytes = fs::read(&archive_path).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, &archive_bytes);
        let mut expected_sha = [0u8; 32];
        expected_sha.copy_from_slice(digest.as_ref());

        let mut bad_sha = expected_sha;
        *bad_sha.first_mut().unwrap() ^= 1;
        let bad = provision_from_source(
            &home,
            asset,
            ProvisionSource::LocalArchive {
                path: archive_path.clone(),
                expected_sha256: bad_sha,
            },
            Some(OsStr::new("")),
        );
        assert!(bad.is_err());

        let report = provision_from_source(
            &home,
            asset,
            ProvisionSource::LocalArchive {
                path: archive_path,
                expected_sha256: expected_sha,
            },
            Some(OsStr::new("")),
        )
        .unwrap();
        assert_eq!(report.source, AgentDesktopSource::LocalOverride);
        assert!(is_regular_executable(&report.executable));
        if let Some(helper_path) = &report.helper {
            assert!(is_regular_executable(helper_path));
            assert_eq!(helper_path.parent(), report.executable.parent());
        }
        let link_path = home.join(".local/bin").join(asset.main_binary);
        assert_eq!(
            report.link,
            AgentDesktopLinkStatus::Linked(link_path.clone())
        );
        assert_eq!(
            fs::canonicalize(&link_path).unwrap(),
            fs::canonicalize(&report.executable).unwrap()
        );

        let foreign_dir = root.join("foreign-bin");
        fs::create_dir_all(&foreign_dir).unwrap();
        let foreign_bin = foreign_dir.join(asset.main_binary);
        fs::write(&foreign_bin, b"#!/bin/sh\necho foreign\n").unwrap();
        fs::set_permissions(&foreign_bin, fs::Permissions::from_mode(0o755)).unwrap();
        let foreign_status =
            ensure_agent_desktop_cli_link(&home, &report.executable, Some(foreign_dir.as_os_str()))
                .unwrap();
        assert_eq!(
            foreign_status,
            AgentDesktopLinkStatus::ForeignPreserved(foreign_bin)
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stage_agent_desktop_skips_recopy_when_colocated_version_is_already_staged() {
        let root = temp_home("colocated-idempotent");
        let home = root.join("home");
        let bundle = root.join("bundle");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bundle).unwrap();

        let Some(asset) = current_agent_desktop_asset() else {
            fs::remove_dir_all(root).ok();
            return;
        };
        let fake_krill = bundle.join(if cfg!(windows) {
            "krilld.exe"
        } else {
            "krilld"
        });
        fs::write(&fake_krill, b"krilld").unwrap();
        for name in std::iter::once(asset.main_binary).chain(asset.colocated_helper) {
            let path = bundle.join(name);
            fs::write(&path, b"colocated-binary-payload").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let first = stage_agent_desktop(Some(&fake_krill), &home)
            .unwrap()
            .unwrap();
        assert_eq!(first.source, AgentDesktopSource::ColocatedBundle);

        let seq_before = TEMP_SEQ.load(Ordering::Relaxed);
        let second = stage_agent_desktop(Some(&fake_krill), &home)
            .unwrap()
            .unwrap();
        assert_eq!(second.source, AgentDesktopSource::AlreadyStaged);
        assert_eq!(
            TEMP_SEQ.load(Ordering::Relaxed),
            seq_before,
            "already-staged binary must not be copied or rewritten again"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, windows))]
#[test]
fn windows_extract_archive_in_unicode_directory() {
    let root = std::env::temp_dir().join(format!("krill 한글 경로 tar {}", std::process::id()));
    let source = root.join("source");
    let bin = root.join("bin");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let asset = current_agent_desktop_asset().unwrap();
    fs::write(source.join(asset.main_binary), b"test executable").unwrap();
    let status = std::process::Command::new("tar")
        .current_dir(&root)
        .args(["-czf", "archive.tar.gz", "-C", "source", asset.main_binary])
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = fs::read(root.join("archive.tar.gz")).unwrap();
    extract_and_stage_archive(&bytes, &bin, asset, asset.archive_name, asset.version).unwrap();
    assert_eq!(
        fs::read(bin.join(asset.main_binary)).unwrap(),
        b"test executable"
    );
    fs::remove_dir_all(root).unwrap();
}
