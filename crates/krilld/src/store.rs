//! 세그먼트 저장소(ARCHITECTURE §4 디스크, MK-024): 세션마다 append-only zstd 세그먼트, 전역 토큰 버킷, 보존·GC,
//! 콜드 스크롤백 페이지 재생. 포맷은 [`segment`] 모듈 문서.
//!
//! - 위치: `<root>/sessions/<id>.noindex/seg-<start_seq 20자리>.krs`(`.noindex`로 Spotlight 제외). 디렉터리 0700, 파일 0600.
//! - fsync는 하지 않는다. 청크는 `write(2)` 한 번으로 페이지 캐시에 들어가니 krilld가 죽어도 남는다.
//! - 쓰기량은 krilld 전체가 나눠 쓰는 토큰 버킷(지속 8MB/분, 버스트 32MB, 디스크에 쓴 바이트 기준)으로 묶는다.
//!   토큰이 모자라면 원본을 압축하지 않고 버리고 길이만 센다. 토큰이 [`RESUME`]만큼 다시 차면 `Gap` 레코드를 남기고
//!   이어 쓴다. 쓰기 실패(ENOSPC·부분 쓰기)도 그 세그먼트를 온전한 데까지 잘라 닫고 `Gap`으로 이어 간다.
//! - 원본 1MiB를 담으면 봉인한다. 봉인할 때 세션 상한(32MB)을 넘는 오래된 세그먼트를 지운다. 전역 상한(1GB)과
//!   닫힌 세션 보존(7일)은 [`Store::gc`].
//!
//! # 콜드 페이지와 근사 범위
//!
//! 페이지 하나 = 세그먼트 하나 동안 스크롤백으로 밀려난 행. 남은 연속 원본의 시작점부터 워밍업한다.
//! 앞 원본이 GC됐다면 남은 첫 세그먼트의 ANSI 키프레임을 주입하고, 없으면 빈 엔진에서 시작한다.
//! 따라서 단일 페이지의 비용은 앞에 남은 연속 원본 길이에 비례한다(K-18).
// ponytail: ANSI 키프레임은 CLI 캡처 범위(화면·커서·SGR·입력 모드)다. 비활성 화면·저장 커서·스크롤 영역은
// 완전 복원되지 않는다. 미완성 제어열·sync 버퍼가 있는 경계는 키프레임을 생략하고 이전 워밍업을 쓴다.
// MK-006 엔진 전체 상태 내보내기가 들어오면 이 제한을 없앤다.

pub(crate) mod archive;
mod crypto;
mod privacy;
mod recorder;
pub(crate) mod replay_gate;
mod segment;
pub(crate) mod stable;

pub use recorder::Recorder;

use krill_proto::os::fs::DirBuilderExt;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant, SystemTime};

use krill_proto::SessionId;
use krill_proto::holder::WinSize;
use krill_proto::screen::{Cell, Flags};

use crate::appearance::Appearance;
use crate::session::{Gap, lock};
use crate::term::{Engine, Feed};
use segment::{Base, Record, Segment};

/// 드롭 상태에서 다시 쓰기 시작하는 토큰 양. 1바이트씩 찰 때마다 잘게 쓰지 않고 한 번에 이어 쓴다.
const RESUME: u64 = 256 << 10;
/// 원본 n바이트를 쓸 때 압축·청크·헤더·Gap 레코드에 더 드는 최대 바이트(zstd 블록 헤더는 128KiB당 3B).
const OVERHEAD: u64 = 256;
/// zstd 레벨(ARCHITECTURE §3 zstd 하나, T2 D8 zstd-3).
const LEVEL: i32 = 3;
/// zstd 창 1MiB = 봉인 크기. 더 크게 잡아도 한 세그먼트 안에서만 참조하니 이득이 없다.
const WINDOW_LOG: u32 = 20;
/// 재생 때 스크롤백을 거두는 간격. 이 안에서 [`HOT_LINES`](crate::HOT_LINES)행 넘게 밀려나면 넘친 행은 페이지에서 빠진다.
// ponytail: 256B로 1,000행을 넘기려면 `CSI n S` 반복 같은 병적 입력뿐이다. 실제로 잘리면 간격을 줄이거나 재생 Term 상한을 키운다.
pub(crate) type CheckpointFn<'a> = &'a mut dyn FnMut(usize, usize, &Engine);

const REPLAY_STEP: usize = 256;
/// 세션별 스크롤백 지우기 기준선(`u64` LE 8B)을 남기는 파일 이름(OWE-873b).
const CLEARED_FILE: &str = "cleared";
/// 명시적 `Request::Close`로 닫힌 세션을 표시하는 파일 이름. 재부팅 소실 세션과 구분해 부활을 막는다.
const CLOSED_FILE: &str = "closed";

/// 저장소 상한. 기본값이 ARCHITECTURE §4의 값이다(MB = 10⁶B).
#[derive(Debug, Clone)]
pub struct Limits {
    /// 이만큼 원본을 담으면 봉인한다.
    pub seal_bytes: u64,
    /// 세션 하나의 디스크 상한(압축 후).
    pub session_bytes: u64,
    /// 전역 디스크 상한.
    pub total_bytes: u64,
    /// 닫힌 세션 보존 기간(마지막 쓰기 기준).
    pub keep_closed: Duration,
    /// 토큰 버킷 지속 속도(바이트/분).
    pub rate_per_min: u64,
    /// 토큰 버킷 버스트.
    pub burst: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            seal_bytes: 1 << 20,
            session_bytes: 32_000_000,
            total_bytes: 1_000_000_000,
            keep_closed: Duration::from_hours(7 * 24),
            rate_per_min: 8_000_000,
            burst: 32_000_000,
        }
    }
}

/// 토큰 버킷. 정수 연산을 위해 토큰을 `바이트 × 1분(ns)` 단위로 센다.
#[derive(Debug)]
struct Bucket {
    scaled: u128,
    at: Instant,
}

const MINUTE_NS: u128 = 60_000_000_000;

impl Bucket {
    fn refill(&mut self, now: Instant, l: &Limits) {
        let dt = now.saturating_duration_since(self.at).as_nanos();
        self.at = self.at.max(now);
        let cap = u128::from(l.burst) * MINUTE_NS;
        self.scaled = self
            .scaled
            .saturating_add(dt.saturating_mul(u128::from(l.rate_per_min)))
            .min(cap);
    }

    fn avail(&self) -> u64 {
        u64::try_from(self.scaled / MINUTE_NS).unwrap_or(u64::MAX)
    }

    fn take(&mut self, n: u64) {
        self.scaled = self.scaled.saturating_sub(u128::from(n) * MINUTE_NS);
    }

    fn give(&mut self, n: u64) {
        self.scaled = self.scaled.saturating_add(u128::from(n) * MINUTE_NS);
    }
}

/// krilld 전체에 하나. 세션별 [`Recorder`]가 `Arc`로 나눠 갖는다.
#[derive(Debug)]
pub struct Store {
    sessions: PathBuf,
    recording: bool,
    encryption: bool,
    key: Option<Arc<crypto::Key>>,
    generation: AtomicU64,
    /// 생성·rename만 직렬화한다. 압축·열린 파일 쓰기·재귀 삭제는 이 잠금 밖이다.
    gate: Mutex<()>,
    history_publication: RwLock<()>,
    limits: Limits,
    bucket: Mutex<Bucket>,
    written: AtomicU64,
    /// 일시중지 ACK는 진행 중인 쓰기가 끝난 뒤에만 돌려준다.
    pause: RwLock<Pause>,
    /// 세션별 스크롤백 지우기 기준선 `(generation, cleared_seq)`(OWE-873b).
    cleared: Mutex<HashMap<SessionId, (u64, u64)>>,
    /// 명시적 `Close`로 닫힌 세션 집합.
    closed: Mutex<HashSet<SessionId>>,
}

#[derive(Debug, Default)]
struct Pause {
    paused: bool,
    epoch: u64,
}

impl Store {
    /// `root` 아래 `history.toml`의 `recording = false`면 디스크를 건드리지 않는 기록기를 만든다.
    /// 기본값은 기록 켜기: `sessions/`는 0700이며 Time Machine에서 제외한다.
    pub fn open(root: &Path, limits: Limits) -> io::Result<Arc<Self>> {
        let result = Self::open_inner(root, limits);
        let warning = root.join(krill_proto::history::RECORDING_WARNING_FILE);
        if result.is_err() {
            privacy::warn(root);
        } else {
            fs::remove_file(warning).ok();
        }
        result
    }

    fn open_inner(root: &Path, limits: Limits) -> io::Result<Arc<Self>> {
        let config = privacy::config(root)?;
        // 크래시로 남은 삭제 잔재가 1GB 집계에서 빠진 채 평문으로 남지 않게 먼저 치운다(K-21).
        privacy::sweep_detached(root);
        let sessions = root.join("sessions");
        let recording = config.recording;
        if recording {
            privacy::prepare(&sessions)?;
        }
        let existing = recording && segment::has_encrypted(&sessions)?;
        let key = if recording && (config.encryption || existing) {
            Some(crypto::Key::load(root, existing)?)
        } else {
            None
        };
        let store = Self::with_key(sessions, limits, recording, config.encryption, key);
        store.set_paused(privacy::initial_pause(root)?);
        Ok(store)
    }

    fn with_key(
        sessions: PathBuf,
        limits: Limits,
        recording: bool,
        encryption: bool,
        key: Option<Arc<crypto::Key>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            recording,
            encryption,
            key,
            generation: AtomicU64::new(0),
            gate: Mutex::new(()),
            history_publication: RwLock::new(()),
            bucket: Mutex::new(Bucket {
                scaled: u128::from(limits.burst) * MINUTE_NS,
                at: Instant::now(),
            }),
            limits,
            written: AtomicU64::new(0),
            pause: RwLock::default(),
            cleared: Mutex::default(),
            closed: Mutex::default(),
        })
    }

    pub fn session_dir(&self, id: SessionId) -> PathBuf {
        self.sessions.join(format!("{}.noindex", id.0))
    }

    /// 명시적으로 닫힌 세션(`Request::Close`)임을 메모리와 디스크에 기록한다.
    pub fn mark_closed(&self, id: SessionId) {
        let _gate = lock(&self.gate);
        lock(&self.closed).insert(id);
        if self.recording {
            let dir = self.session_dir(id);
            if dir.is_dir() {
                write_closed(&dir);
            }
        }
    }

    /// 명시적으로 닫힌 세션이면 `true`. 재부팅으로 소실된 콜드 세션과 구분한다.
    pub fn is_closed(&self, id: SessionId) -> bool {
        if lock(&self.closed).contains(&id) {
            return true;
        }
        if !self.recording {
            return false;
        }
        let closed = self.session_dir(id).join(CLOSED_FILE).is_file();
        if closed {
            lock(&self.closed).insert(id);
        }
        closed
    }

    pub(crate) fn recording(&self) -> bool {
        self.recording
    }

    /// 일시중지 동안 PTY·화면은 유지하고 새 디스크 기록만 막는다.
    pub fn set_paused(&self, paused: bool) {
        let mut state = self.pause.write().unwrap_or_else(PoisonError::into_inner);
        if state.paused != paused {
            state.epoch = state.epoch.saturating_add(1);
            state.paused = paused;
        }
    }

    fn record_guard(&self) -> RwLockReadGuard<'_, Pause> {
        self.pause.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// 지금까지 디스크에 쓴 바이트(크래시 뒤 잘라 낸 부분 쓰기는 빠진다). 쓰기 지표(MK-068)용.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// 세션 기록기를 연다. 같은 세션의 세그먼트가 이미 있으면(krilld 재기동) 마지막 세그먼트의 잘린 꼬리를 잘라 내고
    /// 그 끝에서 이어 쓴다. 마지막 세그먼트가 봉인되지 않았으면 다음 세그먼트 첫 레코드가 `Gap(0)`(길이 모름)이다.
    pub fn recorder(self: &Arc<Self>, id: SessionId, size: WinSize) -> io::Result<Recorder> {
        let result = Recorder::open(self, id, size);
        if result.is_err()
            && let Some(root) = self.sessions.parent()
        {
            privacy::warn(root);
        }
        result
    }

    /// 세션 `id`의 콜드 스크롤백 기준선(`seq`)을 기록한다(OWE-873b).
    /// 세그먼트 파일은 지우지 않고 기준선 이전 원본에서 밀려난 행만 히스토리 응답에서 제외한다.
    pub fn clear_scrollback(&self, id: SessionId, seq: u64) {
        let _publication = self
            .history_publication
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let pause = self.record_guard();
        let _gate = lock(&self.gate);
        let generation = self.generation.load(Ordering::Acquire);
        let target = {
            let mut cleared = lock(&self.cleared);
            let entry = cleared.entry(id).or_insert((generation, 0));
            if entry.0 == generation {
                entry.1 = entry.1.max(seq);
            } else {
                *entry = (generation, seq);
            }
            entry.1
        };
        if self.recording && !pause.paused {
            let dir = self.session_dir(id);
            if dir.is_dir() {
                write_cleared(&dir, target);
            }
        }
    }

    /// 닫힌(콜드) 세션 `id`의 마지막 세그먼트 끝까지 스크롤백 기준선을 올린다(OWE-873b).
    /// 디스크에 남은 세그먼트가 있으면 `true`.
    pub fn clear_cold_scrollback(&self, id: SessionId) -> bool {
        if !self.recording {
            return false;
        }
        let Ok(mut paths) = segments(&self.session_dir(id)) else {
            return false;
        };
        while let Some(path) = paths.pop() {
            if let Ok(Some(seg)) = segment::read(&path, self.key.as_deref())
                && !seg.records.is_empty()
            {
                self.clear_scrollback(id, seg.end_seq());
                return true;
            }
        }
        false
    }

    fn cleared_seq(&self, id: SessionId) -> u64 {
        let generation = self.generation.load(Ordering::Acquire);
        {
            let cleared = lock(&self.cleared);
            if let Some(&(recorded_gen, seq)) = cleared.get(&id)
                && recorded_gen == generation
            {
                return seq;
            }
        }
        if !self.recording {
            return 0;
        }
        let seq = read_cleared(&self.session_dir(id)).unwrap_or(0);
        if seq > 0 {
            let mut cleared = lock(&self.cleared);
            let entry = cleared.entry(id).or_insert((generation, 0));
            if entry.0 == generation {
                entry.1 = entry.1.max(seq);
            } else {
                *entry = (generation, seq);
            }
            return entry.1;
        }
        0
    }

    /// 세션의 콜드 이력(디스크에 남은 세그먼트 목록).
    pub fn history(&self, id: SessionId) -> io::Result<History> {
        let segs = if self.recording {
            segments(&self.session_dir(id))?
        } else {
            Vec::new()
        };
        let cleared_seq = self.cleared_seq(id);
        let mut first_page = 0usize;
        if cleared_seq > 0 {
            while let Some(path) = segs.get(first_page) {
                if segs
                    .get(first_page + 1)
                    .and_then(|p| start_of(p))
                    .is_some_and(|next| next <= cleared_seq)
                {
                    first_page += 1;
                    continue;
                }
                if let Ok(Some(seg)) = segment::read(path, self.key.as_deref())
                    && seg.end_seq() <= cleared_seq
                {
                    first_page += 1;
                    continue;
                }
                break;
            }
        }
        Ok(History {
            key: self.key.clone(),
            segs,
            first_page,
            cleared_seq,
        })
    }

    /// 세션 `id`의 원본 `[from, to)`를 디스크 세그먼트에서 seq 순서로 꺼내 `each`에 넘긴다(seq 이어받기, MK-023).
    /// 남아 있지 않거나(GC·세션 상한) 기록하지 못한(토큰 버킷·쓰기 실패·크래시) 구간은 [`Piece::Gap`]이다.
    /// 세그먼트를 하나씩 읽으므로 메모리는 세그먼트 하나(원본 ≤ 봉인 크기 + 마지막 묶음) 정도다.
    pub fn raw(
        &self,
        id: SessionId,
        from: u64,
        to: u64,
        mut each: impl FnMut(Piece) -> io::Result<()>,
    ) -> io::Result<()> {
        let segs = match self.history(id).map(|h| h.segs) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            r => r?,
        };
        let mut pos = from;
        for (i, path) in segs.iter().enumerate() {
            if pos >= to {
                break;
            }
            // 다음 세그먼트가 `pos` 이하에서 시작하면 이 세그먼트는 통째로 앞이다.
            if segs
                .get(i + 1)
                .and_then(|p| start_of(p))
                .is_some_and(|next| next <= pos)
            {
                continue;
            }
            // 목록을 만든 뒤 GC·상한 정리로 지워졌으면 없는 것으로 본다.
            let seg = match segment::read(path, self.key.as_deref()) {
                Ok(Some(seg)) => seg,
                Ok(None) => continue,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let mut seq = seg.header.start_seq;
            if seq > pos {
                let end = seq.min(to);
                each(Piece::Gap {
                    seq: pos,
                    len: end - pos,
                })?;
                pos = end;
            }
            for rec in seg.records {
                if pos >= to {
                    break;
                }
                let end = seq.saturating_add(rec.seq_len());
                match rec {
                    Record::Data(mut d) if end > pos => {
                        let cut = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
                        d.truncate(cut(to.min(end) - seq));
                        d.drain(..cut(pos - seq).min(d.len()));
                        let n = d.len() as u64;
                        each(Piece::Data { seq: pos, bytes: d })?;
                        pos += n;
                    }
                    Record::Gap(0) if seq >= pos => each(Piece::Gap { seq, len: 0 })?,
                    Record::Gap(_) if end > pos => {
                        let stop = to.min(end);
                        each(Piece::Gap {
                            seq: pos,
                            len: stop - pos,
                        })?;
                        pos = stop;
                    }
                    _ => {}
                }
                seq = end;
            }
        }
        if pos < to {
            each(Piece::Gap {
                seq: pos,
                len: to - pos,
            })?;
        }
        Ok(())
    }

    /// 보존 정리. `live`에 없는 세션은 마지막 쓰기가 보존 기간보다 오래면 통째로 지우고, 전역 상한을 넘으면
    /// 오래된 세그먼트부터 지운다(열린 세션의 마지막 세그먼트는 남긴다). krilld 기동 때와 주기적으로 부른다.
    /// 항목 하나의 IO 오류가 전체 주기를 중단시키지 않는다. 실패한 항목은 경고만 남기고 건너뛴다(K-20).
    pub fn gc(&self, live: &[SessionId], now: SystemTime) -> io::Result<()> {
        let generation = self.generation.load(Ordering::Acquire);
        if !self.recording || !self.sessions.exists() {
            return Ok(());
        }
        // 재개 이력은 새 라이브 세션의 일부다. closed 보존 기간만으로 조상을 지우지 않는다.
        // 전역 디스크 상한은 기존대로 적용한다.
        let live: std::collections::HashSet<_> =
            live.iter().flat_map(|id| self.history_chain(*id)).collect();
        let mut files = Vec::new();
        let mut total = 0;
        let entries = match fs::read_dir(&self.sessions) {
            // 잠금 없이 훑는 중 clear가 디렉터리를 떼어 내면 다음 주기에 본다.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            result => result?,
        };
        for entry in entries {
            let dir = match entry {
                Ok(entry) => entry.path(),
                Err(e) => {
                    tracing::warn!(kind = ?e.kind(), "history gc skipped an entry");
                    continue;
                }
            };
            let id = dir
                .file_name()
                .and_then(|n| n.to_str()?.strip_suffix(".noindex")?.parse().ok())
                .map(SessionId);
            let Some(id) = id else { continue };
            let segs = match segments(&dir) {
                Ok(segs) => segs,
                Err(e) => {
                    tracing::warn!(kind = ?e.kind(), "history gc skipped a session dir");
                    continue;
                }
            };
            let mut stats = Vec::with_capacity(segs.len());
            for path in segs {
                let meta = match fs::metadata(&path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        tracing::warn!(kind = ?e.kind(), "history gc skipped a segment");
                        continue;
                    }
                    Ok(meta) => meta,
                };
                let modified = match meta.modified() {
                    Ok(modified) => modified,
                    Err(e) => {
                        tracing::warn!(kind = ?e.kind(), "history gc skipped a segment");
                        continue;
                    }
                };
                stats.push((modified, meta.len(), path));
            }
            let is_live = live.contains(&id);
            let newest = stats.iter().map(|s| s.0).max();
            let age = newest.map_or(Duration::MAX, |t| now.duration_since(t).unwrap_or_default());
            // 세그먼트가 하나도 없는 죽은 세션 디렉터리도 걷는다. 살아 있는 세션의 빈 디렉터리는 둔다.
            if !is_live && (newest.is_none() || age > self.limits.keep_closed) {
                if let Err(e) = self.discard(&dir, generation) {
                    tracing::warn!(kind = ?e.kind(), "history gc could not discard a session");
                }
                continue;
            }
            total += stats.iter().map(|s| s.1).sum::<u64>();
            if is_live {
                stats.pop(); // 열려 있을 수 있는 마지막 세그먼트
            }
            files.extend(stats);
        }
        files.sort();
        for (_, len, path) in files {
            if total <= self.limits.total_bytes {
                break;
            }
            match self.discard(&path, generation) {
                Ok(()) => total -= len,
                Err(e) => {
                    tracing::warn!(kind = ?e.kind(), "history gc could not discard a segment");
                }
            }
        }
        Ok(())
    }

    /// 토큰 `need`를 예약한다. 남은 토큰이 `floor`보다 적으면 예약하지 않고 `false`.
    fn reserve(&self, need: u64, floor: u64, now: Instant) -> bool {
        let mut b = lock(&self.bucket);
        b.refill(now, &self.limits);
        let ok = b.avail() >= floor;
        if ok {
            b.take(need);
        }
        ok
    }

    /// 예약한 `need` 중 실제로 쓴 `spent`만 남기고 돌려준다.
    fn settle(&self, need: u64, spent: u64) {
        lock(&self.bucket).give(need.saturating_sub(spent));
        self.written.fetch_add(spent, Ordering::Relaxed);
    }

    /// 예약 없이 쓴 작은 기록(봉인 꼬리·리사이즈)을 센다.
    fn charge(&self, n: u64) {
        lock(&self.bucket).take(n);
        self.written.fetch_add(n, Ordering::Relaxed);
    }
}

fn mkdir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// 세션 디렉터리의 세그먼트 파일을 seq 순서로(이름이 0으로 채운 `start_seq`라 이름순 = seq순).
fn segments(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        result => result?,
    };
    for entry in entries {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with("seg-") && path.extension().is_some_and(|e| e == "krs") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// 세그먼트 파일 이름(`seg-<start_seq 20자리>.krs`)의 시작 seq.
fn start_of(path: &Path) -> Option<u64> {
    path.file_stem()?
        .to_str()?
        .strip_prefix("seg-")?
        .parse()
        .ok()
}

/// 세션 디렉터리에 스크롤백 지우기 기준선을 남긴다. 최선형이라 실패해도 메모리 기준선은 동작한다.
fn write_cleared(dir: &Path, seq: u64) {
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::io::Write as _;
    if let Ok(mut file) = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join(CLEARED_FILE))
    {
        file.write_all(&seq.to_le_bytes()).ok();
    }
}

fn read_cleared(dir: &Path) -> Option<u64> {
    let bytes = fs::read(dir.join(CLEARED_FILE)).ok()?;
    let arr: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(arr))
}

fn write_closed(dir: &Path) {
    use krill_proto::os::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join(CLOSED_FILE))
        .ok();
}

/// [`Store::raw`]가 넘기는 원본 구간 조각.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Data {
        seq: u64,
        bytes: Vec<u8>,
    },
    /// 보낼 수 없는 구간. `len == 0`은 크래시로 길이를 모르는 구간(seq는 이어진다).
    Gap {
        seq: u64,
        len: u64,
    },
}

/// 콜드 페이지 하나: 세그먼트 하나를 재생하는 동안 스크롤백으로 밀려난 행.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub start_seq: u64,
    pub end_seq: u64,
    /// 오래된 행부터, 화면 전송과 같은 행 타입(끝 기본 칸 제거). 줄바꿈된 긴 줄은 화면 행마다 따로.
    pub rows: Vec<Vec<Cell>>,
    /// 이 세그먼트 안에서 기록하지 못한 구간(`len == 0`은 크래시로 길이 모름).
    pub gaps: Vec<Gap>,
}

/// 디스크에 남은 한 세션의 세그먼트 목록. 페이지 번호 = 기준선(`cleared_seq`) 이후 세그먼트 순서(0이 가장 오래됨).
#[derive(Debug)]
pub struct History {
    segs: Vec<PathBuf>,
    first_page: usize,
    cleared_seq: u64,
    key: Option<Arc<crypto::Key>>,
}

/// A search consumes harvested batches immediately; no full page of cells is retained.
pub(crate) enum HistoryEvent {
    Rows(Vec<Vec<Cell>>),
    End(io::Result<Page>),
}

/// 디스크에서 재구성한 세션 화면(크래시·재부팅 뒤 복원).
pub struct Restored {
    engine: Engine,
    pub end_seq: u64,
}

impl std::fmt::Debug for Restored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Restored")
            .field("end_seq", &self.end_seq)
            .finish_non_exhaustive()
    }
}

impl Restored {
    pub fn screen_text(&self) -> String {
        self.engine.screen_text()
    }

    pub(crate) fn into_engine(self) -> Engine {
        self.engine
    }

    /// 마지막 화면(콜드 복원 뷰, MK-032).
    pub fn screen(&self) -> krill_proto::screen::Screen {
        self.engine.screen()
    }

    /// 화면 위 스크롤백, 오래된 행부터(최대 [`HOT_LINES`](crate::HOT_LINES)행).
    pub fn history(&self) -> Vec<Vec<Cell>> {
        self.engine.history_rows(0, self.engine.history_len()).1
    }
}

impl Page {
    /// 행마다 평문(끝 공백 제거, 넓은 문자는 한 글자).
    pub fn lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let mut text = String::new();
                for c in row.iter().filter(|c| !c.flags.contains(Flags::SPACER)) {
                    text.push(c.ch);
                    text.extend(&c.zerowidth);
                }
                text.truncate(text.trim_end_matches(' ').len());
                text
            })
            .collect()
    }
}

impl History {
    pub fn len(&self) -> usize {
        self.segs.len().saturating_sub(self.first_page)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn raw_index(&self, i: usize) -> io::Result<usize> {
        let raw_i = self
            .first_page
            .checked_add(i)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such page"))?;
        if raw_i >= self.segs.len() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such page"));
        }
        Ok(raw_i)
    }

    /// 페이지 `i`를 재생한다. 메모리는 재생 Term 하나(화면 + 최대 [`HOT_LINES`](crate::HOT_LINES)행)와 세그먼트 원본(≤ 봉인 크기) 정도다.
    pub fn page(&self, i: usize) -> io::Result<Page> {
        let raw_i = self.raw_index(i)?;
        let seg = self.read_raw(raw_i)?;
        let look = no_theme();
        let mut engine = self.warm(raw_i, &seg, &look);
        engine.take_history(); // 워밍업에서 밀려난 행은 앞 페이지 몫
        let mut rows = Vec::new();
        let gaps = replay(&mut engine, &seg, &look, Some(&mut rows), self.cleared_seq);
        Ok(Page {
            start_seq: seg.header.start_seq.max(self.cleared_seq),
            end_seq: seg.end_seq(),
            rows,
            gaps,
        })
    }

    /// Lazy screen history: retain only the requested rows, even for a large segment.
    /// Two bounded replays determine the total first, then harvest the requested range.
    pub fn rows(&self, i: usize, skip: usize, count: usize) -> io::Result<(usize, Vec<Vec<Cell>>)> {
        let raw_i = self.raw_index(i)?;
        let seg = self.read_raw(raw_i)?;
        let look = no_theme();
        let mut total = 0usize;
        {
            let mut engine = self.warm(raw_i, &seg, &look);
            engine.take_history();
            replay_harvest(
                &mut engine,
                &seg,
                &look,
                Some(&mut |batch: Vec<Vec<Cell>>| {
                    total = total.saturating_add(batch.len());
                }),
                None,
                self.cleared_seq,
            );
        }
        let end = total.saturating_sub(skip);
        let start =
            end.saturating_sub(count.min(usize::from(krill_proto::screen::MAX_HISTORY_ROWS)));
        let mut rows = Vec::new();
        if start < end {
            let mut engine = self.warm(raw_i, &seg, &look);
            engine.take_history();
            let mut at = 0usize;
            replay_harvest(
                &mut engine,
                &seg,
                &look,
                Some(&mut |batch: Vec<Vec<Cell>>| {
                    for row in batch {
                        if (start..end).contains(&at) {
                            rows.push(row);
                        }
                        at = at.saturating_add(1);
                    }
                }),
                None,
                self.cleared_seq,
            );
        }
        Ok((total, rows))
    }

    /// 페이지 `i`를 `limit_seq`까지만 재생한다. 세그먼트 시작이 `limit_seq` 이상이면 `None`.
    pub fn page_until(&self, i: usize, limit_seq: u64) -> Option<io::Result<Page>> {
        if limit_seq <= self.cleared_seq {
            return None;
        }
        let raw_i = match self.raw_index(i) {
            Ok(raw_i) => raw_i,
            Err(e) => return Some(Err(e)),
        };
        let seg = match self.read_raw(raw_i) {
            Ok(seg) => seg,
            Err(e) => return Some(Err(e)),
        };
        if seg.header.start_seq >= limit_seq {
            return None;
        }
        let look = no_theme();
        let mut engine = self.warm(raw_i, &seg, &look);
        engine.take_history(); // 워밍업에서 밀려난 행은 앞 페이지 몫
        let mut rows = Vec::new();
        let (gaps, end_seq) = replay_until(
            &mut engine,
            &seg,
            &look,
            Some(&mut rows),
            Some(limit_seq),
            self.cleared_seq,
        );
        if end_seq <= self.cleared_seq {
            return None;
        }
        Some(Ok(Page {
            start_seq: seg.header.start_seq.max(self.cleared_seq),
            end_seq,
            rows,
            gaps,
        }))
    }

    /// Read each segment once, with bounded canonical and harvesting replay views.
    /// The canonical view never loses history needed by a later resize/reflow; the
    /// harvesting view keeps its raw parser but rebases terminal state at each page.
    /// Production callers hold a shared `replay_gate` permit; this method does not
    /// acquire again so snapshot validation and index construction can share that scope.
    pub(crate) fn visit_until(
        &self,
        limit_seq: u64,
        cancelled: &dyn Fn() -> bool,
        visitor: impl FnMut(usize, HistoryEvent),
    ) {
        self.visit_until_checkpoints(limit_seq, cancelled, None, visitor);
    }

    pub(crate) fn visit_until_checkpoints(
        &self,
        limit_seq: u64,
        cancelled: &dyn Fn() -> bool,
        mut checkpoint: Option<CheckpointFn<'_>>,
        mut visitor: impl FnMut(usize, HistoryEvent),
    ) -> Option<Engine> {
        if limit_seq <= self.cleared_seq || self.is_empty() {
            return None;
        }
        let look = no_theme();
        let mut canonical: Option<Engine> = None;
        let mut harvested: Option<Engine> = None;
        let mut previous_end = None;
        let mut parser_reset = false;
        let mut run_id = 0usize;
        // Cleared pages still contain parser state needed by the first visible page.
        // Warm that prefix once in forward order too, instead of rereading it twice.
        for raw_i in 0..self.segs.len() {
            if cancelled() {
                return None;
            }
            let page = raw_i.checked_sub(self.first_page);
            let seg = match self.read_raw(raw_i) {
                Ok(seg) => seg,
                Err(error) => {
                    canonical = None;
                    harvested = None;
                    previous_end = None;
                    if let Some(page) = page {
                        visitor(page, HistoryEvent::End(Err(error)));
                    }
                    continue;
                }
            };
            if seg.header.start_seq >= limit_seq {
                break;
            }
            let new_run =
                previous_end != Some(seg.header.start_seq) || seg.header.base == Base::Start;
            if new_run {
                run_id = run_id.saturating_add(1);
                let start = || {
                    if cancelled() {
                        return None;
                    }
                    Some(if seg.header.base == Base::Start {
                        Engine::new(seg.header.size)
                    } else {
                        keyframe_engine(&seg, &look).unwrap_or_else(|| Engine::new(seg.header.size))
                    })
                };
                canonical = start();
                harvested = start();
            } else if parser_reset {
                // A parser panic can reset only one view. Rebuild that exceptional
                // prefix once; ordinary resize/alt/sync histories never use this path.
                harvested = self.warm_checked(raw_i, &seg, &look, cancelled).ok();
            }
            let (Some(raw), Some(view)) = (canonical.as_mut(), harvested.as_mut()) else {
                return None;
            };
            raw.resize(seg.header.size);
            if seg.header.start_seq <= self.cleared_seq {
                raw.clear_history();
            }
            if !parser_reset && let Some(record_cp) = checkpoint.as_deref_mut() {
                record_cp(run_id, raw_i, raw);
            }
            if page.is_some() {
                view.rebase_replay(raw);
                view.clear_history();
            } else {
                view.resize(seg.header.size);
                if seg.header.start_seq <= self.cleared_seq {
                    view.clear_history();
                }
            }
            // Same raw bytes, sizes, feed boundaries and explicit Gap handling in both
            // views. No disk reread and no ANSI snapshot masquerading as parser state.
            let (_, _, raw_reset) = replay_harvest_report(
                raw,
                &seg,
                &look,
                None,
                Some(limit_seq),
                self.cleared_seq,
                cancelled,
            );
            if cancelled() {
                return None;
            }
            let (gaps, end_seq, view_reset) = match page {
                Some(page) => replay_harvest_report(
                    view,
                    &seg,
                    &look,
                    Some(&mut |batch| visitor(page, HistoryEvent::Rows(batch))),
                    Some(limit_seq),
                    self.cleared_seq,
                    cancelled,
                ),
                None => replay_harvest_report(
                    view,
                    &seg,
                    &look,
                    None,
                    Some(limit_seq),
                    self.cleared_seq,
                    cancelled,
                ),
            };
            if cancelled() {
                return None;
            }
            parser_reset = raw_reset || view_reset;
            previous_end = Some(end_seq);
            if let Some(page) = page
                && end_seq > self.cleared_seq
            {
                visitor(
                    page,
                    HistoryEvent::End(Ok(Page {
                        start_seq: seg.header.start_seq.max(self.cleared_seq),
                        end_seq,
                        rows: Vec::new(),
                        gaps,
                    })),
                );
            }
        }
        canonical
    }

    /// 마지막 세그먼트까지 재생해 화면과 핫 스크롤백을 되살린다.
    pub fn restore(&self) -> io::Result<Restored> {
        let raw_i = self
            .segs
            .len()
            .checked_sub(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no segments"))?;
        let seg = self.read_raw(raw_i)?;
        let look = no_theme();
        let mut engine = self.warm(raw_i, &seg, &look);
        replay(&mut engine, &seg, &look, None, self.cleared_seq);
        Ok(Restored {
            engine,
            end_seq: seg.end_seq(),
        })
    }

    fn read_raw(&self, raw_i: usize) -> io::Result<Segment> {
        #[cfg(all(test, unix))]
        tests::note_raw_read();
        let path = self
            .segs
            .get(raw_i)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such page"))?;
        segment::read(path, self.key.as_deref())?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad segment header"))
    }

    /// 세그먼트 `seg`(= `raw_i`번)의 시작 상태를 복원한 엔진. 키프레임이 있으면 바로 주입하고, 없으면 앞 세그먼트를 재생한다.
    fn warm(&self, raw_i: usize, seg: &Segment, look: &Appearance) -> Engine {
        self.warm_checked(raw_i, seg, look, &|| false)
            .unwrap_or_else(|_| Engine::new(seg.header.size))
    }

    fn warm_checked(
        &self,
        raw_i: usize,
        seg: &Segment,
        look: &Appearance,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Engine> {
        if seg.header.base == Base::Start {
            return Ok(Engine::new(seg.header.size));
        }
        // 안전하지 않은 TUI 상태가 여러 세그먼트에 걸쳐도 마지막 안전 키프레임/시작점부터 순서대로 재생한다.
        // 한 번에 세그먼트 하나만 메모리에 두며, GC로 끊긴 구간 너머는 재생하지 않는다.
        let mut first = raw_i;
        let mut start = seg.header.start_seq;
        while let Some(p) = first.checked_sub(1) {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let Ok(prev) = self.read_raw(p) else { break };
            if prev.end_seq() != start {
                break;
            }
            first = p;
            start = prev.header.start_seq;
            if prev.header.base == Base::Start {
                break;
            }
        }
        // 남은 원본이 우선이다. 앞 원본이 GC된 가장 이른 세그먼트에서만 시작 키프레임을 쓴다.
        let initial = self.read_raw(first).ok();
        let mut engine = initial
            .as_ref()
            .and_then(|s| {
                (s.header.base != Base::Start)
                    .then(|| keyframe_engine(s, look))
                    .flatten()
            })
            .unwrap_or_else(|| {
                Engine::new(initial.as_ref().map_or(seg.header.size, |s| s.header.size))
            });
        for p in first..raw_i {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if let Ok(previous) = self.read_raw(p) {
                engine.resize(previous.header.size);
                if previous.header.start_seq < self.cleared_seq {
                    engine.clear_history();
                }
                replay_harvest_cancelled(
                    &mut engine,
                    &previous,
                    look,
                    None,
                    None,
                    self.cleared_seq,
                    cancelled,
                );
            }
        }
        engine.resize(seg.header.size);
        if seg.header.start_seq <= self.cleared_seq {
            engine.clear_history();
        }
        Ok(engine)
    }
}

fn keyframe_engine(seg: &Segment, look: &Appearance) -> Option<Engine> {
    if seg.keyframe_bytes.is_empty() {
        return None;
    }
    let mut engine = Engine::new(seg.header.size);
    panic::catch_unwind(AssertUnwindSafe(|| {
        engine.feed(&seg.keyframe_bytes, Feed::Replay, look);
    }))
    .ok()?;
    Some(engine)
}

/// 재생에는 질의 응답이 없으니 색은 쓰이지 않는다. 내장 기본 테마로 둔다.
fn no_theme() -> Appearance {
    Appearance::load(PathBuf::new())
}

/// 세그먼트 레코드를 엔진에 다시 넣는다. `harvest`가 있으면 밀려난 행을 거둬 담는다. Gap 목록을 돌려준다.
fn replay(
    engine: &mut Engine,
    seg: &Segment,
    look: &Appearance,
    harvest: Option<&mut Vec<Vec<Cell>>>,
    cleared_seq: u64,
) -> Vec<Gap> {
    replay_until(engine, seg, look, harvest, None, cleared_seq).0
}

fn replay_until(
    engine: &mut Engine,
    seg: &Segment,
    look: &Appearance,
    harvest: Option<&mut Vec<Vec<Cell>>>,
    limit_seq: Option<u64>,
    cleared_seq: u64,
) -> (Vec<Gap>, u64) {
    match harvest {
        Some(rows) => replay_harvest(
            engine,
            seg,
            look,
            Some(&mut |batch| rows.extend(batch)),
            limit_seq,
            cleared_seq,
        ),
        None => replay_harvest(engine, seg, look, None, limit_seq, cleared_seq),
    }
}

fn replay_harvest(
    engine: &mut Engine,
    seg: &Segment,
    look: &Appearance,
    harvest: Option<&mut dyn FnMut(Vec<Vec<Cell>>)>,
    limit_seq: Option<u64>,
    cleared_seq: u64,
) -> (Vec<Gap>, u64) {
    replay_harvest_cancelled(engine, seg, look, harvest, limit_seq, cleared_seq, &|| {
        false
    })
}

fn replay_harvest_cancelled(
    engine: &mut Engine,
    seg: &Segment,
    look: &Appearance,
    harvest: Option<&mut dyn FnMut(Vec<Vec<Cell>>)>,
    limit_seq: Option<u64>,
    cleared_seq: u64,
    cancelled: &dyn Fn() -> bool,
) -> (Vec<Gap>, u64) {
    let (gaps, end, _) = replay_harvest_report(
        engine,
        seg,
        look,
        harvest,
        limit_seq,
        cleared_seq,
        cancelled,
    );
    (gaps, end)
}

fn replay_harvest_report(
    engine: &mut Engine,
    seg: &Segment,
    look: &Appearance,
    mut harvest: Option<&mut dyn FnMut(Vec<Vec<Cell>>)>,
    limit_seq: Option<u64>,
    cleared_seq: u64,
    cancelled: &dyn Fn() -> bool,
) -> (Vec<Gap>, u64, bool) {
    let mut gaps = Vec::new();
    let mut parser_reset = false;
    let mut seq = seg.header.start_seq;
    let mut size = seg.header.size;
    let mut reap = |engine: &mut Engine| {
        if let Some(h) = harvest.as_deref_mut()
            && engine.history_len() > 0
        {
            h(engine.take_history());
        }
    };
    if seq <= cleared_seq {
        engine.clear_history();
    }
    for rec in &seg.records {
        if cancelled() || limit_seq.is_some_and(|limit| seq >= limit) {
            break;
        }
        match rec {
            Record::Data(d) => {
                let take = limit_seq.map_or(d.len(), |limit| {
                    usize::try_from(limit.saturating_sub(seq))
                        .unwrap_or(usize::MAX)
                        .min(d.len())
                });
                let slice = d.get(..take).unwrap_or(d);
                let pre = usize::try_from(cleared_seq.saturating_sub(seq))
                    .unwrap_or(usize::MAX)
                    .min(slice.len());
                for part in slice.get(..pre).unwrap_or_default().chunks(REPLAY_STEP) {
                    if cancelled() {
                        return (gaps, seq, parser_reset);
                    }
                    let fed = panic::catch_unwind(AssertUnwindSafe(|| {
                        engine.feed(part, Feed::Replay, look);
                    }));
                    if fed.is_err() {
                        *engine = Engine::new(size);
                        parser_reset = true;
                    }
                    engine.clear_history();
                }
                for part in slice.get(pre..).unwrap_or_default().chunks(REPLAY_STEP) {
                    if cancelled() {
                        return (gaps, seq, parser_reset);
                    }
                    // 라이브에서 panic한 바이트는 재생에서도 panic한다. 세션 리더와 같은 방식으로 엔진을 새로 만든다.
                    let fed = panic::catch_unwind(AssertUnwindSafe(|| {
                        engine.feed(part, Feed::Replay, look);
                    }));
                    if fed.is_err() {
                        *engine = Engine::new(size);
                        parser_reset = true;
                    }
                    reap(engine);
                }
                seq = seq.saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
                if take < d.len() {
                    break;
                }
                continue;
            }
            Record::Keyframe(_) => {}
            Record::Resize(s) => {
                size = *s;
                engine.resize(size);
                if seq < cleared_seq {
                    engine.clear_history();
                } else {
                    reap(engine);
                }
            }
            Record::Gap(n) => {
                let len = limit_seq.map_or(*n, |limit| (*n).min(limit.saturating_sub(seq)));
                let end = seq.saturating_add(len);
                if *n == 0 {
                    if seq >= cleared_seq {
                        gaps.push(Gap { start: seq, len: 0 });
                    }
                } else if end > cleared_seq {
                    let start = seq.max(cleared_seq);
                    gaps.push(Gap {
                        start,
                        len: end - start,
                    });
                }
                seq = end;
                if len < *n {
                    break;
                }
                continue;
            }
        }
        seq = seq.saturating_add(rec.seq_len());
    }
    (gaps, seq, parser_reset)
}

#[cfg(test)]
#[cfg(unix)]
mod encryption_tests;
#[cfg(test)]
#[cfg(unix)]
mod keyframe_tests;
#[cfg(test)]
#[cfg(unix)]
mod tests;
