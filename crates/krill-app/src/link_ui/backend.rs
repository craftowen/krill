//! 기기 짝짓기 백엔드 경계(`LinkBackend`)와 테스트·미리보기용 `MockLinkBackend`(MK-133b, OWE-669).
//!
//! 모든 백엔드 호출은 UI 스레드 블로킹을 막기 위해 `cx.background_executor()`에서 실행한다(CONVENTIONS T7).
//! 실제 구현은 `real::RealLinkBackend`, 인메모리 구현은 `MockLinkBackend`다.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::qr::{DefaultQrMatrix, QrMatrixSource as _};
use crate::i18n::link_ui as text;

/// 초대 링크 기본 유효 시간(10분 = 600초).
pub const DEFAULT_INVITE_TTL_SECS: u64 = 600;

/// 초대 링크 URL 접두어.
pub const INVITE_SCHEME_PREFIX: &str = "krill://pair";

/// "이 Mac에 다른 기기 접속 허용" 서버 상태.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatus {
    /// 들어오는 접속 허용 여부. 다른 Mac으로 나가는 연결 수요와는 별개다.
    pub enabled: bool,
}

/// 1회용 초대 링크와 만료 시각.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// `krill://pair?...` 초대 링크.
    pub link: String,
    /// 만료 시각(Unix epoch 초).
    pub expires_at: u64,
}

impl Invite {
    /// 현재 시각(`now_secs`) 기준으로 만료되었는지 판정한다.
    #[must_use]
    pub const fn is_expired(&self, now_secs: u64) -> bool {
        now_secs >= self.expires_at
    }

    /// 현재 시각(`now_secs`) 기준 남은 유효 시간(초)을 돌려준다.
    #[must_use]
    pub const fn remaining_secs(&self, now_secs: u64) -> u64 {
        self.expires_at.saturating_sub(now_secs)
    }
}

/// 짝지은 기기 한 대의 표시 정보.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    /// 기기 고유 식별자(회수 호출용; 화면에는 기술 식별자를 노출하지 않는다).
    pub id: String,
    /// 기기 표시 이름(예: "오웬의 `MacBook` Air", "집 맥미니").
    pub name: String,
    /// 짝짓기 추가 시각(Unix epoch 초).
    pub added_at: u64,
    /// 마지막 접속 시각(Unix epoch 초, 접속 기록이 없으면 `None`).
    pub last_seen_at: Option<u64>,
}

/// 이 Mac에서 초대 링크로 등록한 원격 Mac. 수신 접근 권한 목록과는 별개다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedHost {
    pub id: String,
    pub name: String,
    pub status: krill_hosts::HostStatus,
}

/// 같은 Wi-Fi에서 발견된 근처 Mac 항목.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NearbyMac {
    /// 근처 기기 식별자.
    pub id: String,
    /// 근처 Mac 표시 이름(예: "거실 맥미니").
    pub name: String,
    /// 선택 시 채워 넣을 초대 링크(`krill://pair?...`).
    pub invite_link: String,
}

/// 클라이언트 짝짓기 실패 사유(비개발자 안내 문구와 1:1 대응).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairFailure {
    /// 만료된 링크.
    ExpiredLink,
    /// 이미 쓴 1회용 링크.
    AlreadyUsedLink,
    /// 상대 Mac이 꺼져 있거나 접속 허용이 꺼져 있음.
    RemoteMacOffline,
    /// 인터넷(네트워크) 연결 없음.
    NoInternet,
    /// 올바른 `krill://pair` 초대 링크 형식이 아님.
    InvalidLink,
    RateLimited,
    Unavailable,
}

impl PairFailure {
    /// 실패 사유에 대응하는 다국어 카탈로그 [`crate::i18n::Message`]를 돌려준다.
    #[must_use]
    pub const fn message(self) -> crate::i18n::Message {
        match self {
            Self::ExpiredLink => text::ERR_EXPIRED_LINK,
            Self::AlreadyUsedLink => text::ERR_ALREADY_USED_LINK,
            Self::RemoteMacOffline => text::ERR_REMOTE_MAC_OFFLINE,
            Self::NoInternet => text::ERR_NO_INTERNET,
            Self::InvalidLink => text::ERR_INVALID_LINK,
            Self::RateLimited => text::ERR_RATE_LIMITED,
            Self::Unavailable => text::ERR_UNAVAILABLE,
        }
    }

    /// 현재 활성 언어의 실패 안내 문구를 돌려준다.
    #[must_use]
    pub fn text(self) -> &'static str {
        self.message().get()
    }

    /// 테스트 선택자용 짧은 식별자.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ExpiredLink => "expired",
            Self::AlreadyUsedLink => "already-used",
            Self::RemoteMacOffline => "offline",
            Self::NoInternet => "no-internet",
            Self::InvalidLink => "invalid",
            Self::RateLimited => "rate-limited",
            Self::Unavailable => "unavailable",
        }
    }
}

/// `LinkBackend::pair(link, device_name)` 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairOutcome {
    /// 짝짓기 완료.
    Paired(PairedDevice),
    /// 짝짓기 실패 및 비개발자 한국어/영어 사유.
    Failed(PairFailure),
}

/// 기기 짝짓기 백엔드 경계.
///
/// 모든 메서드는 UI 스레드가 아닌 백그라운드 실행기(`cx.background_executor()`)에서 호출한다.
pub trait LinkBackend: Send + Sync + std::fmt::Debug {
    /// "이 Mac에 다른 기기 접속 허용"을 켠다.
    fn enable_server(&self) -> Result<ServerStatus, String>;
    /// "이 Mac에 다른 기기 접속 허용"을 끄고 수신 서버를 정리한다.
    fn disable_server(&self) -> Result<ServerStatus, String>;
    /// 현재 서버 허용 상태를 조회한다.
    fn server_status(&self) -> Result<ServerStatus, String>;
    /// 새 1회용 초대 링크와 만료 시각을 발급한다.
    fn create_invite(&self) -> Result<Invite, String>;
    /// 초대 링크와 이 기기 이름으로 상대 Mac과 짝짓기를 시도한다. `host_name`이 비어 있지
    /// 않으면 상대 Mac이 알려준 이름 대신 이 이름으로 원격 Mac을 저장한다.
    fn pair(&self, link: &str, device_name: &str, host_name: &str) -> PairOutcome;
    /// 현재 짝지어진 기기 목록을 조회한다.
    fn devices(&self) -> Result<Vec<PairedDevice>, String>;
    /// 이 Mac이 연결한 원격 Mac 목록과 현재 연결 상태. 조회만 하며 터널을 시작하지 않는다.
    fn remote_hosts(&self) -> Result<Vec<PairedHost>, String> {
        Ok(Vec::new())
    }
    /// 등록된 원격 Mac 연결을 제거한다. 상대 Mac의 세션은 계속 실행된다.
    fn disconnect_host(&self, _id: &str) -> Result<Vec<PairedHost>, String> {
        Err(text::ERR_DISCONNECT.get().to_owned())
    }
    /// 지정한 기기의 연결 권한을 회수(연결 끊기)하고 갱신된 기기 목록을 돌려준다.
    fn revoke(&self, id: &str) -> Result<Vec<PairedDevice>, String>;
    /// 같은 Wi-Fi의 근처 Mac 목록을 조회한다.
    fn nearby(&self) -> Result<Vec<NearbyMac>, String>;
    /// 초대 링크에 대한 QR 모듈 행렬(`Vec<Vec<bool>>`)을 생성한다.
    fn qr_matrix(&self, link: &str) -> Result<Vec<Vec<bool>>, String> {
        DefaultQrMatrix.encode(link)
    }
}

#[derive(Debug, Clone)]
struct MockState {
    enabled: bool,
    now_secs: u64,
    invite_ttl_secs: u64,
    next_seq: u64,
    active_invite: Option<Invite>,
    used_links: BTreeSet<String>,
    devices: Vec<PairedDevice>,
    nearby: Vec<NearbyMac>,
    forced_outcome: Option<PairOutcome>,
}

/// 테스트·화면 미리보기용 인메모리 `LinkBackend` 구현체.
#[derive(Debug)]
pub struct MockLinkBackend {
    state: Mutex<MockState>,
}

impl Default for MockLinkBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockLinkBackend {
    /// 초기 상태(접속 허용 꺼짐, 기기 0대)의 모의 백엔드를 만든다.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MockState {
                enabled: false,
                now_secs: current_epoch_secs(),
                invite_ttl_secs: DEFAULT_INVITE_TTL_SECS,
                next_seq: 1,
                active_invite: None,
                used_links: BTreeSet::new(),
                devices: Vec::new(),
                nearby: Vec::new(),
                forced_outcome: None,
            }),
        }
    }

    /// 미리보기·스크린샷 촬영용 예시 데이터가 채워진 모의 백엔드를 만든다.
    #[must_use]
    pub fn with_demo_data() -> Self {
        let now = current_epoch_secs();
        let invite = Invite {
            link: "krill://pair?code=home-mac-mini-7f3a9c2e".to_owned(),
            expires_at: now.saturating_add(DEFAULT_INVITE_TTL_SECS),
        };
        let devices = vec![
            PairedDevice {
                id: "dev-macbook-air".to_owned(),
                name: text::DEMO_DEVICE_AIR.get().to_owned(),
                added_at: now.saturating_sub(86_400 * 3),
                last_seen_at: Some(now.saturating_sub(180)),
            },
            PairedDevice {
                id: "dev-studio-mac".to_owned(),
                name: text::DEMO_DEVICE_STUDIO.get().to_owned(),
                added_at: now.saturating_sub(86_400 * 10),
                last_seen_at: Some(now.saturating_sub(7_200)),
            },
        ];
        let nearby = vec![
            NearbyMac {
                id: "nearby-home-mini".to_owned(),
                name: text::DEMO_NEARBY_HOME.get().to_owned(),
                invite_link: "krill://pair?nearby=home-mac-mini".to_owned(),
            },
            NearbyMac {
                id: "nearby-living-imac".to_owned(),
                name: text::DEMO_NEARBY_IMAC.get().to_owned(),
                invite_link: "krill://pair?nearby=living-room-imac".to_owned(),
            },
        ];
        Self {
            state: Mutex::new(MockState {
                enabled: true,
                now_secs: now,
                invite_ttl_secs: DEFAULT_INVITE_TTL_SECS,
                next_seq: 2,
                active_invite: Some(invite),
                used_links: BTreeSet::new(),
                devices,
                nearby,
                forced_outcome: None,
            }),
        }
    }

    /// 모의 백엔드의 현재 기준 시각(Unix epoch 초)을 읽는다.
    #[must_use]
    pub fn now_secs(&self) -> u64 {
        self.state
            .lock()
            .map_or_else(|e| e.into_inner().now_secs, |g| g.now_secs)
    }

    /// 모의 백엔드의 기준 시각을 설정한다(만료 테스트용).
    pub fn set_now_secs(&self, now_secs: u64) {
        if let Ok(mut guard) = self.state.lock() {
            guard.now_secs = now_secs;
        }
    }

    /// 모의 백엔드의 기준 시각을 `delta_secs`만큼 앞으로 감는다.
    pub fn advance_secs(&self, delta_secs: u64) {
        if let Ok(mut guard) = self.state.lock() {
            guard.now_secs = guard.now_secs.saturating_add(delta_secs);
        }
    }

    /// 마지막으로 발급된 활성 초대 링크를 돌려준다.
    #[must_use]
    pub fn active_invite(&self) -> Option<Invite> {
        self.state.lock().ok().and_then(|g| g.active_invite.clone())
    }

    /// 다음 `pair()` 호출에 대해 강제 결과를 지정한다(테스트용).
    pub fn set_next_pair_outcome(&self, outcome: Option<PairOutcome>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.forced_outcome = outcome;
        }
    }

    /// 근처 Mac 목록을 설정한다.
    pub fn set_nearby(&self, nearby: Vec<NearbyMac>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.nearby = nearby;
        }
    }

    /// 짝지은 기기 목록을 설정한다.
    pub fn set_devices(&self, devices: Vec<PairedDevice>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.devices = devices;
        }
    }
}

impl LinkBackend for MockLinkBackend {
    fn enable_server(&self) -> Result<ServerStatus, String> {
        let mut guard = self.state.lock().map_err(|e| e.to_string())?;
        guard.enabled = true;
        Ok(ServerStatus { enabled: true })
    }

    fn disable_server(&self) -> Result<ServerStatus, String> {
        let mut guard = self.state.lock().map_err(|e| e.to_string())?;
        guard.enabled = false;
        guard.active_invite = None;
        Ok(ServerStatus { enabled: false })
    }

    fn server_status(&self) -> Result<ServerStatus, String> {
        let guard = self.state.lock().map_err(|e| e.to_string())?;
        Ok(ServerStatus {
            enabled: guard.enabled,
        })
    }

    fn create_invite(&self) -> Result<Invite, String> {
        let mut guard = self.state.lock().map_err(|e| e.to_string())?;
        let seq = guard.next_seq;
        guard.next_seq = guard.next_seq.saturating_add(1);
        let invite = Invite {
            link: format!("{INVITE_SCHEME_PREFIX}?invite={seq:04x}"),
            expires_at: guard.now_secs.saturating_add(guard.invite_ttl_secs),
        };
        guard.active_invite = Some(invite.clone());
        Ok(invite)
    }

    fn pair(&self, link: &str, device_name: &str, host_name: &str) -> PairOutcome {
        let Ok(mut guard) = self.state.lock() else {
            return PairOutcome::Failed(PairFailure::RemoteMacOffline);
        };
        if let Some(forced) = guard.forced_outcome.take() {
            return forced;
        }
        let trimmed = link.trim();
        if !is_valid_invite_link(trimmed) {
            return PairOutcome::Failed(PairFailure::InvalidLink);
        }
        if trimmed.contains("expired") {
            return PairOutcome::Failed(PairFailure::ExpiredLink);
        }
        if let Some(active) = &guard.active_invite
            && active.link == trimmed
            && active.is_expired(guard.now_secs)
        {
            return PairOutcome::Failed(PairFailure::ExpiredLink);
        }
        if trimmed.contains("used") || guard.used_links.contains(trimmed) {
            return PairOutcome::Failed(PairFailure::AlreadyUsedLink);
        }
        if trimmed.contains("offline") {
            return PairOutcome::Failed(PairFailure::RemoteMacOffline);
        }
        if trimmed.contains("no-internet") || trimmed.contains("no_internet") {
            return PairOutcome::Failed(PairFailure::NoInternet);
        }

        guard.used_links.insert(trimmed.to_owned());
        let seq = guard.next_seq;
        guard.next_seq = guard.next_seq.saturating_add(1);
        let clean_name = match clean_host_name(host_name) {
            Some(name) => name,
            None => sanitize_device_name(device_name),
        };
        let now = guard.now_secs;
        let paired = PairedDevice {
            id: format!("paired-{seq}"),
            name: clean_name,
            added_at: now,
            last_seen_at: Some(now),
        };
        guard.devices.push(paired.clone());
        PairOutcome::Paired(paired)
    }

    fn devices(&self) -> Result<Vec<PairedDevice>, String> {
        let guard = self.state.lock().map_err(|e| e.to_string())?;
        Ok(guard.devices.clone())
    }

    fn revoke(&self, id: &str) -> Result<Vec<PairedDevice>, String> {
        let mut guard = self.state.lock().map_err(|e| e.to_string())?;
        guard.devices.retain(|d| d.id != id);
        Ok(guard.devices.clone())
    }

    fn nearby(&self) -> Result<Vec<NearbyMac>, String> {
        let guard = self.state.lock().map_err(|e| e.to_string())?;
        Ok(guard.nearby.clone())
    }
}

/// 현재 시스템 Unix epoch 초를 돌려준다.
#[must_use]
pub fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 문자열이 유효한 `krill://pair` 초대 링크인지 검사한다.
/// 이미 나간 옛 `minikrill://pair?...` 링크도 받는다.
#[must_use]
pub fn is_valid_invite_link(raw: &str) -> bool {
    let trimmed = raw.trim();
    [
        krill_link::PAIR_URI_PREFIX,
        krill_link::LEGACY_PAIR_URI_PREFIX,
    ]
    .into_iter()
    .any(|prefix| trimmed.starts_with(prefix) && trimmed.len() > prefix.len())
        && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// 클립보드 문자열에 `krill://pair` 초대 링크가 들어 있으면 정규화해 돌려준다.
#[must_use]
pub fn extract_clipboard_invite(clipboard_text: &str) -> Option<String> {
    let trimmed = clipboard_text.trim();
    is_valid_invite_link(trimmed).then(|| trimmed.to_owned())
}

/// 이 Mac의 기본 표시 이름을 결정한다(UI 스레드 블로킹 없이 즉시 반환).
#[must_use]
pub fn default_device_name() -> String {
    for key in ["KRILL_DEVICE_NAME", "COMPUTERNAME", "HOSTNAME"] {
        if let Ok(val) = std::env::var(key) {
            let cleaned = val.trim().trim_end_matches(".local").trim();
            if !cleaned.is_empty() && !cleaned.chars().any(char::is_control) {
                return cleaned.to_owned();
            }
        }
    }
    if let Ok(user) = std::env::var("USER") {
        let cleaned = user.trim();
        if !cleaned.is_empty() && !cleaned.chars().any(char::is_control) {
            return text::USER_MAC_NAME.format(&[("user", cleaned)]);
        }
    }
    text::DEFAULT_DEVICE_NAME.get().to_owned()
}

/// 사용자가 입력한 기기 이름을 정리하고, 비어 있으면 기본 기기 이름을 돌려준다.
#[must_use]
pub fn sanitize_device_name(raw: &str) -> String {
    let cleaned: String = raw.trim().chars().filter(|c| !c.is_control()).collect();
    if cleaned.is_empty() {
        default_device_name()
    } else {
        cleaned
    }
}

/// 사용자가 정한 원격 Mac 이름을 정리한다. 비어 있으면 상대 Mac이 알려준 이름을 쓰도록 `None`.
#[must_use]
pub fn clean_host_name(raw: &str) -> Option<String> {
    let cleaned: String = raw.trim().chars().filter(|c| !c.is_control()).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// 남은 초(`remaining_secs`)를 "남은 유효 시간: M분 S초" 형태로 포맷한다.
#[must_use]
pub fn format_remaining(remaining_secs: u64) -> String {
    let mins = remaining_secs / 60;
    let secs = remaining_secs % 60;
    if mins > 0 {
        text::EXPIRES_IN_MS.format(&[("m", &mins.to_string()), ("s", &secs.to_string())])
    } else {
        text::EXPIRES_IN_S.format(&[("s", &secs.to_string())])
    }
}

/// 과거 시각(`ts_secs`)을 현재(`now_secs`) 기준 비개발자 친화 상대 시각으로 포맷한다.
#[must_use]
pub fn format_relative_time(ts_secs: u64, now_secs: u64) -> String {
    let elapsed = now_secs.saturating_sub(ts_secs);
    if elapsed < 60 {
        return text::TIME_JUST_NOW.get().to_owned();
    }
    let mins = elapsed / 60;
    if mins < 60 {
        return text::TIME_MINUTES_AGO.format(&[("m", &mins.to_string())]);
    }
    let hours = mins / 60;
    if hours < 24 {
        return text::TIME_HOURS_AGO.format(&[("h", &hours.to_string())]);
    }
    let days = hours / 24;
    text::TIME_DAYS_AGO.format(&[("d", &days.to_string())])
}

/// 짝지은 기기의 "추가 시각 · 마지막 접속" 메타 줄을 포맷한다.
#[must_use]
pub fn format_device_meta(device: &PairedDevice, now_secs: u64) -> String {
    let added = format_relative_time(device.added_at, now_secs);
    let last_seen = device.last_seen_at.map_or_else(
        || text::LAST_SEEN_NEVER.get().to_owned(),
        |ts| format_relative_time(ts, now_secs),
    );
    text::DEVICE_META.format(&[("added", &added), ("last_seen", &last_seen)])
}

impl std::fmt::Debug for Invite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invite")
            .field("link", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}
impl std::fmt::Debug for NearbyMac {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NearbyMac")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("invite_link", &"[redacted]")
            .finish()
    }
}
