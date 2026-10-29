// Krill modification (Apache-2.0 §4(b)): decode Windows DIB/V5 screenshots to PNG
// and preserve embedded bitfield masks when constructing the BMP header.
use std::{borrow::Cow, sync::LazyLock};

use anyhow::Result;
use collections::FxHashMap;
use itertools::Itertools;
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats,
            GetClipboardData, GetClipboardFormatNameW, OpenClipboard, RegisterClipboardFormatW,
            SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT},
    },
    UI::Shell::{DragQueryFileW, HDROP},
};
use windows::core::{Owned, PCWSTR};

use gpui::{
    ClipboardEntry, ClipboardItem, ClipboardString, ExternalPaths, Image, ImageFormat, hash,
};

const DRAGDROP_GET_FILES_COUNT: u32 = 0xFFFFFFFF;
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

static CLIPBOARD_HASH_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GPUI internal text hash")));
static CLIPBOARD_METADATA_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GPUI internal metadata")));
static CLIPBOARD_SVG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("image/svg+xml")));
static CLIPBOARD_GIF_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GIF")));
static CLIPBOARD_PNG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("PNG")));
static CLIPBOARD_JPG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("JFIF")));

static IMAGE_FORMATS_MAP: LazyLock<FxHashMap<u32, ImageFormat>> = LazyLock::new(|| {
    let mut map = FxHashMap::default();
    map.insert(*CLIPBOARD_PNG_FORMAT, ImageFormat::Png);
    map.insert(*CLIPBOARD_GIF_FORMAT, ImageFormat::Gif);
    map.insert(*CLIPBOARD_JPG_FORMAT, ImageFormat::Jpeg);
    map.insert(*CLIPBOARD_SVG_FORMAT, ImageFormat::Svg);
    map
});

fn register_clipboard_format(format: PCWSTR) -> u32 {
    let ret = unsafe { RegisterClipboardFormatW(format) };
    if ret == 0 {
        panic!(
            "Error when registering clipboard format: {}",
            std::io::Error::last_os_error()
        );
    }
    log::debug!(
        "Registered clipboard format {} as {}",
        unsafe { format.display() },
        ret
    );
    ret
}

fn get_clipboard_data(format: u32) -> Option<LockedGlobal> {
    let global = HGLOBAL(unsafe { GetClipboardData(format).ok() }?.0);
    LockedGlobal::lock(global)
}

pub(crate) fn write_to_clipboard(item: ClipboardItem) {
    let Some(_clip) = ClipboardGuard::open() else {
        return;
    };

    let result: Result<()> = (|| {
        unsafe { EmptyClipboard()? };
        for entry in item.entries() {
            match entry {
                ClipboardEntry::String(string) => write_string(string)?,
                ClipboardEntry::Image(image) => write_image(image)?,
                ClipboardEntry::ExternalPaths(_) => {}
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        log::error!("Failed to write to clipboard: {e}");
    }
}

pub(crate) fn read_from_clipboard() -> Option<ClipboardItem> {
    let _clip = ClipboardGuard::open()?;

    let mut entries = Vec::new();
    let mut have_text = false;
    let mut images = Vec::new();
    let mut have_files = false;

    let count = unsafe { CountClipboardFormats() };
    let mut format = 0;
    for _ in 0..count {
        format = unsafe { EnumClipboardFormats(format) };

        if !have_text && format == CF_UNICODETEXT.0 as u32 {
            if let Some(entry) = read_string() {
                entries.push(entry);
                have_text = true;
            }
        } else if is_image_format(format) {
            images.push(format);
        } else if !have_files && format == CF_HDROP.0 as u32 {
            if let Some(entry) = read_files() {
                entries.push(entry);
                have_files = true;
            }
        }
    }

    if entries.is_empty() && images.is_empty() {
        log_unsupported_clipboard_formats();
        return None;
    }
    images.sort_by_key(|format| {
        if *format == *CLIPBOARD_PNG_FORMAT {
            0
        } else if IMAGE_FORMATS_MAP.contains_key(format) {
            1
        } else if is_dib_format(*format) {
            // Stable sorting preserves Windows' native-before-synthesized order.
            // A synthesized V5 can retain the classic DIB's external masks;
            // preferring it over the original DIB shifts the decoded pixels.
            2
        } else {
            3
        }
    });
    // Prefer explicit formats over Windows' synthesized DIB copies. Bound the
    // total copied data, then decode outside the system clipboard lock.
    let mut remaining = MAX_IMAGE_BYTES;
    let images: Vec<_> = images
        .into_iter()
        .filter_map(|format| {
            let locked = get_clipboard_data(format)?;
            let size = locked.as_bytes().len();
            if size == 0 || size > remaining {
                return None;
            }
            remaining -= size;
            Some((format, locked.as_bytes().to_vec()))
        })
        .collect();
    drop(_clip);
    if let Some(entry) = images
        .into_iter()
        .find_map(|(format, bytes)| read_image(format, bytes))
    {
        entries.push(entry);
    }
    if entries.is_empty() {
        return None;
    }
    Some(ClipboardItem { entries })
}

pub(crate) fn with_file_names<F>(hdrop: HDROP, mut f: F)
where
    F: FnMut(String),
{
    let file_count = unsafe { DragQueryFileW(hdrop, DRAGDROP_GET_FILES_COUNT, None) };
    for file_index in 0..file_count {
        let filename_length = unsafe { DragQueryFileW(hdrop, file_index, None) } as usize;
        let mut buffer = vec![0u16; filename_length + 1];
        let ret = unsafe { DragQueryFileW(hdrop, file_index, Some(buffer.as_mut_slice())) };
        if ret == 0 {
            log::error!("unable to read file name of dragged file");
            continue;
        }
        match String::from_utf16(&buffer[0..filename_length]) {
            Ok(file_name) => f(file_name),
            Err(e) => log::error!("dragged file name is not UTF-16: {}", e),
        }
    }
}

fn set_clipboard_bytes<T>(data: &[T], format: u32) -> Result<()> {
    unsafe {
        let global = Owned::new(GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(data))?);
        let ptr = GlobalLock(*global);
        anyhow::ensure!(!ptr.is_null(), "GlobalLock returned null");
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as _, data.len());
        GlobalUnlock(*global).ok();
        SetClipboardData(format, Some(HANDLE(global.0)))?;
        // SetClipboardData succeeded — the system now owns the memory.
        std::mem::forget(global);
    }
    Ok(())
}

fn get_clipboard_string(format: u32) -> Option<String> {
    let locked = get_clipboard_data(format)?;
    let bytes = locked.as_bytes();
    let words_len = bytes.len() / std::mem::size_of::<u16>();
    if words_len == 0 {
        return Some(String::new());
    }
    let slice = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u16, words_len) };
    let actual_len = slice.iter().position(|&c| c == 0).unwrap_or(words_len);
    Some(String::from_utf16_lossy(&slice[..actual_len]))
}

fn is_image_format(format: u32) -> bool {
    IMAGE_FORMATS_MAP.contains_key(&format) || is_dib_format(format)
}

fn is_dib_format(format: u32) -> bool {
    format == CF_DIB.0 as u32 || format == CF_DIBV5.0 as u32
}

fn write_string(item: &ClipboardString) -> Result<()> {
    // CF_UNICODETEXT is null-terminated, so replace embedded NUL characters with spaces.
    let text = if item.text.contains('\0') {
        Cow::Owned(item.text.replace('\0', " "))
    } else {
        Cow::Borrowed(item.text.as_str())
    };
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect_vec();
    set_clipboard_bytes(&wide, CF_UNICODETEXT.0 as u32)?;

    if let Some(metadata) = item.metadata.as_ref() {
        let hash_bytes = ClipboardString::text_hash(&text).to_ne_bytes();
        set_clipboard_bytes(&hash_bytes, *CLIPBOARD_HASH_FORMAT)?;

        let wide: Vec<u16> = metadata.encode_utf16().chain(Some(0)).collect_vec();
        set_clipboard_bytes(&wide, *CLIPBOARD_METADATA_FORMAT)?;
    }
    Ok(())
}

fn write_image(item: &Image) -> Result<()> {
    let native_format = match item.format {
        ImageFormat::Svg => Some(*CLIPBOARD_SVG_FORMAT),
        ImageFormat::Gif => Some(*CLIPBOARD_GIF_FORMAT),
        ImageFormat::Png => Some(*CLIPBOARD_PNG_FORMAT),
        ImageFormat::Jpeg => Some(*CLIPBOARD_JPG_FORMAT),
        _ => None,
    };
    if let Some(format) = native_format {
        set_clipboard_bytes(item.bytes(), format)?;
    }

    // Also provide a PNG copy for broad compatibility.
    // SVG can't be rasterized by the image crate, so skip it.
    if item.format != ImageFormat::Svg && native_format != Some(*CLIPBOARD_PNG_FORMAT) {
        if let Some(png_bytes) = convert_to_png(item.bytes(), item.format) {
            set_clipboard_bytes(&png_bytes, *CLIPBOARD_PNG_FORMAT)?;
        }
    }
    Ok(())
}

fn convert_to_png(bytes: &[u8], format: ImageFormat) -> Option<Vec<u8>> {
    let img_format = gpui_to_image_format(format)?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), img_format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_IMAGE_BYTES as u64);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| log::warn!("Failed to decode image for PNG conversion: {e}"))
        .ok()?;
    let mut buf = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| log::warn!("Failed to encode PNG: {e}"))
        .ok()?;
    Some(buf)
}

fn read_string() -> Option<ClipboardEntry> {
    let text = get_clipboard_string(CF_UNICODETEXT.0 as u32)?;
    let metadata = read_clipboard_metadata(&text);
    Some(ClipboardEntry::String(ClipboardString { text, metadata }))
}

fn read_clipboard_metadata(text: &str) -> Option<String> {
    let locked = get_clipboard_data(*CLIPBOARD_HASH_FORMAT)?;
    let hash_bytes: [u8; 8] = locked.as_bytes().get(..8)?.try_into().ok()?;
    let hash = u64::from_ne_bytes(hash_bytes);
    if hash != ClipboardString::text_hash(text) {
        return None;
    }
    get_clipboard_string(*CLIPBOARD_METADATA_FORMAT)
}

fn read_image(format: u32, bytes: Vec<u8>) -> Option<ClipboardEntry> {
    let (bytes, image_format) = if is_dib_format(format) {
        // Agent image attachments accept PNG, while native screenshots are DIBs.
        let bmp = convert_dib_to_bmp(&bytes)?;
        (convert_to_png(&bmp, ImageFormat::Bmp)?, ImageFormat::Png)
    } else {
        let image_format = *IMAGE_FORMATS_MAP.get(&format)?;
        (bytes, image_format)
    };
    let id = hash(&bytes);
    Some(ClipboardEntry::Image(Image {
        format: image_format,
        bytes,
        id,
    }))
}

fn read_files() -> Option<ClipboardEntry> {
    let locked = get_clipboard_data(CF_HDROP.0 as u32)?;
    let hdrop = HDROP(locked.ptr as *mut _);
    let mut filenames = Vec::new();
    with_file_names(hdrop, |name| filenames.push(std::path::PathBuf::from(name)));
    Some(ClipboardEntry::ExternalPaths(ExternalPaths(
        filenames.into(),
    )))
}

/// DIB is BMP without the 14-byte BITMAPFILEHEADER. Prepend one.
fn convert_dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }

    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?);
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);

    if !matches!(header_size, 40 | 52 | 56 | 108 | 124)
        || usize::try_from(header_size).ok()? > dib.len()
        || !matches!(bit_count, 1 | 4 | 8 | 16 | 24 | 32)
    {
        return None;
    }
    let colors_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);
    let colors = if bit_count <= 8 && colors_used == 0 {
        1u32 << bit_count
    } else {
        colors_used
    };
    if bit_count <= 8 && colors > 1u32 << bit_count {
        return None;
    }
    // BITMAPINFOHEADER stores BI_BITFIELDS masks after its 40-byte header;
    // V2/V3/V4/V5 include them in the declared header size already.
    let external_masks = if header_size == 40 && compression == 3 {
        12
    } else if header_size == 40 && compression == 6 {
        16 // BI_ALPHABITFIELDS
    } else {
        0
    };
    let data_offset = header_size
        .checked_add(external_masks)?
        .checked_add(colors.checked_mul(4)?)?;
    if usize::try_from(data_offset).ok()? >= dib.len() {
        return None;
    }
    let pixel_offset = 14u32.checked_add(data_offset)?;
    let file_size = 14u32.checked_add(u32::try_from(dib.len()).ok()?)?;

    let mut bmp = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]); // reserved
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

fn log_unsupported_clipboard_formats() {
    let count = unsafe { CountClipboardFormats() };
    let mut format = 0;
    for _ in 0..count {
        format = unsafe { EnumClipboardFormats(format) };
        let mut buffer = [0u16; 64];
        unsafe { GetClipboardFormatNameW(format, &mut buffer) };
        let format_name = String::from_utf16_lossy(&buffer);
        log::warn!(
            "Try to paste with unsupported clipboard format: {}, {}.",
            format,
            format_name
        );
    }
}

fn gpui_to_image_format(value: ImageFormat) -> Option<image::ImageFormat> {
    match value {
        ImageFormat::Png => Some(image::ImageFormat::Png),
        ImageFormat::Jpeg => Some(image::ImageFormat::Jpeg),
        ImageFormat::Webp => Some(image::ImageFormat::WebP),
        ImageFormat::Gif => Some(image::ImageFormat::Gif),
        ImageFormat::Bmp => Some(image::ImageFormat::Bmp),
        ImageFormat::Tiff => Some(image::ImageFormat::Tiff),
        other => {
            log::warn!("No image crate equivalent for format: {other:?}");
            None
        }
    }
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Option<Self> {
        match unsafe { OpenClipboard(None) } {
            Ok(()) => Some(Self),
            Err(e) => {
                log::error!("Failed to open clipboard: {e}");
                None
            }
        }
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        if let Err(e) = unsafe { CloseClipboard() } {
            log::error!("Failed to close clipboard: {e}");
        }
    }
}

struct LockedGlobal {
    global: HGLOBAL,
    ptr: *const u8,
    size: usize,
}

impl LockedGlobal {
    fn lock(global: HGLOBAL) -> Option<Self> {
        let size = unsafe { GlobalSize(global) };
        let ptr = unsafe { GlobalLock(global) };
        if ptr.is_null() {
            return None;
        }
        Some(Self {
            global,
            ptr: ptr as *const u8,
            size,
        })
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
    }
}

impl Drop for LockedGlobal {
    fn drop(&mut self) {
        unsafe { GlobalUnlock(self.global).ok() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dib(header: usize, bits: u16, compression: u32, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; header];
        bytes[..4].copy_from_slice(&(header as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&1i32.to_le_bytes());
        bytes[8..12].copy_from_slice(&1i32.to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&bits.to_le_bytes());
        bytes[16..20].copy_from_slice(&compression.to_le_bytes());
        bytes.extend_from_slice(pixels);
        bytes
    }

    fn png_pixel(dib: &[u8]) -> [u8; 4] {
        let bmp = convert_dib_to_bmp(dib).expect("valid DIB");
        let png = convert_to_png(&bmp, ImageFormat::Bmp).expect("PNG conversion");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        image::load_from_memory(&png)
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)
            .0
    }

    #[test]
    fn native_screenshot_is_a_png_with_the_same_pixel() {
        assert_eq!(png_pixel(&dib(40, 24, 0, &[3, 2, 1, 0])), [1, 2, 3, 255]);
        // GDI's reserved fourth byte is not an alpha channel in BI_RGB.
        assert_eq!(png_pixel(&dib(40, 32, 0, &[3, 2, 1, 0])), [1, 2, 3, 255]);
        let mut top_down = dib(40, 24, 0, &[3, 2, 1, 0]);
        top_down[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(png_pixel(&top_down), [1, 2, 3, 255]);
        assert!(is_image_format(CF_DIBV5.0 as u32));
    }

    #[test]
    fn v5_screenshot_masks_are_not_counted_twice() {
        let mut bytes = dib(124, 32, 3, &[3, 2, 1, 255]);
        for (offset, mask) in [
            (40, 0x00ff0000u32),
            (44, 0x0000ff00),
            (48, 0x000000ff),
            (52, 0xff000000),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&mask.to_le_bytes());
        }
        bytes[56..60].copy_from_slice(&0x73524742u32.to_le_bytes());
        assert_eq!(png_pixel(&bytes), [1, 2, 3, 255]);
    }

    #[test]
    fn v5_nondefault_masks_preserve_partial_and_zero_alpha() {
        let mut bytes = dib(124, 32, 3, &[3, 2, 1, 128, 6, 5, 4, 0]);
        bytes[4..8].copy_from_slice(&2i32.to_le_bytes());
        for (offset, mask) in [
            (40, 0x000000ffu32),
            (44, 0x00ff0000),
            (48, 0x0000ff00),
            (52, 0xff000000),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&mask.to_le_bytes());
        }
        let png = convert_to_png(&convert_dib_to_bmp(&bytes).unwrap(), ImageFormat::Bmp).unwrap();
        assert_eq!(
            image::load_from_memory(&png).unwrap().to_rgba8().into_raw(),
            [3, 1, 2, 128, 6, 4, 5, 0]
        );
    }

    #[test]
    fn classic_bitfields_follow_the_info_header() {
        let mut pixels = Vec::new();
        for mask in [0xf800u32, 0x07e0, 0x001f] {
            pixels.extend_from_slice(&mask.to_le_bytes());
        }
        pixels.extend_from_slice(&[0, 248, 0, 0]);
        assert_eq!(png_pixel(&dib(40, 16, 3, &pixels)), [255, 0, 0, 255]);
    }

    #[test]
    fn invalid_or_truncated_dib_is_refused() {
        assert!(convert_dib_to_bmp(&[0; 39]).is_none());
        assert!(convert_dib_to_bmp(&dib(40, 24, 0, &[])).is_none());
        let mut bytes = dib(40, 8, 0, &[0; 4]);
        bytes[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(convert_dib_to_bmp(&bytes).is_none());
        let mut bytes = dib(40, 24, 0, &[0; 4]);
        bytes[..4].copy_from_slice(&124u32.to_le_bytes());
        assert!(convert_dib_to_bmp(&bytes).is_none());
    }

    #[test]
    #[ignore = "Requires a desktop clipboard with the controlled Korean text fixture"]
    fn desktop_unicode_clipboard_keeps_text() {
        assert_eq!(
            read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("윈도우 검증 작업")
        );
    }

    #[test]
    #[ignore = "Requires a desktop clipboard with the controlled 3x2 RGB DIB fixture"]
    fn desktop_dib_clipboard_keeps_pixels_as_png() {
        let item = read_from_clipboard().expect("desktop clipboard item");
        let image = item
            .entries
            .iter()
            .find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image),
                _ => None,
            })
            .expect("native clipboard image");
        assert_eq!(image.format, ImageFormat::Png);
        let pixels = image::load_from_memory(&image.bytes).unwrap().to_rgba8();
        assert_eq!(pixels.dimensions(), (3, 2));
        assert_eq!(
            pixels.into_raw(),
            [
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255, 255, 0, 255, 255,
                0, 255, 255, 255
            ]
        );
    }
}
