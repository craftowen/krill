//! 세그먼트 파일 포맷 v1(평문)·v2(인증 암호문). 모든 정수는 리틀 엔디언.
//!
//! ```text
//! 헤더 32B : b"MKSG" | version u16 | base u8 | keyframe u8 | session u64 | start_seq u64 | cols u16 | rows u16 | crc32 u32(앞 28B)
//! 청크 반복: len u32 | crc32 u32(본문) | 본문 len B
//! ```
//!
//! - 청크 본문은 세그먼트당 zstd 스트림 인코더 하나에 레코드 하나를 넣고 flush한 출력이다(ARCHITECTURE §4.2).
//!   마지막 청크는 봉인할 때 프레임 끝이다. 봉인되지 않은 세그먼트(크래시)도 flush된 데까지 읽힌다.
//! - 청크 길이·체크섬이 처음 어긋나는 곳부터 뒤는 버린다(잘린 꼬리·손상 구간, CONVENTIONS P6). 스트림 중간이
//!   빠지면 뒤 청크는 풀 수 없으니 이어 읽지 않는다.
//! - 스트림을 풀면 레코드가 이어진다: `tag u8 | len u32 | body`.
//!   `1` Data(PTY 원본) · `2` Resize(cols u16, rows u16) · `3` Gap(기록하지 못한 원본 바이트 수 u64, 0이면 크래시로 길이 모름).
//! - 레코드에는 seq가 없다. 헤더 `start_seq` + 앞선 Data·Gap 길이의 합이다.
//! - `base`: 0 = 세션 시작(빈 Term), 1 = 앞 세그먼트에서 이어짐.
//! - `keyframe`: 0 = 구 포맷(워밍업), 1 = 첫 레코드에 ANSI 시작 화면(`4` Keyframe).
//!   키프레임도 zstd·청크 CRC·v2 인증 암호화 안에 둔다. 헤더는 나중에 다시 쓰지 않는다.

//!
//! v2는 동일 헤더의 version=2 뒤 32B 무작위 salt를 둔다. 마스터 키+salt로 HKDF-SHA256 세그먼트 키를 유도하고,
//! 각 zstd 청크 본문을 AES-256-GCM으로 감싼다(16B 태그 포함, crc는 암호문+태그에 적용).
//! nonce는 0u32 | 청크 순번 u64 BE, AAD는 32B 헤더 전체+32B salt다. 봉인 꼬리도 인증한다.
//! 키 없음·인증 실패는 IO 오류이며 파일을 지우거나 잘라 내지 않는다. 잘린 청크·CRC 손상 꼬리는 v1처럼 복구한다.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use super::crypto::{Key, SALT_LEN, Stream};
use krill_proto::SessionId;
use krill_proto::holder::WinSize;
use zstd::stream::raw::Operation as _;

fn decompress_stream(
    decoder: &mut zstd::stream::raw::Decoder<'static>,
    mut input: &[u8],
    out: &mut Vec<u8>,
) -> bool {
    if decoder.reinit().is_err() {
        return false;
    }
    let mut buf = [0u8; 8 * 1024];
    let mut remaining = usize::try_from(MAX_RAW.saturating_add(1)).unwrap_or(usize::MAX);
    let mut finished_frame = false;
    loop {
        let cap = buf.len().min(remaining);
        if cap == 0 {
            return false;
        }
        let Some(dst) = buf.get_mut(..cap) else {
            return false;
        };
        let Ok(status) = decoder.run_on_buffers(input, dst) else {
            return false;
        };
        let Some(rest) = input.get(status.bytes_read..) else {
            return false;
        };
        input = rest;
        let Some(written) = buf.get(..status.bytes_written) else {
            return false;
        };
        out.extend_from_slice(written);
        remaining = remaining.saturating_sub(status.bytes_written);
        if status.remaining == 0 {
            finished_frame = true;
            if input.is_empty() {
                return true;
            }
        }
        if status.bytes_read == 0 && status.bytes_written == 0 {
            return finished_frame && input.is_empty();
        }
    }
}

fn shared_decoder() -> &'static Mutex<Option<zstd::stream::raw::Decoder<'static>>> {
    static DECODER: OnceLock<Mutex<Option<zstd::stream::raw::Decoder<'static>>>> = OnceLock::new();
    DECODER.get_or_init(|| {
        let mut decoder = zstd::stream::raw::Decoder::new().ok();
        if let Some(d) = decoder.as_mut()
            && let Ok(sample) = zstd::encode_all(b"krill".as_slice(), 1)
        {
            let mut sink = Vec::with_capacity(8);
            decompress_stream(d, &sample, &mut sink);
            d.reinit().ok();
        }
        Mutex::new(decoder)
    })
}

pub(super) fn warm_decoder() {
    let _ = shared_decoder();
}

pub(crate) const MAGIC: [u8; 4] = *b"MKSG";
pub(crate) const VERSION: u16 = 1;
pub(crate) const HEADER_LEN: usize = 32;
pub const KEYFRAME_NONE: u8 = 0;
pub const KEYFRAME_V1: u8 = 1;
const CHUNK_HEAD: usize = 8;
/// 레코드 하나·세그먼트 하나를 풀 때의 상한. 봉인 크기(1MiB)보다 넉넉히 크고, 깨진 파일이 메모리를 키우지 못하게 한다.
const MAX_RAW: u64 = 16 << 20;
/// flush마다 붙는 zstd 블록 헤더를 포함한 압축 스트림 상한.
const MAX_STREAM: usize = 32 << 20;
/// 청크 헤더·인증 태그를 포함한 파일 상한. 빈 청크 반복도 유한하게 읽는다.
const MAX_FILE: u64 = 128 << 20;

#[cfg(all(test, unix))]
mod input_tests;
#[cfg(all(test, unix))]
mod limit_tests;

const TAG_DATA: u8 = 1;
const TAG_RESIZE: u8 = 2;
const TAG_GAP: u8 = 3;
const TAG_KEYFRAME: u8 = 4;

/// 세그먼트 시작 시점의 화면 상태를 어디서 얻는가.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Base {
    /// 세션 시작. 빈 Term에서 재생하면 정확하다.
    Start = 0,
    /// 앞 세그먼트 끝에서 이어진다. 키프레임이 없으면 앞 세그먼트를 재생해 근사한다.
    Continue = 1,
}

impl TryFrom<u8> for Base {
    type Error = ();
    fn try_from(v: u8) -> Result<Self, ()> {
        match v {
            0 => Ok(Self::Start),
            1 => Ok(Self::Continue),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub base: Base,
    pub encrypted: bool,
    pub session: SessionId,
    pub start_seq: u64,
    pub size: WinSize,
}

impl Header {
    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn encode(&self) -> [u8; HEADER_LEN] {
        self.encode_with_keyframe(KEYFRAME_NONE)
    }

    pub(crate) fn encode_with_keyframe(&self, keyframe: u8) -> [u8; HEADER_LEN] {
        let mut b = Vec::with_capacity(HEADER_LEN);
        b.extend_from_slice(&MAGIC);
        b.extend_from_slice(&if self.encrypted { 2_u16 } else { VERSION }.to_le_bytes());
        b.push(self.base as u8);
        b.push(keyframe);
        b.extend_from_slice(&self.session.0.to_le_bytes());
        b.extend_from_slice(&self.start_seq.to_le_bytes());
        b.extend_from_slice(&self.size.cols.to_le_bytes());
        b.extend_from_slice(&self.size.rows.to_le_bytes());
        b.extend_from_slice(&crc32fast::hash(&b).to_le_bytes());
        let mut out = [0; HEADER_LEN];
        out.copy_from_slice(&b);
        out
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn decode(b: &[u8]) -> Option<Self> {
        Self::decode_with_keyframe(b).map(|(h, _)| h)
    }

    pub(crate) fn decode_with_keyframe(b: &[u8]) -> Option<(Self, u8)> {
        let mut c = Cur(b);
        let body = b.get(..HEADER_LEN - 4)?;
        if c.take(4)? != MAGIC {
            return None;
        }
        let encrypted = match c.u16()? {
            VERSION => false,
            2 => true,
            _ => return None,
        };
        let base = Base::try_from(c.u8()?).ok()?;
        let keyframe = c.u8()?;
        if keyframe != KEYFRAME_NONE && keyframe != KEYFRAME_V1 {
            return None; // 모르는 키프레임 종류
        }
        let h = Self {
            base,
            encrypted,
            session: SessionId(c.u64()?),
            start_seq: c.u64()?,
            size: WinSize {
                cols: c.u16()?,
                rows: c.u16()?,
            },
        };
        (c.u32()? == crc32fast::hash(body)).then_some((h, keyframe))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Record {
    Data(Vec<u8>),
    Resize(WinSize),
    Gap(u64),
    Keyframe(Vec<u8>),
}

impl Record {
    /// 이 레코드가 차지하는 원본 seq 길이.
    pub(crate) fn seq_len(&self) -> u64 {
        match self {
            Self::Data(d) => d.len() as u64,
            Self::Resize(_) | Self::Keyframe(_) => 0,
            Self::Gap(n) => *n,
        }
    }

    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        let mut put = |tag: u8, body: &[u8]| {
            out.push(tag);
            // 레코드 본문은 PTY 묶음(≤ 64KiB)·고정 크기라 u32를 넘지 않는다.
            out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
            out.extend_from_slice(body);
        };
        match self {
            Self::Data(d) => put(TAG_DATA, d),
            Self::Resize(size) => {
                let mut body = size.cols.to_le_bytes().to_vec();
                body.extend_from_slice(&size.rows.to_le_bytes());
                put(TAG_RESIZE, &body);
            }
            Self::Gap(n) => put(TAG_GAP, &n.to_le_bytes()),
            Self::Keyframe(bytes) => put(TAG_KEYFRAME, bytes),
        }
    }
}

/// 풀린 레코드열. 덜 들어온 마지막 레코드나 모르는 태그에서 멈춘다.
fn decode_records(raw: &[u8]) -> Vec<Record> {
    let mut c = Cur(raw);
    let mut out = Vec::new();
    while let Some(rec) = (|| {
        let tag = c.u8()?;
        let len = usize::try_from(c.u32()?).ok()?;
        let mut body = Cur(c.take(len)?);
        Some(match tag {
            TAG_DATA => Record::Data(body.0.to_vec()),
            TAG_RESIZE => Record::Resize(WinSize {
                cols: body.u16()?,
                rows: body.u16()?,
            }),
            TAG_GAP => Record::Gap(body.u64()?),
            TAG_KEYFRAME => Record::Keyframe(body.0.to_vec()),
            _ => return None,
        })
    })() {
        out.push(rec);
    }
    out
}

/// 청크 하나(`len | crc | body`)를 만든다.
pub(crate) fn chunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNK_HEAD + body.len());
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(body).to_le_bytes());
    out.extend_from_slice(body);
    out
}

#[derive(Debug)]
pub(crate) struct Segment {
    pub header: Header,
    pub keyframe_bytes: Vec<u8>,
    pub records: Vec<Record>,
    /// 온전한 청크까지의 파일 길이. 그 뒤는 잘렸거나 손상됐다.
    pub valid_len: u64,
    /// zstd 프레임이 끝까지 있다(정상 봉인).
    pub sealed: bool,
}

impl Segment {
    pub(crate) fn end_seq(&self) -> u64 {
        self.records
            .iter()
            .fold(self.header.start_seq, |s, r| s.saturating_add(r.seq_len()))
    }
}

/// 세그먼트 파일을 읽는다. 헤더가 깨졌거나 모르는 버전이면 `Ok(None)`.
/// 크기 상한 초과는 오류이며, 복구 호출자가 원본을 잘라 내지 않게 한다.
pub(super) fn read(path: &Path, key: Option<&Key>) -> io::Result<Option<Segment>> {
    let file = fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_FILE {
        return Err(size_error());
    }
    read_from(io::BufReader::with_capacity(64 * 1024, file), file_len, key)
}

#[cfg(test)]
#[cfg(unix)]
pub(super) fn decode(bytes: &[u8], key: Option<&Key>) -> io::Result<Option<Segment>> {
    read_from(bytes, bytes.len() as u64, key)
}

fn size_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "segment encoded size exceeds limit",
    )
}

/// EOF로 잘린 꼬리와 실제 읽기 오류를 구분한다.
fn read_full(reader: &mut impl Read, bytes: &mut [u8]) -> io::Result<bool> {
    match reader.read_exact(bytes) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_from(reader: impl Read, file_len: u64, key: Option<&Key>) -> io::Result<Option<Segment>> {
    // 읽는 중 파일이 자라도 시작 시점의 범위 밖은 읽거나 할당하지 않는다.
    let mut reader = reader.take(file_len);
    let mut prefix = [0; HEADER_LEN];
    if !read_full(&mut reader, &mut prefix)? {
        return Ok(None);
    }
    let Some((header, keyframe)) = Header::decode_with_keyframe(&prefix) else {
        return Ok(None);
    };
    let mut crypto = if header.encrypted {
        let key = key.ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, "segment key unavailable")
        })?;
        let mut salt = [0; SALT_LEN];
        if !read_full(&mut reader, &mut salt)? {
            return Ok(None);
        }
        Some(Stream::open(key, &prefix, &salt)?)
    } else {
        None
    };
    let mut stream = Vec::new();
    let mut valid_len = (HEADER_LEN + if header.encrypted { SALT_LEN } else { 0 }) as u64;
    let mut meta = [0; CHUNK_HEAD];
    while read_full(&mut reader, &mut meta)? {
        let mut meta = Cur(&meta);
        let Some((len, crc)) = meta.u32().zip(meta.u32()) else {
            break;
        };
        let Ok(len) = usize::try_from(len) else {
            break;
        };
        // 인증 태그도 할당 전에 검사하고, 복호화 뒤에는 버퍼에서 제거한다.
        let start = stream.len();
        let end = start.checked_add(len).ok_or_else(size_error)?;
        let next_len = valid_len
            .checked_add(CHUNK_HEAD as u64)
            .and_then(|size| size.checked_add(len as u64))
            .ok_or_else(size_error)?;
        // 파일 범위 밖을 가리키는 길이는 잘린 꼬리다. 큰 할당 없이 앞 청크를 복구한다.
        if next_len > file_len {
            break;
        }
        if end > MAX_STREAM || next_len > MAX_FILE {
            return Err(size_error());
        }
        if end > stream.capacity() {
            let capacity = stream.capacity().saturating_mul(2).max(end).min(MAX_STREAM);
            stream
                .try_reserve_exact(capacity - start)
                .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
        }
        stream.resize(end, 0);
        let body = stream.get_mut(start..end).ok_or_else(size_error)?;
        if !read_full(&mut reader, body)? {
            stream.truncate(start);
            break;
        }
        if crc32fast::hash(body) != crc {
            stream.truncate(start);
            break;
        }
        if let Some(crypto) = &mut crypto {
            let decrypted_len = crypto.decrypt(body)?.len();
            stream.truncate(start + decrypted_len);
        }
        valid_len = next_len;
    }
    drop(crypto);
    let mut raw = Vec::new();
    let mut guard = crate::session::lock(shared_decoder());
    if guard.is_none() {
        *guard = Some(zstd::stream::raw::Decoder::new()?);
    }
    // 프레임이 끝나지 않았으면(봉인 전 크래시) 풀 수 있는 데까지 담고 미봉인으로 표시한다.
    // 상한 초과 시 끝 seq를 신뢰할 수 없어 복구를 거부한다.
    let finished = guard
        .as_mut()
        .is_some_and(|d| decompress_stream(d, &stream, &mut raw));
    if let Some(d) = guard.as_mut() {
        d.reinit().ok();
    }
    drop(guard);
    let sealed = finished && !stream.is_empty();
    drop(stream);
    crate::sys::release_malloc_cache();
    if raw.len() as u64 > MAX_RAW {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "segment decompressed size exceeds limit",
        ));
    }
    let mut records = decode_records(&raw);
    drop(raw);
    let keyframe_bytes =
        if keyframe == KEYFRAME_V1 && matches!(records.first(), Some(Record::Keyframe(_))) {
            match records.remove(0) {
                Record::Keyframe(bytes) => bytes,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
    Ok(Some(Segment {
        header,
        keyframe_bytes,
        records,
        valid_len,
        sealed,
    }))
}

/// 바이트 커서. 모자라면 `None`(panic 없음).
struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (a, b) = self.0.split_at_checked(n)?;
        self.0 = b;
        Some(a)
    }

    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u8(&mut self) -> Option<u8> {
        self.arr::<1>().map(|[b]| b)
    }

    fn u16(&mut self) -> Option<u16> {
        self.arr().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.arr().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.arr().map(u64::from_le_bytes)
    }
}

/// 키가 없으면 이전 암호문을 지우거나 새 키로 덮어쓰지 않기 위한 기동 검사.
pub(super) fn has_encrypted(sessions: &Path) -> io::Result<bool> {
    let dirs = match fs::read_dir(sessions) {
        Ok(dirs) => dirs,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    for dir in dirs {
        let dir = dir?;
        // 세션 디렉터리가 아닌 파일(Finder의 `.DS_Store` 등)은 기록과 무관하다.
        // 그냥 열면 ENOTDIR이 `Store::open`까지 전파돼 데몬 전체 기록이 꺼진다(K-07).
        if !dir.file_type()?.is_dir() {
            continue;
        }
        for path in super::segments(&dir.path())? {
            let mut prefix = [0; 6];
            if fs::File::open(path)?.read_exact(&mut prefix).is_ok() && prefix == *b"MKSG\x02\x00" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
