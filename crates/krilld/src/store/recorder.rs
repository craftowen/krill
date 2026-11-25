//! 세션별 기록기. 쓰기 허용 read 잠금 → 개인정보 gate 순서로만 잠근다.
//! 압축·열린 파일 쓰기는 gate 밖에서 하고, 일시중지 ACK는 쓰기 read 잠금이 풀린 뒤 반환한다.

use krill_proto::os::fs::OpenOptionsExt;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError, atomic::Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::holder::WinSize;

use super::crypto::{SALT_LEN, Stream, key_unavailable};
use super::segment::{self, Base, HEADER_LEN, Header, Record};
use super::{LEVEL, OVERHEAD, RESUME, Store, WINDOW_LOG, lock, mkdir, privacy, segments};
use crate::session::Gap;

/// Keep the compression window during bursts, then release it for quiet sessions.
const IDLE_SEAL: Duration = Duration::from_secs(5);
/// Per-session bounded async recording queue (10 sessions <= 6.25 MiB worst-case, 0 when idle).
pub(crate) const MAX_ASYNC_QUEUE_BYTES: usize = 640 * 1024;
pub(crate) const MAX_ASYNC_QUEUE_ITEMS: usize = 128;
const MAX_ASYNC_COALESCE_BYTES: usize = 64 * 1024;

struct Open {
    file: File,
    path: PathBuf,
    crypto: Option<Stream>,
    enc: zstd::stream::write::Encoder<'static, Vec<u8>>,
    /// 이 세그먼트에 담은 원본 바이트.
    raw: u64,
    /// 온전히 쓴 파일 길이.
    len: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Snapshots {
    Allowed,
    Suppressed,
}

/// 세션 하나의 기록기. PTY 리더 스레드가 들고 읽은 묶음마다 [`Recorder::append`]를 부른다.
pub struct Recorder {
    store: Arc<Store>,
    dir: PathBuf,
    id: SessionId,
    size: WinSize,
    /// 지금까지 받은 원본의 끝 seq(기록했든 버렸든).
    seq: u64,
    /// 아직 `Gap` 레코드로 남기지 못한 버린 바이트.
    lost: u64,
    /// 앞 krilld가 봉인 없이 끝났다. 다음 기록 앞에 `Gap(0)`.
    crashed: bool,
    /// 이 세션의 세그먼트를 하나라도 만들었다(다음 세그먼트는 `Base::Continue`).
    started: bool,
    /// 토큰이 모자라 버리는 중.
    dropping: bool,
    open: Option<Open>,
    /// 다음 세그먼트 시작 화면. 원본과 같이 압축·암호화하고 seq에는 포함하지 않는다.
    keyframe: Vec<u8>,
    pause_epoch: u64,
    snapshots: Snapshots,
    generation: u64,
    /// Reused only for record encoding; never retains privacy-sensitive data between writes.
    encoded: Vec<u8>,
    /// A quiet PTY must not retain the last segment's compression context forever.
    last_output: Option<Instant>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("id", &self.id)
            .field("seq", &self.seq)
            .field("lost", &self.lost)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    pub(super) fn open(store: &Arc<Store>, id: SessionId, size: WinSize) -> io::Result<Self> {
        let pause = store.record_guard();
        let _gate = lock(&store.gate);
        let generation = store.generation.load(Ordering::Acquire);
        let dir = store.session_dir(id);
        let mut rec = Self {
            store: Arc::clone(store),
            dir,
            id,
            size,
            seq: 0,
            lost: 0,
            crashed: false,
            started: false,
            dropping: false,
            open: None,
            keyframe: Vec::new(),
            pause_epoch: pause.epoch,
            snapshots: if pause.paused {
                Snapshots::Suppressed
            } else {
                Snapshots::Allowed
            },
            generation,
            encoded: Vec::new(),
            last_output: None,
        };
        if !store.recording {
            return Ok(rec);
        }
        segment::warm_decoder();
        if !pause.paused {
            privacy::prepare(&store.sessions)?;
            mkdir(&rec.dir)?;
        }
        let mut paths = segments(&rec.dir)?;
        while let Some(path) = paths.pop() {
            match segment::read(&path, store.key.as_deref())? {
                Some(seg) if !seg.records.is_empty() => {
                    if !pause.paused && fs::metadata(&path)?.len() > seg.valid_len {
                        OpenOptions::new()
                            .write(true)
                            .open(&path)?
                            .set_len(seg.valid_len)?;
                    }
                    rec.seq = seg.end_seq();
                    rec.crashed = !seg.sealed;
                    rec.started = true;
                    break;
                }
                _ if !pause.paused => fs::remove_file(&path)?,
                _ => {}
            }
        }
        Ok(rec)
    }

    /// 삭제 전 인코더를 버리고 같은 seq부터 새 세그먼트를 시작한다.
    fn refresh(&mut self, generation: u64) {
        if self.generation != generation {
            self.open = None;
            self.keyframe.clear();
            self.lost = 0;
            self.crashed = false;
            self.started = false;
            self.dropping = false;
            self.generation = generation;
        }
    }

    /// PTY에서 읽은 원본을 기록한다. 실패는 `Gap`으로 남기고 삼킨다(기록 때문에 PTY 읽기를 멈추지 않는다).
    pub fn append(&mut self, bytes: &[u8]) {
        self.append_at(bytes, Instant::now());
    }

    /// 새 세그먼트가 필요할 때만 라이브 엔진의 시작 화면을 캡처한다.
    pub(crate) fn needs_keyframe(&self) -> bool {
        let pause = self.store.record_guard();
        self.store.recording
            && self.open.is_none()
            && self.snapshots == Snapshots::Allowed
            && !pause.paused
    }

    /// The reader wakes once to seal an idle stream, then waits indefinitely again.
    pub(crate) fn idle_deadline(&self) -> Option<Instant> {
        self.open.as_ref()?;
        self.last_output?.checked_add(IDLE_SEAL)
    }

    pub(crate) fn keyframe(&mut self, bytes: Vec<u8>) {
        let store = Arc::clone(&self.store);
        let pause = store.record_guard();
        self.refresh(store.generation.load(Ordering::Acquire));
        self.observe_pause(pause.epoch);
        if self.snapshots == Snapshots::Allowed && !pause.paused {
            self.keyframe = bytes;
        }
    }

    fn observe_pause(&mut self, epoch: u64) {
        if self.pause_epoch != epoch {
            // 열린 파일은 봉인하지 않고 버린다. 이미 flush한 청크는 복구할 수 있고, 중지 ACK 뒤 쓰기는 없다.
            self.crashed |= self.open.take().is_some();
            self.keyframe.clear();
            // 화면에 남은 중지 중 텍스트를 나중의 키프레임에 담지 않는다.
            self.snapshots = Snapshots::Suppressed;
            self.pause_epoch = epoch;
        }
    }

    pub(crate) fn append_at(&mut self, bytes: &[u8], now: Instant) {
        let store = Arc::clone(&self.store);
        let pause = store.record_guard();
        self.refresh(store.generation.load(Ordering::Acquire));
        self.observe_pause(pause.epoch);
        let n = bytes.len() as u64;
        if !store.recording {
            self.seq += n;
            return;
        }
        if pause.paused {
            self.lost = self.lost.saturating_add(n);
            self.seq = self.seq.saturating_add(n);
            self.keyframe.clear();
            return;
        }
        if n == 0 {
            return;
        }
        let snapshot = std::mem::take(&mut self.keyframe);
        let raw = n.saturating_add(snapshot.len() as u64);
        let need = raw + raw / (128 << 10) * 3 + OVERHEAD;
        let floor = if self.dropping {
            need.max(RESUME)
        } else {
            need
        };
        if !self.store.reserve(need, floor, now) {
            self.dropping = true;
            self.lost += n;
            self.seq += n;
            return;
        }
        self.dropping = false;
        let mut recs = Vec::with_capacity(3);
        if !snapshot.is_empty() && self.open.is_none() {
            recs.push(Record::Keyframe(snapshot));
        }
        if self.crashed {
            recs.push(Record::Gap(0));
        }
        if self.lost > 0 {
            recs.push(Record::Gap(self.lost));
        }
        recs.push(Record::Data(bytes.to_vec()));
        let mut spent = 0;
        let res = self.write(&recs, n, &mut spent);
        self.store.settle(need, spent);
        self.seq += n;
        if res.is_err() {
            self.abandon();
            self.lost += n;
            return;
        }
        self.crashed = false;
        self.lost = 0;
        self.last_output = Some(now);
        if self
            .open
            .as_ref()
            .is_some_and(|o| o.raw >= self.store.limits.seal_bytes)
        {
            self.seal_open();
        }
    }

    /// 지금까지 받은 원본의 끝 seq. krilld가 재기동하면 디스크 마지막 세그먼트 끝에서 이어진다.
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    pub(crate) fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// panic 전 부분 갱신된 인코더는 버리고 디스크에서 온전한 꼬리를 다시 찾는다.
    /// 디스크에 없는 원본 seq는 다음 기록 앞의 Gap으로 표시한다.
    pub(crate) fn recover_after_panic(self, end: u64, size: WinSize) -> io::Result<Self> {
        let store = Arc::clone(&self.store);
        let id = self.id;
        drop(self);
        let mut recovered = Self::open(&store, id, size)?;
        recovered.lost = recovered
            .lost
            .saturating_add(end.saturating_sub(recovered.seq));
        recovered.seq = recovered.seq.max(end);
        recovered.crashed = true;
        Ok(recovered)
    }

    /// 디스크 부족 등 일시적 복구 실패 시 기록기를 버리지 않고 열린 파일 없는 상태로 유지해,
    /// 디스크 공간이 돌아오면 다음 출력부터 자동으로 세그먼트 기록을 재개한다.
    fn degraded_after_recovery_failure(
        store: Arc<Store>,
        id: SessionId,
        end: u64,
        size: WinSize,
    ) -> Self {
        let generation = store.generation.load(Ordering::Acquire);
        let pause_epoch = store.record_guard().epoch;
        let dir = store.session_dir(id);
        Self {
            store,
            dir,
            id,
            size,
            seq: end,
            open: None,
            dropping: false,
            lost: 0,
            crashed: true,
            started: true,
            generation,
            pause_epoch,
            snapshots: Snapshots::Allowed,
            keyframe: Vec::new(),
            encoded: Vec::new(),
            last_output: None,
        }
    }

    /// 창 크기가 바뀌었다. 열린 세그먼트에 `Resize`를 남기고, 없으면 다음 세그먼트 헤더에 담긴다.
    pub fn resize(&mut self, size: WinSize) {
        let store = Arc::clone(&self.store);
        let pause = store.record_guard();
        self.refresh(store.generation.load(Ordering::Acquire));
        self.observe_pause(pause.epoch);
        self.size = size;
        if pause.paused {
            return;
        }
        if self.open.is_some() {
            let mut spent = 0;
            let res = self.write(&[Record::Resize(size)], 0, &mut spent);
            self.store.charge(spent);
            if res.is_err() {
                self.abandon();
            }
        }
    }

    /// 열린 세그먼트의 zstd 프레임을 끝내고 닫은 뒤 세션 상한을 넘는 오래된 세그먼트를 지운다. 세션이 끝날 때도 부른다.
    pub fn seal(&mut self) {
        let store = Arc::clone(&self.store);
        let pause = store.record_guard();
        self.refresh(store.generation.load(Ordering::Acquire));
        self.observe_pause(pause.epoch);
        if pause.paused {
            return;
        }
        if store.recording && (self.lost > 0 || self.crashed) {
            // 열린 세그먼트도 버린 바이트도 없으면 빈 봉인 세그먼트를 만들지 않는다.
            // 빈 세그먼트는 seq 길이가 0이라 다음 세그먼트와 이름이 겹쳐 `create_new`가
            // 영원히 실패하고 그 세션 기록이 영구 정지한다(K-02). 크래시 표시는
            // 다음 출력·봉인으로 넘긴다.
            if self.open.is_none() && self.lost == 0 {
                return;
            }
            let mut records = Vec::new();
            if self.crashed {
                records.push(Record::Gap(0));
            }
            if self.lost > 0 {
                records.push(Record::Gap(self.lost));
            }
            let mut spent = 0;
            let result = self.write(&records, 0, &mut spent);
            // 작은 Gap 메타데이터는 토큰이 없어도 기록한다(resize·봉인 꼬리와 같다).
            store.charge(spent);
            if result.is_err() {
                self.abandon();
                return;
            }
            self.lost = 0;
            self.crashed = false;
        }
        self.seal_open();
    }

    fn seal_open(&mut self) {
        let Some(Open {
            mut file,
            path,
            enc,
            mut crypto,
            len,
            ..
        }) = self.open.take()
        else {
            return;
        };
        #[cfg(test)]
        write_hook::before_seal(self.id);
        if let Ok(mut tail) = enc.finish() {
            if let Some(crypto) = &mut crypto
                && crypto.seal(&mut tail).is_err()
            {
                return;
            }
            let frame = segment::chunk(&tail);
            if file.write_all(&frame).is_ok() {
                self.store.charge(frame.len() as u64);
            } else {
                file.set_len(len).ok();
            }
        }
        drop(file);
        self.trim(&path).ok();
    }

    /// Advances `seq` across `Store::clear` without touching disk.
    fn discard_for_clear(&mut self, generation: u64, len: u64, resized: Option<WinSize>) {
        self.open = None;
        self.last_output = None;
        self.keyframe.clear();
        self.lost = 0;
        self.crashed = false;
        self.started = false;
        self.dropping = false;
        self.generation = generation;
        if let Some(size) = resized {
            self.size = size;
        }
        self.seq = self.seq.saturating_add(len);
    }

    fn record_overflow_gap(&mut self, len: u64, resized: Option<WinSize>) {
        let store = Arc::clone(&self.store);
        let pause = store.record_guard();
        self.refresh(store.generation.load(Ordering::Acquire));
        self.observe_pause(pause.epoch);
        if let Some(size) = resized {
            self.size = size;
            if store.recording && !pause.paused && self.open.is_some() {
                let mut spent = 0;
                let res = self.write(&[Record::Resize(size)], 0, &mut spent);
                self.store.charge(spent);
                if res.is_err() {
                    self.abandon();
                }
            }
        }
        if !store.recording {
            self.seq = self.seq.saturating_add(len);
            return;
        }
        self.keyframe.clear();
        self.lost = self.lost.saturating_add(len);
        self.seq = self.seq.saturating_add(len);
    }

    /// 레코드들을 청크 하나로 쓴다. 세그먼트가 없으면 새로 연다. 실패하면 호출자가 [`Self::abandon`].
    fn write(&mut self, recs: &[Record], raw: u64, spent: &mut u64) -> io::Result<()> {
        #[cfg(test)]
        write_hook::before_write(self.id);
        if self.open.is_none() {
            let keyframe = matches!(recs.first(), Some(Record::Keyframe(_)));
            match self.create(spent, keyframe) {
                Ok(open) => self.open = Some(open),
                // 같은 이름의 빈 세그먼트(옛 `seal()`이 남긴 seq 길이 0의 크래시
                // 표시, K-02)가 있으면 치우고 한 번만 다시 연다. 데이터가 든
                // 세그먼트와 겹치면 지우지 않고 오류를 돌려준다.
                Err(e)
                    if e.kind() == io::ErrorKind::AlreadyExists && self.reclaim_empty_segment() =>
                {
                    self.open = Some(self.create(spent, keyframe)?);
                }
                Err(e) => return Err(e),
            }
        }
        let Some(open) = &mut self.open else {
            return Err(io::ErrorKind::NotFound.into());
        };
        self.encoded.clear();
        for r in recs {
            r.encode_into(&mut self.encoded);
        }
        let written = open.enc.write_all(&self.encoded);
        self.encoded.fill(0);
        self.encoded.clear();
        // A one-off large keyframe must not pin its largest allocation for the
        // remaining lifetime of a quiet session.
        if self.encoded.capacity() > 256 * 1024 {
            self.encoded = Vec::new();
        }
        written?;
        open.enc.flush()?;
        if let Some(crypto) = &mut open.crypto {
            crypto.seal(open.enc.get_mut())?;
        }
        let frame = segment::chunk(open.enc.get_ref());
        open.enc.get_mut().clear();
        open.file.write_all(&frame)?;
        open.len += frame.len() as u64;
        open.raw += raw;
        *spent += frame.len() as u64;
        Ok(())
    }

    fn create(&mut self, spent: &mut u64, keyframe: bool) -> io::Result<Open> {
        let start = self.seq - self.lost;
        let header = Header {
            encrypted: self.store.encryption,
            base: if start == 0 && !self.started {
                Base::Start
            } else {
                Base::Continue
            },
            session: self.id,
            start_seq: start,
            size: self.size,
        };
        let mut prefix = header
            .encode_with_keyframe(if keyframe {
                segment::KEYFRAME_V1
            } else {
                segment::KEYFRAME_NONE
            })
            .to_vec();
        let crypto = if header.encrypted {
            let keys = self.store.key.as_deref();
            let (crypto, salt) = Stream::create(keys.ok_or_else(key_unavailable)?.get()?, &prefix)?;
            prefix.extend_from_slice(&salt);
            Some(crypto)
        } else {
            None
        };
        let path = self.dir.join(format!("seg-{start:020}.krs"));
        let mut file = {
            let _gate = lock(&self.store.gate);
            if self.generation != self.store.generation.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if !self.store.sessions.exists() {
                privacy::prepare(&self.store.sessions)?;
            }
            mkdir(&self.dir)?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?
        };
        let opened = file.write_all(&prefix).and_then(|()| {
            let mut enc = zstd::stream::write::Encoder::new(Vec::new(), LEVEL)?;
            enc.window_log(WINDOW_LOG)?;
            Ok(enc)
        });
        let enc = match opened {
            Ok(enc) => enc,
            Err(e) => {
                drop(file);
                self.store.discard(&path, self.generation).ok();
                return Err(e);
            }
        };
        *spent += prefix.len() as u64;
        self.started = true;
        Ok(Open {
            file,
            path,
            enc,
            crypto,
            raw: 0,
            len: prefix.len() as u64,
        })
    }

    /// 쓰기에 실패한 세그먼트를 온전한 데까지 잘라 닫는다. 인코더 상태가 디스크와 어긋났으니 이어 쓰지 않는다.
    fn abandon(&mut self) {
        if let Some(o) = self.open.take() {
            if o.len <= (HEADER_LEN + if o.crypto.is_some() { SALT_LEN } else { 0 }) as u64 {
                drop(o.file);
                self.store.discard(&o.path, self.generation).ok();
            } else {
                o.file.set_len(o.len).ok();
            }
        }
    }

    /// 다음에 열 세그먼트 자리(`create`와 같은 이름)에 원본이 하나도 없는 세그먼트나
    /// 크래시로 잘린 빈 헤더 파일만 치운다(K-02 복구). `Gap(0)` 크래시 표시·`Resize`·`Keyframe`만
    /// 든 세그먼트는 seq 길이가 0이라 지워도 원본을 잃지 않고, 다음 세그먼트 헤더가 최신 크기를 담는다.
    /// 데이터가 있거나 알 수 없는 미래 버전·복호화 실패면 `false`(그대로 오류)다.
    fn reclaim_empty_segment(&self) -> bool {
        let start = self.seq.saturating_sub(self.lost);
        let path = self.dir.join(format!("seg-{start:020}.krs"));
        let empty = match segment::read(&path, self.store.key.as_deref()) {
            Ok(Some(seg)) => seg.end_seq() == seg.header.start_seq,
            Ok(None) => segment::is_reclaimable_unreadable(&path),
            Err(_) => false,
        };
        empty && fs::remove_file(&path).is_ok()
    }

    /// 다음에 열 세그먼트(원본 봉인 크기 + 마지막 묶음, 비압축이어도 봉인 크기의 2배 미만)가 들어갈 자리를 남기도록,
    /// 방금 봉인한 `keep`만 빼고 오래된 세그먼트부터 지운다. 그래서 열린 세그먼트까지 합쳐도 세션 상한을 넘지 않는다.
    fn trim(&self, keep: &Path) -> io::Result<()> {
        let l = &self.store.limits;
        let target = l.session_bytes.saturating_sub(2 * l.seal_bytes);
        let mut segs = Vec::new();
        for path in segments(&self.dir)? {
            match fs::metadata(&path) {
                Ok(meta) => segs.push((meta.len(), path)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let mut total: u64 = segs.iter().map(|s| s.0).sum();
        for (len, path) in segs {
            if total <= target || path == keep {
                break;
            }
            self.store.discard(&path, self.generation)?;
            total -= len;
        }
        Ok(())
    }
}

struct BatchItem {
    seq: u64,
    bytes: Vec<u8>,
    resized: Option<WinSize>,
    snapshot: Vec<u8>,
    generation: u64,
}

enum AsyncOp {
    Batch {
        ticket: u64,
        batch: BatchItem,
    },
    DropSpan {
        ticket: u64,
        len: u64,
        resized: Option<WinSize>,
        generation: u64,
        clear: bool,
    },
    Seal {
        ticket: u64,
        resized: Option<WinSize>,
        generation: u64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerLifecycle {
    Running,
    Stopping,
    Dead,
}

struct AsyncState {
    queue: VecDeque<AsyncOp>,
    queued_bytes: usize,
    enqueued_gen: u64,
    completed_gen: u64,
    in_flight: bool,
    idle_sealing: bool,
    worker_open: bool,
    lifecycle: WorkerLifecycle,
}

pub(super) struct AsyncHandle {
    mu: Mutex<AsyncState>,
    cv: Condvar,
}

impl AsyncHandle {
    fn flush(&self) {
        let mut state = lock(&self.mu);
        let target = state.enqueued_gen;
        while (state.completed_gen < target || state.idle_sealing)
            && state.lifecycle != WorkerLifecycle::Dead
        {
            state = self.cv.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn flush_cancelled(&self, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        let mut state = lock(&self.mu);
        let target = state.enqueued_gen;
        while (state.completed_gen < target || state.idle_sealing)
            && state.lifecycle != WorkerLifecycle::Dead
        {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let (guard, _) = self
                .cv
                .wait_timeout(state, Duration::from_millis(10))
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
        Ok(())
    }

    fn quiesce_for_clear(&self, generation: u64) {
        let mut state = lock(&self.mu);
        let mut dropped_len = 0u64;
        let mut dropped_resized = None;
        let mut max_ticket = state.enqueued_gen;
        let mut had_ops = false;
        while let Some(op) = state.queue.pop_front() {
            had_ops = true;
            match op {
                AsyncOp::Batch { ticket, batch } => {
                    max_ticket = max_ticket.max(ticket);
                    dropped_len = dropped_len.saturating_add(batch.bytes.len() as u64);
                    if batch.resized.is_some() {
                        dropped_resized = batch.resized;
                    }
                }
                AsyncOp::DropSpan {
                    ticket,
                    len,
                    resized,
                    ..
                } => {
                    max_ticket = max_ticket.max(ticket);
                    dropped_len = dropped_len.saturating_add(len);
                    if resized.is_some() {
                        dropped_resized = resized;
                    }
                }
                AsyncOp::Seal {
                    ticket, resized, ..
                } => {
                    max_ticket = max_ticket.max(ticket);
                    if resized.is_some() {
                        dropped_resized = resized;
                    }
                }
            }
        }
        state.queued_bytes = 0;
        if (had_ops || state.worker_open || state.in_flight)
            && state.lifecycle != WorkerLifecycle::Dead
        {
            let ticket = if had_ops {
                max_ticket
            } else {
                state.enqueued_gen = state.enqueued_gen.saturating_add(1);
                state.enqueued_gen
            };
            state.queue.push_back(AsyncOp::DropSpan {
                ticket,
                len: dropped_len,
                resized: dropped_resized,
                generation,
                clear: true,
            });
            self.cv.notify_all();
        }
        let target = state.enqueued_gen;
        while (state.completed_gen < target || state.in_flight || state.worker_open)
            && state.lifecycle != WorkerLifecycle::Dead
        {
            state = self.cv.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

pub(crate) struct ClearGuard<'a>(&'a Store);

impl Drop for ClearGuard<'_> {
    fn drop(&mut self) {
        self.0.clearing.fetch_sub(1, Ordering::Release);
    }
}

impl Store {
    pub(super) fn begin_clear(&self) -> ClearGuard<'_> {
        self.clearing.fetch_add(1, Ordering::AcqRel);
        ClearGuard(self)
    }

    pub(super) fn is_clearing(&self) -> bool {
        self.clearing.load(Ordering::Acquire) > 0
    }

    fn register_async(&self, id: SessionId, handle: &Arc<AsyncHandle>) -> u64 {
        let reg_id = self.next_async_id.fetch_add(1, Ordering::Relaxed);
        let mut map = lock(&self.async_recorders);
        map.retain(|(_, _, weak)| weak.strong_count() > 0);
        map.push((reg_id, id, Arc::downgrade(handle)));
        reg_id
    }

    fn unregister_async(&self, reg_id: u64) {
        let mut map = lock(&self.async_recorders);
        map.retain(|(existing, _, weak)| *existing != reg_id && weak.strong_count() > 0);
    }

    fn active_async(&self, filter: Option<SessionId>) -> Vec<Arc<AsyncHandle>> {
        let map = lock(&self.async_recorders);
        map.iter()
            .filter(|(_, id, _)| filter.is_none_or(|target| *id == target))
            .filter_map(|(_, _, weak)| weak.upgrade())
            .collect()
    }

    pub(super) fn flush_session(&self, id: SessionId) {
        for handle in self.active_async(Some(id)) {
            handle.flush();
        }
    }

    pub(super) fn flush_session_cancelled(
        &self,
        id: SessionId,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        for handle in self.active_async(Some(id)) {
            handle.flush_cancelled(cancelled)?;
        }
        Ok(())
    }

    pub(super) fn flush_all_async(&self) {
        for handle in self.active_async(None) {
            handle.flush();
        }
    }

    pub(super) fn discard_async_recorders_for_clear(&self) {
        let generation = self.generation.load(Ordering::Acquire);
        for handle in self.active_async(None) {
            handle.quiesce_for_clear(generation);
        }
    }
}

/// Asynchronous bounded per-session recorder wrapper used by [`crate::session`].
/// Disk compression, encryption, and file I/O run off the PTY reader thread.
pub(crate) struct AsyncRecorder {
    store: Arc<Store>,
    handle: Arc<AsyncHandle>,
    reg_id: u64,
    worker: Option<JoinHandle<()>>,
    projected_open: bool,
    projected_open_raw: u64,
    projected_pause_epoch: u64,
    projected_snapshots: Snapshots,
    projected_generation: u64,
}

impl AsyncRecorder {
    pub(crate) fn start(
        recorder: Recorder,
        on_panic_gap: impl Fn(Gap) + Send + 'static,
    ) -> Result<Self, Box<Recorder>> {
        let store = Arc::clone(&recorder.store);
        let id = recorder.id;
        let projected_open = recorder.open.is_some();
        let projected_pause_epoch = recorder.pause_epoch;
        let projected_snapshots = recorder.snapshots;
        let projected_generation = recorder.generation;

        let handle = Arc::new(AsyncHandle {
            mu: Mutex::new(AsyncState {
                queue: VecDeque::new(),
                queued_bytes: 0,
                enqueued_gen: 0,
                completed_gen: 0,
                in_flight: false,
                idle_sealing: false,
                worker_open: projected_open,
                lifecycle: WorkerLifecycle::Running,
            }),
            cv: Condvar::new(),
        });
        let reg_id = store.register_async(id, &handle);
        let worker_store = Arc::clone(&store);
        let worker_handle = Arc::clone(&handle);
        let slot = Arc::new(Mutex::new(Some(recorder)));
        let worker_slot = Arc::clone(&slot);

        if let Ok(worker) = thread::Builder::new()
            .name(format!("rec-{}", id.0))
            .stack_size(256 * 1024)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                let recorder = lock(&worker_slot).take();
                let Some(recorder) = recorder else { return };
                recorder_worker(
                    recorder,
                    &worker_store,
                    &worker_handle,
                    reg_id,
                    &on_panic_gap,
                );
            })
        {
            Ok(Self {
                store,
                handle,
                reg_id,
                worker: Some(worker),
                projected_open,
                projected_open_raw: 0,
                projected_pause_epoch,
                projected_snapshots,
                projected_generation,
            })
        } else {
            store.unregister_async(reg_id);
            let mut state = lock(&handle.mu);
            state.lifecycle = WorkerLifecycle::Dead;
            handle.cv.notify_all();
            drop(state);
            let recorder = lock(&slot).take();
            match recorder {
                Some(r) => Err(Box::new(r)),
                None => unreachable!("recorder slot only consumed by spawned thread"),
            }
        }
    }

    pub(crate) fn needs_keyframe(&mut self) -> bool {
        let pause = self.store.record_guard();
        if !self.store.recording || pause.paused {
            return false;
        }
        if self.projected_pause_epoch != pause.epoch {
            self.projected_pause_epoch = pause.epoch;
            self.projected_snapshots = Snapshots::Suppressed;
            self.projected_open = false;
            self.projected_open_raw = 0;
        }
        drop(pause);
        if self.projected_snapshots != Snapshots::Allowed {
            return false;
        }
        let generation = self.store.generation.load(Ordering::Acquire);
        if self.projected_generation != generation {
            self.projected_generation = generation;
            self.projected_open = false;
            self.projected_open_raw = 0;
        }
        let state = lock(&self.handle.mu);
        if state.queue.is_empty()
            && !state.in_flight
            && state.completed_gen >= state.enqueued_gen
            && !state.worker_open
        {
            self.projected_open = false;
            self.projected_open_raw = 0;
        }
        !self.projected_open
    }

    pub(crate) fn record(
        &mut self,
        seq: u64,
        bytes: &[u8],
        resized: Option<WinSize>,
        snapshot: Option<Vec<u8>>,
    ) {
        if bytes.is_empty() && resized.is_none() {
            return;
        }
        let generation = self.store.generation.load(Ordering::Acquire);
        if self.projected_generation != generation {
            self.projected_generation = generation;
            self.projected_open = false;
            self.projected_open_raw = 0;
        }
        let mut state_guard = lock(&self.handle.mu);
        let state = &mut *state_guard;
        if state.lifecycle == WorkerLifecycle::Dead {
            return;
        }
        let snapshot_bytes = snapshot.unwrap_or_default();
        let batch_mem = bytes.len().saturating_add(snapshot_bytes.len());
        let coalesce_cap = MAX_ASYNC_COALESCE_BYTES
            .min(usize::try_from(self.store.limits.seal_bytes.max(1)).unwrap_or(usize::MAX));
        let accepted = if resized.is_none()
            && snapshot_bytes.is_empty()
            && !bytes.is_empty()
            && state.queued_bytes.saturating_add(bytes.len()) <= MAX_ASYNC_QUEUE_BYTES
            && let Some(AsyncOp::Batch { ticket, batch }) = state.queue.back_mut()
            && batch.generation == generation
            && batch.seq.saturating_add(batch.bytes.len() as u64) == seq
            && batch.bytes.len().saturating_add(bytes.len()) <= coalesce_cap
        {
            state.enqueued_gen = state.enqueued_gen.saturating_add(1);
            *ticket = state.enqueued_gen;
            batch.bytes.extend_from_slice(bytes);
            state.queued_bytes = state.queued_bytes.saturating_add(bytes.len());
            true
        } else if state.queue.is_empty()
            || (state.queue.len() < MAX_ASYNC_QUEUE_ITEMS
                && state.queued_bytes.saturating_add(batch_mem) <= MAX_ASYNC_QUEUE_BYTES)
        {
            state.enqueued_gen = state.enqueued_gen.saturating_add(1);
            let ticket = state.enqueued_gen;
            state.queued_bytes = state.queued_bytes.saturating_add(batch_mem);
            state.queue.push_back(AsyncOp::Batch {
                ticket,
                batch: BatchItem {
                    seq,
                    bytes: bytes.to_vec(),
                    resized,
                    snapshot: snapshot_bytes,
                    generation,
                },
            });
            true
        } else {
            // Queue bounded: do not stall PTY reader/publish; fold overflow into a Gap span.
            let drop_len = bytes.len() as u64;
            let next_ticket = state.enqueued_gen.saturating_add(1);
            state.enqueued_gen = next_ticket;
            if let Some(AsyncOp::DropSpan {
                ticket,
                len,
                resized: tail_resized,
                generation: tail_gen,
                clear: false,
            }) = state.queue.back_mut()
                && *tail_gen == generation
            {
                *ticket = next_ticket;
                *len = len.saturating_add(drop_len);
                if resized.is_some() {
                    *tail_resized = resized;
                }
            } else {
                state.queue.push_back(AsyncOp::DropSpan {
                    ticket: next_ticket,
                    len: drop_len,
                    resized,
                    generation,
                    clear: false,
                });
            }
            self.projected_open = false;
            self.projected_open_raw = 0;
            false
        };
        if accepted && !bytes.is_empty() {
            self.projected_open_raw = self.projected_open_raw.saturating_add(bytes.len() as u64);
            if self.projected_open_raw >= self.store.limits.seal_bytes {
                self.projected_open = false;
                self.projected_open_raw = 0;
            } else {
                self.projected_open = true;
            }
        }
        self.handle.cv.notify_all();
    }

    pub(crate) fn seal(&mut self, resized: Option<WinSize>) {
        let generation = self.store.generation.load(Ordering::Acquire);
        let mut state = lock(&self.handle.mu);
        if state.lifecycle != WorkerLifecycle::Dead {
            state.enqueued_gen = state.enqueued_gen.saturating_add(1);
            let ticket = state.enqueued_gen;
            state.queue.push_back(AsyncOp::Seal {
                ticket,
                resized,
                generation,
            });
            self.handle.cv.notify_all();
            while (state.completed_gen < ticket || state.in_flight)
                && state.lifecycle != WorkerLifecycle::Dead
            {
                state = self
                    .handle
                    .cv
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        self.projected_open = false;
        self.projected_open_raw = 0;
    }

    pub(crate) fn finish(mut self, resized: Option<WinSize>) {
        self.shutdown(resized);
    }

    fn shutdown(&mut self, resized: Option<WinSize>) {
        let generation = self.store.generation.load(Ordering::Acquire);
        {
            let mut state = lock(&self.handle.mu);
            if state.lifecycle != WorkerLifecycle::Dead {
                state.enqueued_gen = state.enqueued_gen.saturating_add(1);
                let ticket = state.enqueued_gen;
                state.queue.push_back(AsyncOp::Seal {
                    ticket,
                    resized,
                    generation,
                });
                state.lifecycle = WorkerLifecycle::Stopping;
                self.handle.cv.notify_all();
            }
        }
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
        self.store.unregister_async(self.reg_id);
        self.projected_open = false;
        self.projected_open_raw = 0;
    }

    #[cfg(test)]
    pub(crate) fn queued_bytes(&self) -> usize {
        lock(&self.handle.mu).queued_bytes
    }
}

impl Drop for AsyncRecorder {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.shutdown(None);
        }
    }
}

struct WorkerExitGuard<'a> {
    store: &'a Store,
    handle: &'a AsyncHandle,
    reg_id: u64,
}

impl Drop for WorkerExitGuard<'_> {
    fn drop(&mut self) {
        self.store.unregister_async(self.reg_id);
        let mut state = lock(&self.handle.mu);
        state.in_flight = false;
        state.idle_sealing = false;
        state.worker_open = false;
        state.lifecycle = WorkerLifecycle::Dead;
        self.handle.cv.notify_all();
    }
}

fn recorder_worker(
    recorder: Recorder,
    store: &Store,
    handle: &AsyncHandle,
    reg_id: u64,
    on_panic_gap: &dyn Fn(Gap),
) {
    let _exit_guard = WorkerExitGuard {
        store,
        handle,
        reg_id,
    };
    let mut rec = Some(recorder);
    let mut last_size = rec
        .as_ref()
        .map_or(WinSize { cols: 80, rows: 24 }, |r| r.size);

    loop {
        enum NextWork {
            Op(AsyncOp),
            IdleSeal,
            Exit,
        }

        let work = {
            let mut state = lock(&handle.mu);
            loop {
                if let Some(op) = state.queue.pop_front() {
                    if let AsyncOp::Batch { batch, .. } = &op {
                        state.queued_bytes = state
                            .queued_bytes
                            .saturating_sub(batch.bytes.len().saturating_add(batch.snapshot.len()));
                    }
                    state.in_flight = true;
                    break NextWork::Op(op);
                }
                if state.lifecycle == WorkerLifecycle::Stopping {
                    break NextWork::Exit;
                }
                if let Some(at) = rec.as_ref().and_then(Recorder::idle_deadline) {
                    let now = Instant::now();
                    if at <= now {
                        state.in_flight = true;
                        state.idle_sealing = true;
                        break NextWork::IdleSeal;
                    }
                    let (guard, _) = handle
                        .cv
                        .wait_timeout(state, at.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner);
                    state = guard;
                } else {
                    state = handle
                        .cv
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        };

        let mut completed_ticket = None;
        if !matches!(work, NextWork::Exit) {
            store.wait_for_pending_pause();
        }
        match work {
            NextWork::Exit => break,
            NextWork::IdleSeal => {
                if let Some(r) = &mut rec {
                    if store.is_clearing() {
                        r.discard_for_clear(store.generation.load(Ordering::Acquire), 0, None);
                    } else if panic::catch_unwind(AssertUnwindSafe(|| r.seal())).is_err() {
                        tracing::warn!("pty seal panicked; continuing");
                        let seq = r.seq();
                        rec = recover_worker_recorder(rec.take(), seq, last_size);
                    }
                }
            }
            NextWork::Op(AsyncOp::Batch { ticket, batch }) => {
                completed_ticket = Some(ticket);
                let batch_seq = batch.seq;
                let batch_len = batch.bytes.len() as u64;
                if let Some(sz) = batch.resized {
                    last_size = sz;
                }
                let current_gen = store.generation.load(Ordering::Acquire);
                if store.is_clearing() || batch.generation != current_gen {
                    if let Some(r) = &mut rec {
                        r.discard_for_clear(current_gen, batch_len, batch.resized);
                    }
                } else if let Some(r) = &mut rec {
                    let expected_end = r.seq().max(batch_seq.saturating_add(batch_len));
                    let BatchItem {
                        bytes,
                        resized,
                        snapshot,
                        ..
                    } = batch;
                    let result = panic::catch_unwind(AssertUnwindSafe(|| {
                        if let Some(sz) = resized {
                            r.resize(sz);
                        }
                        if !snapshot.is_empty() {
                            r.keyframe(snapshot);
                        }
                        r.append(&bytes);
                    }));
                    if result.is_err() {
                        tracing::warn!("pty batch recording panicked; kept as gap");
                        on_panic_gap(Gap {
                            start: batch_seq,
                            len: batch_len,
                        });
                        rec = recover_worker_recorder(rec.take(), expected_end, last_size);
                    }
                }
            }
            NextWork::Op(AsyncOp::DropSpan {
                ticket,
                len,
                resized,
                generation,
                clear,
            }) => {
                completed_ticket = Some(ticket);
                if let Some(sz) = resized {
                    last_size = sz;
                }
                let current_gen = store.generation.load(Ordering::Acquire);
                if let Some(r) = &mut rec {
                    if clear || store.is_clearing() || generation != current_gen {
                        r.discard_for_clear(current_gen, len, resized);
                    } else {
                        let expected_end = r.seq().saturating_add(len);
                        if panic::catch_unwind(AssertUnwindSafe(|| {
                            r.record_overflow_gap(len, resized);
                        }))
                        .is_err()
                        {
                            tracing::warn!("pty overflow gap recording panicked; recovering");
                            rec = recover_worker_recorder(rec.take(), expected_end, last_size);
                        }
                    }
                }
            }
            NextWork::Op(AsyncOp::Seal {
                ticket,
                resized,
                generation,
            }) => {
                completed_ticket = Some(ticket);
                if let Some(sz) = resized {
                    last_size = sz;
                }
                let current_gen = store.generation.load(Ordering::Acquire);
                if let Some(r) = &mut rec {
                    if store.is_clearing() || generation != current_gen {
                        r.discard_for_clear(current_gen, 0, resized);
                    } else if panic::catch_unwind(AssertUnwindSafe(|| {
                        if let Some(sz) = resized {
                            r.resize(sz);
                        }
                        r.seal();
                    }))
                    .is_err()
                    {
                        tracing::warn!("pty seal panicked; continuing shutdown");
                        let expected_end = r.seq();
                        rec = recover_worker_recorder(rec.take(), expected_end, last_size);
                    }
                }
            }
        }

        let mut state = lock(&handle.mu);
        state.in_flight = false;
        state.idle_sealing = false;
        if let Some(ticket) = completed_ticket {
            state.completed_gen = state.completed_gen.max(ticket);
        }
        state.worker_open = rec.as_ref().is_some_and(|r| r.open.is_some());
        handle.cv.notify_all();
    }
}

fn recover_worker_recorder(
    recorder: Option<Recorder>,
    end: u64,
    size: WinSize,
) -> Option<Recorder> {
    let recorder = recorder?;
    let store = Arc::clone(&recorder.store);
    let id = recorder.id;
    match panic::catch_unwind(AssertUnwindSafe(|| recorder.recover_after_panic(end, size))) {
        Ok(Ok(recovered)) => Some(recovered),
        Ok(Err(error)) => {
            tracing::warn!(
                kind = ?error.kind(),
                "pty recorder recovery failed; keeping writer alive for automatic retry"
            );
            Some(Recorder::degraded_after_recovery_failure(
                store, id, end, size,
            ))
        }
        Err(_) => {
            tracing::warn!(
                "pty recorder recovery panicked; keeping writer alive for automatic retry"
            );
            Some(Recorder::degraded_after_recovery_failure(
                store, id, end, size,
            ))
        }
    }
}

#[cfg(test)]
pub(crate) mod write_hook {
    use super::*;

    type Hook = (SessionId, Arc<dyn Fn() + Send + Sync>);
    static BEFORE_WRITE: Mutex<Vec<Hook>> = Mutex::new(Vec::new());
    static BEFORE_SEAL: Mutex<Vec<Hook>> = Mutex::new(Vec::new());

    pub(crate) struct Guard {
        table: &'static Mutex<Vec<Hook>>,
        id: SessionId,
    }

    pub(crate) fn install(id: SessionId, hook: impl Fn() + Send + Sync + 'static) -> Guard {
        install_in(&BEFORE_WRITE, id, hook)
    }

    pub(crate) fn install_seal(id: SessionId, hook: impl Fn() + Send + Sync + 'static) -> Guard {
        install_in(&BEFORE_SEAL, id, hook)
    }

    fn install_in(
        table: &'static Mutex<Vec<Hook>>,
        id: SessionId,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Guard {
        lock(table).push((id, Arc::new(hook)));
        Guard { table, id }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            lock(self.table).retain(|(target, _)| *target != self.id);
        }
    }

    fn fire(table: &'static Mutex<Vec<Hook>>, id: SessionId) {
        let hook = lock(table)
            .iter()
            .find(|(target, _)| *target == id)
            .map(|(_, hook)| Arc::clone(hook));
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(super) fn before_write(id: SessionId) {
        fire(&BEFORE_WRITE, id);
    }

    pub(super) fn before_seal(id: SessionId) {
        fire(&BEFORE_SEAL, id);
    }
}

#[cfg(test)]
mod async_tests {
    use super::*;
    use crate::store::{Limits, Piece, Store};
    use std::sync::mpsc;

    fn tmp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("krill-rec-async-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn clear_discards_queued_async_writes_and_preserves_seq_alignment() {
        let root = tmp("clear");
        let store = Store::open(&root, Limits::default()).unwrap();
        let id = SessionId(9501);
        let size = WinSize { cols: 80, rows: 24 };
        let rec = store.recorder(id, size).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = write_hook::install(id, move || {
            if !once.swap(true, Ordering::SeqCst) {
                entered_tx.send(()).ok();
                lock(&release_rx).recv().ok();
            }
        });

        let mut async_rec = AsyncRecorder::start(rec, |_| {}).ok().unwrap();
        async_rec.record(0, b"secret-1", None, None);
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        async_rec.record(8, b"secret-2", None, None);
        assert!(async_rec.queued_bytes() >= 8);

        let clear_store = Arc::clone(&store);
        let (clear_tx, clear_rx) = mpsc::channel();
        let clearer = thread::spawn(move || {
            clear_store.clear().unwrap();
            clear_tx.send(()).unwrap();
        });
        assert!(clear_rx.recv_timeout(Duration::from_millis(80)).is_err());
        release_tx.send(()).unwrap();
        drop(hook);
        clear_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        clearer.join().unwrap();

        assert_eq!(async_rec.queued_bytes(), 0);
        assert_eq!(store.history(id).unwrap().len(), 0);

        async_rec.record(16, b"public-3", None, None);
        async_rec.finish(None);

        let mut pieces = Vec::new();
        store
            .raw(id, 0, 24, |piece| {
                pieces.push(piece);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            pieces,
            vec![
                Piece::Gap { seq: 0, len: 16 },
                Piece::Data {
                    seq: 16,
                    bytes: b"public-3".to_vec(),
                },
            ]
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn set_paused_true_quiesces_in_flight_writes_without_deadlock() {
        let root = tmp("pause");
        let store = Store::open(&root, Limits::default()).unwrap();
        let id = SessionId(9502);
        let size = WinSize { cols: 80, rows: 24 };
        let rec = store.recorder(id, size).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = write_hook::install(id, move || {
            if !once.swap(true, Ordering::SeqCst) {
                entered_tx.send(()).ok();
                lock(&release_rx).recv().ok();
            }
        });

        let mut async_rec = AsyncRecorder::start(rec, |_| {}).ok().unwrap();
        async_rec.record(0, b"before-pause", None, None);
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        async_rec.record(12, b"secret-paused", None, None);

        let pause_store = Arc::clone(&store);
        let (pause_tx, pause_rx) = mpsc::channel();
        let pauser = thread::spawn(move || {
            pause_store.set_paused(true);
            pause_tx.send(()).unwrap();
        });
        assert!(pause_rx.recv_timeout(Duration::from_millis(80)).is_err());
        release_tx.send(()).unwrap();
        drop(hook);
        pause_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        pauser.join().unwrap();

        let written_at_pause = store.written();
        store.set_paused(false);
        async_rec.record(25, b"after-resume", None, None);
        async_rec.finish(None);
        assert!(store.written() > written_at_pause);

        let mut pieces = Vec::new();
        store
            .raw(id, 0, 37, |piece| {
                pieces.push(piece);
                Ok(())
            })
            .unwrap();
        assert!(pieces.iter().all(|piece| match piece {
            Piece::Data { bytes, .. } => !bytes.windows(6).any(|w| w == b"secret"),
            Piece::Gap { .. } => true,
        }));
        assert!(
            pieces
                .iter()
                .any(|p| matches!(p, Piece::Gap { seq: 12, len: 13 }))
        );
        assert!(
            pieces.iter().any(|p| {
                matches!(p, Piece::Data { seq: 25, bytes } if bytes == b"after-resume")
            })
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tiny_burst_under_byte_budget_coalesces_without_dropping_gap() {
        let root = tmp("tiny-coalesce");
        let store = Store::open(&root, Limits::default()).unwrap();
        let id = SessionId(9503);
        let size = WinSize { cols: 80, rows: 24 };
        let rec = store.recorder(id, size).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = write_hook::install(id, move || {
            if !once.swap(true, Ordering::SeqCst) {
                entered_tx.send(()).ok();
                lock(&release_rx).recv().ok();
            }
        });

        let mut async_rec = AsyncRecorder::start(rec, |_| {}).ok().unwrap();
        async_rec.record(0, b"seed", None, None);
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let mut seq = 4u64;
        let mut expected = b"seed".to_vec();
        for i in 0..500u32 {
            let line = format!("line-{i:04}\n").into_bytes();
            async_rec.record(seq, &line, None, None);
            seq += line.len() as u64;
            expected.extend_from_slice(&line);
        }
        assert!(async_rec.queued_bytes() <= MAX_ASYNC_QUEUE_BYTES);
        assert!(
            lock(&async_rec.handle.mu).queue.len() < MAX_ASYNC_QUEUE_ITEMS,
            "tiny writes should coalesce instead of exhausting item slots"
        );

        release_tx.send(()).unwrap();
        drop(hook);
        async_rec.finish(None);

        let mut actual = Vec::new();
        store
            .raw(id, 0, seq, |piece| {
                match piece {
                    Piece::Data { bytes, .. } => actual.extend_from_slice(&bytes),
                    Piece::Gap { seq, len } => panic!("unexpected gap at {seq} len={len}"),
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(actual, expected);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn flush_waits_for_in_flight_idle_seal_without_new_ticket() {
        let root = tmp("idle-seal-flush");
        let store = Store::open(&root, Limits::default()).unwrap();
        let id = SessionId(9504);
        let size = WinSize { cols: 80, rows: 24 };
        let mut rec = store.recorder(id, size).unwrap();
        rec.append_at(
            b"before-idle",
            Instant::now()
                .checked_sub(IDLE_SEAL + Duration::from_secs(1))
                .unwrap(),
        );
        assert!(rec.idle_deadline().is_some_and(|at| at <= Instant::now()));

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = write_hook::install_seal(id, move || {
            if !once.swap(true, Ordering::SeqCst) {
                entered_tx.send(()).ok();
                lock(&release_rx).recv().ok();
            }
        });

        let async_rec = AsyncRecorder::start(rec, |_| {}).ok().unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let flush_store = Arc::clone(&store);
        let (flush_tx, flush_rx) = mpsc::channel();
        let flusher = thread::spawn(move || {
            let hist = flush_store.history(id).unwrap();
            flush_tx.send(hist).unwrap();
        });
        assert!(flush_rx.recv_timeout(Duration::from_millis(80)).is_err());

        release_tx.send(()).unwrap();
        drop(hook);
        let hist = flush_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        flusher.join().unwrap();
        assert_eq!(hist.len(), 1);
        let segs = segments(&store.session_dir(id)).unwrap();
        assert_eq!(segs.len(), 1);
        assert!(
            segment::read(segs.first().unwrap(), store.key.as_deref())
                .unwrap()
                .unwrap()
                .sealed
        );
        async_rec.finish(None);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn flush_cancelled_waits_for_idle_seal_and_honors_cancellation() {
        let root = tmp("idle-seal-cancel");
        let store = Store::open(&root, Limits::default()).unwrap();
        let id = SessionId(9505);
        let size = WinSize { cols: 80, rows: 24 };
        let mut rec = store.recorder(id, size).unwrap();
        rec.append_at(
            b"before-cancel",
            Instant::now()
                .checked_sub(IDLE_SEAL + Duration::from_secs(1))
                .unwrap(),
        );

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = write_hook::install_seal(id, move || {
            if !once.swap(true, Ordering::SeqCst) {
                entered_tx.send(()).ok();
                lock(&release_rx).recv().ok();
            }
        });

        let async_rec = AsyncRecorder::start(rec, |_| {}).ok().unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_flag = Arc::clone(&cancelled);
        let cancel_store = Arc::clone(&store);
        let canceller = thread::spawn(move || {
            cancel_store.flush_session_cancelled(id, &|| cancel_flag.load(Ordering::Acquire))
        });
        thread::sleep(Duration::from_millis(30));
        assert!(!canceller.is_finished());
        cancelled.store(true, Ordering::Release);
        let err = canceller.join().unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);

        release_tx.send(()).unwrap();
        drop(hook);
        store.flush_session_cancelled(id, &|| false).unwrap();
        let segs = segments(&store.session_dir(id)).unwrap();
        assert_eq!(segs.len(), 1);
        assert!(
            segment::read(segs.first().unwrap(), store.key.as_deref())
                .unwrap()
                .unwrap()
                .sealed
        );
        async_rec.finish(None);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clear_closes_open_segment_when_queue_is_empty_or_in_flight() {
        let root = tmp("clear-open-empty-queue");
        let store = Store::open(&root, Limits::default()).unwrap();
        let size = WinSize { cols: 80, rows: 24 };

        // 1. Empty queue with an open segment still waiting for IDLE_SEAL: clear must drop rec.open.
        let id1 = SessionId(9506);
        let rec1 = store.recorder(id1, size).unwrap();
        let mut async1 = AsyncRecorder::start(rec1, |_| {}).ok().unwrap();
        async1.record(0, b"open-before-clear", None, None);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = lock(&async1.handle.mu);
            if state.completed_gen >= 1 && !state.in_flight {
                assert!(state.queue.is_empty());
                assert!(state.worker_open);
                break;
            }
            drop(state);
            assert!(
                Instant::now() < deadline,
                "timed out waiting for open batch"
            );
            thread::sleep(Duration::from_millis(5));
        }
        store.clear().unwrap();
        assert!(!lock(&async1.handle.mu).worker_open);
        async1.record(17, b"after-clear", None, None);
        async1.finish(None);
        let mut pieces = Vec::new();
        store
            .raw(id1, 0, 28, |piece| {
                pieces.push(piece);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            pieces,
            vec![
                Piece::Gap { seq: 0, len: 17 },
                Piece::Data {
                    seq: 17,
                    bytes: b"after-clear".to_vec(),
                },
            ]
        );

        // 2. In-flight batch with an empty queue: clear must wait and close the segment opened by it.
        let id2 = SessionId(9507);
        let rec2 = store.recorder(id2, size).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = write_hook::install(id2, move || {
            if !once.swap(true, Ordering::SeqCst) {
                entered_tx.send(()).ok();
                lock(&release_rx).recv().ok();
            }
        });
        let mut async2 = AsyncRecorder::start(rec2, |_| {}).ok().unwrap();
        async2.record(0, b"in-flight-only", None, None);
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(lock(&async2.handle.mu).queue.is_empty());
        assert!(lock(&async2.handle.mu).in_flight);

        let clear_store = Arc::clone(&store);
        let (done_tx, done_rx) = mpsc::channel();
        let clearer = thread::spawn(move || {
            clear_store.clear().unwrap();
            done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(80)).is_err());
        release_tx.send(()).unwrap();
        drop(hook);
        done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        clearer.join().unwrap();
        assert!(!lock(&async2.handle.mu).worker_open);
        async2.finish(None);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn idle_seal_during_clear_discards_open_segment_without_spinning() {
        let root = tmp("idle-seal-clear");
        let store = Store::open(&root, Limits::default()).unwrap();
        let id = SessionId(9508);
        let size = WinSize { cols: 80, rows: 24 };
        let mut rec = store.recorder(id, size).unwrap();
        rec.append_at(
            b"stale-before-clear",
            Instant::now()
                .checked_sub(IDLE_SEAL + Duration::from_secs(1))
                .unwrap(),
        );
        assert!(rec.idle_deadline().is_some_and(|at| at <= Instant::now()));

        let clear_guard = store.begin_clear();
        let async_rec = AsyncRecorder::start(rec, |_| {}).ok().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = lock(&async_rec.handle.mu);
            if !state.worker_open && !state.in_flight && !state.idle_sealing {
                break;
            }
            drop(state);
            assert!(
                Instant::now() < deadline,
                "idle seal while clearing must discard open segment instead of spinning"
            );
            thread::sleep(Duration::from_millis(5));
        }
        drop(clear_guard);
        async_rec.finish(None);
        fs::remove_dir_all(&root).ok();
    }
}
