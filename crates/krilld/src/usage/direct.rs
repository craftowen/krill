//! OWE-719: 계정별 읽기 전용 직접 사용량. 토큰·응답 원문은 캐시·로그·IPC에 넣지 않는다.
//! Orca(MIT)의 rate-limits/claude-oauth-usage-request.ts, codex-backend-usage-client.ts,
//! claude-accounts/keychain.ts의 엔드포인트·저장소 규약을 참고한 독립 Rust 구현이다.
//! Copyright (c) 2026 Lovecast Inc., MIT License (Orca reference).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::account_login::AuthStatus;
use krill_proto::usage::{ProviderKind, ResetConsumeOutcome};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::{ProfileUsage, Usage, Window, accounts};
use crate::session::lock;

type Key = (PathBuf, ProviderKind, PathBuf);
static CACHE: Mutex<BTreeMap<Key, (Instant, ProfileUsage)>> = Mutex::new(BTreeMap::new());
static PENDING: Mutex<BTreeSet<Key>> = Mutex::new(BTreeSet::new());
/// 실패·429 뒤 다음 조회 가능 시각과 연속 실패 수. 이 시각 전에는 같은 프로필을 다시 조회하지 않는다.
static RETRY: Mutex<BTreeMap<Key, (Instant, u32)>> = Mutex::new(BTreeMap::new());
// 아래 값은 Orca(MIT) rate-limits/service/service-types.ts·usage-retry-after.ts를 따른다.
// 사용량 엔드포인트는 예산이 빡빡하고 사용량은 참고용이라, 촘촘히 조회하기보다 최근 값을 쓴다.
/// 주기 틱의 조회 간격.
const POLL_INTERVAL: Duration = Duration::from_mins(15);
/// `ListAccounts`·창 활성화 같은 자동 요청이 다시 조회하기 전 최소 간격.
const MIN_REFETCH: Duration = Duration::from_mins(5);
/// Claude 세션 statusline 값이 이보다 새로우면 자동 조회를 건너뛴다.
const LIVE_FRESH: Duration = MIN_REFETCH;
/// 이보다 오래된 정상 값은 버린다.
const CACHE_TTL: Duration = Duration::from_mins(30);
/// 실패 재시도 간격: 30초부터 실패마다 두 배, 15분 상한.
const FAILURE_RETRY: Duration = Duration::from_secs(30);
const MAX_FAILURE_RETRY: Duration = Duration::from_mins(15);
/// 429에 Retry-After가 없거나 못 읽을 때(HTTP-date·curl 7.84 미만) 기다리는 시간.
const RATE_LIMIT_RETRY: Duration = Duration::from_mins(5);
/// 손상된 Retry-After가 며칠씩 조회를 막지 않게 하는 상한. 429 동안 마지막 정상 값도 이만큼 유지한다.
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_hours(24);
const MAX_BYTES: u64 = 512 * 1024;

/// 소진 직후 스냅샷이 소진 전 수치를 내놓지 않게 해당 프로필 캐시만 버린다.
/// 다음 `refresh_on_request`가 백그라운드에서 다시 읽는다(직접 조회가 켜져 있을 때).
pub(super) fn evict_profile(home: &Path, provider: ProviderKind, dir: &Path) {
    lock(&CACHE).remove(&(home.to_path_buf(), provider, dir.to_path_buf()));
    super::meters::evict_profile(home, provider, dir);
}

pub(super) fn cached(home: &Path, provider: ProviderKind, dir: &Path) -> Option<ProfileUsage> {
    if provider == ProviderKind::Antigravity {
        return agy::cached(home, dir);
    }
    lock(&CACHE)
        .get(&(home.to_path_buf(), provider, dir.to_path_buf()))
        .filter(|(at, usage)| alive(*at, usage))
        .map(|(_, usage)| usage.clone())
}

/// 정상 값은 `CACHE_TTL`, 429로 오래됨 처리된 값은 마지막 성공 시각부터 하루까지 쓴다.
fn alive(at: Instant, usage: &ProfileUsage) -> bool {
    at.elapsed()
        < if usage.stale {
            MAX_RATE_LIMIT_WAIT
        } else {
            CACHE_TTL
        }
}

/// OWE-702의 주기 틱이 호출한다. 네트워크·자격증명 IO는 별도 스레드에서만 수행한다.
pub fn refresh(home: &Path, state: Option<&Path>) {
    refresh_with(home, state, POLL_INTERVAL, false);
}

/// `ListAccounts`·계정 변경 요청이 호출한다. 앱 폴링·창 전환이 몰려도 5분에 한 번만 조회한다.
pub fn refresh_on_request(home: &Path, state: Option<&Path>) {
    refresh_with(home, state, MIN_REFETCH, false);
}

/// 사용자가 누른 '사용량 새로고침'. Orca처럼 주기·Retry-After·statusline 대기를 모두 무시하고 바로 조회한다.
pub fn refresh_manual(home: &Path, state: Option<&Path>) {
    refresh_with(home, state, Duration::ZERO, true);
}

fn refresh_with(home: &Path, state: Option<&Path>, interval: Duration, force: bool) {
    let file = accounts::load_file(home, state);
    if !file.direct_usage {
        lock(&CACHE).retain(|(h, _, _), _| h != home);
        agy::evict(home);
        super::meters::evict(home);
        return;
    }
    if cfg!(test) || (cfg!(debug_assertions) && std::env::var_os("KRILL_TEST_OFFLINE").is_some()) {
        return;
    } // 실제 자격증명·네트워크는 단위 테스트에서 금지.
    agy::refresh(home, state, &file, interval);
    let profiles = [
        ProviderKind::Claude,
        ProviderKind::Codex,
        ProviderKind::OpenCode,
    ]
    .into_iter()
    .map(|p| (p, accounts::host_default_dir(home, p), true))
    .chain(file.accounts.iter().filter_map(|a| {
        Some((
            ProviderKind::parse(&a.provider)?,
            PathBuf::from(&a.profile_dir),
            false,
        ))
    }));
    for (provider, dir, host) in profiles {
        if !matches!(
            provider,
            ProviderKind::Claude | ProviderKind::Codex | ProviderKind::OpenCode
        ) || !(dir.is_dir() || provider == ProviderKind::OpenCode && host)
        {
            continue;
        }
        let key = (home.to_path_buf(), provider, dir);
        // 실패·429는 `RETRY` 시각만 보고, 정상 값은 주기(`interval`)만 본다.
        if !force
            && (lock(&RETRY)
                .get(&key)
                .is_some_and(|(until, _)| Instant::now() < *until)
                || (live_fresh(provider, &key.2)
                    && super::meters::has_cached(&key.0, provider, &key.2)))
            || lock(&CACHE)
                .get(&key)
                .is_some_and(|(at, u)| u.error.is_none() && !u.stale && at.elapsed() < interval)
            || !lock(&PENDING).insert(key.clone())
        {
            continue;
        }
        spawn(&key, host, state.map(Path::to_path_buf));
    }
}

fn spawn(key: &Key, host: bool, state: Option<PathBuf>) {
    let job = key.clone();
    let launched = std::thread::Builder::new()
        .name("account-usage".into())
        .spawn(move || {
            let result =
                std::panic::catch_unwind(|| fetch(job.1, &job.2, host, &job.0, state.as_deref()))
                    .unwrap_or(Fetched::Failed);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            let usage = match result {
                Fetched::Usage(usage, meters) => {
                    lock(&RETRY).remove(&job);
                    super::meters::remember(&job.0, job.1, &job.2, meters);
                    Some(ProfileUsage {
                        usage,
                        updated_at: now,
                        ..ProfileUsage::default()
                    })
                }
                // 429는 오류가 아니다: 서버가 말한 시각까지 쉬고 마지막 정상 값을 오래됨으로 유지한다.
                Fetched::RateLimited(wait) => {
                    let mut retry = lock(&RETRY);
                    let streak = retry.get(&job).map_or(0, |r| r.1);
                    let wait = wait.unwrap_or(RATE_LIMIT_RETRY).min(MAX_RATE_LIMIT_WAIT);
                    retry.insert(job.clone(), (Instant::now() + wait, streak));
                    drop(retry);
                    mark_rate_limited(&mut lock(&CACHE), &job, now);
                    None
                }
                Fetched::Failed => {
                    let mut retry = lock(&RETRY);
                    let streak = retry.get(&job).map_or(0, |r| r.1).saturating_add(1);
                    retry.insert(job.clone(), (Instant::now() + failure_wait(streak), streak));
                    Some(ProfileUsage {
                        error: Some("갱신 실패".into()),
                        detail: Some("사용량 갱신 실패".into()),
                        updated_at: now,
                        ..ProfileUsage::default()
                    })
                }
            };
            // 조회 중 사용자가 끈 경우에는 결과도 버린다.
            if let Some(usage) = usage
                && accounts::load_file(&job.0, state.as_deref()).direct_usage
            {
                let mut cache = lock(&CACHE);
                cache.retain(|_, (at, u)| alive(*at, u));
                cache.insert(job.clone(), (Instant::now(), usage));
            }
            lock(&PENDING).remove(&job);
        });
    if launched.is_err() {
        lock(&PENDING).remove(key);
    }
}

#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "테스트 빌드의 fetch는 네트워크 없이 Failed만 돌려준다"
    )
)]
enum Fetched {
    Usage(Usage, krill_proto::usage::AccountMeters),
    /// HTTP 429와 Retry-After. 같은 토큰을 다른 도구(Claude Code·Orca)와 함께 쓰면 자주 걸린다.
    RateLimited(Option<Duration>),
    Failed,
}

/// 실행 중인 Claude 세션이 statusline으로 방금 사용량을 남겼는가. 같은 값을 API로 또 받지 않는다.
fn live_fresh(provider: ProviderKind, dir: &Path) -> bool {
    provider == ProviderKind::Claude
        && accounts::claude_statusline_usage(dir)
            .and_then(|u| u.updated_at)
            .is_some_and(|at| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .is_ok_and(|now| now.as_secs().saturating_sub(at) < LIVE_FRESH.as_secs())
            })
}

/// `streak`번째 연속 실패 뒤 기다릴 시간(30초·60초·…·15분).
fn failure_wait(streak: u32) -> Duration {
    FAILURE_RETRY
        .saturating_mul(1 << streak.saturating_sub(1).min(5))
        .min(MAX_FAILURE_RETRY)
}

/// 429 때: 마지막 정상 값은 받은 시각을 그대로 둔 채 오래됨으로 바꾼다. 정상 값이 없으면
/// '모름(--)' 대신 요청 제한 오류로 둔다(값은 지어내지 않는다). 사용량이 빈 오류 행이라
/// 프로필 사용량 파일(statusline 캐시)이 있으면 그 값이 이긴다.
fn mark_rate_limited(
    cache: &mut BTreeMap<Key, (Instant, ProfileUsage)>,
    key: &Key,
    now: Option<u64>,
) {
    match cache.get_mut(key) {
        Some((_, u)) if u.error.is_none() && !u.usage.is_empty() => u.stale = true,
        _ => {
            cache.insert(
                key.clone(),
                (
                    Instant::now(),
                    ProfileUsage {
                        error: Some("요청 제한".into()),
                        detail: Some("사용량 API 요청 제한(429)".into()),
                        updated_at: now,
                        ..ProfileUsage::default()
                    },
                ),
            );
        }
    }
}

/// 단위 테스트는 네트워크·실제 키체인을 절대 읽지 않는다.
#[cfg(test)]
fn fetch(_: ProviderKind, _: &Path, _: bool, _: &Path, _: Option<&Path>) -> Fetched {
    Fetched::Failed
}

#[cfg(not(test))]
fn fetch(
    provider: ProviderKind,
    dir: &Path,
    host: bool,
    home: &Path,
    state: Option<&Path>,
) -> Fetched {
    let Some((token, account)) = credentials(provider, dir, host, home, state) else {
        return Fetched::Failed;
    };
    let mut limited = None;
    let out = query(provider, &token, account.as_deref(), |config| {
        let (code, retry_after, body) = curl_response(config)?;
        if code == 429 {
            limited = Some(retry_after);
        }
        (200..300).contains(&code).then_some(body)
    });
    let Some(mut out) = out else {
        return limited.map_or(Fetched::Failed, Fetched::RateLimited);
    };
    // Claude 리셋(주간 세션·프로모션)은 일반 사용량과 같은 주기로 프로브한다.
    // 프로브 실패는 조용히 건너뛰고 기존 미터를 유지한다.
    if provider == ProviderKind::Claude
        && let Some(count) = claude_reset_count(&token, get_text)
    {
        out.1.meters.push(krill_proto::usage::UsageMeter {
            key: krill_proto::usage::RESET_CREDITS_METER_KEY.into(),
            label: krill_proto::usage::RESET_CREDITS_METER_LABEL.into(),
            window: None,
            value: Some(count.to_string()),
        });
    }
    Fetched::Usage(out.0, out.1)
}

/// curl GET 전송. 설정·본문에 비밀(토큰)은 로그에 남기지 않는다.
pub(super) fn get_text(config: &str) -> Option<Zeroizing<Vec<u8>>> {
    pipe_command(
        system_curl(),
        &[
            "-q",
            "--config",
            "-",
            "--silent",
            "--fail",
            "--max-time",
            "10",
            "--max-filesize",
            "524288",
        ],
        config.as_bytes(),
    )
}

fn query(
    provider: ProviderKind,
    token: &str,
    account: Option<&str>,
    send: impl FnOnce(&str) -> Option<Zeroizing<Vec<u8>>>,
) -> Option<(Usage, krill_proto::usage::AccountMeters)> {
    let config = request_config(provider, token, account)?;
    let body = send(&config)?;
    let text = std::str::from_utf8(&body).ok()?;
    let meters = super::meters::parse(provider, text);
    let usage = if provider == ProviderKind::Claude {
        super::parse_claude(text)
    } else if provider == ProviderKind::OpenCode {
        let window = |label| {
            meters
                .meters
                .iter()
                .find(|m| m.label == label)
                .and_then(|m| m.window.as_ref())
                .map(|w| Window {
                    percent: w.percent,
                    resets_at: w.resets_at.clone(),
                })
        };
        Usage {
            five_hour: window("5h"),
            seven_day: window("7d"),
        }
    } else {
        parse_codex_backend(text)
    };
    (!usage.is_empty() || !meters.meters.is_empty()).then_some((usage, meters))
}

/// Codex banked reset 크레딧 소진(사용량 초기화). 성공하면 `(결과, 초기화된 창 수)`.
/// 토큰·요청 ID·원문은 로그·캐시·IPC에 넣지 않는다. 재시도는 새 멱등 키로 새 소진이 되니
/// 호출자가 자동 재시도하지 않는다(타임아웃은 크레딧부터 다시 확인). CC-BY: Orca MIT 참고 구현과
/// 별개의 Wham `rate-limit-reset-credits/consume` 호출이다.
pub(crate) fn consume_reset_credit(
    provider: ProviderKind,
    dir: &Path,
    host: bool,
    home: &Path,
    state: Option<&Path>,
) -> Option<(ResetConsumeOutcome, u64)> {
    if provider != ProviderKind::Codex {
        return None;
    }
    let (token, account) = credentials(provider, dir, host, home, state)?;
    let redeem_id = redeem_request_id()?;
    consume_request(provider, &token, account.as_deref(), &redeem_id, |config| {
        pipe_command(
            system_curl(),
            &[
                "-q",
                "--config",
                "-",
                "--silent",
                "--fail",
                "--max-time",
                "8",
                "--max-filesize",
                "524288",
            ],
            config.as_bytes(),
        )
    })
}

fn consume_request(
    provider: ProviderKind,
    token: &str,
    account: Option<&str>,
    redeem_id: &str,
    send: impl FnOnce(&str) -> Option<Zeroizing<Vec<u8>>>,
) -> Option<(ResetConsumeOutcome, u64)> {
    let config = consume_config(provider, token, account, redeem_id)?;
    let body = send(&config)?;
    parse_consume_response(std::str::from_utf8(&body).ok()?)
}

/// 소진 요청 curl 설정. `data`는 JSON 큰따옴표와 겹치지 않게 작은따옴표로 감싼다.
fn consume_config(
    provider: ProviderKind,
    token: &str,
    account: Option<&str>,
    redeem_id: &str,
) -> Option<Zeroizing<String>> {
    if provider != ProviderKind::Codex || !check_credential_text(token, account) {
        return None;
    }
    if redeem_id.is_empty()
        || !redeem_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    let mut config = Zeroizing::new(format!(
        "url = \"https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"User-Agent: codex-cli\"\nheader = \"OpenAI-Beta: codex-1\"\nheader = \"originator: Codex Desktop\"\nheader = \"Content-Type: application/json\"\nheader = \"Accept: application/json\"\n"
    ));
    if let Some(account) = account {
        config.push_str("header = \"ChatGPT-Account-Id: ");
        config.push_str(account);
        config.push_str("\"\n");
    }
    config.push_str("data = '{\"redeem_request_id\": \"");
    config.push_str(redeem_id);
    config.push_str("\"}'\n");
    Some(config)
}

/// `consume` 응답 `code` 그대로의 결과와 초기화된 창 수. 모르는 코드는 `None`이다.
fn parse_consume_response(text: &str) -> Option<(ResetConsumeOutcome, u64)> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let outcome = match json.get("code")?.as_str()? {
        "reset" => ResetConsumeOutcome::Reset,
        "nothing_to_reset" => ResetConsumeOutcome::NothingToReset,
        "no_credit" => ResetConsumeOutcome::NoCredit,
        "already_redeemed" => ResetConsumeOutcome::AlreadyRedeemed,
        _ => return None,
    };
    let windows = json
        .get("windows_reset")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some((outcome, windows))
}

/// Claude 리셋 프로브 종류. 서버가 아는 쿼리만 쓴다.
#[derive(Clone, Copy)]
enum ClaudeResetProbe {
    /// 주간 세션 리셋(`juniper_tide`).
    Juniper,
    /// 프로모션 grant(`cedar_ember`).
    Cedar,
}

/// Claude 소진 프로그램. 서버가 아는 값만 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaudeResetProgram {
    Juniper,
    Cedar,
}

impl ClaudeResetProgram {
    fn as_str(self) -> &'static str {
        match self {
            Self::Juniper => "juniper_tide",
            Self::Cedar => "cedar_ember",
        }
    }
}

/// Claude 리셋 2종 프로브 합산(주간 세션 제공 1 + 사용 가능 grant). 두 프로브가 모두 실패하면
/// `None`(기존 미터를 유지하고 다음 주기에 다시 시도한다).
pub(super) fn claude_reset_count(
    token: &str,
    send: impl Fn(&str) -> Option<Zeroizing<Vec<u8>>>,
) -> Option<u64> {
    let juniper = claude_probe_config(token, ClaudeResetProbe::Juniper)
        .and_then(|cfg| send(&cfg))
        .and_then(|body| parse_juniper_status(std::str::from_utf8(&body).ok()?))
        .map(u64::from);
    let cedar = claude_probe_config(token, ClaudeResetProbe::Cedar)
        .and_then(|cfg| send(&cfg))
        .and_then(|body| parse_cedar_status(std::str::from_utf8(&body).ok()?, now_secs()));
    match (juniper, cedar) {
        (Some(j), Some(c)) => Some(j.saturating_add(c)),
        (Some(j), None) => Some(j),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

/// 소진 시점에 제공 중인 프로그램. 바깥은 `None`(프로브 실패)과 제공 없음(`Some(None)`)을
/// 구분한다. 실패는 `Err`(재시도 유도), 제공 없음은 `NoCredit`으로 답한다.
#[expect(clippy::option_option, reason = "프로브 실패와 제공 없음을 구분한다")]
pub(super) fn claude_offered_program(
    token: &str,
    send: impl Fn(&str) -> Option<Zeroizing<Vec<u8>>>,
) -> Option<Option<ClaudeResetProgram>> {
    let juniper = claude_probe_config(token, ClaudeResetProbe::Juniper)
        .and_then(|cfg| send(&cfg))
        .and_then(|body| parse_juniper_status(std::str::from_utf8(&body).ok()?));
    if juniper == Some(true) {
        return Some(Some(ClaudeResetProgram::Juniper));
    }
    let cedar = claude_probe_config(token, ClaudeResetProbe::Cedar)
        .and_then(|cfg| send(&cfg))
        .and_then(|body| parse_cedar_status(std::str::from_utf8(&body).ok()?, now_secs()));
    match (juniper, cedar) {
        (_, Some(usable)) if usable > 0 => Some(Some(ClaudeResetProgram::Cedar)),
        (Some(_), _) | (_, Some(_)) => Some(None),
        (None, None) => None,
    }
}

/// Claude 리셋 상태 조회 설정. 프로브마다 고정 쿼리를 쓴다.
fn claude_probe_config(token: &str, probe: ClaudeResetProbe) -> Option<Zeroizing<String>> {
    if !check_credential_text(token, None) {
        return None;
    }
    let query = match probe {
        ClaudeResetProbe::Juniper => "at_wall=1&skip_spend=1",
        ClaudeResetProbe::Cedar => "cedar_ember=1&skip_spend=1",
    };
    Some(Zeroizing::new(format!(
        "url = \"https://api.anthropic.com/api/oauth/usage?{query}\"\nrequest = \"GET\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"User-Agent: claude-code/2.1.0\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Accept: application/json\"\n"
    )))
}

/// `juniper_tide` 블록의 주간 세션 리셋 제공 여부. `None`은 읽을 수 없음이다.
fn parse_juniper_status(text: &str) -> Option<bool> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let block = json.get("juniper_tide")?;
    if block.is_null() {
        return Some(false);
    }
    let offered = block.get("eligible").and_then(serde_json::Value::as_bool) == Some(true)
        && block.get("arm").and_then(serde_json::Value::as_str) == Some("reset")
        && block.get("available").and_then(serde_json::Value::as_bool) == Some(true);
    Some(offered)
}

/// `cedar_ember` 블록의 사용 가능 grant 합. `None`은 읽을 수 없음이다.
fn parse_cedar_status(text: &str, now: i64) -> Option<u64> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let block = json.get("cedar_ember")?;
    if block.is_null() {
        return Some(0);
    }
    if block.get("eligible").and_then(serde_json::Value::as_bool) == Some(false) {
        return Some(0);
    }
    let mut total = 0_u64;
    for grant in block.get("grants").and_then(serde_json::Value::as_array)? {
        if grant.get("usable_now").and_then(serde_json::Value::as_bool) != Some(true) {
            continue;
        }
        if grant.get("paused").and_then(serde_json::Value::as_bool) == Some(true) {
            continue;
        }
        let left = grant
            .get("resets_left")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if left == 0 {
            continue;
        }
        if let Some(start) = grant.get("starts_at").and_then(serde_json::Value::as_str) {
            let Some(start) = krill_proto::time::parse_reset_epoch(start) else {
                continue;
            };
            if start > now {
                continue;
            }
        }
        if let Some(end) = grant.get("ends_at").and_then(serde_json::Value::as_str) {
            let Some(end) = krill_proto::time::parse_reset_epoch(end) else {
                continue;
            };
            if end <= now {
                continue;
            }
        }
        total = total.saturating_add(left);
    }
    Some(total)
}

/// 소진 경로용 Claude 토큰. 프로필·프로브·소진이 같은 자격증명을 쓴다.
pub(super) fn claude_token(
    dir: &Path,
    host: bool,
    home: &Path,
    state: Option<&Path>,
) -> Option<Zeroizing<String>> {
    credentials(ProviderKind::Claude, dir, host, home, state).map(|(token, _)| token)
}

/// Claude OAuth 프로필의 조직 UUID. 소진 POST 경로에 쓴다.
pub(super) fn claude_org_uuid(
    token: &str,
    send: impl FnOnce(&str) -> Option<Zeroizing<Vec<u8>>>,
) -> Option<String> {
    if !check_credential_text(token, None) {
        return None;
    }
    let config = Zeroizing::new(format!(
        "url = \"https://api.anthropic.com/api/oauth/profile\"\nrequest = \"GET\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"User-Agent: claude-code/2.1.0\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Accept: application/json\"\n"
    ));
    let body = send(&config)?;
    let json: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let uuid = json.pointer("/organization/uuid")?.as_str()?;
    (!uuid.is_empty() && uuid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .then(|| uuid.to_owned())
}

/// Claude 리셋 소진. 성공하면 `(결과, 초기화된 창 수)`. 멱등 키가 없어 재시도 금지:
/// 타임아웃 후 재소진은 서버의 `already_used` 가드로 막힌다.
pub(super) fn claude_claim_reset(
    token: &str,
    org_uuid: &str,
    program: ClaudeResetProgram,
    send: impl FnOnce(&str) -> Option<(u16, Zeroizing<Vec<u8>>)>,
) -> Option<(ResetConsumeOutcome, u64)> {
    if !check_credential_text(token, None) {
        return None;
    }
    if org_uuid.is_empty()
        || !org_uuid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    let config = Zeroizing::new(format!(
        "url = \"https://api.anthropic.com/api/organizations/{org_uuid}/reset_rate_limits\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"User-Agent: claude-code/2.1.0\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Content-Type: application/json\"\nheader = \"Accept: application/json\"\ndata = '{{\"program\": \"{}\"}}'\n",
        program.as_str()
    ));
    let (status, body) = send(&config)?;
    if status == 429 || status == 401 || status == 403 {
        return None;
    }
    if !(200..300).contains(&status) {
        return None;
    }
    match serde_json::from_slice::<serde_json::Value>(&body)
        .ok()?
        .get("result")?
        .as_str()?
    {
        "reset" => Some((ResetConsumeOutcome::Reset, 1)),
        "already_used" => Some((ResetConsumeOutcome::AlreadyRedeemed, 0)),
        "not_limited" => Some((ResetConsumeOutcome::NothingToReset, 0)),
        "ineligible" | "unavailable" => Some((ResetConsumeOutcome::NoCredit, 0)),
        _ => None,
    }
}

/// curl 전송 후 `(상태 코드, 본문)`. `--fail`을 쓰지 않아 429·401도 구분한다.
pub(super) fn curl_status(config: &str) -> Option<(u16, Zeroizing<Vec<u8>>)> {
    curl_response(config).map(|(code, _, body)| (code, body))
}

/// `curl_status`에 초 단위 Retry-After를 더한다.
fn curl_response(config: &str) -> Option<(u16, Option<Duration>, Zeroizing<Vec<u8>>)> {
    let out = pipe_command(
        system_curl(),
        &[
            "-q",
            "--config",
            "-",
            "--silent",
            "--max-time",
            "10",
            "--max-filesize",
            "524288",
            "-w",
            "\n%{http_code} %header{retry-after}",
        ],
        config.as_bytes(),
    )?;
    let split = out.iter().rposition(|&b| b == b'\n')?;
    let (body, tail) = out.split_at(split);
    let (code, retry_after) = parse_status_tail(std::str::from_utf8(tail).ok()?)?;
    Some((code, retry_after, Zeroizing::new(body.to_vec())))
}

/// `-w` 꼬리(`429 191`)를 푼다. Retry-After는 초 단위만 읽고 HTTP-date·빈 값은 `None`이다.
fn parse_status_tail(tail: &str) -> Option<(u16, Option<Duration>)> {
    let mut parts = tail.split_whitespace();
    let code = parts.next()?.parse().ok()?;
    let retry_after = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|&secs| secs > 0)
        .map(Duration::from_secs);
    Some((code, retry_after))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// 멱등 키(UUID v4). `ring`은 이미 `krilld` 직접 의존성이다(새 크레이트 없음).
fn redeem_request_id() -> Option<String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0_u8; 16];
    ring::rand::SystemRandom::new().fill(&mut bytes).ok()?;
    if let Some(b) = bytes.get_mut(6) {
        *b = (*b & 0x0f) | 0x40;
    }
    if let Some(b) = bytes.get_mut(8) {
        *b = (*b & 0x3f) | 0x80;
    }
    let mut id = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            id.push('-');
        }
        for shift in [4, 0] {
            id.push(char::from_digit(u32::from((b >> shift) & 0xf), 16).unwrap_or('0'));
        }
    }
    Some(id)
}

/// 토큰이 명령행·환경 변수에 노출되지 않도록 curl 설정을 stdin으로 전한다.
/// curl 설정에 그대로 들어가는 토큰·계정 ID 검증(인젝션 방지). 소진 요청도 같은 규칙이다.
fn check_credential_text(token: &str, account: Option<&str>) -> bool {
    if token.is_empty()
        || !token
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\'))
    {
        return false;
    }
    !account.is_some_and(|a| {
        !a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    })
}

fn request_config(
    provider: ProviderKind,
    token: &str,
    account: Option<&str>,
) -> Option<Zeroizing<String>> {
    if !check_credential_text(token, account) {
        return None;
    }
    let (url, headers) = match provider {
        ProviderKind::Claude => (
            "https://api.anthropic.com/api/oauth/usage",
            "header = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"User-Agent: claude-code/2.1.0\"\n",
        ),
        ProviderKind::Codex => (
            "https://chatgpt.com/backend-api/wham/usage",
            "header = \"User-Agent: codex-cli\"\nheader = \"OpenAI-Beta: codex-1\"\nheader = \"originator: Codex Desktop\"\n",
        ),
        ProviderKind::OpenCode => (
            "https://opencode.ai/zen/go/v1/usage",
            "header = \"Accept: application/json\"\n",
        ),
        _ => return None,
    };
    let mut config = Zeroizing::new(format!(
        "url = \"{url}\"\nrequest = \"GET\"\nheader = \"Authorization: Bearer {token}\"\n{headers}"
    ));
    if let Some(account) = account {
        config.push_str("header = \"ChatGPT-Account-Id: ");
        config.push_str(account);
        config.push_str("\"\n");
    }
    Some(config)
}

pub(super) fn read_file(path: &Path) -> Option<Zeroizing<Vec<u8>>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_BYTES).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn opencode_go_override(home: &Path, state: Option<&Path>) -> Option<Zeroizing<String>> {
    let candidates = [
        state.map(|s| s.join("opencode-go-api-key")),
        Some(krill_proto::user_dirs::config_dir(home).join("opencode-go-api-key")),
    ];
    for path in candidates.into_iter().flatten() {
        if let Some(raw) = read_file(&path) {
            let trimmed = String::from_utf8_lossy(&raw).trim().to_owned();
            if !trimmed.is_empty() {
                return Some(Zeroizing::new(trimmed));
            }
        }
    }
    None
}

fn credentials(
    provider: ProviderKind,
    dir: &Path,
    host: bool,
    home: &Path,
    state: Option<&Path>,
) -> Option<(Zeroizing<String>, Option<String>)> {
    if provider == ProviderKind::OpenCode {
        if let Some(key) = opencode_go_override(home, state) {
            return Some((key, None));
        }
        let auth = if host {
            let default_home = accounts::default_home();
            std::env::var_os("XDG_DATA_HOME")
                .map_or_else(|| default_home.join(".local/share"), PathBuf::from)
                .join("opencode/auth.json")
        } else {
            dir.join("data/opencode/auth.json")
        };
        let from_auth = read_file(&auth).and_then(|raw| {
            let mut json: serde_json::Value = serde_json::from_slice(&raw).ok()?;
            ["opencode-go", "opencode"].into_iter().find_map(|name| {
                let credential = json.get_mut(name)?;
                if credential.get("type").and_then(serde_json::Value::as_str) != Some("api") {
                    return None;
                }
                match credential.get_mut("key")?.take() {
                    serde_json::Value::String(s) if !s.is_empty() => Some(Zeroizing::new(s)),
                    _ => None,
                }
            })
        });
        if let Some(key) = from_auth {
            return Some((key, None));
        }
        let env_key = std::env::var("OPENCODE_API_KEY")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())?;
        return Some((Zeroizing::new(env_key), None));
    }
    let raw = if provider == ProviderKind::Claude {
        let user = claude_keychain_user();
        let mut dirs = vec![dir.to_path_buf()];
        for candidate in [
            krill_proto::os::canonicalize(dir).ok(),
            dir.canonicalize().ok(),
        ]
        .into_iter()
        .flatten()
        {
            if !dirs.contains(&candidate) {
                dirs.push(candidate);
            }
        }
        let accounts: &[&str] = if user == "claude-code-user" {
            &["claude-code-user"]
        } else {
            &[user.as_str(), "claude-code-user"]
        };
        dirs.iter()
            .find_map(|dir| {
                let service = claude_service((!host).then_some(dir.as_path()))?;
                accounts
                    .iter()
                    .find_map(|account| keychain(&service, account))
            })
            .or_else(|| read_file(&dir.join(".credentials.json")))
            .or_else(|| read_file(&dir.join("credentials.json")))?
    } else {
        read_file(&dir.join("auth.json"))?
    };
    parse_credentials(provider, &raw)
}

fn credential_utf8(raw: &[u8]) -> Option<Zeroizing<String>> {
    let raw = raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(raw);
    if let Ok(text) = std::str::from_utf8(raw)
        && !text.trim_end_matches('\0').contains('\0')
    {
        return Some(Zeroizing::new(text.trim_end_matches('\0').to_owned()));
    }
    let (raw, big_endian) = if let Some(body) = raw.strip_prefix(&[0xfe, 0xff]) {
        (body, true)
    } else {
        (raw.strip_prefix(&[0xff, 0xfe]).unwrap_or(raw), false)
    };
    if raw.len() >= 2 && raw.len().is_multiple_of(2) {
        let units = Zeroizing::new(
            raw.as_chunks::<2>()
                .0
                .iter()
                .copied()
                .map(|unit| {
                    if big_endian {
                        u16::from_be_bytes(unit)
                    } else {
                        u16::from_le_bytes(unit)
                    }
                })
                .collect::<Vec<_>>(),
        );
        let text = Zeroizing::new(String::from_utf16(&units).ok()?);
        let text = text.trim_end_matches('\0');
        return (!text.contains('\0')).then(|| Zeroizing::new(text.to_owned()));
    }
    None
}

fn decode_base64(input: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut bits = 0_u32;
    let mut count = 0_u32;
    let mut pad = 0_u8;
    let mut out = Zeroizing::new(Vec::with_capacity(input.len() * 3 / 4));
    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            pad = pad.saturating_add(1);
            if pad > 2 {
                return None;
            }
            continue;
        }
        if pad > 0 {
            return None;
        }
        let val = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(val);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
        if u64::try_from(out.len()).ok()? > MAX_BYTES {
            return None;
        }
    }
    if count >= 6 {
        return None;
    }
    Some(out)
}

fn parse_credentials(
    provider: ProviderKind,
    raw: &[u8],
) -> Option<(Zeroizing<String>, Option<String>)> {
    let text = credential_utf8(raw)?;
    let mut json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let (token_path, account_path) = if provider == ProviderKind::Claude {
        ("/claudeAiOauth/accessToken", "")
    } else {
        ("/tokens/access_token", "/tokens/account_id")
    };
    let account = json
        .pointer(account_path)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let token = json.pointer_mut(token_path)?.take();
    let serde_json::Value::String(token) = token else {
        return None;
    };
    (!token.is_empty()).then(|| (Zeroizing::new(token), account))
}

/// Claude Code가 키체인 항목 account로 쓰는 사용자 이름.
pub(crate) fn claude_keychain_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
        .unwrap_or_else(|| "claude-code-user".into())
}

/// Claude Code 자격증명 키체인 서비스 이름. 프로필 폴더면 경로(NFC) SHA-256 앞 8자를 붙인다.
pub(crate) fn claude_service(dir: Option<&Path>) -> Option<String> {
    let Some(dir) = dir else {
        return Some("Claude Code-credentials".into());
    };
    let path = dir.to_str()?;
    // macOS 파일명은 분해형일 수 있어 Claude의 NFC 해시 규약에 맞춘다. Windows·Linux는 이미 NFC다.
    #[cfg(target_os = "macos")]
    let normalized = if path.is_ascii() {
        path.as_bytes().to_vec()
    } else {
        pipe_command(
            "/usr/bin/iconv",
            &["-f", "UTF-8-MAC", "-t", "UTF-8"],
            path.as_bytes(),
        )?
        .to_vec()
    };
    #[cfg(not(target_os = "macos"))]
    let normalized = path.as_bytes().to_vec();
    let digest = format!("{:x}", Sha256::digest(&normalized));
    Some(format!(
        "Claude Code-credentials-{}",
        digest.chars().take(8).collect::<String>()
    ))
}

// Claude Code·agy(go-keyring)는 `/usr/bin/security`로 항목을 만들어 ACL이 그 도구만 믿는다.
// krilld가 SecItem으로 직접 읽으면 macOS가 매번 로그인 키체인 암호를 묻는다 — Orca처럼 같은 도구로 읽는다.
#[cfg(all(not(test), not(windows)))]
fn keychain(service: &str, account: &str) -> Option<Zeroizing<Vec<u8>>> {
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let stdout = Zeroizing::new(output.stdout);
    (output.status.success() && u64::try_from(stdout.len()).ok()? <= MAX_BYTES).then_some(stdout)
}
// Windows 자격 증명 관리자: go-keyring(`service:account`), keytar/Bun(`service/account`), keyring-rs(`account.service`) 순으로 찾는다.
#[cfg(all(not(test), windows))]
fn keychain(service: &str, account: &str) -> Option<Zeroizing<Vec<u8>>> {
    [
        format!("{service}:{account}"),
        format!("{service}/{account}"),
        format!("{account}.{service}"),
        service.to_owned(),
    ]
    .into_iter()
    .find_map(|target| crate::sys::generic_credential(&target))
}
#[cfg(test)]
fn keychain(_: &str, _: &str) -> Option<Zeroizing<Vec<u8>>> {
    None
}

/// 인증 정보를 전달하므로 PATH를 검색하지 않고 OS 기본 curl을 쓴다.
fn system_curl() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
            .join("curl.exe")
    } else {
        PathBuf::from("/usr/bin/curl")
    }
}

fn pipe_command(
    exe: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    input: &[u8],
) -> Option<Zeroizing<Vec<u8>>> {
    let mut command = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let write = child.stdin.take()?.write_all(input);
    if write.is_err() {
        child.kill().ok();
        child.wait().ok();
        return None;
    }
    let output = child.wait_with_output().ok()?;
    (output.status.success() && u64::try_from(output.stdout.len()).ok()? <= MAX_BYTES)
        .then(|| Zeroizing::new(output.stdout))
}

pub(super) fn antigravity_auth(dir: &Path, host: bool) -> Option<AuthStatus> {
    if !host {
        // 로그인 판정은 krill-proto와 공유한다. 신원은 로그인된 프로필에서만 읽는다.
        let logged_in = krill_proto::account_login::agy_token_logged_in(dir);
        return Some(AuthStatus {
            logged_in,
            identity: logged_in
                .then(|| {
                    read_file(&dir.join(".gemini/antigravity-cli/antigravity-oauth-token"))
                        .and_then(|raw| {
                            accounts::identity_from_credentials(ProviderKind::Antigravity, &raw)
                        })
                })
                .flatten(),
        });
    }
    // 호스트(OWE-901): 키체인 파싱 → 토큰 파일 존재 순(코디네이터 결정: 마커 검사 없음).
    // 로그인은 존재·크기로 판정하고, 표시용 이메일만 상한 있는 읽기로 얻는다. 호스트 `dir`은 이미
    // `~/.gemini/antigravity-cli`라 토큰 파일명을 직접 붙인다(프로필 헬퍼와 다름).
    if let Some(raw) = keychain("gemini", "antigravity")
        && let Some(status) = parse_antigravity_auth(&raw)
    {
        return Some(status);
    }
    let token = dir.join("antigravity-oauth-token");
    if std::fs::symlink_metadata(&token)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0)
    {
        return Some(AuthStatus {
            logged_in: true,
            identity: read_file(&token).and_then(|raw| {
                accounts::identity_from_credentials(ProviderKind::Antigravity, &raw)
            }),
        });
    }
    None
}

fn parse_antigravity_auth(raw: &[u8]) -> Option<AuthStatus> {
    let raw_text = credential_utf8(raw)?;
    let text = raw_text.trim();
    let decoded = if let Some(encoded) = text.strip_prefix("go-keyring-base64:") {
        decode_base64(encoded)?
    } else if let Some(encoded) = text.strip_prefix("go-keyring-encoded:") {
        let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
        if !remainder.is_empty() {
            return None;
        }
        let bytes = pairs
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
            .collect::<Option<Vec<_>>>()?;
        Zeroizing::new(bytes)
    } else {
        Zeroizing::new(text.as_bytes().to_vec())
    };
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let valid = json
        .get("auth_method")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty())
        && json
            .pointer("/token/access_token")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.is_empty());
    Some(AuthStatus {
        logged_in: valid,
        identity: valid
            .then(|| accounts::identity_from_credentials(ProviderKind::Antigravity, &decoded))
            .flatten(),
    })
}

fn parse_codex_backend(text: &str) -> Usage {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Usage::default();
    };
    let mut usage = Usage::default();
    for name in ["primary_window", "secondary_window"] {
        let Some(raw) = json.get("rate_limit").and_then(|v| v.get(name)) else {
            continue;
        };
        let slot = match raw
            .get("limit_window_seconds")
            .and_then(serde_json::Value::as_u64)
        {
            Some(18_000) => &mut usage.five_hour,
            Some(604_800) => &mut usage.seven_day,
            _ => continue,
        };
        let Some(percent) = raw.get("used_percent").and_then(super::percent) else {
            continue;
        };
        *slot = Some(Window {
            percent,
            resets_at: raw.get("reset_at").and_then(super::resets),
        });
    }
    usage
}

/// OWE-901: agy 프로필·호스트 사용량 실조회.
///
/// `agy [--gemini_dir=<프로필>/.gemini] -p /usage --output-format json --print-timeout 20s`를
/// `SSH_CONNECTION=krill`과 함께 백그라운드 스레드에서 주기 실행한다. `--disable-slash-commands`는
/// 절대 붙이지 않는다(일반 프롬프트 실행과 구분이 안 되면 모델 턴을 소모할 수 있다).
/// 토큰·응답 원문은 캐시·로그·IPC에 넣지 않고 파싱된 5h/7d 수치만 둔다.
///
/// 안전장치:
/// - agy 1.1.11 미만·버전 미상이면 건너뛴다(구버전은 `/usage`를 프롬프트로 받아 모델 턴을 쓸 수 있다).
/// - 응답에 모델 턴(`num_turns`>0) 흔적이 있으면 프로세스 전역 래치로 영구 중단한다.
/// - 출력 512KB 상한·실행 30초 타임아웃.
pub(crate) mod agy {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use krill_proto::usage::ProviderKind;

    use super::super::{ProfileUsage, Usage, Window, accounts};
    use super::MAX_BYTES;
    use crate::session::lock;

    type Key = (PathBuf, PathBuf);
    static CACHE: Mutex<BTreeMap<Key, (Instant, ProfileUsage)>> = Mutex::new(BTreeMap::new());
    static FAILED: Mutex<BTreeMap<Key, Instant>> = Mutex::new(BTreeMap::new());
    static PENDING: Mutex<BTreeSet<Key>> = Mutex::new(BTreeSet::new());
    /// 주기 실행 간격(5분)·캐시 유효(10분). 5h·주간 창에는 충분하고 agy 실행 부담을 낮춘다.
    const TTL: Duration = super::CACHE_TTL;
    /// `agy -p /usage` 실행 타임아웃(agy 자체 `--print-timeout 20s`보다 길게).
    const TIMEOUT: Duration = Duration::from_secs(30);
    /// `/usage`를 지원하는 최소 agy 버전(OWE-901).
    const MIN_VERSION: (u64, u64, u64) = (1, 1, 11);
    /// 버전 확인용 `agy --version` 타임아웃.
    const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

    /// 모델 턴 흔적을 본 뒤에는 이 프로세스에서 agy 사용량 조회를 영구 중단한다.
    static MODEL_TURN_LATCH: AtomicBool = AtomicBool::new(false);

    pub(super) fn latched() -> bool {
        MODEL_TURN_LATCH.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(super) fn reset_latch_for_tests() {
        MODEL_TURN_LATCH.store(false, Ordering::SeqCst);
        lock(&CACHE).clear();
        lock(&FAILED).clear();
        lock(&PENDING).clear();
        lock(&VERSION_CACHE).clear();
        lock(&VERSION_TEXT_CACHE).clear();
    }

    pub(super) fn cached(home: &Path, dir: &Path) -> Option<ProfileUsage> {
        if latched() {
            return None;
        }
        lock(&CACHE)
            .get(&(home.to_path_buf(), dir.to_path_buf()))
            .filter(|(at, _)| at.elapsed() < TTL)
            .map(|(_, usage)| usage.clone())
    }

    pub(super) fn evict(home: &Path) {
        lock(&CACHE).retain(|(h, _), _| h != home);
        lock(&FAILED).retain(|(h, _), _| h != home);
        lock(&PENDING).retain(|(h, _)| h != home);
    }

    /// `direct::refresh`가 `SAVE_LOCK` 없이 호출한다. 실제 실행은 스레드에서만.
    pub(super) fn refresh(
        home: &Path,
        state: Option<&Path>,
        file: &accounts::AccountsFile,
        interval: Duration,
    ) {
        if cfg!(test) || latched() {
            return;
        }
        let mut profiles = vec![(
            accounts::host_default_dir(home, ProviderKind::Antigravity),
            true,
        )];
        for a in &file.accounts {
            if ProviderKind::parse(&a.provider) != Some(ProviderKind::Antigravity) {
                continue;
            }
            let dir = PathBuf::from(&a.profile_dir);
            if dir.as_os_str().is_empty() || !dir.is_dir() {
                continue;
            }
            profiles.push((dir, false));
        }
        for (dir, host) in profiles {
            let key = (home.to_path_buf(), dir);
            if lock(&CACHE)
                .get(&key)
                .is_some_and(|(at, _)| at.elapsed() < interval)
                || lock(&FAILED)
                    .get(&key)
                    .is_some_and(|at| at.elapsed() < interval)
                || lock(&PENDING).contains(&key)
            {
                continue;
            }
            // 로그인 안 된 agy에 `-p /usage`를 보내면 매번 대화형 OAuth를 시작한다(설 실기, v0.1.9:
            // 격리 HOME에서 2~3분마다 OAuth 시도 46회). 로그인된 프로필만 조회한다.
            if !should_probe(&key.1, host) || !lock(&PENDING).insert(key.clone()) {
                continue;
            }
            spawn(&key, host, state.map(Path::to_path_buf));
        }
    }

    fn should_probe(dir: &Path, host: bool) -> bool {
        super::antigravity_auth(dir, host).is_some_and(|a| a.logged_in)
    }

    fn spawn(key: &Key, host: bool, state: Option<PathBuf>) {
        let job = key.clone();
        let launched = std::thread::Builder::new()
            .name("agy-usage".into())
            .spawn(move || {
                let usage = std::panic::catch_unwind(|| {
                    let exe = super::agy_exe()?;
                    fetch(&exe, &job.1, host)
                })
                .ok()
                .flatten();
                // 조회 중 사용자가 직접 조회를 끄면 결과도 버린다.
                let enabled = accounts::load_file(&job.0, state.as_deref()).direct_usage;
                if enabled {
                    if let Some((usage, meters)) = usage {
                        lock(&FAILED).remove(&job);
                        super::super::meters::remember(
                            &job.0,
                            ProviderKind::Antigravity,
                            &job.1,
                            meters,
                        );
                        // `/usage` 폴링에서는 런타임 훅 전용 `RATE_LIMITED`를 찍지 않는다.
                        // Gemini 기본 사용량이 100% 미만으로 회복되면 기존 한도 표기를 지운다.
                        if usage_max(&usage) < 100
                            && let Some(id) = account_id_for(&job.0, state.as_deref(), &job.1, host)
                        {
                            accounts::clear_rate_limited(ProviderKind::Antigravity, &id);
                        }
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs());
                        let mut cache = lock(&CACHE);
                        cache.retain(|_, (at, _)| at.elapsed() < TTL);
                        if cache.len() >= 256 {
                            cache.pop_first();
                        }
                        cache.insert(
                            job.clone(),
                            (
                                Instant::now(),
                                ProfileUsage {
                                    usage,
                                    updated_at: now,
                                    ..ProfileUsage::default()
                                },
                            ),
                        );
                    } else {
                        let mut failed = lock(&FAILED);
                        failed.retain(|_, at| at.elapsed() < TTL);
                        if failed.len() >= 256 {
                            failed.pop_first();
                        }
                        failed.insert(job.clone(), Instant::now());
                    }
                }
                lock(&PENDING).remove(&job);
            });
        if launched.is_err() {
            lock(&PENDING).remove(key);
        }
    }

    fn usage_max(usage: &Usage) -> u8 {
        usage
            .five_hour
            .as_ref()
            .map_or(0, |w| w.percent)
            .max(usage.seven_day.as_ref().map_or(0, |w| w.percent))
    }

    /// 프로필 폴더로 계정 id를 찾는다(읽기 전용). 호스트는 호스트 id다.
    fn account_id_for(home: &Path, state: Option<&Path>, dir: &Path, host: bool) -> Option<String> {
        if host {
            return Some(accounts::host_default_id(ProviderKind::Antigravity));
        }
        let file = accounts::load_file(home, state);
        file.accounts.iter().find_map(|a| {
            (ProviderKind::parse(&a.provider) == Some(ProviderKind::Antigravity)
                && Path::new(&a.profile_dir) == dir)
                .then(|| a.id.clone())
        })
    }

    /// 버전 게이트를 포함한 실조회 한 번. `exe`는 호출자가 정한다(실행은 로그인 셸 PATH의 agy,
    /// 테스트는 가짜 바이너리).
    fn fetch(
        exe: &Path,
        dir: &Path,
        host: bool,
    ) -> Option<(Usage, krill_proto::usage::AccountMeters)> {
        if latched() {
            return None;
        }
        if !version_supported(exe) {
            return None;
        }
        let gemini_dir;
        let gemini_flag;
        let mut argv: Vec<String> = Vec::new();
        if !host {
            gemini_dir = dir.join(".gemini");
            let text = gemini_dir.to_str()?;
            gemini_flag = format!("--gemini_dir={text}");
            argv.push(gemini_flag);
        }
        argv.extend(
            [
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--print-timeout",
                "20s",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        // `--disable-slash-commands`는 절대 붙이지 않는다(OWE-901).
        debug_assert!(!argv.iter().any(|a| a.contains("disable-slash-commands")));
        // 호스트 기본 프로필이 macOS 키체인만으로 로그인된 경우 `SSH_CONNECTION=krill`을 주면
        // 파일 토큰(`antigravity-oauth-token`)만 찾다가 대화형 OAuth 대기에 빠진다.
        let ssh_connection = !host || dir.join("antigravity-oauth-token").is_file() || cfg!(test);
        let body = run_agy(exe, &argv, ssh_connection, TIMEOUT, MAX_BYTES)?;
        let text = std::str::from_utf8(&body).ok()?;
        if has_model_turn_trace(text) {
            MODEL_TURN_LATCH.store(true, Ordering::SeqCst);
            return None;
        }
        let usage = krill_proto::usage::parse_antigravity_usage(text);
        let meters = super::super::meters::parse(ProviderKind::Antigravity, text);
        (!usage.is_empty() || !meters.meters.is_empty())
            .then_some((from_proto_snapshot(&usage), meters))
    }

    /// krill-proto 봉투를 krilld 내부 사용량으로 옮긴다(토큰·원문은 버린다).
    fn from_proto_snapshot(snap: &krill_proto::usage::Snapshot) -> Usage {
        let window = |w: Option<&krill_proto::usage::Window>| {
            w.map(|w| Window {
                percent: w.percent,
                resets_at: w.resets_at.clone(),
            })
        };
        Usage {
            five_hour: window(snap.five_hour.as_ref()),
            seven_day: window(snap.seven_day.as_ref()),
        }
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(super) fn fetch_for_tests(exe: &Path, dir: &Path, host: bool) -> Option<Usage> {
        fetch(exe, dir, host).map(|(usage, _)| usage)
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(super) fn probe_version_for_tests(exe: &Path) -> bool {
        version_supported(exe)
    }

    /// `agy --version` 첫 `X.Y.Z`가 1.1.11 이상이면 참. 모르면 거짓(건너뜀).
    /// 캐시 키는 (실행 경로, mtime, 버전 문자열)이라 업그레이드하면 자동으로 다시 본다.
    fn version_supported(exe: &Path) -> bool {
        let (mtime, len) = std::fs::symlink_metadata(exe)
            .and_then(|m| {
                m.modified().map(|t| {
                    (
                        t.duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs()),
                        m.len(),
                    )
                })
            })
            .unwrap_or((0, 0));
        let version = version_string(exe, mtime, len);
        if let Some(cached) = lock(&VERSION_CACHE)
            .get(&(exe.to_path_buf(), mtime, version.clone()))
            .map(|(_, supported)| *supported)
        {
            return cached;
        }
        let supported = parse_version(&version).is_some_and(|v| v >= MIN_VERSION);
        let mut cache = lock(&VERSION_CACHE);
        if cache.len() >= 64 {
            cache.pop_first();
        }
        cache.insert(
            (exe.to_path_buf(), mtime, version),
            (Instant::now(), supported),
        );
        supported
    }

    /// 버전 문자열 조회(바이너리 식별용 캐시). 내용은 파싱 전 원문 그대로 둔다.
    fn version_string(exe: &Path, mtime: u64, len: u64) -> String {
        if let Some(hit) = lock(&VERSION_TEXT_CACHE).get(&(exe.to_path_buf(), mtime, len)) {
            return hit.clone();
        }
        let text = run_agy(
            exe,
            &["--version".to_owned()],
            true,
            VERSION_TIMEOUT,
            64 * 1024,
        )
        .and_then(|b| String::from_utf8(b).ok());
        let Some(text) = text.filter(|s| parse_version(s).is_some()) else {
            // A failed/partial cold-start probe says nothing about the installed
            // version. Retry at the next usage poll instead of caching failure
            // until the executable is replaced.
            return String::new();
        };
        // 키 팽창 방지: 앞 64글자만 쓴다(버전 표기는 앞에 있다).
        let text = text.chars().take(64).collect::<String>();
        let mut cache = lock(&VERSION_TEXT_CACHE);
        if cache.len() >= 64 {
            cache.pop_first();
        }
        cache.insert((exe.to_path_buf(), mtime, len), text.clone());
        text
    }

    pub(crate) fn version_text_for_guard(exe: &Path) -> String {
        let (mtime, len) = std::fs::symlink_metadata(exe)
            .and_then(|m| {
                m.modified().map(|t| {
                    (
                        t.duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs()),
                        m.len(),
                    )
                })
            })
            .unwrap_or((0, 0));
        version_string(exe, mtime, len)
    }

    type VersionKey = (PathBuf, u64, String);
    static VERSION_CACHE: Mutex<BTreeMap<VersionKey, (Instant, bool)>> =
        Mutex::new(BTreeMap::new());
    static VERSION_TEXT_CACHE: Mutex<BTreeMap<(PathBuf, u64, u64), String>> =
        Mutex::new(BTreeMap::new());

    /// 출력에서 첫 `X.Y.Z`를 찾는다. 인덱싱 없이 `.get()`으로만 읽는다.
    fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
        let bytes = text.as_bytes();
        let mut pos = 0;
        while pos < bytes.len() {
            if !bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                pos += 1;
                continue;
            }
            let mut parts = [0u64; 3];
            let mut cur = pos;
            let mut ok = true;
            for (idx, slot) in parts.iter_mut().enumerate() {
                let start = cur;
                while bytes.get(cur).is_some_and(u8::is_ascii_digit) {
                    cur += 1;
                }
                if start == cur {
                    ok = false;
                    break;
                }
                *slot = text.get(start..cur)?.parse().ok()?;
                if idx < 2 {
                    if bytes.get(cur) != Some(&b'.') {
                        ok = false;
                        break;
                    }
                    cur += 1;
                }
            }
            if ok {
                let [first, second, third] = parts;
                return Some((first, second, third));
            }
            pos += 1;
        }
        None
    }

    const NUM_TURNS_PAT: &str = "\"num_turns\"";

    /// 모델 턴 흔적(`num_turns`>0). 파싱되면 재귀 탐색, 아니면 `"num_turns":<0이 아닌 숫자>`.
    fn has_model_turn_trace(text: &str) -> bool {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            return contains_num_turns(&value);
        }
        let mut rest = text;
        while let Some(found) = rest.find(NUM_TURNS_PAT) {
            let Some(tail) = rest.get(found + NUM_TURNS_PAT.len()..) else {
                return false;
            };
            rest = tail;
            let after = tail.trim_start_matches([' ', '\t', '\r', '\n']);
            let Some(num) = after.strip_prefix(':') else {
                continue;
            };
            let num = num.trim_start_matches([' ', '\t', '\r', '\n']);
            if let Some(d) = num.as_bytes().first()
                && d.is_ascii_digit()
                && *d != b'0'
            {
                return true;
            }
        }
        false
    }

    fn contains_num_turns(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => map.iter().any(|(k, v)| {
                (k == "num_turns"
                    && match v {
                        serde_json::Value::Number(n) => {
                            n.as_u64().is_some_and(|n| n > 0) || n.as_f64().is_some_and(|f| f > 0.0)
                        }
                        _ => false,
                    })
                    || contains_num_turns(v)
            }),
            serde_json::Value::Array(items) => items.iter().any(contains_num_turns),
            _ => false,
        }
    }

    /// agy 한 번 실행. 타임아웃·출력 상한을 지킨다. 파이프 교착을 피하려고 stdout은 별도 스레드에서 읽는다.
    fn run_agy(
        exe: &Path,
        args: &[String],
        ssh_connection: bool,
        timeout: Duration,
        max_bytes: u64,
    ) -> Option<Vec<u8>> {
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if ssh_connection {
            cmd.env("SSH_CONNECTION", "krill");
        } else {
            cmd.env_remove("SSH_CONNECTION");
        }
        let mut child = crate::child::spawn(crate::child::own_group(&mut cmd)).ok()?;
        let stdout = child.stdout.take()?;
        let reader = std::thread::Builder::new()
            .name("agy-read".into())
            .spawn(move || {
                use std::io::Read as _;
                let mut buf = Vec::new();
                let limit = max_bytes.saturating_add(1);
                stdout.take(limit).read_to_end(&mut buf).ok()?;
                (u64::try_from(buf.len()).ok()? <= max_bytes).then_some(buf)
            })
            .ok()?;
        let deadline = Instant::now() + timeout;
        let ok = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() >= deadline => break false,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                _ => break false,
            }
        };
        // Drop the job/group before joining stdout so lingering grandchild helpers cannot block EOF.
        drop(child);
        if !ok {
            return None;
        }
        reader.join().ok()?
    }

    // 가짜 agy 실행 파일(셸 스크립트·실행 권한)을 쓰므로 unix 전용이다.
    #[cfg(test)]
    #[cfg(unix)]
    mod tests {
        use std::os::unix::fs::PermissionsExt as _;

        use super::*;
        use crate::session::lock;

        /// 래치·캐시는 프로세스 전역이라 agy 테스트끼리 직렬화한다.
        static SERIAL: Mutex<()> = Mutex::new(());

        fn serial() -> std::sync::MutexGuard<'static, ()> {
            lock(&SERIAL)
        }

        fn write_fake_agy(dir: &std::path::Path, name: &str, script: &str) -> PathBuf {
            let exe = dir.join(name);
            std::fs::write(&exe, script).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
            exe
        }

        const USAGE_ENVELOPE: &str = r#"{
          "status": "SUCCESS",
          "command": {"name": "usage", "data": {"groups": [
            {"name": "Gemini Models", "buckets": [
              {"id": "g-5h", "window": "5h", "remaining_fraction": 0.65, "reset_time": "2026-10-16T15:00:00Z"},
              {"id": "g-wk", "window": "weekly", "remaining_fraction": 0.20, "reset_time": "2026-10-22T00:00:00Z"}
            ]}
          ]}}
        }"#;

        /// 가짜 agy: `--version`은 버전 출력, 그 밖은 인자 검증 뒤 봉투 출력.
        /// `SSH_CONNECTION=krill`이 아니면 실패하고, `--disable-slash-commands`가 있으면 실패한다.
        fn usage_script(version: &str, body: &str) -> String {
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = '--version' ]; then printf '%s\\n' '{version}'; exit 0; fi\n\
                 [ \"$SSH_CONNECTION\" = krill ] || exit 3\n\
                 case \" $* \" in *'--disable-slash-commands'*) exit 4;; esac\n\
                 case \"$1\" in --gemini_dir=*) shift;; esac\n\
                 [ \"$1 $2 $3 $4 $5 $6\" = '-p /usage --output-format json --print-timeout 20s' ] || exit 5\n\
                 printf '%s' '{body}'\n",
            )
        }

        #[test]
        fn failed_version_probe_is_retried_without_reinstalling_agy() {
            let _serial = serial();
            reset_latch_for_tests();
            let root =
                std::env::temp_dir().join(format!("agy-version-retry-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let ready = root.join("ready");
            let exe = write_fake_agy(
                &root,
                "agy",
                &format!(
                    "#!/bin/sh\n[ -f '{}' ] || exit 1\nprintf 'agy 1.1.11\\n'\n",
                    ready.display()
                ),
            );
            assert!(!probe_version_for_tests(&exe));
            std::fs::write(&ready, b"ready").unwrap();
            assert!(
                probe_version_for_tests(&exe),
                "a transient launch failure must not disable usage until the binary changes"
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn version_gate_parses_first_triplet_and_rejects_old_or_unknown() {
            let _serial = serial();
            reset_latch_for_tests();
            let root = std::env::temp_dir().join(format!("owe901-agy-ver-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            let old = write_fake_agy(&root, "agy-old", "#!/bin/sh\necho 'agy 1.1.10'\n");
            let min = write_fake_agy(
                &root,
                "agy-min",
                "#!/bin/sh\necho 'agy version 1.1.11 (darwin arm64)'\n",
            );
            let new = write_fake_agy(&root, "agy-new", "#!/bin/sh\necho '2.0.0'\n");
            let unknown =
                write_fake_agy(&root, "agy-unknown", "#!/bin/sh\necho 'no version here'\n");
            let failing = write_fake_agy(&root, "agy-fail", "#!/bin/sh\nexit 1\n");
            assert!(!probe_version_for_tests(&old));
            assert!(probe_version_for_tests(&min));
            assert!(probe_version_for_tests(&new));
            assert!(!probe_version_for_tests(&unknown));
            assert!(!probe_version_for_tests(&failing));
            assert!(!probe_version_for_tests(&root.join("missing")));
            assert_eq!(parse_version("agy 1.2.3-beta"), Some((1, 2, 3)));
            assert_eq!(parse_version(""), None);
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn live_query_checks_env_and_flags_then_parses_envelope() {
            let _serial = serial();
            reset_latch_for_tests();
            let root = std::env::temp_dir().join(format!("owe901-agy-live-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            let exe = write_fake_agy(&root, "agy", &usage_script("agy 1.1.11", USAGE_ENVELOPE));
            assert!(probe_version_for_tests(&exe));
            // 프로필 조회는 `--gemini_dir=<profile>/.gemini`을 붙인다.
            let profile = root.join("profile");
            std::fs::create_dir_all(&profile).unwrap();
            let usage = fetch_for_tests(&exe, &profile, false).unwrap();
            assert_eq!(usage.five_hour.unwrap().percent, 35);
            assert_eq!(usage.seven_day.unwrap().percent, 80);
            // 호스트 조회는 플래그 없이 뜬다.
            let usage = fetch_for_tests(&exe, &root, true).unwrap();
            assert_eq!(usage.five_hour.unwrap().percent, 35);
            // 오래된 버전이면 건너뛴다.
            let old = write_fake_agy(
                &root,
                "agy-old",
                &usage_script("agy 1.1.10", USAGE_ENVELOPE),
            );
            assert!(fetch_for_tests(&old, &profile, false).is_none());
            std::fs::remove_dir_all(root).unwrap();
        }

        /// 로그인 안 된 agy는 `/usage`가 대화형 로그인을 띄우므로 조회 대상이 아니다(설 실기).
        #[test]
        fn probe_only_logged_in_profiles() {
            let root = std::env::temp_dir().join(format!("agy-probe-gate-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            let host_dir = root.join("home/.gemini/antigravity-cli");
            std::fs::create_dir_all(&host_dir).unwrap();
            assert!(
                !should_probe(&host_dir, true),
                "host without token or keychain"
            );
            let profile = root.join("profile");
            std::fs::create_dir_all(profile.join(".gemini/antigravity-cli")).unwrap();
            assert!(!should_probe(&profile, false), "profile without token");
            std::fs::write(
                profile.join(".gemini/antigravity-cli/antigravity-oauth-token"),
                "x",
            )
            .unwrap();
            assert!(should_probe(&profile, false));
            std::fs::write(host_dir.join("antigravity-oauth-token"), "x").unwrap();
            assert!(should_probe(&host_dir, true));
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn model_turn_trace_latches_permanently() {
            let _serial = serial();
            reset_latch_for_tests();
            let root =
                std::env::temp_dir().join(format!("owe901-agy-latch-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            assert!(!latched());
            let turning = write_fake_agy(
                &root,
                "agy-turn",
                &usage_script(
                    "agy 1.1.11",
                    r#"{"command":{"name":"usage"},"num_turns":2}"#,
                ),
            );
            assert!(fetch_for_tests(&turning, &root, true).is_none());
            assert!(latched());
            // 래치 뒤에는 정상 봉투도 조회하지 않는다.
            let good = write_fake_agy(
                &root,
                "agy-good",
                &usage_script("agy 1.1.11", USAGE_ENVELOPE),
            );
            assert!(fetch_for_tests(&good, &root, true).is_none());
            assert!(cached(&root, &root).is_none());
            // 순수 함수 수준 확인.
            assert!(has_model_turn_trace(r#"{"a":{"num_turns":1}}"#));
            assert!(!has_model_turn_trace(USAGE_ENVELOPE));
            assert!(!has_model_turn_trace(r#"{"num_turns":0}"#));
            assert!(!has_model_turn_trace("not json at all"));
            reset_latch_for_tests();
            assert!(!latched());
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn output_over_cap_is_rejected() {
            let _serial = serial();
            reset_latch_for_tests();
            let root = std::env::temp_dir().join(format!("owe901-agy-cap-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            let big = write_fake_agy(
                &root,
                "agy-big",
                "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'agy 1.1.11'; exit 0; fi\nhead -c 600000 /dev/zero | tr '\\0' 'x'\n",
            );
            assert!(probe_version_for_tests(&big));
            assert!(fetch_for_tests(&big, &root, true).is_none());
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn host_auth_falls_back_to_token_file_without_reading_it() {
            let _serial = serial();
            reset_latch_for_tests();
            let root = std::env::temp_dir().join(format!("owe901-agy-auth-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            // 단위 테스트의 키체인은 항상 비어 있다. 토큰 파일만으로 호스트 로그인이 선다.
            let host = root.join("home").join(".gemini").join("antigravity-cli");
            std::fs::create_dir_all(&host).unwrap();
            assert!(super::super::antigravity_auth(&host, true).is_none());
            let token = host.join("antigravity-oauth-token");
            std::fs::write(&token, b"fixture-only-exists").unwrap();
            assert!(
                super::super::antigravity_auth(&host, true)
                    .unwrap()
                    .logged_in
            );
            // 프로필 판정은 공유 헬퍼와 같다.
            let profile = root.join("profile");
            std::fs::create_dir_all(&profile).unwrap();
            assert!(
                !super::super::antigravity_auth(&profile, false)
                    .unwrap()
                    .logged_in
            );
            let profile_token = profile.join(".gemini/antigravity-cli/antigravity-oauth-token");
            std::fs::create_dir_all(profile_token.parent().unwrap()).unwrap();
            std::fs::write(&profile_token, b"fixture").unwrap();
            assert!(
                super::super::antigravity_auth(&profile, false)
                    .unwrap()
                    .logged_in
            );
            assert_eq!(
                std::fs::read(&token).unwrap(),
                b"fixture-only-exists",
                "판정은 내용을 읽지 않는다"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

/// `PATH`에서 agy를 찾는다(테스트·오프라인에서는 호출하지 않는다).
#[cfg(not(test))]
fn agy_exe() -> Option<PathBuf> {
    use krill_proto::os::ffi::OsStrExt as _;
    let path = std::ffi::OsStr::from_bytes(crate::shell::login_path());
    krill_proto::agent::which("agy", path)
}

#[cfg(test)]
fn agy_exe() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_files_accept_windows_bom_encodings() {
        let json = r#"{"claudeAiOauth":{"accessToken":"fixture-bom-token"}}"#;
        let mut utf8 = vec![0xef, 0xbb, 0xbf];
        utf8.extend_from_slice(json.as_bytes());
        let mut utf16le = vec![0xff, 0xfe];
        let mut utf16be = vec![0xfe, 0xff];
        for unit in json.encode_utf16() {
            utf16le.extend_from_slice(&unit.to_le_bytes());
            utf16be.extend_from_slice(&unit.to_be_bytes());
        }
        for (encoding, raw) in [("utf8", utf8), ("utf16le", utf16le), ("utf16be", utf16be)] {
            let parsed = parse_credentials(ProviderKind::Claude, &raw);
            assert_eq!(
                parsed.map(|(token, _)| token.to_string()).as_deref(),
                Some("fixture-bom-token"),
                "{encoding}"
            );
        }
    }

    /// 로그인 정보만 있는 기본 계정도 조회할 수 있다. HTTP 대신 로컬 fixture를 읽어
    /// 실제 OS curl 실행·stdin 설정 전달·응답 파싱을 네트워크 없이 검증한다.
    #[test]
    fn host_account_usage_uses_system_curl_without_statusline_cache() {
        let root = std::env::temp_dir().join(format!("host-usage-curl-{}", std::process::id()));
        let dir = root.join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"host-fixture-only"}}"#,
        )
        .unwrap();
        let response = root.join("usage response.json");
        std::fs::write(
            &response,
            br#"{"five_hour":{"utilization":42},"seven_day":{"utilization":8}}"#,
        )
        .unwrap();
        let path = response.to_str().unwrap().replace('\\', "/");
        let encoded: String = path
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"/:-._~".contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        let url = format!("file://{}{encoded}", if cfg!(windows) { "/" } else { "" });
        let (token, account) = credentials(ProviderKind::Claude, &dir, true, &root, None).unwrap();
        assert_eq!(&*token, "host-fixture-only");
        let (usage, meters) = query(ProviderKind::Claude, &token, account.as_deref(), |config| {
            let config =
                Zeroizing::new(config.replace("https://api.anthropic.com/api/oauth/usage", &url));
            pipe_command(
                system_curl(),
                &[
                    "-q",
                    "--config",
                    "-",
                    "--silent",
                    "--fail",
                    "--max-time",
                    "2",
                ],
                config.as_bytes(),
            )
        })
        .unwrap();
        assert_eq!(usage.five_hour.unwrap().percent, 42);
        assert_eq!(usage.seven_day.unwrap().percent, 8);
        assert!(!meters.meters.is_empty());
        assert!(!dir.join("statusline_cache.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn credential_file_fallback_reads_only_the_selected_profile() {
        let root = std::env::temp_dir().join(format!("owe719-credentials-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(".credentials.json");
        let raw = br#"{"claudeAiOauth":{"accessToken":"fixture-only"}}"#;
        std::fs::write(&path, raw).unwrap();
        let (token, _) = credentials(ProviderKind::Claude, &root, false, &root, None).unwrap();
        assert_eq!(&*token, "fixture-only");
        assert_eq!(std::fs::read(&path).unwrap(), raw);
        assert!(credentials(ProviderKind::Codex, &root, false, &root, None).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provider_credentials_and_headers_stay_scoped_and_read_only() {
        let (token, account) = parse_credentials(
            ProviderKind::Codex,
            br#"{"tokens":{"access_token":"test-only","account_id":"acct-1"}}"#,
        )
        .unwrap();
        assert_eq!(&*token, "test-only");
        assert_eq!(account.as_deref(), Some("acct-1"));
        assert!(
            parse_credentials(
                ProviderKind::Claude,
                br#"{"tokens":{"access_token":"wrong"}}"#
            )
            .is_none()
        );
        let config = request_config(ProviderKind::Codex, &token, account.as_deref()).unwrap();
        assert!(config.contains("request = \"GET\""));
        assert!(config.contains("ChatGPT-Account-Id: acct-1"));
        // 소진 설정도 같은 자격증명 검증·계정 스코프를 쓴다.
        let consume = consume_config(
            ProviderKind::Codex,
            &token,
            account.as_deref(),
            "55cbe0c1-71ab-4c52-b855-70746ff52",
        )
        .unwrap();
        assert!(consume.contains("rate-limit-reset-credits/consume"));
        assert!(consume.contains("request = \"POST\""));
        assert!(consume.contains("ChatGPT-Account-Id: acct-1"));
        assert!(consume.contains("\"redeem_request_id\": \"55cbe0c1-71ab-4c52-b855-70746ff52\""));
        assert!(consume_config(ProviderKind::Claude, &token, None, "k").is_none());
        assert!(consume_config(ProviderKind::Codex, &token, None, "bad id!").is_none());
        for invalid in ["token\r\ninjected", "token\"", "token\\"] {
            assert!(consume_config(ProviderKind::Codex, invalid, None, "k").is_none());
        }
        for invalid in ["token\r\ninjected", "token\"", "token\\"] {
            assert!(request_config(ProviderKind::Claude, invalid, None).is_none());
        }
        assert_eq!(
            claude_service(None).as_deref(),
            Some("Claude Code-credentials")
        );
        assert_ne!(
            claude_service(Some(Path::new("/tmp/a"))),
            claude_service(None)
        );
    }

    #[test]
    fn consume_response_maps_backend_codes_and_rejects_unknown() {
        use ResetConsumeOutcome::*;
        for (code, want) in [
            ("reset", Reset),
            ("nothing_to_reset", NothingToReset),
            ("no_credit", NoCredit),
            ("already_redeemed", AlreadyRedeemed),
        ] {
            let text = format!(r#"{{"code": "{code}", "windows_reset": 2}}"#);
            assert_eq!(parse_consume_response(&text), Some((want, 2)));
        }
        assert!(parse_consume_response(r#"{"code": "reset"}"#).is_some());
        assert_eq!(
            parse_consume_response(r#"{"code": "reset"}"#),
            Some((Reset, 0))
        );
        for bad in [
            r#"{"code": "redeemed"}"#,
            "{}",
            "not json",
            r#"{"code": 7}"#,
        ] {
            assert!(parse_consume_response(bad).is_none(), "{bad}");
        }
        assert!(
            consume_config(
                ProviderKind::Codex,
                "t",
                Some("a-1"),
                "550e8400-e29b-41d4-a716-446655440000"
            )
            .is_some()
        );
        assert!(consume_config(ProviderKind::Codex, "t", None, "").is_none());
        assert!(consume_config(ProviderKind::Claude, "t", None, "abc").is_none());
    }
    #[test]
    fn antigravity_native_metadata_is_validated_without_a_model_turn() {
        let raw = br#"{"auth_method":"oauth","token":{"access_token":"fixture-only"}}"#;
        assert!(parse_antigravity_auth(raw).unwrap().logged_in);
        assert!(
            !parse_antigravity_auth(br#"{"auth_method":"oauth","token":{}}"#)
                .unwrap()
                .logged_in
        );
        assert!(parse_antigravity_auth(b"invalid").is_none());
        assert!(parse_antigravity_auth(b"go-keyring-encoded:7").is_none());
        let wrapped =
            b"go-keyring-base64:eyJhdXRoX21ldGhvZCI6Im9hdXRoIiwidG9rZW4iOnsiYWNjZXNzX3Rva2VuIjoiZml4dHVyZS1vbmx5In19\n";
        assert!(parse_antigravity_auth(wrapped).unwrap().logged_in);
        let utf16_claude: Vec<u8> = br#"{"claudeAiOauth":{"accessToken":"utf16-token"}}"#
            .iter()
            .flat_map(|&b| [b, 0])
            .collect();
        assert_eq!(
            parse_credentials(ProviderKind::Claude, &utf16_claude)
                .unwrap()
                .0
                .as_str(),
            "utf16-token"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            claude_service(Some(Path::new("/tmp/é"))),
            claude_service(Some(Path::new("/tmp/e\u{301}")))
        );
    }

    #[test]
    fn injected_http_reads_quota_and_maps_failures_to_unknown() {
        let (usage, meters) = query(ProviderKind::Claude, "fixture-only", None, |config| {
            assert!(config.contains("request = \"GET\""));
            assert!(config.contains("anthropic-beta: oauth-2025-04-20"));
            Some(Zeroizing::new(
                br#"{"five_hour":{"utilization":42},"seven_day":{"utilization":8}}"#.to_vec(),
            ))
        })
        .unwrap();
        assert_eq!(meters.meters.len(), 2);
        assert_eq!(usage.five_hour.unwrap().percent, 42);
        assert_eq!(usage.seven_day.unwrap().percent, 8);
        assert!(query(ProviderKind::Claude, "fixture-only", None, |_| None).is_none());
        assert!(
            query(ProviderKind::Codex, "fixture-only", None, |_| Some(
                Zeroizing::new(b"invalid response".to_vec())
            ))
            .is_none()
        );
    }

    #[test]
    fn consume_response_codes_map_to_outcomes_without_network() {
        let ok = |code: &str, windows: u64| {
            consume_request(
                ProviderKind::Codex,
                "fixture-only",
                Some("acct-1"),
                "k",
                |_| {
                    Some(Zeroizing::new(
                        format!("{{\"code\":\"{code}\",\"windows_reset\":{windows}}}").into_bytes(),
                    ))
                },
            )
        };
        assert_eq!(ok("reset", 2), Some((ResetConsumeOutcome::Reset, 2)));
        assert_eq!(
            ok("nothing_to_reset", 0),
            Some((ResetConsumeOutcome::NothingToReset, 0))
        );
        assert_eq!(ok("no_credit", 0), Some((ResetConsumeOutcome::NoCredit, 0)));
        assert_eq!(
            ok("already_redeemed", 1),
            Some((ResetConsumeOutcome::AlreadyRedeemed, 1))
        );
        assert!(ok("bogus", 0).is_none());
        assert!(
            consume_request(ProviderKind::Codex, "fixture-only", None, "k", |_| None).is_none()
        );
        // 멱등 키는 UUID v4 형태다(서버는 불투명 키로 취급한다).
        let id = redeem_request_id().unwrap();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(
            parts
                .iter()
                .all(|p| p.bytes().all(|b| b.is_ascii_hexdigit()))
        );
    }

    #[test]
    fn claude_reset_probes_detect_offer_and_count_grants() {
        // 주간 세션 리셋 제공 조건: eligible + arm=reset + available.
        assert_eq!(
            parse_juniper_status(
                r#"{"juniper_tide":{"eligible":true,"arm":"reset","available":true}}"#
            ),
            Some(true)
        );
        for body in [
            r#"{"juniper_tide":{"eligible":true,"arm":"control","available":true}}"#,
            r#"{"juniper_tide":{"eligible":false,"arm":"reset","available":true}}"#,
            r#"{"juniper_tide":{"eligible":true,"arm":"reset","available":false}}"#,
            r#"{"juniper_tide":null}"#,
        ] {
            assert_eq!(parse_juniper_status(body), Some(false), "{body}");
        }
        assert!(parse_juniper_status(r#"{"juniper_tide":{}}"#).is_some());
        assert!(parse_juniper_status("{}").is_none());
        assert!(parse_juniper_status("not json").is_none());

        // grant 합산: 사용 가능·기간 내·일시정지 아님만 센다.
        let now = 1_780_000_000;
        let grants = |inner: &str| {
            parse_cedar_status(
                &format!("{{\"cedar_ember\":{{\"eligible\":true,\"grants\":[{inner}]}}}}"),
                now,
            )
        };
        let grant = |id: &str, left: u64, extra: &str| {
            format!("{{\"id\":\"{id}\",\"resets_left\":{left},\"usable_now\":true{extra}}}")
        };
        assert_eq!(
            grants(&format!(
                "{}, {}",
                grant("a", 2, ""),
                grant("b", 1, ",\"paused\":true")
            )),
            Some(2)
        );
        // 만료·미래·사용 불가·0회는 제외한다.
        assert_eq!(
            grants(&format!(
                "{}, {}, {}, {}",
                grant("a", 1, ",\"ends_at\":\"2020-01-01T00:00:00Z\""),
                grant("b", 1, ",\"starts_at\":\"2030-01-01T00:00:00Z\""),
                grant("c", 1, ""),
                r#"{"id":"d","resets_left":3,"usable_now":false}"#,
            )),
            Some(1)
        );
        assert_eq!(
            parse_cedar_status(
                r#"{"cedar_ember":{"eligible":false,"grants":[{"id":"a","resets_left":2,"usable_now":true}]}}"#,
                now
            ),
            Some(0)
        );
        assert_eq!(parse_cedar_status(r#"{"cedar_ember":null}"#, now), Some(0));
        assert!(parse_cedar_status("{}", now).is_none());
        assert!(parse_cedar_status(r#"{"cedar_ember":{}}"#, now).is_none());
        assert!(parse_cedar_status("not json", now).is_none());

        // 프로브 설정은 고정 쿼리·검증된 토큰만 쓴다.
        let juniper = claude_probe_config("fixture-only", ClaudeResetProbe::Juniper).unwrap();
        assert!(juniper.contains("?at_wall=1&skip_spend=1"));
        let cedar = claude_probe_config("fixture-only", ClaudeResetProbe::Cedar).unwrap();
        assert!(cedar.contains("?cedar_ember=1&skip_spend=1"));
        assert!(claude_probe_config("bad\ntoken", ClaudeResetProbe::Juniper).is_none());

        // 제공 프로그램 판정: juniper 우선, 다음 cedar, 없으면 제공 없음.
        let offered = |juniper_body: &str, cedar_body: &str| {
            claude_offered_program("fixture-only", |config| {
                let body = if config.contains("at_wall") {
                    juniper_body
                } else {
                    cedar_body
                };
                Some(Zeroizing::new(body.as_bytes().to_vec()))
            })
        };
        assert_eq!(
            offered(
                r#"{"juniper_tide":{"eligible":true,"arm":"reset","available":true}}"#,
                r#"{"cedar_ember":null}"#
            ),
            Some(Some(ClaudeResetProgram::Juniper))
        );
        assert_eq!(
            offered(
                r#"{"juniper_tide":{"eligible":false}}"#,
                r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"g","resets_left":1,"usable_now":true}]}}"#
            ),
            Some(Some(ClaudeResetProgram::Cedar))
        );
        assert_eq!(
            offered(r#"{"juniper_tide":null}"#, r#"{"cedar_ember":null}"#),
            Some(None)
        );
        assert_eq!(
            offered(
                "{}",
                r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"g","resets_left":1,"usable_now":true}]}}"#
            ),
            Some(Some(ClaudeResetProgram::Cedar))
        );
        assert_eq!(
            claude_reset_count("fixture-only", |config| {
                let body = if config.contains("at_wall") {
                    "{}"
                } else {
                    r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"g","resets_left":2,"usable_now":true}]}}"#
                };
                Some(Zeroizing::new(body.as_bytes().to_vec()))
            }),
            Some(2)
        );
        assert!(
            claude_offered_program("fixture-only", |_| None).is_none(),
            "probe failure must not look like no offer"
        );
    }

    #[test]
    fn claude_reset_claim_maps_business_results() {
        let claim = |status: u16, body: &str| {
            claude_claim_reset(
                "fixture-only",
                "12345678-1234-1234-1234-1234567890ab",
                ClaudeResetProgram::Juniper,
                |_| Some((status, body.as_bytes().to_vec().into())),
            )
        };
        assert_eq!(
            claim(200, r#"{"result":"reset"}"#),
            Some((ResetConsumeOutcome::Reset, 1))
        );
        assert_eq!(
            claim(200, r#"{"result":"already_used"}"#),
            Some((ResetConsumeOutcome::AlreadyRedeemed, 0))
        );
        assert_eq!(
            claim(200, r#"{"result":"not_limited"}"#),
            Some((ResetConsumeOutcome::NothingToReset, 0))
        );
        assert_eq!(
            claim(200, r#"{"result":"ineligible"}"#),
            Some((ResetConsumeOutcome::NoCredit, 0))
        );
        assert_eq!(
            claim(200, r#"{"result":"unavailable"}"#),
            Some((ResetConsumeOutcome::NoCredit, 0))
        );
        assert!(claim(200, r#"{"result":"bogus"}"#).is_none());
        assert!(claim(200, "{}").is_none());
        assert!(claim(500, r#"{"result":"reset"}"#).is_none());
        assert!(claim(429, "{}").is_none());
        assert!(claim(401, "{}").is_none());
        assert!(
            claude_claim_reset("bad\ntoken", "org", ClaudeResetProgram::Cedar, |_| None,).is_none()
        );
        assert!(
            claude_claim_reset(
                "fixture-only",
                "bad uuid!",
                ClaudeResetProgram::Cedar,
                |_| None
            )
            .is_none()
        );
        // 조직 UUID 조회: 경로·형식 검증.
        let org = |body: &str| {
            claude_org_uuid("fixture-only", |_| {
                Some(Zeroizing::new(body.as_bytes().to_vec()))
            })
        };
        assert_eq!(
            org(r#"{"organization":{"uuid":"12345678-1234-1234-1234-1234567890ab"}}"#),
            Some("12345678-1234-1234-1234-1234567890ab".to_owned())
        );
        assert!(org(r#"{"organization":{}}"#).is_none());
        assert!(org("{}").is_none());
        assert!(claude_org_uuid("bad\ntoken", |_| None).is_none());
    }

    #[test]
    fn backend_windows_use_duration_not_primary_order() {
        let usage = parse_codex_backend(
            r#"{"rate_limit":{"primary_window":{"limit_window_seconds":604800,"used_percent":12.4,"reset_at":1900000000},"secondary_window":{"limit_window_seconds":18000,"used_percent":76}}}"#,
        );
        assert_eq!(usage.five_hour.unwrap().percent, 76);
        assert_eq!(usage.seven_day.unwrap().percent, 12);
        assert!(parse_codex_backend("bad").is_empty());
        assert!(parse_codex_backend(r#"{"rate_limit":{"primary_window":{"limit_window_seconds":60,"used_percent":100}}}"#).is_empty());
    }
    #[test]
    fn disabled_setting_drops_cached_usage_without_reading_credentials() {
        let root = std::env::temp_dir().join(format!("owe719-direct-{}", std::process::id()));
        let home = root.join("home");
        let state = root.join("state");
        let key = (home.clone(), ProviderKind::Claude, home.join(".claude"));
        lock(&CACHE).insert(key.clone(), (Instant::now(), ProfileUsage::default()));
        let file = accounts::AccountsFile {
            direct_usage: false,
            ..accounts::AccountsFile::default()
        };
        accounts::save_file(&home, Some(&state), &file).unwrap();
        refresh(&home, Some(&state));
        assert!(!lock(&CACHE).contains_key(&key));
        std::fs::remove_dir_all(root).ok();
    }

    /// 429는 마지막 정상 값은 오래됨으로 남기고, 정상 값이 없을 때는 제한 이유를 보존한다.
    #[test]
    fn rate_limit_keeps_last_good_usage_as_stale() {
        let key = (
            PathBuf::from("/h"),
            ProviderKind::Claude,
            PathBuf::from("/h/.claude"),
        );
        let good = ProfileUsage {
            usage: Usage {
                five_hour: Some(Window {
                    percent: 2,
                    resets_at: None,
                }),
                seven_day: None,
            },
            updated_at: Some(100),
            ..ProfileUsage::default()
        };
        let at = Instant::now();
        let mut cache = BTreeMap::from([(key.clone(), (at, good.clone()))]);
        mark_rate_limited(&mut cache, &key, Some(200));
        let (kept_at, kept) = &cache[&key];
        assert_eq!(*kept_at, at);
        assert!(kept.stale && kept.updated_at == Some(100) && kept.usage == good.usage);
        cache.insert(
            key.clone(),
            (
                at,
                ProfileUsage {
                    error: Some("갱신 실패".into()),
                    ..good
                },
            ),
        );
        mark_rate_limited(&mut cache, &key, Some(200));
        // 정상 값이 없으면 지어낸 값 없이 요청 제한 오류(조회 오류 표시)로 남는다.
        let (_, limited) = &cache[&key];
        assert!(limited.usage.is_empty() && limited.updated_at == Some(200));
        assert_eq!(limited.error.as_deref(), Some("요청 제한"));
        cache.clear();
        mark_rate_limited(&mut cache, &key, None);
        assert_eq!(cache[&key].1.error.as_deref(), Some("요청 제한"));
    }

    /// Orca 패리티: 방금 쓴 statusline 값이 있으면 자동 조회를 건너뛰고, 직접 조회 값보다 새롭거나
    /// 조회가 실패했으면 statusline 값을 보여 준다.
    #[test]
    fn live_statusline_beats_older_or_failed_direct_usage() {
        let root = std::env::temp_dir().join(format!("krill-live-usage-{}", std::process::id()));
        let home = root.join("home");
        let dir = home.join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("statusline_cache.json"),
            r#"{"five_hour":{"utilization":44,"resets_at":"2026-10-23T10:30:00.000Z"}}"#,
        )
        .unwrap();
        assert!(live_fresh(ProviderKind::Claude, &dir));
        assert!(!live_fresh(ProviderKind::Codex, &dir));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let key = (home.clone(), ProviderKind::Claude, dir.clone());
        let direct = |percent, updated_at| ProfileUsage {
            usage: Usage {
                five_hour: Some(Window {
                    percent,
                    resets_at: None,
                }),
                seven_day: None,
            },
            updated_at: Some(updated_at),
            ..ProfileUsage::default()
        };
        let five_hour = || {
            accounts::read_profile_usage(&home, ProviderKind::Claude, &dir, "Host", None, true)
                .usage
                .five_hour
                .map(|w| w.percent)
        };
        lock(&CACHE).insert(key.clone(), (Instant::now(), direct(2, now - 600)));
        assert_eq!(five_hour(), Some(44));
        lock(&CACHE).insert(key.clone(), (Instant::now(), direct(2, now + 600)));
        assert_eq!(five_hour(), Some(2));
        let failed = ProfileUsage {
            error: Some("갱신 실패".into()),
            updated_at: Some(now + 600),
            ..ProfileUsage::default()
        };
        lock(&CACHE).insert(key.clone(), (Instant::now(), failed));
        assert_eq!(five_hour(), Some(44));
        lock(&CACHE).remove(&key);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn retry_waits_follow_orca_policy() {
        assert_eq!(failure_wait(1), Duration::from_secs(30));
        assert_eq!(failure_wait(2), Duration::from_secs(60));
        assert_eq!(failure_wait(6), MAX_FAILURE_RETRY);
        assert_eq!(failure_wait(u32::MAX), MAX_FAILURE_RETRY);
        assert_eq!(
            parse_status_tail("\n429 191"),
            Some((429, Some(Duration::from_secs(191))))
        );
        assert_eq!(parse_status_tail("\n200 "), Some((200, None)));
        assert_eq!(
            parse_status_tail("\n429 Wed, 21 Oct 2026 07:28:00 GMT"),
            Some((429, None))
        );
        assert_eq!(parse_status_tail("\n"), None);
    }
}
