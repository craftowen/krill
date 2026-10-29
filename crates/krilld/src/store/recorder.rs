//! 세션별 기록기. 쓰기 허용 read 잠금 → 개인정보 gate 순서로만 잠근다.
//! 압축·열린 파일 쓰기는 gate 밖에서 하고, 일시중지 ACK는 쓰기 read 잠금이 풀린 뒤 반환한다.

use krill_proto::os::fs::OpenOptionsExt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::Ordering};
use std::time::Instant;

use krill_proto::SessionId;
use krill_proto::holder::WinSize;

use super::crypto::{SALT_LEN, Stream};
use super::segment::{self, Base, HEADER_LEN, Header, Record};
use super::{LEVEL, OVERHEAD, RESUME, Store, WINDOW_LOG, lock, mkdir, privacy, segments};

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

#[derive(PartialEq, Eq)]
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

    /// 레코드들을 청크 하나로 쓴다. 세그먼트가 없으면 새로 연다. 실패하면 호출자가 [`Self::abandon`].
    fn write(&mut self, recs: &[Record], raw: u64, spent: &mut u64) -> io::Result<()> {
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
            let key = self.store.key.as_deref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "segment key unavailable")
            })?;
            let (crypto, salt) = Stream::create(key, &prefix)?;
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

    /// 다음에 열 세그먼트 자리(`create`와 같은 이름)에 원본이 하나도 없는 세그먼트만
    /// 치운다(K-02 복구). `Gap(0)` 크래시 표시·`Resize`·`Keyframe`만 든 세그먼트는 seq
    /// 길이가 0이라 지워도 원본을 잃지 않고, 다음 세그먼트 헤더가 최신 크기를 담는다.
    /// 데이터가 있거나 읽히지 않으면 `false`(그대로 오류)다.
    fn reclaim_empty_segment(&self) -> bool {
        let start = self.seq.saturating_sub(self.lost);
        let path = self.dir.join(format!("seg-{start:020}.krs"));
        let empty = segment::read(&path, self.store.key.as_deref())
            .ok()
            .flatten()
            .is_some_and(|seg| seg.end_seq() == seg.header.start_seq);
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
