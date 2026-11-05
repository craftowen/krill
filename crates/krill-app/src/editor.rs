//! MK-119 가운데 파일 탭 코어: 파일 열기와 디스크 외부 변경 감지. 파일 탭은 읽기 전용이고 편집은
//! "Open in"(외부 에디터)으로 한다. gpui에 의존하지 않는다.
//! MK-121: 열기는 [`file_format::classify`]로 뷰어(코드·미리보기·이미지·표·자리표시자)를 고른다.
//!
//! 동작 규칙은 Orca 에디터(`.ref/orca/src/renderer/src/components/editor/`: editor-changed-on-disk-mark.ts,
//! editor-external-change-tab-reset.ts)를 참고했다 — Copyright (c) 2026 Lovecast Inc., MIT License,
//! .ref/orca/LICENSE. 코드는 옮기지 않았다.
//!
//! - 디스크가 바뀌면 다시 읽는다. 지워지면 표시하고, 다시 생기면 다시 읽는다.

pub mod quick_open;
pub mod syntax;

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
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

    #[must_use]
    pub fn from_wire(w: krill_proto::remote_fs::WireStamp) -> Self {
        let modified = w
            .modified_unix_ns
            .map(|ns| std::time::UNIX_EPOCH + std::time::Duration::from_nanos(ns));
        Self {
            modified,
            len: w.len,
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
    /// 코드 보기(읽기 전용)로 연다. `Kind`는 Text·Markdown·Csv·Ipynb·Image(Svg) 중 하나이고
    /// [`preview`]가 원본/미리보기 토글을 정한다.
    Text(Kind, String),
    /// 래스터 이미지(헤더 검사 결과). 픽셀은 뷰어(gpui `img`)가 경로에서 디코드한다.
    Image(Result<Dimensions, RasterError>),
    /// 텍스트로 열기엔 큰 CSV/TSV. 앞부분 표만 읽기 전용으로 보인다.
    Table(Table),
    /// 미리보기 없음(Media·Font·Pdf·Binary·TooLarge) — 자리표시자. 파일 길이를 함께 보인다.
    Placeholder(Kind, u64),
    Unopenable(Unopenable),
}

/// 텍스트 탭의 미리보기 화면. 있으면 탭 헤더에 원본/미리보기 토글이 생기고 미리보기로 시작한다
/// (Orca `getDefaultMarkdownViewMode`: 기본값이 'rich' — markdown·csv·notebook·html 공통).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Markdown,
    Table(Delimiter),
    Notebook,
    Svg,
    Html,
}

/// 분류 → 미리보기. 일반 텍스트는 코드 보기만 쓴다.
pub fn preview(kind: Kind) -> Option<Preview> {
    match kind {
        Kind::Markdown => Some(Preview::Markdown),
        Kind::Csv { delimiter } => Some(Preview::Table(delimiter)),
        Kind::Ipynb => Some(Preview::Notebook),
        Kind::Image(ImageFormat::Svg) => Some(Preview::Svg),
        Kind::Text { language: "html" } => Some(Preview::Html),
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

/// 디스크 쪽 상태(탭에 보이는 표시).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disk {
    /// 마지막으로 읽은 그대로.
    Same,
    /// 바깥에서 지워졌다.
    Deleted,
}

/// 디스크 감시 한 번의 결과로 탭이 할 일.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    Nothing,
    /// 디스크 내용을 다시 읽는다.
    Reload,
    /// 표시가 바뀌었다(다시 그린다).
    Marked,
}

/// 열린 파일 하나의 디스크 상태. 파일 탭은 읽기 전용이라 내용은 에디터만 들고 있다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    path: PathBuf,
    stamp: Option<Stamp>,
    disk: Disk,
}

impl Doc {
    pub fn new(path: PathBuf, stamp: Stamp) -> Self {
        Self {
            path,
            stamp: Some(stamp),
            disk: Disk::Same,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn disk(&self) -> Disk {
        self.disk
    }

    /// 디스크 감시 결과를 반영한다. `now`는 지금 디스크 지문(없으면 지워짐).
    pub fn poll(&mut self, now: Option<Stamp>) -> Poll {
        if now == self.stamp {
            return Poll::Nothing;
        }
        if now.is_none() {
            self.stamp = None;
            self.disk = Disk::Deleted;
            return Poll::Marked;
        }
        Poll::Reload
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
