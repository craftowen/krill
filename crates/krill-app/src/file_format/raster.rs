//! 래스터 이미지 헤더에서 가로·세로만 읽어 디코드 전에 크기 한도를 검사한다(압축 폭탄 방지).
//! 한도의 출처는 Orca `raster-image-preview-limits.ts`.

/// 한 변 상한. Orca `MAX_RASTER_IMAGE_PREVIEW_DIMENSION_PX`와 같다.
pub const MAX_DIMENSION: u32 = 32_768;
/// 픽셀 수 상한. Orca는 32Mi px(RGBA 128MiB)이지만 앱 메모리 예산(≤ 120MB)을 넘는다.
/// 8Mi px = 8,388,608 ≥ 4K UHD 3840×2160(8,294,400)이라 4K 한 장이 들어가고, RGBA 디코드 버퍼는 32MiB다.
pub const MAX_PIXELS: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterError {
    /// 헤더를 읽지 못했다(모르는 형식·잘린 파일).
    Invalid,
    TooLarge(Dimensions),
}

/// 헤더를 읽고 한도를 검사한다.
pub fn check(bytes: &[u8]) -> Result<Dimensions, RasterError> {
    let dims = dimensions(bytes)
        .filter(|d| d.width > 0 && d.height > 0)
        .ok_or(RasterError::Invalid)?;
    if dims.width > MAX_DIMENSION
        || dims.height > MAX_DIMENSION
        || u64::from(dims.width) * u64::from(dims.height) > MAX_PIXELS
    {
        return Err(RasterError::TooLarge(dims));
    }
    Ok(dims)
}

/// PNG·GIF·BMP·WebP·JPEG·ICO 헤더의 크기. 디코드하지 않는다.
pub fn dimensions(b: &[u8]) -> Option<Dimensions> {
    let dims = |width, height| Some(Dimensions { width, height });
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return dims(be32(b, 16)?, be32(b, 20)?);
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return dims(le16(b, 6)?.into(), le16(b, 8)?.into());
    }
    if b.starts_with(b"BM") {
        // BITMAPCOREHEADER(12바이트)는 16비트, 그 밖의 정보 헤더는 부호 있는 32비트(음수 높이 = 위에서 아래로).
        return if le32(b, 14)? == 12 {
            dims(le16(b, 18)?.into(), le16(b, 20)?.into())
        } else {
            let w = i32::from_le_bytes(le32(b, 18)?.to_le_bytes());
            let h = i32::from_le_bytes(le32(b, 22)?.to_le_bytes());
            dims(w.unsigned_abs(), h.unsigned_abs())
        };
    }
    if b.starts_with(b"\0\0\x01\0") {
        // ICO 첫 디렉터리 항목. 0은 256px.
        let side = |i| b.get(i).map(|&v| if v == 0 { 256 } else { u32::from(v) });
        return dims(side(6)?, side(7)?);
    }
    if b.get(0..4) == Some(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return webp(b);
    }
    if b.starts_with(&[0xff, 0xd8]) {
        return jpeg(b);
    }
    None
}

fn webp(b: &[u8]) -> Option<Dimensions> {
    let dims = |width, height| Some(Dimensions { width, height });
    match b.get(12..16)? {
        // 손실: 키프레임 시작 코드(9d 01 2a) 뒤 14비트 가로·세로.
        b"VP8 " => dims(
            u32::from(le16(b, 26)? & 0x3fff),
            u32::from(le16(b, 28)? & 0x3fff),
        ),
        // 무손실: 서명(0x2f) 뒤 14비트씩 (값 - 1).
        b"VP8L" => {
            let bits = le32(b, 21)?;
            dims((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        // 확장: 24비트씩 (값 - 1).
        b"VP8X" => dims(le24(b, 24)? + 1, le24(b, 27)? + 1),
        _ => None,
    }
}

/// SOF 마커(C0~CF, C4·C8·CC 제외)를 찾을 때까지 세그먼트를 건너뛴다.
fn jpeg(b: &[u8]) -> Option<Dimensions> {
    let mut i = 2usize;
    loop {
        if *b.get(i)? != 0xff {
            return None;
        }
        let marker = *b.get(i + 1)?;
        match marker {
            0xff => i += 1, // 채움 바이트
            0x01 | 0xd0..=0xd9 => i += 2,
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                return Some(Dimensions {
                    width: be16(b, i + 7)?.into(),
                    height: be16(b, i + 5)?.into(),
                });
            }
            _ => i = i.checked_add(2 + usize::from(be16(b, i + 2)?))?,
        }
    }
}

fn bytes<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    bytes(b, at).map(u16::from_be_bytes)
}
fn le16(b: &[u8], at: usize) -> Option<u16> {
    bytes(b, at).map(u16::from_le_bytes)
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    bytes(b, at).map(u32::from_be_bytes)
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    bytes(b, at).map(u32::from_le_bytes)
}
fn le24(b: &[u8], at: usize) -> Option<u32> {
    let [x, y, z] = bytes(b, at)?;
    Some(u32::from_le_bytes([x, y, z, 0]))
}
