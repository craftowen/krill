//! 디스크 대기열과 전송 상태기계. 네트워크 코드는 없다 — 전송은 호출자가 넘기는 함수다.
//!
//! - 항목: `outbox/<event_id>.json`(0600, 이벤트 4KiB + 봉투, 임시 파일 + 이름 바꾸기). 기록 당시 동의 세대·시각·
//!   시도 횟수를 함께 둔다. 시도 횟수는 보내기 전에 올려 둬 전송 중 종료돼도 상한이 지켜진다.
//! - 잠금: 큐 쓰기는 `queue.lock`, 업로드 single-flight는 `upload.lock`(따로 쥔다). 전송 중에는 큐 잠금을
//!   쥐지 않고, 응답을 반영하기 전에 잠금 안에서 동의 세대를 다시 확인한다. 철회 뒤에는 어떤 항목도 다시 쓰지 않는다.
//! - 상한: 64건, 7일, 이벤트별 10회, flush당 HTTP 4회, 큐 전체 재시도 60초~24시간(+지터).

use std::fs::{self, DirBuilder, File, TryLockError};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use ring::rand::SecureRandom as _;
use serde::{Deserialize, Serialize};

use super::consent::{self, Consent, write_atomic};
use super::file::{self, read_bounded, real_dir};
use super::types::{
    Ack, Event, MAX_AGE_MS, MAX_ATTEMPTS, MAX_BATCH, MAX_EVENT_BYTES, MAX_FLUSH_REQUESTS,
    MAX_PENDING, MAX_REQUEST_BYTES, RETRY_MAX_MS, RETRY_MIN_MS, Request, SCHEMA_VERSION, is_hex32,
};
use super::{dir, now_ms};
use crate::os::fs::DirBuilderExt as _;

pub(crate) const QUEUE_LOCK: &str = "queue.lock";
const UPLOAD_LOCK: &str = "upload.lock";
const OUTBOX: &str = "outbox";
const RETRY_FILE: &str = "retry.json";
const ENTRY_VERSION: u32 = 1;
/// 대기열 파일 하나의 상한: 이벤트 상한([`MAX_EVENT_BYTES`]) + 봉투(세대·시각·시도 횟수, 실제 약 120바이트).
pub(crate) const MAX_ENTRY_BYTES: usize = MAX_EVENT_BYTES + 512;
/// 디렉터리 한 번 훑을 때 보는 항목 상한(대기열 상한의 4배).
const MAX_SCAN: usize = 256;
/// 재시도 상태 파일 상한(바이트).
const MAX_RETRY_BYTES: usize = 4096;
/// 기록 시각이 이만큼 미래면 시계가 어긋난 항목으로 보고 버린다.
const MAX_FUTURE_MS: u64 = 86_400_000;
/// flush가 큐 잠금을 기다리는 상한. 기록 쪽은 파일 몇 개만 다루고 곧 놓는다.
const FLUSH_WAIT: Duration = Duration::from_secs(1);

/// 파일 advisory lock. 값을 놓으면 풀린다.
#[derive(Debug)]
pub(crate) struct Lock {
    _file: File,
}

impl Lock {
    fn open(dir: &Path, name: &str) -> io::Result<File> {
        file::open_lock(&dir.join(name))
    }

    fn try_once(file: File) -> io::Result<Option<Self>> {
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }

    /// 한 번만 시도한다. 다른 쪽이 쥐고 있으면 `None`.
    pub(crate) fn try_acquire(dir: &Path, name: &str) -> io::Result<Option<Self>> {
        Self::try_once(Self::open(dir, name)?)
    }

    /// `wait` 동안 10ms 간격으로 다시 시도한다. 시간이 다 되면 `WouldBlock` 오류.
    pub(crate) fn wait(dir: &Path, name: &str, wait: Duration) -> io::Result<Self> {
        let file = Self::open(dir, name)?;
        let deadline = Instant::now().checked_add(wait);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => return Err(e),
            }
            if deadline.is_none_or(|d| Instant::now() >= d) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "error reporting queue is busy",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// 업로드 single-flight 잠금. 쥔 동안 다른 업로더는 [`UploadLock::try_acquire`]에서 `None`을 받는다.
/// 큐 잠금과 별개라 쥔 채로 네트워크를 기다려도 기록은 막히지 않는다.
#[derive(Debug)]
pub struct UploadLock {
    _lock: Lock,
}

impl UploadLock {
    /// 한 번만 시도한다(기다리지 않는다).
    ///
    /// # Errors
    /// 동의 디렉터리가 없거나 잠금 파일을 열지 못하면 OS 오류.
    pub fn try_acquire(state: &Path) -> io::Result<Option<Self>> {
        Ok(Lock::try_acquire(&dir(state), UPLOAD_LOCK)?.map(|lock| Self { _lock: lock }))
    }
}

/// 대기열 항목(파일 하나).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    v: u32,
    generation: String,
    created_at_ms: u64,
    attempts: u32,
    event: Event,
}

/// 큐 전체 재시도 상태. `isolate`는 400/413/415를 받은 묶음의 ID로, 비워질 때까지 하나씩 보낸다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retry {
    generation: String,
    not_before_ms: u64,
    failures: u32,
    isolate: Vec<String>,
}

/// 전송 결과. 전송 계층(`krill errors flush`의 curl)이 채운다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// 응답을 받지 못했다(연결 실패·시간 초과·전송 도구 오류·응답 상한 초과).
    Transport,
    /// HTTP 응답. 본문은 [`MAX_REQUEST_BYTES`] 안에서만 읽는다.
    Http {
        status: u16,
        retry_after_secs: Option<u64>,
        body: Vec<u8>,
    },
}

/// flush 결과 요약.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlushOutcome {
    /// 보낼 것이 없었다.
    #[default]
    Empty,
    /// 보냈고 마지막 요청이 성공했다(요청 상한으로 남은 항목이 있을 수 있다).
    Sent,
    /// 요청이 실패하거나 일부만 ACK돼 큐 전체 재시도 시각을 정했다.
    Deferred,
    /// 재시도 시각 전이라 보내지 않았다.
    Backoff,
    /// 다른 업로드·기록이 잠금을 쥐고 있었다.
    Busy,
    /// 동의가 없다.
    NotConsented,
    /// 진행 중 동의가 철회·교체돼 대기열을 더 건드리지 않았다.
    Withdrawn,
}

/// flush 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlushReport {
    pub outcome: FlushOutcome,
    /// 보낸 HTTP 요청 수(최대 [`MAX_FLUSH_REQUESTS`]).
    pub requests: usize,
    /// 서버가 새로 받은 이벤트 수.
    pub accepted: usize,
    /// 서버가 이미 가진 이벤트 수(ACK로 지웠다).
    pub duplicates: usize,
    /// 단건 요청이 400/413/415로 거부돼 버린 이벤트 수.
    pub rejected: usize,
    /// 손상·만료·시도 초과·다른 동의 세대라 로컬에서 버린 항목 수.
    pub dropped: usize,
    /// 남은 대기 항목 수.
    pub pending: usize,
    /// 큐 전체 다음 시도 시각(UNIX 밀리초).
    pub next_attempt_at_ms: Option<u64>,
}

/// 현재 동의로 기록한다. 동의 없음·거부·잠금 경합·중복·대기열 가득이면 `false`(기다리지 않는다).
///
/// # Errors
/// 이벤트 계약 위반(`InvalidData`)이나 파일 쓰기 실패. 호출자(panic·UI 보조 함수)는 삼킨다.
pub fn record_at(state: &Path, event: Event) -> io::Result<bool> {
    let Some(consent) = consent::consent_at(state)? else {
        return Ok(false);
    };
    record_with_consent(state, event, &consent)
}

/// 큐 잠금 안에서 현재 동의가 `expected`와 같을 때만 기록한다. 수집 전에 받은 동의를 넘기면 그 사이의
/// 철회·재동의를 넘지 않는다. 반환값은 [`record_at`]과 같다.
///
/// # Errors
/// 이벤트 계약 위반(`InvalidData`)이나 파일 쓰기 실패.
pub fn record_with_consent(state: &Path, event: Event, expected: &Consent) -> io::Result<bool> {
    event.validate()?;
    // 잠금 파일을 열기 전에 먼저 본다: 철회 뒤에는 잠금 파일조차 새로 만들지 않는다.
    if consent::read(state)?.as_ref() != Some(expected) {
        return Ok(false);
    }
    let dir = dir(state);
    let lock = match Lock::try_acquire(&dir, QUEUE_LOCK) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if consent::read(state)?.as_ref() != Some(expected) {
        return Ok(false);
    }
    let now = now_ms();
    let outbox = dir.join(OUTBOX);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&outbox)?;
    if !real_dir(&outbox) {
        return Ok(false);
    }
    let name = entry_name(&event.event_id);
    if fs::symlink_metadata(outbox.join(&name)).is_ok() {
        return Ok(false);
    }
    if count(&outbox) >= MAX_PENDING
        && sweep(&outbox, &expected.generation, now)?.entries.len() >= MAX_PENDING
    {
        return Ok(false);
    }
    let entry = Entry {
        v: ENTRY_VERSION,
        generation: expected.generation.clone(),
        created_at_ms: now,
        attempts: 0,
        event,
    };
    let bytes = serde_json::to_vec(&entry).map_err(io::Error::other)?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Ok(false);
    }
    write_atomic(&outbox, &name, &bytes)?;
    drop(lock);
    Ok(true)
}

/// 대기 항목 수(이름만 센다, 상한 [`MAX_SCAN`]).
pub(crate) fn pending(dir: &Path) -> usize {
    count(&dir.join(OUTBOX))
}

/// 큐 전체 다음 시도 시각. 재시도 대기 중이 아니면 `None`.
pub(crate) fn next_attempt(dir: &Path, generation: &str, now: u64) -> Option<u64> {
    let retry = read_retry(dir, generation, now);
    (retry.not_before_ms > now).then_some(retry.not_before_ms)
}

/// 대기열·재시도 상태를 지운다(동의·철회가 잠금 안에서 부른다).
pub(crate) fn wipe(dir: &Path) {
    fs::remove_dir_all(dir.join(OUTBOX)).ok();
    fs::remove_file(dir.join(RETRY_FILE)).ok();
}

/// 대기열을 비운다: 큐 잠금 안에서 정리·선택하고, 잠금을 놓은 채 `transport`로 보내고, 다시 잠금 안에서
/// 동의를 확인한 뒤 응답을 반영한다. HTTP는 최대 [`MAX_FLUSH_REQUESTS`]번.
///
/// - 200 ACK: 요청한 ID만 겹치지 않게 담겨야 유효하다. 유효하면 ACK된 항목만 지우고, 일부만 ACK되면
///   나머지에 큐 전체 재시도를 건다. 파싱 실패·모르는 ID·겹친 ID는 묶음 전체를 남긴다.
/// - 400/413/415: 여러 건 묶음이면 아무것도 지우지 않고 단건 전송으로 바꾼다. 단건이 거부될 때만 버린다.
/// - 429·5xx 등 그 밖의 실패: `Retry-After`가 있으면 그 값, 없으면 지수 증가(60초~24시간 + 지터)로 큐 전체
///   재시도를 건다. 응답이 없으면(연결 실패 등) 지수 증가.
///
/// # Errors
/// 대기열 파일 읽기·쓰기 실패.
pub fn flush(
    state: &Path,
    expected: &Consent,
    flight: &UploadLock,
    mut transport: impl FnMut(&[u8]) -> Reply,
) -> io::Result<FlushReport> {
    let mut clock = now_ms;
    let mut jitter = random_jitter;
    flush_with(
        state,
        expected,
        flight,
        &mut transport,
        &mut clock,
        &mut jitter,
    )
}

/// [`flush`]에 시계·지터를 주입한다(테스트).
pub(crate) fn flush_with(
    state: &Path,
    expected: &Consent,
    _flight: &UploadLock,
    transport: &mut impl FnMut(&[u8]) -> Reply,
    clock: &mut impl FnMut() -> u64,
    jitter: &mut impl FnMut(u64) -> u64,
) -> io::Result<FlushReport> {
    let dir = dir(state);
    let mut report = FlushReport::default();
    while report.requests < MAX_FLUSH_REQUESTS {
        let batch = match prepare(state, expected, clock(), &mut report.dropped)? {
            Prepared::Send(batch) => batch,
            Prepared::Empty => break,
            Prepared::Backoff(until) => {
                report.next_attempt_at_ms = Some(until);
                if report.requests == 0 {
                    report.outcome = FlushOutcome::Backoff;
                }
                break;
            }
            Prepared::Busy => {
                report.outcome = FlushOutcome::Busy;
                break;
            }
            Prepared::Withdrawn => {
                report.outcome = FlushOutcome::Withdrawn;
                break;
            }
        };
        // 업로드 직전 재확인: 잠금 밖 대기 중에 철회·재동의됐으면 보내지 않는다.
        if consent::read(state)?.as_ref() != Some(expected) {
            report.outcome = FlushOutcome::Withdrawn;
            break;
        }
        report.requests = report.requests.saturating_add(1);
        let reply = transport(&batch.body);
        let settled = settle(state, expected, &batch, &reply, clock(), jitter)?;
        report.accepted = report.accepted.saturating_add(settled.accepted);
        report.duplicates = report.duplicates.saturating_add(settled.duplicates);
        report.rejected = report.rejected.saturating_add(settled.rejected);
        match settled.next {
            Next::Continue => report.outcome = FlushOutcome::Sent,
            Next::Retry(until) => {
                report.outcome = FlushOutcome::Deferred;
                report.next_attempt_at_ms = Some(until);
                break;
            }
            Next::Busy => {
                report.outcome = FlushOutcome::Busy;
                break;
            }
            Next::Withdrawn => {
                report.outcome = FlushOutcome::Withdrawn;
                break;
            }
        }
    }
    report.pending = pending(&dir);
    Ok(report)
}

/// 한 번 보낼 묶음. 잠금 밖(네트워크 중)에 들고 다닌다.
#[derive(Debug)]
struct Batch {
    body: Vec<u8>,
    ids: Vec<String>,
}

#[derive(Debug)]
enum Prepared {
    Send(Batch),
    Empty,
    Backoff(u64),
    Busy,
    Withdrawn,
}

/// 큐 잠금 안에서 정리하고 다음 묶음을 고른다. 고른 항목의 시도 횟수를 먼저 올려 쓴다.
fn prepare(
    state: &Path,
    expected: &Consent,
    now: u64,
    dropped: &mut usize,
) -> io::Result<Prepared> {
    let dir = dir(state);
    let lock = match Lock::wait(&dir, QUEUE_LOCK, FLUSH_WAIT) {
        Ok(lock) => lock,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Prepared::Busy),
        Err(e) => return Err(e),
    };
    if consent::read(state)?.as_ref() != Some(expected) {
        return Ok(Prepared::Withdrawn);
    }
    let mut retry = read_retry(&dir, &expected.generation, now);
    if retry.not_before_ms > now {
        return Ok(Prepared::Backoff(retry.not_before_ms));
    }
    let outbox = dir.join(OUTBOX);
    let scan = sweep(&outbox, &expected.generation, now)?;
    *dropped = dropped.saturating_add(scan.dropped);
    let before = retry.isolate.len();
    retry
        .isolate
        .retain(|id| scan.entries.iter().any(|e| &e.event.event_id == id));
    if retry.isolate.len() != before {
        write_retry(&dir, &retry)?;
    }
    let mut chosen: Vec<Entry> = match retry.isolate.first() {
        Some(id) => scan
            .entries
            .into_iter()
            .filter(|e| &e.event.event_id == id)
            .collect(),
        None => scan.entries.into_iter().take(MAX_BATCH).collect(),
    };
    let mut body = request_body(&chosen)?;
    while body.len() > MAX_REQUEST_BYTES && chosen.len() > 1 {
        chosen.pop();
        body = request_body(&chosen)?;
    }
    if chosen.is_empty() {
        return Ok(Prepared::Empty);
    }
    for entry in &mut chosen {
        entry.attempts = entry.attempts.saturating_add(1);
        let bytes = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        write_atomic(&outbox, &entry_name(&entry.event.event_id), &bytes)?;
    }
    drop(lock);
    Ok(Prepared::Send(Batch {
        body,
        ids: chosen.into_iter().map(|e| e.event.event_id).collect(),
    }))
}

fn request_body(entries: &[Entry]) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&Request {
        schema_version: SCHEMA_VERSION,
        events: entries.iter().map(|e| e.event.clone()).collect(),
    })
    .map_err(io::Error::other)
}

#[derive(Debug, Default)]
struct Settled {
    accepted: usize,
    duplicates: usize,
    rejected: usize,
    next: Next,
}

#[derive(Debug, Default)]
enum Next {
    #[default]
    Continue,
    Retry(u64),
    Busy,
    Withdrawn,
}

/// 응답을 반영한다. 잠금 안에서 동의 세대가 그대로일 때만 지우거나 재시도 상태를 쓴다(항목은 다시 쓰지 않는다).
fn settle(
    state: &Path,
    expected: &Consent,
    batch: &Batch,
    reply: &Reply,
    now: u64,
    jitter: &mut impl FnMut(u64) -> u64,
) -> io::Result<Settled> {
    let dir = dir(state);
    let mut out = Settled::default();
    let lock = match Lock::wait(&dir, QUEUE_LOCK, FLUSH_WAIT) {
        Ok(lock) => lock,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            out.next = Next::Busy;
            return Ok(out);
        }
        Err(e) => return Err(e),
    };
    if consent::read(state)?.as_ref() != Some(expected) {
        out.next = Next::Withdrawn;
        return Ok(out);
    }
    let mut retry = read_retry(&dir, &expected.generation, now);
    let outbox = dir.join(OUTBOX);
    let forget = |retry: &mut Retry, id: &str| {
        fs::remove_file(outbox.join(entry_name(id))).ok();
        retry.isolate.retain(|i| i != id);
    };
    let mut proceed = false;
    match reply {
        Reply::Http {
            status: 200, body, ..
        } => {
            if let Some(ack) = Ack::parse(body, &batch.ids) {
                for id in ack.acked() {
                    forget(&mut retry, id);
                }
                out.accepted = ack.accepted_ids.len();
                out.duplicates = ack.duplicate_ids.len();
                proceed = ack.acked().count() == batch.ids.len();
                if !proceed {
                    backoff(&mut retry, None, now, jitter);
                }
            } else {
                backoff(&mut retry, None, now, jitter);
            }
        }
        Reply::Http {
            status: 400 | 413 | 415,
            ..
        } => {
            if let [id] = batch.ids.as_slice() {
                forget(&mut retry, id);
                out.rejected = 1;
            } else {
                retry.isolate.clone_from(&batch.ids);
            }
            proceed = true;
        }
        Reply::Http {
            retry_after_secs, ..
        } => backoff(&mut retry, *retry_after_secs, now, jitter),
        Reply::Transport => backoff(&mut retry, None, now, jitter),
    }
    if proceed {
        retry.failures = 0;
        retry.not_before_ms = 0;
    } else {
        out.next = Next::Retry(retry.not_before_ms);
    }
    write_retry(&dir, &retry)?;
    drop(lock);
    Ok(out)
}

/// 큐 전체 재시도 시각을 정한다: `Retry-After` 또는 60초×2^(실패-1)을 60초~24시간으로 자르고 최대 20% 지터.
fn backoff(
    retry: &mut Retry,
    retry_after_secs: Option<u64>,
    now: u64,
    jitter: &mut impl FnMut(u64) -> u64,
) {
    retry.failures = retry.failures.saturating_add(1);
    let exponent = retry.failures.saturating_sub(1).min(20);
    let base = retry_after_secs.map_or_else(
        || RETRY_MIN_MS.saturating_mul(2u64.saturating_pow(exponent)),
        |secs| secs.saturating_mul(1000),
    );
    let delay = base.clamp(RETRY_MIN_MS, RETRY_MAX_MS);
    let spread = delay.checked_div(5).unwrap_or(0);
    let delay = delay
        .saturating_add(jitter(spread).min(spread))
        .min(RETRY_MAX_MS);
    retry.not_before_ms = now.saturating_add(delay);
}

/// `0..=max` 균등 난수. 난수를 못 얻으면 0.
fn random_jitter(max: u64) -> u64 {
    let mut bytes = [0u8; 8];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        return 0;
    }
    u64::from_le_bytes(bytes)
        .checked_rem(max.saturating_add(1))
        .unwrap_or(0)
}

fn entry_name(id: &str) -> String {
    format!("{id}.json")
}

fn is_entry_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(is_hex32)
}

fn count(outbox: &Path) -> usize {
    if !real_dir(outbox) {
        return 0;
    }
    fs::read_dir(outbox).map_or(0, |items| {
        items
            .take(MAX_SCAN)
            .filter_map(Result::ok)
            .filter(|item| item.file_name().to_str().is_some_and(is_entry_name))
            .count()
    })
}

/// 잠금 안에서 쓴다. 정상 항목(오래된 순)과 버린 항목 수.
#[derive(Debug, Default)]
struct Scan {
    entries: Vec<Entry>,
    dropped: usize,
}

/// 손상·과대·만료·시도 초과·다른 세대 항목과 남은 임시 파일을 하나씩 지우고, 정상 항목만 오래된 순으로 돌려준다.
/// 하위 디렉터리는 건드리지 않는다. 대기열 자리가 실제 디렉터리가 아니면(링크) 아무것도 지우지 않는다.
/// 일반 파일이 아닌 항목(FIFO·장치·링크)은 열지 않고 이름만 지운다.
fn sweep(outbox: &Path, generation: &str, now: u64) -> io::Result<Scan> {
    if !real_dir(outbox) {
        return Ok(Scan::default());
    }
    let items = match fs::read_dir(outbox) {
        Ok(items) => items,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Scan::default()),
        Err(e) => return Err(e),
    };
    let mut scan = Scan::default();
    for item in items.take(MAX_SCAN) {
        let Ok(item) = item else {
            continue;
        };
        if item.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = item.path();
        let name = item.file_name();
        let name = name.to_str().unwrap_or_default();
        if !is_entry_name(name) {
            // 잠금 안이라 남은 임시 파일은 끝나지 못한 쓰기다.
            fs::remove_file(&path).ok();
            continue;
        }
        let entry = item
            .file_type()
            .is_ok_and(|t| t.is_file())
            .then(|| load(&path, name, generation, now))
            .flatten();
        if let Some(entry) = entry {
            scan.entries.push(entry);
        } else {
            fs::remove_file(&path).ok();
            scan.dropped = scan.dropped.saturating_add(1);
        }
    }
    scan.entries.sort_by(|a, b| {
        (a.created_at_ms, &a.event.event_id).cmp(&(b.created_at_ms, &b.event.event_id))
    });
    Ok(scan)
}

fn load(path: &Path, name: &str, generation: &str, now: u64) -> Option<Entry> {
    let raw = read_bounded(path, MAX_ENTRY_BYTES).ok()??;
    let entry: Entry = serde_json::from_slice(&raw).ok()?;
    let fresh = entry.created_at_ms.saturating_add(MAX_AGE_MS) >= now
        && entry.created_at_ms <= now.saturating_add(MAX_FUTURE_MS);
    (entry.v == ENTRY_VERSION
        && name == entry_name(&entry.event.event_id)
        && entry.generation == generation
        && entry.attempts < MAX_ATTEMPTS
        && fresh
        && entry.event.validate().is_ok())
    .then_some(entry)
}

/// 재시도 상태. 없거나 손상되거나 다른 세대면 빈 상태다. 시계가 뒤로 가도 24시간 넘게 막지 않는다.
fn read_retry(dir: &Path, generation: &str, now: u64) -> Retry {
    let empty = Retry {
        generation: generation.to_owned(),
        ..Retry::default()
    };
    let Some(raw) = read_bounded(&dir.join(RETRY_FILE), MAX_RETRY_BYTES)
        .ok()
        .flatten()
    else {
        return empty;
    };
    match serde_json::from_slice::<Retry>(&raw) {
        Ok(mut retry)
            if retry.generation == generation
                && retry.isolate.len() <= MAX_BATCH
                && retry.isolate.iter().all(|id| is_hex32(id)) =>
        {
            retry.not_before_ms = retry.not_before_ms.min(now.saturating_add(RETRY_MAX_MS));
            retry
        }
        _ => empty,
    }
}

fn write_retry(dir: &Path, retry: &Retry) -> io::Result<()> {
    if retry.failures == 0 && retry.not_before_ms == 0 && retry.isolate.is_empty() {
        return match fs::remove_file(dir.join(RETRY_FILE)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec(retry).map_err(io::Error::other)?;
    write_atomic(dir, RETRY_FILE, &bytes)
}
