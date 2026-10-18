//! 짝짓기(`krill-link invite` / `krill-link pair`) 링크 포맷, ALPN `minikrill/pair/1` 핸드셰이크, 속도 제한기.

use std::collections::VecDeque;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::config::{LinkConfig, RelaySetting};
use crate::identity::DeviceIdentity;
use crate::qr::{QrMatrix, encode_qr, render_qr_unicode};
use crate::serve::build_iroh_endpoint;
use crate::store::{
    InviteToken, RemoteHost, compact_endpoint_addr, create_invite, decode_hex, encode_hex,
};
use crate::{
    ALPN_PAIR,
    store::{DEFAULT_INVITE_TTL, now_unix_secs, sanitize_id_slug, upsert_host},
};

/// 짝짓기 링크 스킴 접두어.
pub const PAIR_URI_PREFIX: &str = "krill://pair?";
/// 개명 전 링크 접두어. 이미 나간 초대 링크를 위해 해석만 받는다.
pub const LEGACY_PAIR_URI_PREFIX: &str = "minikrill://pair?";
const TICKET_OPTIONAL_PREFIX: &str = "mklink1:";
const MAX_PAIR_FRAME_BYTES: usize = 8192;

/// 기본 짝짓기 속도 제한 윈도우 (60초).
pub const DEFAULT_RATE_WINDOW: Duration = Duration::from_secs(60);
/// 윈도우당 최대 실패 허용 횟수 (5회).
pub const DEFAULT_MAX_FAILURES: usize = 5;
/// 윈도우당 최대 전체 시도 횟수 (15회).
pub const DEFAULT_MAX_ATTEMPTS: usize = 15;

/// `krill://pair?t=<ticket>&k=<token>` 짝짓기 링크.
///
/// `fmt::Debug`는 토큰 원문을 `"<redacted>"`로 마스킹한다.
#[derive(Clone)]
pub struct PairLink {
    pub addr: EndpointAddr,
    pub token: InviteToken,
}

impl fmt::Debug for PairLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairLink")
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl PairLink {
    /// 서버 주소와 1회용 토큰으로 짝짓기 링크를 만든다.
    pub fn new(addr: EndpointAddr, token: InviteToken) -> Self {
        Self { addr, token }
    }

    /// `krill://pair?t=<ticket_hex>&k=<token_hex>` 문자열로 인코딩한다.
    pub fn encode(&self) -> Result<String, Error> {
        let ticket_hex = encode_addr_ticket(&self.addr)?;
        Ok(format!(
            "{PAIR_URI_PREFIX}t={ticket_hex}&k={}",
            self.token.as_str()
        ))
    }

    /// `krill://pair?...`(옛 `minikrill://pair?...`도) 문자열을 파싱한다. 오류에 토큰이나 링크 원문을 남기지 않는다.
    pub fn parse(input: &str) -> Result<Self, Error> {
        let trimmed = input.trim();
        let query = trimmed
            .strip_prefix(PAIR_URI_PREFIX)
            .or_else(|| trimmed.strip_prefix(LEGACY_PAIR_URI_PREFIX))
            .ok_or(Error::InvalidPairLink(
                "pair link must start with krill://pair?",
            ))?;

        let mut ticket_val: Option<&str> = None;
        let mut token_val: Option<&str> = None;

        for part in query.split('&') {
            if part.is_empty() {
                continue;
            }
            let Some((k, v)) = part.split_once('=') else {
                return Err(Error::InvalidPairLink("malformed query parameter"));
            };
            match k {
                "t" | "ticket" => ticket_val = Some(v),
                "k" | "token" => token_val = Some(v),
                _ => {}
            }
        }

        let raw_ticket =
            ticket_val.ok_or(Error::InvalidPairLink("missing ticket parameter (t)"))?;
        let raw_token = token_val.ok_or(Error::InvalidPairLink("missing token parameter (k)"))?;

        let addr = decode_addr_ticket(raw_ticket)?;
        let token = InviteToken::from_raw(raw_token)?;
        Ok(Self { addr, token })
    }
}

/// `EndpointAddr`를 hex 티켓 문자열로 인코딩한다.
pub fn encode_addr_ticket(addr: &EndpointAddr) -> Result<String, Error> {
    let bytes = postcard::to_stdvec(addr)
        .map_err(|_| Error::InvalidPairLink("failed to serialize endpoint address"))?;
    Ok(encode_hex(&bytes))
}

/// hex 티켓 문자열에서 `EndpointAddr`를 복원한다.
pub fn decode_addr_ticket(ticket: &str) -> Result<EndpointAddr, Error> {
    let trimmed = ticket.trim();
    let hex_str = trimmed
        .strip_prefix(TICKET_OPTIONAL_PREFIX)
        .unwrap_or(trimmed);
    if hex_str.is_empty() {
        return Err(Error::InvalidPairLink("empty ticket"));
    }
    let bytes =
        decode_hex(hex_str).map_err(|_| Error::InvalidPairLink("invalid ticket hex encoding"))?;
    postcard::from_bytes(&bytes)
        .map_err(|_| Error::InvalidPairLink("invalid ticket postcard payload"))
}

/// `ALPN_PAIR` 와이어 요청 메시지.
#[derive(Clone, Serialize, Deserialize)]
pub struct PairWireRequest {
    pub token: String,
    pub device_name: String,
}

impl fmt::Debug for PairWireRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairWireRequest")
            .field("token", &"<redacted>")
            .field("device_name", &self.device_name)
            .finish()
    }
}

/// `ALPN_PAIR` 와이어 응답 메시지.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairWireResponse {
    pub accepted: bool,
    pub server_name: String,
    pub host_id: String,
    pub reason: Option<String>,
}

/// 슬라이딩 윈도우 기반 짝짓기 시도 속도 제한기.
#[derive(Debug)]
pub struct PairRateLimiter {
    window: Duration,
    max_failures: usize,
    max_attempts: usize,
    state: Mutex<RateState>,
}

#[derive(Debug, Default)]
struct RateState {
    attempts: VecDeque<Instant>,
    failures: VecDeque<Instant>,
}

impl Default for PairRateLimiter {
    fn default() -> Self {
        Self::new(
            DEFAULT_RATE_WINDOW,
            DEFAULT_MAX_FAILURES,
            DEFAULT_MAX_ATTEMPTS,
        )
    }
}

impl PairRateLimiter {
    /// 지정한 윈도우·실패 상한·전체 시도 상한으로 속도 제한기를 만든다.
    pub fn new(window: Duration, max_failures: usize, max_attempts: usize) -> Self {
        Self {
            window,
            max_failures,
            max_attempts,
            state: Mutex::new(RateState::default()),
        }
    }

    /// 새 짝짓기 시도가 허용되는지 확인하고 시도 시각을 기록한다.
    pub fn check_and_record_attempt(&self, now: Instant) -> bool {
        let Ok(mut guard) = self.state.lock() else {
            return false;
        };
        prune_old(&mut guard.attempts, now, self.window);
        prune_old(&mut guard.failures, now, self.window);
        if guard.failures.len() >= self.max_failures || guard.attempts.len() >= self.max_attempts {
            return false;
        }
        guard.attempts.push_back(now);
        true
    }

    /// 짝짓기 토큰 검증 실패를 기록한다.
    pub fn record_failure(&self, now: Instant) {
        if let Ok(mut guard) = self.state.lock() {
            prune_old(&mut guard.failures, now, self.window);
            guard.failures.push_back(now);
        }
    }
}

fn prune_old(queue: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while let Some(&front) = queue.front() {
        if now.saturating_duration_since(front) >= window {
            queue.pop_front();
        } else {
            break;
        }
    }
}

/// QUIC 스트림으로 길이 접두(u32 BE) postcard 프레임을 보낸다.
pub async fn write_pair_frame<T: Serialize>(
    send: &mut iroh::endpoint::SendStream,
    msg: &T,
) -> Result<(), Error> {
    let payload =
        postcard::to_stdvec(msg).map_err(|_| Error::Protocol("failed to encode pairing frame"))?;
    let len = u32::try_from(payload.len())
        .map_err(|_| Error::Protocol("pairing frame exceeds u32 length"))?;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|_| Error::Protocol("failed to write pairing frame length"))?;
    send.write_all(&payload)
        .await
        .map_err(|_| Error::Protocol("failed to write pairing frame payload"))?;
    Ok(())
}

/// QUIC 스트림에서 길이 접두(u32 BE) postcard 프레임을 읽는다.
pub async fn read_pair_frame<T: for<'de> Deserialize<'de>>(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<T, Error> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|_| Error::Protocol("failed to read pairing frame length"))?;
    let len = usize::try_from(u32::from_be_bytes(len_buf)).unwrap_or(usize::MAX);
    if len == 0 || len > MAX_PAIR_FRAME_BYTES {
        return Err(Error::Protocol("pairing frame size out of bounds"));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|_| Error::Protocol("failed to read pairing frame payload"))?;
    postcard::from_bytes(&buf).map_err(|_| Error::Protocol("failed to decode pairing frame"))
}

/// `krill-link invite` 결과물.
///
/// JSON 정책: `invite --json` 출력에는 `qr_matrix`를 항상 포함한다.
/// `--no-qr` 플래그는 터미널 유니코드(`qr_unicode`) 출력만 생략하고 JSON에는 영향을 주지 않는다.
#[derive(Clone, Serialize)]
pub struct InviteOutput {
    pub invite_id: String,
    pub link: String,
    pub qr_unicode: String,
    /// QR 모듈 행렬(`true` = 어두운 칸). UI의 `Vec<Vec<bool>>` 행렬과 같은 모양이라 변환 없이 쓴다.
    pub qr_matrix: Vec<Vec<bool>>,
    pub expires_at_ms: u64,
    pub node_id: String,
}

impl fmt::Debug for InviteOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InviteOutput")
            .field("invite_id", &self.invite_id)
            .field("link", &"<redacted>")
            .field("qr_unicode", &"<redacted>")
            .field("qr_matrix", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("node_id", &self.node_id)
            .finish()
    }
}

/// `QrMatrix`를 행 우선 `Vec<Vec<bool>>`로 펼친다(`true` = 어두운 칸).
fn matrix_rows(matrix: &QrMatrix) -> Vec<Vec<bool>> {
    let size = matrix.size();
    (0..size)
        .map(|row| (0..size).map(|col| matrix.get(row, col)).collect())
        .collect()
}

/// 서버 주소(`server_addr`)와 상태 디렉터리를 받아 1회용 초대 링크와 터미널 QR 코드를 생성한다.
pub fn generate_invite_for_addr(
    state_dir: &Path,
    server_addr: &EndpointAddr,
    include_relay: bool,
    ttl: Duration,
) -> Result<InviteOutput, Error> {
    let compact_addr = compact_endpoint_addr(server_addr, include_relay);
    let (token, record) = create_invite(state_dir, ttl)?;
    let pair_link = PairLink::new(compact_addr.clone(), token);
    let link_str = pair_link.encode()?;
    let qr_unicode = render_qr_unicode(&link_str)?;
    let qr_matrix = matrix_rows(&encode_qr(link_str.as_bytes())?);
    Ok(InviteOutput {
        invite_id: record.id,
        link: link_str,
        qr_unicode,
        qr_matrix,
        expires_at_ms: record.expires_at_ms,
        node_id: compact_addr.id.to_string(),
    })
}

/// 기본 10분 만료로 초대 링크를 생성한다 (`serve`가 실행 중이면 그 주소를 쓰고, 아니면 임시 엔드포인트를 바인드해 주소를 얻는다).
pub async fn create_invite_command(
    state_dir: &Path,
    ttl: Option<Duration>,
) -> Result<InviteOutput, Error> {
    let cfg = LinkConfig::load(state_dir)?;
    let include_relay = cfg.relay != RelaySetting::Off;
    let effective_ttl = ttl.unwrap_or(DEFAULT_INVITE_TTL);

    // 1. 실행 중인 serve의 주소 파일/소켓 확인
    if let Some(live_addr) = crate::serve::read_live_server_addr(state_dir) {
        return generate_invite_for_addr(state_dir, &live_addr, include_relay, effective_ttl);
    }

    // 2. serve가 아직 떠 있지 않으면 저장된 포트(또는 새 포트)로 잠시 바인드해 주소를 얻고 포트를 고정한다.
    let identity = DeviceIdentity::load_or_create(state_dir)?;
    let ep = build_iroh_endpoint(state_dir, &identity, &cfg, false, None).await?;
    if include_relay {
        tokio::time::timeout(Duration::from_secs(5), ep.online())
            .await
            .ok();
    }
    let addr = ep.addr();
    let res = generate_invite_for_addr(state_dir, &addr, include_relay, effective_ttl);
    ep.close().await;
    res
}

/// 클라이언트 `krill-link pair <링크> --name <기기 이름>`:
/// ALPN `minikrill/pair/1`로 서버에 토큰·기기 이름을 보내 검증받고 `link/hosts.toml`에 호스트를 저장한다.
pub async fn pair_with_server(
    state_dir: &Path,
    link_str: &str,
    device_name: &str,
) -> Result<RemoteHost, Error> {
    let pair_link = PairLink::parse(link_str)?;
    let trimmed_name = device_name.trim();
    if trimmed_name.is_empty() {
        return Err(Error::InvalidConfig(
            "device name (--name) must not be empty".to_owned(),
        ));
    }

    let identity = DeviceIdentity::load_or_create(state_dir)?;
    let mut cfg = LinkConfig::load(state_dir)?;
    // 티켓에 relay 주소가 없으면 로컬/직접 연결이므로 불필요한 외부 relay 대기를 생략한다.
    if pair_link.addr.relay_urls().next().is_none() && cfg.relay == RelaySetting::N0 {
        cfg.relay = RelaySetting::Off;
    }

    let endpoint = build_iroh_endpoint(state_dir, &identity, &cfg, false, None).await?;
    let pair_result = async {
        let conn = tokio::time::timeout(
            Duration::from_secs(10),
            endpoint.connect(pair_link.addr.clone(), ALPN_PAIR),
        )
        .await
        .map_err(|_| Error::Timeout("pairing connection timed out"))?
        .map_err(|e| Error::Transport(format!("pairing connect failed: {e}")))?;

        let (mut send, mut recv) =
            match tokio::time::timeout(Duration::from_secs(5), conn.open_bi()).await {
                Ok(Ok(streams)) => streams,
                Ok(Err(e)) => {
                    if let Some(iroh::endpoint::ConnectionError::ApplicationClosed(close)) =
                        conn.close_reason()
                        && close.error_code == iroh::endpoint::VarInt::from_u32(429)
                    {
                        return Err(Error::PairRejected(
                            "too many pairing attempts (rate limit exceeded)".to_owned(),
                        ));
                    }
                    return Err(Error::Transport(format!(
                        "opening pairing bi-stream failed: {e}"
                    )));
                }
                Err(_) => return Err(Error::Timeout("opening pairing stream timed out")),
            };

        let req = PairWireRequest {
            token: pair_link.token.as_str().to_owned(),
            device_name: trimmed_name.to_owned(),
        };
        if let Err(e) = write_pair_frame(&mut send, &req).await {
            if let Some(iroh::endpoint::ConnectionError::ApplicationClosed(close)) =
                conn.close_reason()
                && close.error_code == iroh::endpoint::VarInt::from_u32(429)
            {
                return Err(Error::PairRejected(
                    "too many pairing attempts (rate limit exceeded)".to_owned(),
                ));
            }
            return Err(e);
        }
        send.finish().ok();

        let resp: PairWireResponse =
            match tokio::time::timeout(Duration::from_secs(5), read_pair_frame(&mut recv)).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    if let Some(iroh::endpoint::ConnectionError::ApplicationClosed(close)) =
                        conn.close_reason()
                        && close.error_code == iroh::endpoint::VarInt::from_u32(429)
                    {
                        return Err(Error::PairRejected(
                            "too many pairing attempts (rate limit exceeded)".to_owned(),
                        ));
                    }
                    return Err(e);
                }
                Err(_) => return Err(Error::Timeout("waiting for pairing response timed out")),
            };

        conn.close(0u32.into(), b"pair done");
        Ok::<PairWireResponse, Error>(resp)
    }
    .await;

    endpoint.close().await;
    let resp = pair_result?;

    if !resp.accepted {
        let reason = resp
            .reason
            .unwrap_or_else(|| "pairing rejected by server".to_owned());
        return Err(Error::PairRejected(reason));
    }

    let host_id = sanitize_id_slug(&resp.host_id, "host", &pair_link.addr.id.to_string());
    let server_name = if resp.server_name.trim().is_empty() {
        host_id.clone()
    } else {
        resp.server_name.trim().to_owned()
    };

    let host =
        RemoteHost::from_endpoint_addr(host_id, &server_name, &pair_link.addr, now_unix_secs());
    upsert_host(state_dir, host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn pair_link_roundtrip_and_redaction() {
        let sk = SecretKey::generate();
        let addr =
            EndpointAddr::new(sk.public()).with_ip_addr("192.168.1.20:41234".parse().unwrap());
        let token = InviteToken::generate().unwrap();
        let raw_tok = token.as_str().to_owned();

        let link = PairLink::new(addr.clone(), token);
        let dbg = format!("{link:?}");
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains(&raw_tok));

        let encoded = link.encode().unwrap();
        assert!(encoded.starts_with("krill://pair?"));
        let parsed = PairLink::parse(&encoded).unwrap();
        assert_eq!(parsed.addr, addr);
        assert_eq!(parsed.token.as_str(), raw_tok);

        // OWE-669: 이미 나간 옛 minikrill:// 링크도 같은 값으로 해석된다.
        let legacy = encoded.replacen("krill://", "minikrill://", 1);
        let parsed = PairLink::parse(&legacy).unwrap();
        assert_eq!(parsed.addr, addr);
        assert_eq!(parsed.token.as_str(), raw_tok);
        assert!(PairLink::parse(&encoded.replacen("krill://", "other://", 1)).is_err());

        // 잘못된 토큰이 들어간 링크 파싱 실패 시 오류에 토큰 원문이 남지 않는지 확인
        let bad = "minikrill://pair?t=00&k=secret_bad_token_value_12345";
        let err = PairLink::parse(bad).unwrap_err();
        assert!(!err.to_string().contains("secret_bad_token_value_12345"));
    }

    #[test]
    fn invite_output_qr_matrix_matches_link_and_survives_json() {
        #[derive(serde::Deserialize)]
        struct LegacyReader {
            invite_id: String,
            link: String,
        }
        let dir = std::env::temp_dir().join(format!("mklink-invqr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sk = SecretKey::generate();
        let addr =
            EndpointAddr::new(sk.public()).with_ip_addr("192.168.1.20:41234".parse().unwrap());

        let out = generate_invite_for_addr(&dir, &addr, false, Duration::from_secs(600)).unwrap();

        // 같은 링크를 다시 인코딩하면 JSON 행렬과 같은 행이 나온다(결정성).
        let expected = crate::qr::encode_qr(out.link.as_bytes()).unwrap();
        assert_eq!(expected.size(), out.qr_matrix.len());
        for (row_idx, row) in out.qr_matrix.iter().enumerate() {
            assert_eq!(expected.size(), row.len());
            for (col_idx, cell) in row.iter().enumerate() {
                assert_eq!(expected.get(row_idx, col_idx), *cell);
            }
        }

        // JSON에 항상 포함되고, 구 필드 무시 리더와도 호환된다(추가 필드만 읽음).
        let json = serde_json::to_string(&out).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let qr_json = value
            .get("qr_matrix")
            .expect("invite --json must always include qr_matrix");
        assert_eq!(qr_json.as_array().unwrap().len(), expected.size());

        let legacy: LegacyReader = serde_json::from_str(&json).unwrap();
        assert_eq!(legacy.invite_id, out.invite_id);
        assert_eq!(legacy.link, out.link);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rate_limiter_blocks_after_max_failures_and_recovers_after_window() {
        let limiter = PairRateLimiter::new(Duration::from_secs(10), 3, 10);
        let t0 = Instant::now();
        for i in 0..3 {
            let now = t0 + Duration::from_secs(i);
            assert!(limiter.check_and_record_attempt(now));
            limiter.record_failure(now);
        }
        // 4번째 시도는 실패 3회 누적 때문에 차단됨
        assert!(!limiter.check_and_record_attempt(t0 + Duration::from_secs(4)));
        // 윈도우(10초) 경과 후에는 다시 허용됨
        assert!(limiter.check_and_record_attempt(t0 + Duration::from_secs(15)));
    }
}
