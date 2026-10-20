//! MK-119 가운데 에디터 탭 코어: 파일 열기·dirty·원자적 저장·디스크 외부 변경 상태기계. gpui에 의존하지 않는다.
//! MK-121: 열기는 [`file_format::classify`]로 뷰어(코드·미리보기·이미지·표·자리표시자)를 고른다.
//!
//! 동작 규칙은 Orca 에디터(`.ref/orca/src/renderer/src/components/editor/`: editor-content-dirty-state.ts,
//! editor-changed-on-disk-mark.ts, editor-external-change-tab-reset.ts)를 참고했다 — Copyright (c) 2026 Lovecast Inc.,
//! MIT License, .ref/orca/LICENSE. 코드는 옮기지 않았다.
//!
//! - dirty는 "편집 중 내용 ≠ 마지막으로 읽거나 저장한 내용"이다(되돌리기로 원래대로 돌아오면 깨끗해진다).
//! - 디스크가 바뀌면 깨끗한 탭은 다시 읽고, dirty 탭은 덮어쓰지 않고 표시만 한다(사용자가 재로드/유지 선택).
//! - 지워지면 표시한다. 저장하면 다시 생긴다.

pub mod quick_open;

use krill_proto::os::fs::OpenOptionsExt as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use crate::file_format::csv::{self, Delimiter, Table};
use crate::file_format::raster::{self, Dimensions, RasterError};
use crate::file_format::{self, ImageFormat, Kind};

/// 코드 에디터로 여는 파일 크기 상한. 앱 메모리 예산(≤ 120MB)을 지키기 위해 고른 값이다(측정값 아님 — 추정 상한).
// ponytail: 고정 상한. 더 큰 CSV는 앞부분 표만 보이고, 나머지는 자리표시자(기본 앱으로 열기)로 보낸다.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 분류에 쓰는 앞부분. NUL 검사([`file_format::BINARY_PROBE_BYTES`])보다 크게 잡아 JPEG처럼 크기 정보가
/// 메타데이터 뒤에 오는 래스터 헤더도 읽는다.
// ponytail: 256KiB 안에 SOF가 없는 JPEG(거대한 EXIF 썸네일)는 '크기를 읽을 수 없음'으로 보인다.
const HEAD_BYTES: u64 = 256 * 1024;

/// 디스크 파일의 변경 감지 지문. 내용 비교 없이 stat만으로 판단한다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            modified: meta.modified().ok(),
            len: meta.len(),
        }
    }
}

/// 지금 디스크의 지문. 파일이 없으면 `Ok(None)`.
pub fn stamp(path: &Path) -> io::Result<Option<Stamp>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(Stamp::of(&meta))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 열 수 없는 이유(자리표시자로도 보이지 않는 것).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unopenable {
    NotUtf8,
    Directory,
    /// FIFO·소켓·장치처럼 일반 파일이 아닌 것. FIFO는 `open`이 쓰는 쪽을 기다리며 멈추므로 열지 않는다.
    NotFile,
}

/// 파일을 연 결과 — 파일 탭이 어떤 화면을 띄울지 정한다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// 코드 에디터로 편집한다. `Kind`는 Text·Markdown·Csv·Ipynb·Image(Svg) 중 하나이고
    /// [`preview`]가 편집/미리보기 토글을 정한다.
    Text(Kind, String),
    /// 래스터 이미지(헤더 검사 결과). 픽셀은 뷰어(gpui `img`)가 경로에서 디코드한다.
    Image(Result<Dimensions, RasterError>),
    /// 텍스트로 열기엔 큰 CSV/TSV. 앞부분 표만 읽기 전용으로 보인다.
    Table(Table),
    /// 미리보기 없음(Media·Font·Pdf·Binary·TooLarge) — 자리표시자. 파일 길이를 함께 보인다.
    Placeholder(Kind, u64),
    Unopenable(Unopenable),
}

/// 텍스트 탭의 미리보기 화면. 있으면 탭 헤더에 편집/미리보기 토글이 생기고 미리보기로 시작한다
/// (Orca `getDefaultMarkdownViewMode`: 편집 모드 기본값이 'rich' — markdown·csv·notebook 공통).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Markdown,
    Table(Delimiter),
    Notebook,
    Svg,
}

/// 분류 → 미리보기. 일반 텍스트는 코드 에디터만 쓴다.
pub fn preview(kind: Kind) -> Option<Preview> {
    match kind {
        Kind::Markdown => Some(Preview::Markdown),
        Kind::Csv { delimiter } => Some(Preview::Table(delimiter)),
        Kind::Ipynb => Some(Preview::Notebook),
        Kind::Image(ImageFormat::Svg) => Some(Preview::Svg),
        _ => None,
    }
}

/// 코드 에디터 하이라이트 언어(gpui-component highlighter 이름). 켜지지 않은 문법은 하이라이트 없이 보인다.
pub fn language(kind: Kind, path: &Path) -> &'static str {
    match kind {
        Kind::Text { language } => language,
        Kind::Markdown => "markdown",
        Kind::Ipynb => "json",
        _ => file_format::language_for(path),
    }
}

/// 파일을 읽어 탭 화면을 정한다. **열기 경로의 유일한 분기 지점** — [`file_format::classify`]의 `Kind`로 나눈다.
pub fn open(path: &Path) -> io::Result<(Opened, Stamp)> {
    let meta = fs::metadata(path)?;
    let stamp = Stamp::of(&meta);
    if meta.is_dir() {
        return Ok((Opened::Unopenable(Unopenable::Directory), stamp));
    }
    if !meta.is_file() {
        return Ok((Opened::Unopenable(Unopenable::NotFile), stamp));
    }
    let len = meta.len();
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    (&mut file).take(HEAD_BYTES).read_to_end(&mut bytes)?;
    let kind = file_format::classify(path, &bytes, len);
    let opened = match kind {
        Kind::Image(ImageFormat::Raster) => Opened::Image(raster::check(&bytes)),
        kind if kind.is_text_editable() => {
            // 읽는 사이 파일이 커져도 상한 + 1바이트까지만 읽는다.
            let rest = (MAX_BYTES + 1).saturating_sub(byte_len(&bytes));
            file.take(rest).read_to_end(&mut bytes)?;
            let total = byte_len(&bytes);
            match kind {
                _ if total <= MAX_BYTES => String::from_utf8(bytes)
                    .map_or(Opened::Unopenable(Unopenable::NotUtf8), |text| {
                        Opened::Text(kind, text)
                    }),
                Kind::Csv { delimiter } => {
                    bytes.truncate(csv::PREVIEW_BYTES);
                    Opened::Table(csv::preview(&bytes, len.max(total), delimiter))
                }
                _ => Opened::Placeholder(Kind::TooLarge, len.max(total)),
            }
        }
        kind => Opened::Placeholder(kind, len),
    };
    Ok((opened, stamp))
}

fn byte_len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

/// 저장 실패.
#[derive(Debug)]
pub enum SaveError {
    /// 마지막으로 읽거나 저장한 뒤 디스크가 바뀌었다. 덮어쓰면 바깥 변경(에이전트 편집 등)을 잃는다.
    Conflict,
    Io(io::Error),
}

impl From<io::Error> for SaveError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// 같은 폴더의 임시 파일에 쓰고 fsync한 뒤 rename한다. 기존 파일의 권한을 유지하고, 심볼릭 링크는 따라가
/// 링크가 아니라 대상 파일을 바꾼다. 중간에 죽어도 이전 내용이나 새 내용 중 하나가 남는다.
///
/// `expected`는 마지막으로 읽거나 저장했을 때의 지문(`None` = 지워진 파일). rename 직전 디스크 지문이 다르면
/// [`SaveError::Conflict`]로 멈춘다. 쓰기 권한이 없는 파일은 rename으로 갈아끼우지 않고 권한 오류를 낸다.
// ponytail: 검사~rename 사이 틈(수 ms)은 남는다. 막으려면 파일 잠금 협조가 필요하다.
pub fn save_atomic(path: &Path, text: &str, expected: Option<Stamp>) -> Result<Stamp, SaveError> {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let meta = fs::metadata(&target).ok();
    if meta.as_ref().is_some_and(fs::Metadata::is_file) {
        // 잘라내지 않고 쓰기로만 열어 본다 — 읽기 전용 파일을 rename으로 덮어쓰지 않게.
        OpenOptions::new().write(true).open(&target)?;
    }
    let permissions = meta.map(|m| m.permissions());
    let tmp = dir.join(format!(
        ".{}.mk-save-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> Result<(), SaveError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        if stamp(&target)? != expected {
            return Err(SaveError::Conflict);
        }
        fs::rename(&tmp, &target)?;
        Ok(())
    })();
    if let Err(e) = written {
        fs::remove_file(&tmp).ok();
        return Err(e);
    }
    crate::layout::sync_parent(&target)?;
    Ok(stamp(&target)?.ok_or_else(|| io::Error::other("saved file disappeared"))?)
}

/// 디스크 쪽 상태(탭에 보이는 표시).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disk {
    /// 마지막으로 읽거나 저장한 그대로.
    Same,
    /// dirty인 동안 바깥에서 바뀌었다(재로드/유지 선택 대기).
    Changed,
    /// 바깥에서 지워졌다.
    Deleted,
}

/// 디스크 감시 한 번의 결과로 탭이 할 일.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    Nothing,
    /// 깨끗한 탭이라 디스크 내용을 다시 읽는다.
    Reload,
    /// 표시가 바뀌었다(다시 그린다).
    Marked,
}

/// 열린 텍스트 파일 하나의 상태.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    path: PathBuf,
    /// dirty 비교 기준: 마지막으로 읽거나 저장한 내용.
    saved: std::sync::Arc<str>,
    stamp: Option<Stamp>,
    dirty: bool,
    disk: Disk,
}

impl Doc {
    pub fn new(path: PathBuf, text: String, stamp: Stamp) -> Self {
        Self {
            path,
            saved: text.into(),
            stamp: Some(stamp),
            dirty: false,
            disk: Disk::Same,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn saved(&self) -> &str {
        &self.saved
    }

    pub fn saved_snapshot(&self) -> std::sync::Arc<str> {
        self.saved.clone()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn disk(&self) -> Disk {
        self.disk
    }

    /// 에디터가 처음 받은 내용을 기준으로 삼는다(에디터가 내용을 정규화해도 열자마자 dirty가 되지 않게).
    pub fn rebase(&mut self, text: String) {
        self.saved = text.into();
        self.dirty = false;
    }

    /// 편집 뒤 호출한다. `unchanged`는 지금 내용이 [`Self::saved`]와 같은가. dirty가 바뀌면 true.
    pub fn edited(&mut self, unchanged: bool) -> bool {
        let before = self.dirty;
        self.dirty = !unchanged;
        before != self.dirty
    }

    /// ⌘S. '디스크에서 변경됨'에 답하기 전이거나, 읽은 뒤 바깥에서 바뀌었으면 덮어쓰지 않는다(lost update 방지).
    /// 사용자가 "내 편집 유지"([`Self::keep_mine`])를 고른 뒤에는 저장한다.
    pub fn save(&mut self, text: String) -> Result<(), SaveError> {
        if self.disk == Disk::Changed {
            return Err(SaveError::Conflict);
        }
        let stamp = save_atomic(&self.path, &text, self.stamp)?;
        self.saved_as(text, stamp);
        Ok(())
    }

    /// 저장 성공. 저장한 내용이 새 기준이 되고, 디스크 표시는 지운다.
    pub fn saved_as(&mut self, text: String, stamp: Stamp) {
        self.saved = text.into();
        self.stamp = Some(stamp);
        self.dirty = false;
        self.disk = Disk::Same;
    }

    /// 디스크에서 다시 읽었다.
    pub fn reloaded(&mut self, text: String, stamp: Stamp) {
        self.saved_as(text, stamp);
    }

    /// 디스크 변경 표시를 보고 사용자가 "내 편집 유지"를 골랐다. 같은 변경으로 다시 묻지 않는다.
    pub fn keep_mine(&mut self, now: Option<Stamp>) {
        self.stamp = now;
        if self.disk == Disk::Changed {
            self.disk = Disk::Same;
        }
    }

    /// 디스크 감시 결과를 반영한다. `now`는 지금 디스크 지문(없으면 지워짐).
    pub fn poll(&mut self, now: Option<Stamp>) -> Poll {
        if now == self.stamp {
            return Poll::Nothing;
        }
        let Some(now) = now else {
            self.stamp = None;
            self.disk = Disk::Deleted;
            return Poll::Marked;
        };
        if self.dirty {
            self.stamp = Some(now);
            self.disk = Disk::Changed;
            Poll::Marked
        } else {
            Poll::Reload
        }
    }
}

/// 탭 제목: 파일 이름(없으면 경로 전체).
pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
#[cfg(unix)]
#[path = "editor/tests.rs"]
mod tests;
