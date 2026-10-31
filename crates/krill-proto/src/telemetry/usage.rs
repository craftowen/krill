//! Closed install/usage wire schema. No workspace, account, input or path fields.

use std::fmt::Write as _;
use std::io;

use ring::rand::SecureRandom as _;
use serde::{Deserialize, Serialize};

pub const ENDPOINT: &str = "https://telemetry.krill.page/v1/telemetry";
pub const ENDPOINT_VAR: &str = "KRILL_TELEMETRY_ENDPOINT";
pub const MAX_BATCH: usize = 50;
pub const MAX_PENDING: usize = 128;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageName {
    FirstOpen,
    AppOpened,
    ActiveDay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageEvent {
    pub id: String,
    pub install_id: String,
    pub name: UsageName,
    pub ts_unix: u64,
    pub app_version: String,
    pub os: String,
    pub arch: String,
}

impl UsageEvent {
    pub(super) fn new(install_id: &str, name: UsageName, ts_unix: u64) -> io::Result<Self> {
        let event = Self {
            id: random_uuid()?,
            install_id: install_id.to_owned(),
            name,
            ts_unix,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        };
        if !event.valid() {
            return Err(io::Error::other(
                "unsupported telemetry platform or version",
            ));
        }
        Ok(event)
    }

    pub fn valid(&self) -> bool {
        valid_uuid(&self.id)
            && valid_uuid(&self.install_id)
            && valid_version(&self.app_version)
            && matches!(self.os.as_str(), "macos" | "windows" | "linux")
            && matches!(self.arch.as_str(), "aarch64" | "x86_64")
            && self.ts_unix <= 253_402_300_799
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub v: u32,
    pub events: Vec<UsageEvent>,
}
impl Batch {
    pub fn valid(&self) -> bool {
        self.v == 1
            && !self.events.is_empty()
            && self.events.len() <= MAX_BATCH
            && self.events.iter().all(UsageEvent::valid)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub accepted: usize,
}

/// Random `UUIDv4`, persisted independently of account/device identity.
pub fn random_uuid() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("telemetry random source unavailable"))?;
    let mut output = String::with_capacity(36);
    for (i, mut byte) in bytes.into_iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            output.push('-');
        }
        if i == 6 {
            byte = (byte & 0x0f) | 0x40;
        }
        if i == 8 {
            byte = (byte & 0x3f) | 0x80;
        }
        write!(output, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(output)
}

pub(super) fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            14 => b == b'4',
            19 => matches!(b, b'8' | b'9' | b'a' | b'b'),
            _ => b.is_ascii_digit() || matches!(b, b'a'..=b'f'),
        })
}

fn valid_version(value: &str) -> bool {
    static PATTERN: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    value.len() <= 64
        && PATTERN
            .get_or_init(|| {
                regex::RegexBuilder::new(
                    r"\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\z",
                )
                .unicode(false)
                .build()
                .ok()
            })
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(value))
}
