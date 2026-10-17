//! `link/config.toml` 설정 모델 및 원자적 읽기·쓰기.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use iroh::{RelayMode, RelayUrl};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::store::write_atomic_0600;

/// 기본 설정 파일 상대 경로 (`<state_dir>/link/config.toml`).
pub const CONFIG_REL_PATH: &str = "link/config.toml";

/// 중계(Relay) 동작 모드.
///
/// - `N0`: 기본값. n0 공용 중계 서버를 사용한다.
/// - `Custom`: 사용자가 지정한 자체 `iroh-relay` URL을 사용한다.
/// - `Off`: 중계 서버를 끄고 직접(LAN/루프백/공인 IP) UDP 경로만 사용한다.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RelaySetting {
    #[default]
    N0,
    Custom(String),
    Off,
}

impl RelaySetting {
    /// 상태 보고 및 직렬화용 문자열 표현.
    pub fn as_str(&self) -> &str {
        match self {
            Self::N0 => "n0",
            Self::Custom(url) => url.as_str(),
            Self::Off => "off",
        }
    }

    /// `iroh::RelayMode`로 변환한다.
    pub fn to_iroh_relay_mode(&self) -> Result<RelayMode, Error> {
        match self {
            Self::N0 => Ok(RelayMode::Default),
            Self::Off => Ok(RelayMode::Disabled),
            Self::Custom(url) => {
                let parsed: RelayUrl = url.parse().map_err(|e| {
                    Error::InvalidConfig(format!("invalid custom relay url {url:?}: {e}"))
                })?;
                Ok(RelayMode::custom([parsed]))
            }
        }
    }
}

impl fmt::Display for RelaySetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::N0 => f.write_str("n0"),
            Self::Custom(url) => write!(f, "custom({url})"),
            Self::Off => f.write_str("off"),
        }
    }
}

/// `link/config.toml` 직렬화 구조체.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkConfig {
    /// 중계 서버 모드 (기본 `RelaySetting::N0`).
    pub relay: RelaySetting,
    /// 서버 이름 (짝짓기 시 클라이언트에 돌려줄 표시 이름).
    pub server_name: Option<String>,
    /// 고정 바인드 UDP 포트 (생략 시 이전 포트 재사용 또는 동적 할당).
    pub bind_port: Option<u16>,
    /// 같은 Wi-Fi / LAN 내 로컬 주소 탐색 활성화 여부 (기본 `true`).
    pub lan_discovery: bool,
    /// Maximum network outage before closing this client connection.
    pub resume_grace_ms: u64,
    /// Unacknowledged bytes per logical stream; process-wide cap is separate.
    pub replay_buffer_bytes: usize,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            relay: RelaySetting::N0,
            server_name: None,
            bind_port: None,
            lan_discovery: true,
            resume_grace_ms: 120_000,
            replay_buffer_bytes: crate::resume::DEFAULT_STREAM_BUFFER_LIMIT,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct RawLinkConfig {
    #[serde(default)]
    relay: Option<RawRelayField>,
    #[serde(default)]
    relay_mode: Option<String>,
    #[serde(default)]
    relay_url: Option<String>,
    #[serde(default)]
    server_name: Option<String>,
    #[serde(default)]
    bind_port: Option<u16>,
    #[serde(default)]
    lan_discovery: Option<bool>,
    #[serde(default)]
    resume_grace_ms: Option<u64>,
    #[serde(default)]
    replay_buffer_bytes: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum RawRelayField {
    Mode(String),
    Table {
        mode: String,
        #[serde(default)]
        url: Option<String>,
    },
}

impl LinkConfig {
    pub fn validate(&self) -> Result<(), Error> {
        if !(1..=600_000).contains(&self.resume_grace_ms)
            || !(1..=crate::resume::PROCESS_BUFFER_LIMIT).contains(&self.replay_buffer_bytes)
        {
            return Err(Error::InvalidConfig(
                crate::messages::INVALID_RESUME_SETTINGS.to_owned(),
            ));
        }
        Ok(())
    }

    /// 설정 파일 경로 (`<state_dir>/link/config.toml`).
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join(CONFIG_REL_PATH)
    }

    /// `<state_dir>/link/config.toml`을 읽는다. 파일이 없으면 기본 설정을 돌려준다.
    pub fn load(state_dir: &Path) -> Result<Self, Error> {
        let path = Self::path(state_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).map_err(Error::Io)?;
        Self::parse_toml(&text)
    }

    /// TOML 문자열에서 `LinkConfig`를 파싱한다.
    pub fn parse_toml(text: &str) -> Result<Self, Error> {
        let raw: RawLinkConfig = toml::from_str(text)
            .map_err(|e| Error::InvalidConfig(format!("failed to parse config.toml: {e}")))?;

        let relay = match (raw.relay, raw.relay_mode, raw.relay_url) {
            (Some(RawRelayField::Mode(s)), _, url_opt) => parse_mode_and_url(&s, url_opt)?,
            (Some(RawRelayField::Table { mode, url }), _, fallback_url) => {
                parse_mode_and_url(&mode, url.or(fallback_url))?
            }
            (None, Some(mode), url_opt) => parse_mode_and_url(&mode, url_opt)?,
            (None, None, Some(url)) if !url.trim().is_empty() => {
                parse_mode_and_url("custom", Some(url))?
            }
            (None, None, _) => RelaySetting::N0,
        };

        let config = Self {
            relay,
            server_name: raw
                .server_name
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            bind_port: raw.bind_port.filter(|&p| p > 0),
            lan_discovery: raw.lan_discovery.unwrap_or(true),
            resume_grace_ms: raw.resume_grace_ms.unwrap_or(120_000),
            replay_buffer_bytes: raw
                .replay_buffer_bytes
                .unwrap_or(crate::resume::DEFAULT_STREAM_BUFFER_LIMIT),
        };
        config.validate()?;
        Ok(config)
    }

    /// TOML 문자열로 직렬화한다.
    pub fn to_toml_string(&self) -> Result<String, Error> {
        let (relay_mode, relay_url) = match &self.relay {
            RelaySetting::N0 => ("n0".to_owned(), None),
            RelaySetting::Off => ("off".to_owned(), None),
            RelaySetting::Custom(url) => ("custom".to_owned(), Some(url.clone())),
        };
        let raw = RawLinkConfig {
            relay: None,
            relay_mode: Some(relay_mode),
            relay_url,
            server_name: self.server_name.clone(),
            bind_port: self.bind_port,
            lan_discovery: Some(self.lan_discovery),
            resume_grace_ms: Some(self.resume_grace_ms),
            replay_buffer_bytes: Some(self.replay_buffer_bytes),
        };
        toml::to_string_pretty(&raw)
            .map_err(|e| Error::InvalidConfig(format!("failed to serialize config.toml: {e}")))
    }

    /// `<state_dir>/link/config.toml`에 원자적으로(0600) 저장한다.
    pub fn save(&self, state_dir: &Path) -> Result<(), Error> {
        self.validate()?;
        // 저장 전 relay URL 유효성 검증
        let _ = self.relay.to_iroh_relay_mode()?;
        let content = self.to_toml_string()?;
        let path = Self::path(state_dir);
        write_atomic_0600(&path, content.as_bytes()).map_err(Error::Io)
    }

    /// 표시할 서버 이름. 설정값이 없으면 호스트명 또는 기본 이름을 돌려준다.
    pub fn effective_server_name(&self) -> String {
        if let Some(name) = &self.server_name
            && !name.trim().is_empty()
        {
            return name.trim().to_owned();
        }
        if let Ok(out) = std::process::Command::new("/bin/hostname")
            .arg("-s")
            .output()
            && out.status.success()
            && let Ok(name) = String::from_utf8(out.stdout)
        {
            let trimmed = name.trim().trim_end_matches(".local");
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
        "minikrill-mac".to_owned()
    }
}

fn parse_mode_and_url(mode: &str, url: Option<String>) -> Result<RelaySetting, Error> {
    let trimmed = mode.trim();
    match trimmed {
        "n0" | "default" => Ok(RelaySetting::N0),
        "off" | "disabled" | "none" => Ok(RelaySetting::Off),
        "custom" => {
            let u = url
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::InvalidConfig("relay mode 'custom' requires relay_url".to_owned())
                })?;
            let setting = RelaySetting::Custom(u);
            let _ = setting.to_iroh_relay_mode()?;
            Ok(setting)
        }
        other if other.starts_with("http://") || other.starts_with("https://") => {
            let setting = RelaySetting::Custom(other.to_owned());
            let _ = setting.to_iroh_relay_mode()?;
            Ok(setting)
        }
        other => Err(Error::InvalidConfig(format!(
            "unknown relay mode {other:?} (expected 'n0', 'custom', or 'off')"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_all_relay_modes() {
        let dir = std::env::temp_dir().join(format!("mklink-cfg-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();

        let def = LinkConfig::load(&dir).unwrap();
        assert_eq!(def.relay, RelaySetting::N0);
        assert!(def.lan_discovery);

        let off_cfg = LinkConfig {
            relay: RelaySetting::Off,
            server_name: Some("home-mac-mini".to_owned()),
            bind_port: Some(41234),
            lan_discovery: true,
            resume_grace_ms: 120_000,
            replay_buffer_bytes: crate::resume::DEFAULT_STREAM_BUFFER_LIMIT,
        };
        off_cfg.save(&dir).unwrap();
        let loaded = LinkConfig::load(&dir).unwrap();
        assert_eq!(loaded, off_cfg);

        let custom_cfg = LinkConfig {
            relay: RelaySetting::Custom("https://relay.example.com.".to_owned()),
            server_name: None,
            bind_port: None,
            lan_discovery: false,
            ..LinkConfig::default()
        };
        custom_cfg.save(&dir).unwrap();
        let loaded_custom = LinkConfig::load(&dir).unwrap();
        assert_eq!(loaded_custom, custom_cfg);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_accepts_shorthand_and_table_formats() {
        let c1 = LinkConfig::parse_toml("relay = \"off\"\n").unwrap();
        assert_eq!(c1.relay, RelaySetting::Off);

        let c2 = LinkConfig::parse_toml(
            "[relay]\nmode = \"custom\"\nurl = \"https://relay.example.com.\"\n",
        )
        .unwrap();
        assert_eq!(
            c2.relay,
            RelaySetting::Custom("https://relay.example.com.".to_owned())
        );

        assert!(LinkConfig::parse_toml("relay = \"invalid-mode\"\n").is_err());
        assert!(LinkConfig::parse_toml("relay_mode = \"custom\"\n").is_err());
    }
}
