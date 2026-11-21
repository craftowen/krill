// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Pure-Rust sanitized `.zip` bot export/import and PNG metadata stripper (zero extra dependencies).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::types::{
    BotConfig, BotLookAroundConfig, BotMcpAuthKind, BotMemoryTidyConfig, BotNotifications,
    BotScheduleConfig, BotSlug, ExternalSource, is_windows_reserved_slug, validate_bot_name,
    validate_bot_slug,
};

/// Current manifest schema version inside exported bot packages.
pub const BOT_PACKAGE_SCHEMA_VERSION: u32 = 1;
/// Maximum number of entries allowed in a bot package archive.
pub const MAX_ZIP_ENTRIES: usize = 256;
/// Maximum single uncompressed file size in a bot package archive (4 MiB).
pub const MAX_ZIP_ENTRY_BYTES: usize = 4 * 1024 * 1024;
/// Maximum total uncompressed size across all entries in a bot package archive (16 MiB).
pub const MAX_ZIP_TOTAL_BYTES: usize = 16 * 1024 * 1024;
/// Maximum raw `.zip` archive size accepted by [`import_bot_zip`] (16 MiB).
pub const MAX_ZIP_ARCHIVE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum PNG avatar byte size accepted by [`strip_png_metadata`] (4 MiB).
pub const MAX_AVATAR_PNG_BYTES: usize = 4 * 1024 * 1024;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const ZIP_LOCAL_FILE_HEADER_SIG: u32 = 0x0403_4b50;
const ZIP_CENTRAL_DIR_HEADER_SIG: u32 = 0x0201_4b50;
const ZIP_EOCD_SIG: u32 = 0x0605_4b50;
const ZIP_VERSION_STORE: u16 = 20;
const ZIP_FLAG_UTF8: u16 = 0x0800;
const ZIP_METHOD_STORE: u16 = 0;

/// Errors raised during bot package export, import, or PNG metadata stripping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    InvalidArchive(String),
    UnsupportedCompression(u16),
    UnsafePath(String),
    DisallowedPath(String),
    DuplicateEntry(String),
    LimitExceeded(String),
    CrcMismatch {
        path: String,
        expected: u32,
        actual: u32,
    },
    InvalidManifest(String),
    UnsupportedSchemaVersion(u32),
    MissingRequiredFile(&'static str),
    InvalidUtf8(String),
    InvalidPng(String),
    Io(String),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArchive(msg) => write!(f, "invalid zip archive: {msg}"),
            Self::UnsupportedCompression(method) => {
                write!(
                    f,
                    "unsupported zip compression method {method} (only Store=0 supported)"
                )
            }
            Self::UnsafePath(path) => write!(f, "unsafe archive path rejected: {path}"),
            Self::DisallowedPath(path) => write!(f, "disallowed bot package path: {path}"),
            Self::DuplicateEntry(path) => write!(f, "duplicate archive entry: {path}"),
            Self::LimitExceeded(msg) => write!(f, "package limit exceeded: {msg}"),
            Self::CrcMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "CRC32 mismatch for '{path}': expected {expected:#010x}, got {actual:#010x}"
            ),
            Self::InvalidManifest(msg) => write!(f, "invalid bot package manifest: {msg}"),
            Self::UnsupportedSchemaVersion(v) => write!(
                f,
                "bot package schema version {v} is newer than supported version {BOT_PACKAGE_SCHEMA_VERSION}"
            ),
            Self::MissingRequiredFile(name) => write!(f, "missing required package file: {name}"),
            Self::InvalidUtf8(path) => write!(f, "file '{path}' is not valid UTF-8"),
            Self::InvalidPng(msg) => write!(f, "invalid PNG image: {msg}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for PackageError {}

/// Options controlling optional files included in a bot `.zip` export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPackageExportOptions {
    #[serde(default)]
    pub include_memory: bool,
    #[serde(default = "default_true")]
    pub include_avatar: bool,
}

const fn default_true() -> bool {
    true
}

/// Sanitized MCP tool entry written into an exported package (no secret refs, no URL query strings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedMcpTool {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub auth_kind: BotMcpAuthKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_names: Vec<String>,
}

/// Sanitized `bot.json` stored inside exported `.zip` packages.
/// Explicitly excludes all `secret_ref`s, API tokens, channel/chat/user IDs, and host filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedBotManifest {
    pub schema_version: u32,
    pub slug: BotSlug,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_color: Option<String>,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission_mode: crate::agent::Permission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    #[serde(default)]
    pub notifications: BotNotifications,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<BotScheduleConfig>,
    #[serde(default)]
    pub look_around: BotLookAroundConfig,
    #[serde(default)]
    pub memory_tidy: BotMemoryTidyConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<SanitizedMcpTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connector_kinds: Vec<ExternalSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

impl SanitizedBotManifest {
    /// Builds a secret-free portable manifest from a live [`BotConfig`].
    #[must_use]
    pub fn from_config(config: &BotConfig, has_avatar_png: bool) -> Self {
        let schedules = config
            .schedules
            .iter()
            .cloned()
            .map(|mut s| {
                // Imported schedules start disabled so recipients explicitly opt in.
                s.enabled = false;
                s
            })
            .collect();
        let mcp_tools = config
            .mcp_tools
            .iter()
            .filter_map(|tool| {
                // Exclude local arbitrary commands when sharing; export HTTP MCP URLs without query/credentials.
                let endpoint_url = tool.endpoint_url.as_deref().map(strip_url_query_and_auth);
                endpoint_url.as_ref()?;
                Some(SanitizedMcpTool {
                    id: tool.id.clone(),
                    name: tool.name.clone(),
                    endpoint_url,
                    auth_kind: tool.auth_kind,
                    header_names: tool.header_names.clone(),
                })
            })
            .collect();
        let avatar = if has_avatar_png {
            Some("avatar.png".to_owned())
        } else if config.avatar.as_deref() == Some("avatar.png") {
            None
        } else {
            config.avatar.clone()
        };

        Self {
            schema_version: BOT_PACKAGE_SCHEMA_VERSION,
            slug: config.slug.clone(),
            name: config.name.clone(),
            description: config.description.clone(),
            emoji: config.emoji.clone(),
            avatar,
            avatar_color: config.avatar_color.clone(),
            agent_id: config.agent_id.clone(),
            model: config.model.clone(),
            effort: config.effort.clone(),
            permission_mode: config.permission_mode,
            template_id: config.template_id.clone(),
            notifications: config.notifications,
            schedules,
            look_around: config.look_around.clone(),
            memory_tidy: config.memory_tidy,
            mcp_tools,
            connector_kinds: config.connectors.enabled_sources(),
            skills: config.skills.clone(),
        }
    }
}

/// Decoded contents of an imported bot `.zip` package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotPackageData {
    pub manifest: SanitizedBotManifest,
    pub soul: String,
    /// Relative paths under `skills/`, e.g. `"pr-review-checklist/SKILL.md"`.
    pub skills: BTreeMap<String, String>,
    pub memory_md: Option<String>,
    /// Topic files under `memory/`, keyed by topic filename (e.g. `"repos.md"`).
    pub memory_topics: BTreeMap<String, String>,
    /// Sanitized `avatar.png` bytes (metadata chunks already stripped).
    pub avatar_png: Option<Vec<u8>>,
}

/// Computes standard IEEE 802.3 CRC-32 (`0xEDB8_8320` reflected polynomial).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        crc = (crc >> 4) ^ crc32_nibble(crc & 0x0f);
        crc = (crc >> 4) ^ crc32_nibble(crc & 0x0f);
    }
    !crc
}

const fn crc32_nibble(nibble: u32) -> u32 {
    match nibble & 0x0f {
        0 => 0x0000_0000,
        1 => 0x1db7_1064,
        2 => 0x3b6e_20c8,
        3 => 0x26d9_30ac,
        4 => 0x76dc_4190,
        5 => 0x6b6b_51f4,
        6 => 0x4db2_6158,
        7 => 0x5005_713c,
        8 => 0xedb8_8320,
        9 => 0xf00f_9344,
        10 => 0xd6d6_a3e8,
        11 => 0xcb61_b38c,
        12 => 0x9b64_c2b0,
        13 => 0x86d3_d2d4,
        14 => 0xa00a_e278,
        _ => 0xbdbd_f21c,
    }
}

/// Pure-Rust PNG metadata stripper.
///
/// Validates the PNG signature, chunk structure, `IHDR`/`IDAT`/`IEND` invariants, and every chunk's
/// CRC-32, then emits a clean PNG keeping ONLY critical + transparency chunks (`IHDR`, `PLTE`,
/// `tRNS`, `IDAT`, `IEND`). All ancillary metadata chunks (`tEXt`, `iTXt`, `zTXt`, `eXIf`, `tIME`,
/// `pHYs`, `iCCP`, etc.) are stripped.
pub fn strip_png_metadata(bytes: &[u8]) -> Result<Vec<u8>, PackageError> {
    if bytes.len() > MAX_AVATAR_PNG_BYTES {
        return Err(PackageError::LimitExceeded(format!(
            "PNG exceeds {MAX_AVATAR_PNG_BYTES} bytes"
        )));
    }
    if bytes.get(..8) != Some(PNG_SIGNATURE.as_slice()) {
        return Err(PackageError::InvalidPng("missing PNG signature".to_owned()));
    }

    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&PNG_SIGNATURE);

    let mut pos = 8usize;
    let mut chunk_index = 0usize;
    let mut seen_idat = false;
    let mut seen_iend = false;

    while pos < bytes.len() {
        let header_end = pos
            .checked_add(8)
            .ok_or_else(|| PackageError::InvalidPng("chunk header overflow".to_owned()))?;
        let header = bytes
            .get(pos..header_end)
            .ok_or_else(|| PackageError::InvalidPng("truncated chunk header".to_owned()))?;

        let len_bytes: [u8; 4] = header
            .get(0..4)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| PackageError::InvalidPng("missing chunk length".to_owned()))?;
        let chunk_len = usize::try_from(u32::from_be_bytes(len_bytes)).map_err(|_| {
            PackageError::InvalidPng("chunk length exceeds addressable memory".to_owned())
        })?;
        let chunk_type: [u8; 4] = header
            .get(4..8)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| PackageError::InvalidPng("missing chunk type".to_owned()))?;

        if !chunk_type.iter().all(u8::is_ascii_alphabetic) {
            return Err(PackageError::InvalidPng(
                "non-alphabetic PNG chunk type".to_owned(),
            ));
        }

        let data_end = header_end
            .checked_add(chunk_len)
            .ok_or_else(|| PackageError::InvalidPng("chunk data length overflow".to_owned()))?;
        let crc_end = data_end
            .checked_add(4)
            .ok_or_else(|| PackageError::InvalidPng("chunk CRC offset overflow".to_owned()))?;
        let type_and_data = bytes
            .get(pos.saturating_add(4)..data_end)
            .ok_or_else(|| PackageError::InvalidPng("truncated chunk payload".to_owned()))?;
        let crc_slice = bytes
            .get(data_end..crc_end)
            .ok_or_else(|| PackageError::InvalidPng("truncated chunk CRC".to_owned()))?;
        let crc_arr: [u8; 4] = crc_slice
            .try_into()
            .map_err(|_| PackageError::InvalidPng("invalid chunk CRC slice".to_owned()))?;
        let expected_crc = u32::from_be_bytes(crc_arr);
        let computed_crc = crc32(type_and_data);
        if computed_crc != expected_crc {
            return Err(PackageError::InvalidPng(format!(
                "CRC mismatch in chunk {}",
                String::from_utf8_lossy(&chunk_type)
            )));
        }

        if chunk_index == 0 {
            if &chunk_type != b"IHDR" || chunk_len != 13 {
                return Err(PackageError::InvalidPng(
                    "first chunk must be a 13-byte IHDR".to_owned(),
                ));
            }
        } else if &chunk_type == b"IHDR" {
            return Err(PackageError::InvalidPng("duplicate IHDR chunk".to_owned()));
        }

        let keep = matches!(&chunk_type, b"IHDR" | b"PLTE" | b"tRNS" | b"IDAT" | b"IEND");
        if &chunk_type == b"IDAT" {
            seen_idat = true;
        }
        if &chunk_type == b"IEND" {
            if chunk_len != 0 {
                return Err(PackageError::InvalidPng(
                    "IEND chunk must be empty".to_owned(),
                ));
            }
            seen_iend = true;
        }

        if keep {
            out.extend_from_slice(&len_bytes);
            out.extend_from_slice(type_and_data);
            out.extend_from_slice(&computed_crc.to_be_bytes());
        }

        pos = crc_end;
        chunk_index = chunk_index.saturating_add(1);
        if seen_iend {
            break;
        }
    }

    if !seen_idat {
        return Err(PackageError::InvalidPng("PNG has no IDAT chunk".to_owned()));
    }
    if !seen_iend {
        return Err(PackageError::InvalidPng(
            "PNG missing IEND chunk".to_owned(),
        ));
    }

    Ok(out)
}

struct CentralEntry {
    name: Vec<u8>,
    crc: u32,
    size: u32,
    local_offset: u32,
}

/// Writes a pure-Rust Store-mode (method 0) PKZIP archive from sorted `(path, bytes)` entries.
pub fn write_store_zip(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, PackageError> {
    if entries.len() > MAX_ZIP_ENTRIES {
        return Err(PackageError::LimitExceeded(format!(
            "archive has {} entries (max {MAX_ZIP_ENTRIES})",
            entries.len()
        )));
    }

    let mut total_uncompressed = 0usize;
    for (name, data) in entries {
        validate_zip_entry_path(name)?;
        if data.len() > MAX_ZIP_ENTRY_BYTES {
            return Err(PackageError::LimitExceeded(format!(
                "entry '{name}' exceeds {MAX_ZIP_ENTRY_BYTES} bytes"
            )));
        }
        total_uncompressed = total_uncompressed
            .checked_add(data.len())
            .ok_or_else(|| PackageError::LimitExceeded("total size overflow".to_owned()))?;
        if total_uncompressed > MAX_ZIP_TOTAL_BYTES {
            return Err(PackageError::LimitExceeded(format!(
                "total archive payload exceeds {MAX_ZIP_TOTAL_BYTES} bytes"
            )));
        }
    }

    let mut out = Vec::new();
    let mut central = Vec::with_capacity(entries.len());

    for (name, data) in entries {
        let name_bytes = name.as_bytes();
        let name_len =
            u16::try_from(name_bytes.len()).map_err(|_| PackageError::UnsafePath(name.clone()))?;
        let data_len = u32::try_from(data.len())
            .map_err(|_| PackageError::LimitExceeded("entry exceeds 4 GiB".to_owned()))?;
        let local_offset = u32::try_from(out.len())
            .map_err(|_| PackageError::LimitExceeded("archive exceeds 4 GiB".to_owned()))?;
        let checksum = crc32(data);

        out.extend_from_slice(&ZIP_LOCAL_FILE_HEADER_SIG.to_le_bytes());
        out.extend_from_slice(&ZIP_VERSION_STORE.to_le_bytes());
        out.extend_from_slice(&ZIP_FLAG_UTF8.to_le_bytes());
        out.extend_from_slice(&ZIP_METHOD_STORE.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&checksum.to_le_bytes());
        out.extend_from_slice(&data_len.to_le_bytes()); // compressed size
        out.extend_from_slice(&data_len.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        central.push(CentralEntry {
            name: name_bytes.to_vec(),
            crc: checksum,
            size: data_len,
            local_offset,
        });
    }

    let cd_offset = u32::try_from(out.len())
        .map_err(|_| PackageError::LimitExceeded("central directory offset overflow".to_owned()))?;
    let cd_start = out.len();

    for entry in &central {
        let name_len = u16::try_from(entry.name.len())
            .map_err(|_| PackageError::LimitExceeded("name length overflow".to_owned()))?;
        out.extend_from_slice(&ZIP_CENTRAL_DIR_HEADER_SIG.to_le_bytes());
        out.extend_from_slice(&ZIP_VERSION_STORE.to_le_bytes()); // version made by
        out.extend_from_slice(&ZIP_VERSION_STORE.to_le_bytes()); // version needed
        out.extend_from_slice(&ZIP_FLAG_UTF8.to_le_bytes());
        out.extend_from_slice(&ZIP_METHOD_STORE.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&entry.local_offset.to_le_bytes());
        out.extend_from_slice(&entry.name);
    }

    let cd_size = u32::try_from(out.len().saturating_sub(cd_start))
        .map_err(|_| PackageError::LimitExceeded("central directory size overflow".to_owned()))?;
    let entry_count = u16::try_from(central.len())
        .map_err(|_| PackageError::LimitExceeded("entry count overflow".to_owned()))?;

    out.extend_from_slice(&ZIP_EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
    out.extend_from_slice(&entry_count.to_le_bytes());
    out.extend_from_slice(&entry_count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length

    Ok(out)
}

/// Reads and strictly validates a Store-mode PKZIP archive into a map of `(path, bytes)`.
pub fn read_store_zip(archive: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, PackageError> {
    if archive.len() > MAX_ZIP_ARCHIVE_BYTES {
        return Err(PackageError::LimitExceeded(format!(
            "archive exceeds {MAX_ZIP_ARCHIVE_BYTES} bytes"
        )));
    }
    if archive.len() < 22 {
        return Err(PackageError::InvalidArchive(
            "archive shorter than EOCD record".to_owned(),
        ));
    }

    let eocd_pos = find_eocd(archive)
        .ok_or_else(|| PackageError::InvalidArchive("missing EOCD signature".to_owned()))?;

    let disk_num = read_u16_le(archive, eocd_pos.saturating_add(4))?;
    let cd_disk = read_u16_le(archive, eocd_pos.saturating_add(6))?;
    let entries_disk = read_u16_le(archive, eocd_pos.saturating_add(8))?;
    let entries_total = read_u16_le(archive, eocd_pos.saturating_add(10))?;
    let cd_size = read_u32_le(archive, eocd_pos.saturating_add(12))?;
    let cd_offset = read_u32_le(archive, eocd_pos.saturating_add(16))?;

    if disk_num != 0 || cd_disk != 0 || entries_disk != entries_total {
        return Err(PackageError::InvalidArchive(
            "multi-disk zip archives are not supported".to_owned(),
        ));
    }
    let count = usize::from(entries_total);
    if count > MAX_ZIP_ENTRIES {
        return Err(PackageError::LimitExceeded(format!(
            "archive contains {count} entries (max {MAX_ZIP_ENTRIES})"
        )));
    }

    let cd_start = usize::try_from(cd_offset).map_err(|_| {
        PackageError::InvalidArchive("central directory offset overflow".to_owned())
    })?;
    let cd_len = usize::try_from(cd_size)
        .map_err(|_| PackageError::InvalidArchive("central directory size overflow".to_owned()))?;
    let cd_end = cd_start.checked_add(cd_len).ok_or_else(|| {
        PackageError::InvalidArchive("central directory bounds overflow".to_owned())
    })?;
    if cd_end > eocd_pos {
        return Err(PackageError::InvalidArchive(
            "central directory extends past EOCD".to_owned(),
        ));
    }

    let mut pos = cd_start;
    let mut out = BTreeMap::new();
    let mut total_uncompressed = 0usize;

    for _ in 0..count {
        let sig = read_u32_le(archive, pos)?;
        if sig != ZIP_CENTRAL_DIR_HEADER_SIG {
            return Err(PackageError::InvalidArchive(
                "invalid central directory header signature".to_owned(),
            ));
        }
        let method = read_u16_le(archive, pos.saturating_add(10))?;
        if method != ZIP_METHOD_STORE {
            return Err(PackageError::UnsupportedCompression(method));
        }
        let expected_crc = read_u32_le(archive, pos.saturating_add(16))?;
        let comp_size = read_u32_le(archive, pos.saturating_add(20))?;
        let uncomp_size = read_u32_le(archive, pos.saturating_add(24))?;
        if comp_size != uncomp_size {
            return Err(PackageError::InvalidArchive(
                "store-mode entry compressed and uncompressed sizes differ".to_owned(),
            ));
        }
        let name_len = usize::from(read_u16_le(archive, pos.saturating_add(28))?);
        let extra_len = usize::from(read_u16_le(archive, pos.saturating_add(30))?);
        let comment_len = usize::from(read_u16_le(archive, pos.saturating_add(32))?);
        let local_offset = usize::try_from(read_u32_le(archive, pos.saturating_add(42))?)
            .map_err(|_| PackageError::InvalidArchive("local offset overflow".to_owned()))?;

        let name_start = pos
            .checked_add(46)
            .ok_or_else(|| PackageError::InvalidArchive("CD header overflow".to_owned()))?;
        let name_end = name_start
            .checked_add(name_len)
            .ok_or_else(|| PackageError::InvalidArchive("CD name overflow".to_owned()))?;
        let next_pos = name_end
            .checked_add(extra_len)
            .and_then(|v| v.checked_add(comment_len))
            .ok_or_else(|| PackageError::InvalidArchive("CD entry tail overflow".to_owned()))?;
        if next_pos > cd_end {
            return Err(PackageError::InvalidArchive(
                "central directory entry truncated".to_owned(),
            ));
        }

        let name_slice = archive
            .get(name_start..name_end)
            .ok_or_else(|| PackageError::InvalidArchive("truncated CD filename".to_owned()))?;
        let name = std::str::from_utf8(name_slice)
            .map_err(|_| PackageError::InvalidUtf8("<entry name>".to_owned()))?
            .to_owned();
        validate_zip_entry_path(&name)?;

        let size = usize::try_from(uncomp_size)
            .map_err(|_| PackageError::LimitExceeded("entry size overflow".to_owned()))?;
        if size > MAX_ZIP_ENTRY_BYTES {
            return Err(PackageError::LimitExceeded(format!(
                "entry '{name}' exceeds {MAX_ZIP_ENTRY_BYTES} bytes"
            )));
        }
        total_uncompressed = total_uncompressed
            .checked_add(size)
            .ok_or_else(|| PackageError::LimitExceeded("total size overflow".to_owned()))?;
        if total_uncompressed > MAX_ZIP_TOTAL_BYTES {
            return Err(PackageError::LimitExceeded(format!(
                "total uncompressed size exceeds {MAX_ZIP_TOTAL_BYTES} bytes"
            )));
        }

        // Validate matching local file header.
        let local_sig = read_u32_le(archive, local_offset)?;
        if local_sig != ZIP_LOCAL_FILE_HEADER_SIG {
            return Err(PackageError::InvalidArchive(format!(
                "missing local header for '{name}'"
            )));
        }
        let local_method = read_u16_le(archive, local_offset.saturating_add(8))?;
        if local_method != ZIP_METHOD_STORE {
            return Err(PackageError::UnsupportedCompression(local_method));
        }
        let local_name_len = usize::from(read_u16_le(archive, local_offset.saturating_add(26))?);
        let local_extra_len = usize::from(read_u16_le(archive, local_offset.saturating_add(28))?);
        let local_name_start = local_offset
            .checked_add(30)
            .ok_or_else(|| PackageError::InvalidArchive("local header overflow".to_owned()))?;
        let local_name_end = local_name_start
            .checked_add(local_name_len)
            .ok_or_else(|| PackageError::InvalidArchive("local name overflow".to_owned()))?;
        let local_name_slice = archive
            .get(local_name_start..local_name_end)
            .ok_or_else(|| PackageError::InvalidArchive("truncated local filename".to_owned()))?;
        if local_name_slice != name_slice {
            return Err(PackageError::InvalidArchive(format!(
                "local header filename mismatch for '{name}'"
            )));
        }

        let data_start = local_name_end
            .checked_add(local_extra_len)
            .ok_or_else(|| PackageError::InvalidArchive("local extra overflow".to_owned()))?;
        let data_end = data_start
            .checked_add(size)
            .ok_or_else(|| PackageError::InvalidArchive("entry data overflow".to_owned()))?;
        if data_end > cd_start {
            return Err(PackageError::InvalidArchive(format!(
                "entry '{name}' overlaps central directory"
            )));
        }
        let data = archive.get(data_start..data_end).ok_or_else(|| {
            PackageError::InvalidArchive(format!("truncated payload for '{name}'"))
        })?;

        let actual_crc = crc32(data);
        if actual_crc != expected_crc {
            return Err(PackageError::CrcMismatch {
                path: name,
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        if out.insert(name.clone(), data.to_vec()).is_some() {
            return Err(PackageError::DuplicateEntry(name));
        }

        pos = next_pos;
    }

    if pos != cd_end {
        return Err(PackageError::InvalidArchive(
            "unexpected trailing bytes in central directory".to_owned(),
        ));
    }

    Ok(out)
}

/// Exports a sanitized bot `.zip` package in memory (Store-mode PKZIP, stripped PNG avatar,
/// secret-free `bot.json`, `SOUL.md`, `skills/`, and optional `MEMORY.md` + `memory/`).
pub fn export_bot_zip(
    config: &BotConfig,
    soul: &str,
    skills: &BTreeMap<String, String>,
    memory_md: Option<&str>,
    memory_topics: &BTreeMap<String, String>,
    avatar_png: Option<&[u8]>,
    options: BotPackageExportOptions,
) -> Result<Vec<u8>, PackageError> {
    config
        .validate()
        .map_err(|err| PackageError::InvalidManifest(err.to_string()))?;

    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let clean_avatar = if options.include_avatar {
        match avatar_png {
            Some(raw) => Some(strip_png_metadata(raw)?),
            None => None,
        }
    } else {
        None
    };

    let manifest = SanitizedBotManifest::from_config(config, clean_avatar.is_some());
    let mut manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|err| PackageError::InvalidManifest(err.to_string()))?;
    manifest_json.push('\n');
    entries.insert("bot.json".to_owned(), manifest_json.into_bytes());
    entries.insert("SOUL.md".to_owned(), soul.as_bytes().to_vec());

    for (rel_path, content) in skills {
        let archive_path = format!("skills/{rel_path}");
        validate_bot_package_path(&archive_path)?;
        entries.insert(archive_path, content.as_bytes().to_vec());
    }

    if options.include_memory {
        if let Some(mem) = memory_md {
            entries.insert("MEMORY.md".to_owned(), mem.as_bytes().to_vec());
        }
        for (topic_file, content) in memory_topics {
            let archive_path = format!("memory/{topic_file}");
            validate_bot_package_path(&archive_path)?;
            entries.insert(archive_path, content.as_bytes().to_vec());
        }
    }

    if let Some(png) = clean_avatar {
        entries.insert("avatar.png".to_owned(), png);
    }

    write_store_zip(&entries)
}

/// Convenience helper that reads a bot's home folder from disk and exports a sanitized `.zip`.
pub fn export_bot_home_zip(
    home: &Path,
    config: &BotConfig,
    options: BotPackageExportOptions,
) -> Result<Vec<u8>, PackageError> {
    ensure_no_symlink(home, "bot home")?;
    let soul = read_bounded_utf8_file(&home.join("SOUL.md"), "SOUL.md", MAX_ZIP_ENTRY_BYTES)?;

    let mut skills = BTreeMap::new();
    let skills_dir = home.join("skills");
    if let Some(meta) = symlink_metadata_optional(&skills_dir, "skills/")?
        && meta.is_dir()
    {
        collect_text_files(&skills_dir, &skills_dir, &mut skills)?;
    }

    let memory_md = if options.include_memory {
        read_optional_bounded_utf8_file(&home.join("MEMORY.md"), "MEMORY.md", MAX_ZIP_ENTRY_BYTES)?
    } else {
        None
    };

    let mut memory_topics = BTreeMap::new();
    if options.include_memory {
        let memory_dir = home.join("memory");
        if let Some(meta) = symlink_metadata_optional(&memory_dir, "memory/")?
            && meta.is_dir()
        {
            let dir_iter = fs::read_dir(&memory_dir)
                .map_err(|err| PackageError::Io(format!("read memory/: {err}")))?;
            for entry in dir_iter {
                let entry = entry.map_err(|err| PackageError::Io(err.to_string()))?;
                let path = entry.path();
                let entry_meta =
                    fs::symlink_metadata(&path).map_err(|err| PackageError::Io(err.to_string()))?;
                if entry_meta.file_type().is_symlink() || !entry_meta.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                {
                    let content = read_bounded_utf8_file(&path, &name, MAX_ZIP_ENTRY_BYTES)?;
                    memory_topics.insert(name, content);
                }
            }
        }
    }

    let avatar_png = if options.include_avatar {
        read_optional_bounded_file_bytes(
            &home.join("avatar.png"),
            "avatar.png",
            MAX_AVATAR_PNG_BYTES,
        )?
    } else {
        None
    };

    export_bot_zip(
        config,
        &soul,
        &skills,
        memory_md.as_deref(),
        &memory_topics,
        avatar_png.as_deref(),
        options,
    )
}

/// Imports and strictly validates a sanitized bot `.zip` package.
pub fn import_bot_zip(archive: &[u8]) -> Result<BotPackageData, PackageError> {
    let entries = read_store_zip(archive)?;
    for path in entries.keys() {
        validate_bot_package_path(path)?;
    }

    let manifest_bytes = entries
        .get("bot.json")
        .ok_or(PackageError::MissingRequiredFile("bot.json"))?;
    let manifest_text = std::str::from_utf8(manifest_bytes)
        .map_err(|_| PackageError::InvalidUtf8("bot.json".to_owned()))?;

    let raw_value: serde_json::Value = serde_json::from_str(manifest_text)
        .map_err(|err| PackageError::InvalidManifest(err.to_string()))?;
    if let Some(version) = raw_value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        && version > u64::from(BOT_PACKAGE_SCHEMA_VERSION)
    {
        return Err(PackageError::UnsupportedSchemaVersion(
            u32::try_from(version).unwrap_or(u32::MAX),
        ));
    }

    let manifest: SanitizedBotManifest = serde_json::from_value(raw_value)
        .map_err(|err| PackageError::InvalidManifest(err.to_string()))?;
    if manifest.schema_version == 0 || manifest.schema_version > BOT_PACKAGE_SCHEMA_VERSION {
        return Err(PackageError::UnsupportedSchemaVersion(
            manifest.schema_version,
        ));
    }
    validate_bot_slug(manifest.slug.as_str())
        .map_err(|err| PackageError::InvalidManifest(err.to_string()))?;
    validate_bot_name(&manifest.name)
        .map_err(|err| PackageError::InvalidManifest(err.to_string()))?;

    let soul_bytes = entries
        .get("SOUL.md")
        .ok_or(PackageError::MissingRequiredFile("SOUL.md"))?;
    let soul = std::str::from_utf8(soul_bytes)
        .map_err(|_| PackageError::InvalidUtf8("SOUL.md".to_owned()))?
        .to_owned();

    let memory_md = match entries.get("MEMORY.md") {
        Some(bytes) => Some(
            std::str::from_utf8(bytes)
                .map_err(|_| PackageError::InvalidUtf8("MEMORY.md".to_owned()))?
                .to_owned(),
        ),
        None => None,
    };

    let avatar_png = match entries.get("avatar.png") {
        Some(bytes) => Some(strip_png_metadata(bytes)?),
        None => None,
    };

    let mut skills = BTreeMap::new();
    let mut memory_topics = BTreeMap::new();
    for (path, bytes) in &entries {
        if let Some(rel) = path.strip_prefix("skills/") {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| PackageError::InvalidUtf8(path.clone()))?
                .to_owned();
            skills.insert(rel.to_owned(), text);
        } else if let Some(rel) = path.strip_prefix("memory/") {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| PackageError::InvalidUtf8(path.clone()))?
                .to_owned();
            memory_topics.insert(rel.to_owned(), text);
        }
    }

    Ok(BotPackageData {
        manifest,
        soul,
        skills,
        memory_md,
        memory_topics,
        avatar_png,
    })
}

/// Validates a raw ZIP archive entry path against zip-slip / traversal / control bytes.
pub fn validate_zip_entry_path(path: &str) -> Result<(), PackageError> {
    if path.is_empty()
        || path.len() > 512
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.chars().any(char::is_control)
    {
        return Err(PackageError::UnsafePath(path.to_owned()));
    }
    // Reject Windows drive-letter prefixes such as `C:...`.
    if path.as_bytes().get(1) == Some(&b':') {
        return Err(PackageError::UnsafePath(path.to_owned()));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(PackageError::UnsafePath(path.to_owned()));
        }
    }
    Ok(())
}

/// Validates that a zip path belongs to the strict bot package allow-list.
pub fn validate_bot_package_path(path: &str) -> Result<(), PackageError> {
    validate_zip_entry_path(path)?;
    if matches!(path, "bot.json" | "SOUL.md" | "MEMORY.md" | "avatar.png") {
        return Ok(());
    }
    if let Some(rest) = path.strip_prefix("memory/") {
        if !rest.contains('/')
            && let Some(stem) = rest.strip_suffix(".md")
            && is_valid_memory_topic_stem(stem)
        {
            return Ok(());
        }
        return Err(PackageError::DisallowedPath(path.to_owned()));
    }
    if let Some(rest) = path.strip_prefix("skills/") {
        let mut parts = rest.splitn(2, '/');
        if let (Some(skill_name), Some(inner)) = (parts.next(), parts.next())
            && validate_bot_slug(skill_name).is_ok()
            && !inner.is_empty()
        {
            return Ok(());
        }
        return Err(PackageError::DisallowedPath(path.to_owned()));
    }
    Err(PackageError::DisallowedPath(path.to_owned()))
}

fn collect_text_files(
    root: &Path,
    current: &Path,
    out: &mut BTreeMap<String, String>,
) -> Result<(), PackageError> {
    let mut visited = BTreeSet::new();
    collect_text_files_inner(root, current, out, &mut visited, 0)
}

fn collect_text_files_inner(
    root: &Path,
    current: &Path,
    out: &mut BTreeMap<String, String>,
    visited: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), PackageError> {
    if depth > 8 {
        return Err(PackageError::LimitExceeded(
            "skills directory nesting too deep".to_owned(),
        ));
    }
    let iter = fs::read_dir(current)
        .map_err(|err| PackageError::Io(format!("read {}: {err}", current.display())))?;
    for entry in iter {
        let entry = entry.map_err(|err| PackageError::Io(err.to_string()))?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|err| PackageError::Io(err.to_string()))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_text_files_inner(root, &path, out, visited, depth.saturating_add(1))?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|err| PackageError::Io(err.to_string()))?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !visited.insert(rel_str.clone()) {
                continue;
            }
            if out.len() >= MAX_ZIP_ENTRIES {
                return Err(PackageError::LimitExceeded(
                    "too many files under skills/".to_owned(),
                ));
            }
            let text = read_bounded_utf8_file(&path, &rel_str, MAX_ZIP_ENTRY_BYTES)?;
            out.insert(rel_str, text);
        }
    }
    Ok(())
}

/// Scans exported bot text (`SOUL.md`, `MEMORY.md`, skills, etc.) for common leaked key prefixes
/// and Telegram bot token shapes, returning matching secret-type identifiers.
#[must_use]
pub fn scan_export_secrets(text: &str) -> Vec<&'static str> {
    static PATTERNS: LazyLock<[(&'static str, Option<Regex>); 10]> = LazyLock::new(|| {
        [
            (
                "sk-",
                Regex::new(r"(?:^|[^A-Za-z0-9_])sk-[A-Za-z0-9_-]{6,}").ok(),
            ),
            (
                "xoxb-",
                Regex::new(r"(?:^|[^A-Za-z0-9_])xoxb-[A-Za-z0-9-]{6,}").ok(),
            ),
            (
                "xoxp-",
                Regex::new(r"(?:^|[^A-Za-z0-9_])xoxp-[A-Za-z0-9-]{6,}").ok(),
            ),
            (
                "xapp-",
                Regex::new(r"(?:^|[^A-Za-z0-9_])xapp-[A-Za-z0-9-]{6,}").ok(),
            ),
            (
                "ghp_",
                Regex::new(r"(?:^|[^A-Za-z0-9_])ghp_[A-Za-z0-9]{6,}").ok(),
            ),
            (
                "gho_",
                Regex::new(r"(?:^|[^A-Za-z0-9_])gho_[A-Za-z0-9]{6,}").ok(),
            ),
            (
                "lin_api_",
                Regex::new(r"(?:^|[^A-Za-z0-9_])lin_api_[A-Za-z0-9]{6,}").ok(),
            ),
            (
                "ya29.",
                Regex::new(r"(?:^|[^A-Za-z0-9_])ya29\.[A-Za-z0-9._-]{6,}").ok(),
            ),
            (
                "1//",
                Regex::new(r"(?:^|[^A-Za-z0-9_/])1//[A-Za-z0-9_-]{6,}").ok(),
            ),
            (
                "telegram_bot_token",
                Regex::new(r"(?:^|[^A-Za-z0-9_:])[0-9]{6,}:[A-Za-z0-9_-]{35,}").ok(),
            ),
        ]
    });

    let mut found = Vec::new();
    for (label, maybe_re) in PATTERNS.iter() {
        if let Some(re) = maybe_re
            && re.is_match(text)
        {
            found.push(*label);
        }
    }
    found
}

fn is_valid_memory_topic_stem(stem: &str) -> bool {
    if stem.is_empty()
        || stem.len() > 128
        || stem.starts_with('.')
        || stem.starts_with(' ')
        || stem.ends_with(' ')
        || stem.contains("..")
    {
        return false;
    }
    let trimmed = stem.trim();
    if trimmed.is_empty() {
        return false;
    }
    if is_windows_reserved_slug(&trimmed.to_ascii_lowercase()) {
        return false;
    }
    stem.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
}

fn ensure_no_symlink(path: &Path, label: &str) -> Result<(), PackageError> {
    let meta = fs::symlink_metadata(path)
        .map_err(|err| PackageError::Io(format!("stat {label}: {err}")))?;
    if meta.file_type().is_symlink() {
        return Err(PackageError::UnsafePath(label.to_owned()));
    }
    Ok(())
}

fn symlink_metadata_optional(
    path: &Path,
    label: &str,
) -> Result<Option<fs::Metadata>, PackageError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(PackageError::UnsafePath(label.to_owned()));
            }
            Ok(Some(meta))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(PackageError::Io(format!("stat {label}: {err}"))),
    }
}

fn read_bounded_file_bytes(
    path: &Path,
    label: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, PackageError> {
    let meta = fs::symlink_metadata(path)
        .map_err(|err| PackageError::Io(format!("read {label}: {err}")))?;
    read_meta_bounded_bytes(path, &meta, label, max_bytes)
}

fn read_optional_bounded_file_bytes(
    path: &Path,
    label: &str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, PackageError> {
    let Some(meta) = symlink_metadata_optional(path, label)? else {
        return Ok(None);
    };
    read_meta_bounded_bytes(path, &meta, label, max_bytes).map(Some)
}

fn read_meta_bounded_bytes(
    path: &Path,
    meta: &fs::Metadata,
    label: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, PackageError> {
    if meta.file_type().is_symlink() {
        return Err(PackageError::UnsafePath(label.to_owned()));
    }
    if !meta.is_file() {
        return Err(PackageError::Io(format!("'{label}' is not a regular file")));
    }
    let max_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if meta.len() > max_u64 {
        return Err(PackageError::LimitExceeded(format!(
            "'{label}' exceeds {max_bytes} bytes"
        )));
    }
    let file =
        fs::File::open(path).map_err(|err| PackageError::Io(format!("read {label}: {err}")))?;
    let mut buf = Vec::new();
    file.take(max_u64.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|err| PackageError::Io(format!("read {label}: {err}")))?;
    if buf.len() > max_bytes {
        return Err(PackageError::LimitExceeded(format!(
            "'{label}' exceeds {max_bytes} bytes"
        )));
    }
    Ok(buf)
}

fn read_bounded_utf8_file(
    path: &Path,
    label: &str,
    max_bytes: usize,
) -> Result<String, PackageError> {
    let bytes = read_bounded_file_bytes(path, label, max_bytes)?;
    String::from_utf8(bytes).map_err(|_| PackageError::InvalidUtf8(label.to_owned()))
}

fn read_optional_bounded_utf8_file(
    path: &Path,
    label: &str,
    max_bytes: usize,
) -> Result<Option<String>, PackageError> {
    let Some(bytes) = read_optional_bounded_file_bytes(path, label, max_bytes)? else {
        return Ok(None);
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| PackageError::InvalidUtf8(label.to_owned()))
}

fn strip_url_query_and_auth(url: &str) -> String {
    let without_frag = url.split('#').next().unwrap_or(url);
    let without_query = without_frag.split('?').next().unwrap_or(without_frag);
    if let Some((scheme, rest)) = without_query.split_once("://") {
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let authority = rest.get(..authority_end).unwrap_or_default();
        let path = rest.get(authority_end..).unwrap_or_default();
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        format!("{scheme}://{host}{path}")
    } else {
        without_query.to_owned()
    }
}

fn find_eocd(archive: &[u8]) -> Option<usize> {
    let max_back = archive.len().min(65_557);
    let min_pos = archive.len().saturating_sub(max_back);
    let mut pos = archive.len().checked_sub(22)?;
    loop {
        if read_u32_le(archive, pos).ok() == Some(ZIP_EOCD_SIG) {
            let comment_len = usize::from(read_u16_le(archive, pos.saturating_add(20)).ok()?);
            if pos.saturating_add(22).saturating_add(comment_len) == archive.len() {
                return Some(pos);
            }
        }
        if pos <= min_pos {
            return None;
        }
        pos = pos.saturating_sub(1);
    }
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, PackageError> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| PackageError::InvalidArchive("u16 offset overflow".to_owned()))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| PackageError::InvalidArchive("unexpected EOF reading u16".to_owned()))?;
    let arr: [u8; 2] = slice
        .try_into()
        .map_err(|_| PackageError::InvalidArchive("invalid u16 slice".to_owned()))?;
    Ok(u16::from_le_bytes(arr))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, PackageError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| PackageError::InvalidArchive("u32 offset overflow".to_owned()))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| PackageError::InvalidArchive("unexpected EOF reading u32".to_owned()))?;
    let arr: [u8; 4] = slice
        .try_into()
        .map_err(|_| PackageError::InvalidArchive("invalid u32 slice".to_owned()))?;
    Ok(u32::from_le_bytes(arr))
}
