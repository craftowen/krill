// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Bot avatars as Orca draws them: 20 bundled sea-creature mascots on a square background
//! colour, an uploaded `avatar.png`, an emoji, or the name's first letter.
//!
//! Mascot SVGs are copied verbatim from Orca (`mascots/`, hashes in `mascots/sources.json`).
//! Each paints its background with `var(--avatar-bg,#b3dbf4)` and keeps an outline layer at
//! `var(--avatar-rim,0)`; resvg has no CSS variables, so both are substituted before rendering.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use gpui::{AnyElement, Image, ImageFormat, ObjectFit, Pixels, StyledImage as _, div, prelude::*};

use crate::i18n::catalog::bots as text;
use crate::ui;

/// Every bundled mascot, in Orca's order.
pub const MASCOT_IDS: [&str; 20] = [
    "A1", "B1", "D1", "E1", "F1", "G1", "H1", "H2", "I1", "J1", "K2", "L2", "M1", "N1", "O1", "P3",
    "Q1", "R3", "S2", "T1",
];

/// The New bot picker leaves out `S2`, which Orca keeps for a template.
pub const PICKER_MASCOTS: [&str; 19] = [
    "A1", "B1", "D1", "E1", "F1", "G1", "H1", "H2", "I1", "J1", "K2", "L2", "M1", "N1", "O1", "P3",
    "Q1", "R3", "T1",
];

const MASCOT_SVGS: [&[u8]; 20] = [
    include_bytes!("mascots/A1.svg"),
    include_bytes!("mascots/B1.svg"),
    include_bytes!("mascots/D1.svg"),
    include_bytes!("mascots/E1.svg"),
    include_bytes!("mascots/F1.svg"),
    include_bytes!("mascots/G1.svg"),
    include_bytes!("mascots/H1.svg"),
    include_bytes!("mascots/H2.svg"),
    include_bytes!("mascots/I1.svg"),
    include_bytes!("mascots/J1.svg"),
    include_bytes!("mascots/K2.svg"),
    include_bytes!("mascots/L2.svg"),
    include_bytes!("mascots/M1.svg"),
    include_bytes!("mascots/N1.svg"),
    include_bytes!("mascots/O1.svg"),
    include_bytes!("mascots/P3.svg"),
    include_bytes!("mascots/Q1.svg"),
    include_bytes!("mascots/R3.svg"),
    include_bytes!("mascots/S2.svg"),
    include_bytes!("mascots/T1.svg"),
];

/// Orca's default background (Sky). Saved as "no colour".
pub const DEFAULT_BACKGROUND: u32 = ui::BOT_AVATAR_BACKGROUNDS[0].0;

/// What the stored file name of an uploaded picture looks like (`avatar.png` in the bot folder).
pub const UPLOAD_AVATAR: &str = "avatar.png";

/// Orca's picture rules: up to 10 MB, at least 128 px on the short side, stored as a
/// 512 px square PNG.
pub const PICTURE_MAX_BYTES: usize = 10 * 1024 * 1024;
const PICTURE_MIN_SIDE: u32 = 128;
const PICTURE_SIDE: u32 = 512;
/// Decode guard: a 10 MB JPEG can still unpack to gigabytes.
const PICTURE_MAX_DECODE_SIDE: u32 = 8192;
const PICTURE_MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// Why a picked picture was refused (Orca's three messages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureError {
    TooBig,
    Unreadable,
    TooSmall,
}

impl PictureError {
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::TooBig => text::NB_IMG_TOO_BIG.get(),
            Self::Unreadable => text::NB_IMG_UNREADABLE.get(),
            Self::TooSmall => text::NB_IMG_TOO_SMALL.get(),
        }
    }
}

/// Turns a PNG, JPEG, WebP or GIF into the picture krilld stores, as Orca does: honour the
/// camera orientation, crop the centre square, scale it to 512 px and save it as PNG.
pub fn prepare_picture(bytes: &[u8]) -> Result<Vec<u8>, PictureError> {
    use image::ImageDecoder as _;

    if bytes.len() > PICTURE_MAX_BYTES {
        return Err(PictureError::TooBig);
    }
    let too_big_or_unreadable = |e: image::ImageError| match e {
        image::ImageError::Limits(_) => PictureError::TooBig,
        _ => PictureError::Unreadable,
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(PICTURE_MAX_DECODE_SIDE);
    limits.max_image_height = Some(PICTURE_MAX_DECODE_SIDE);
    limits.max_alloc = Some(PICTURE_MAX_DECODE_BYTES);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| PictureError::Unreadable)?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(too_big_or_unreadable)?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let picture = image::DynamicImage::from_decoder(decoder).map_err(too_big_or_unreadable)?;
    let (w, h) = (picture.width(), picture.height());
    let side = w.min(h);
    if side < PICTURE_MIN_SIDE {
        return Err(PictureError::TooSmall);
    }
    // The centre square turns with the picture, so crop and shrink first and turn the small
    // copy: one full-size copy at most.
    let mut square = picture
        .crop_imm((w - side) / 2, (h - side) / 2, side, side)
        .resize_exact(
            PICTURE_SIDE,
            PICTURE_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    drop(picture);
    square.apply_orientation(orientation);
    let mut png = Vec::new();
    square
        .to_rgba8()
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|_| PictureError::Unreadable)?;
    Ok(png)
}

/// Computes a toned-down background tint (`0x00RR_GGBB`) from the outer 10% ring of a 512px PNG,
/// returning `None` when the border ring is mostly transparent (matching Orca `pictureTint`).
#[must_use]
pub fn picture_tint(png_bytes: &[u8]) -> Option<u32> {
    let img = image::load_from_memory(png_bytes).ok()?.to_rgba8();
    let side = img.width().min(img.height());
    if side == 0 {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "ring width is bounded by image dimensions clamped at 8192"
    )]
    let ring = ((side as f32) * 0.1).round().max(1.0) as u32;
    let mut bins: HashMap<u32, (u32, u64, u64, u64)> = HashMap::new();
    let mut sampled: u32 = 0;
    let mut opaque: u32 = 0;
    for y in 0..side {
        for x in 0..side {
            let edge_dist = x.min(y).min(side - 1 - x).min(side - 1 - y);
            if edge_dist >= ring {
                continue;
            }
            sampled += 1;
            let px = img.get_pixel(x, y).0;
            if px[3] < 128 {
                continue;
            }
            opaque += 1;
            let key = ((u32::from(px[0]) >> 4) << 8)
                | ((u32::from(px[1]) >> 4) << 4)
                | (u32::from(px[2]) >> 4);
            let entry = bins.entry(key).or_insert((0, 0, 0, 0));
            entry.0 += 1;
            entry.1 += u64::from(px[0]);
            entry.2 += u64::from(px[1]);
            entry.3 += u64::from(px[2]);
        }
    }
    if opaque * 2 < sampled {
        return None;
    }
    let (_, (count, sum_r, sum_g, sum_b)) =
        bins.into_iter().max_by_key(|(_, (cnt, _, _, _))| *cnt)?;
    if count == 0 {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "bin average precision into 0..1 sRGB is exact enough"
    )]
    let rf = (sum_r as f32 / count as f32) / 255.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "bin average precision into 0..1 sRGB is exact enough"
    )]
    let gf = (sum_g as f32 / count as f32) / 255.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "bin average precision into 0..1 sRGB is exact enough"
    )]
    let bf = (sum_b as f32 / count as f32) / 255.0;

    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let light_raw = f32::midpoint(max, min);
    let (hue, sat_raw) = if (max - min).abs() < f32::EPSILON {
        (0.0, 0.0)
    } else {
        let delta = max - min;
        let sat_calc = if light_raw > 0.5 {
            delta / (2.0 - max - min)
        } else {
            delta / (max + min)
        };
        let hue_calc = if (max - rf).abs() < f32::EPSILON {
            ((gf - bf) / delta + if gf < bf { 6.0 } else { 0.0 }) / 6.0
        } else if (max - gf).abs() < f32::EPSILON {
            ((bf - rf) / delta + 2.0) / 6.0
        } else {
            ((rf - gf) / delta + 4.0) / 6.0
        };
        (hue_calc, sat_calc)
    };
    let sat = sat_raw.min(0.60);
    let light = light_raw.clamp(0.35, 0.80);
    let q_hi = if light < 0.5 {
        light * (1.0 + sat)
    } else {
        light + sat - light * sat
    };
    let p_lo = 2.0 * light - q_hi;
    let chan = |unit: f32| -> u32 {
        let wrapped = if unit < 0.0 {
            unit + 1.0
        } else if unit > 1.0 {
            unit - 1.0
        } else {
            unit
        };
        let scaled = if wrapped < 1.0 / 6.0 {
            p_lo + (q_hi - p_lo) * 6.0 * wrapped
        } else if wrapped < 0.5 {
            q_hi
        } else if wrapped < 2.0 / 3.0 {
            p_lo + (q_hi - p_lo) * (2.0 / 3.0 - wrapped) * 6.0
        } else {
            p_lo
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0.0..=255.0 before rounding and u32 cast"
        )]
        {
            (scaled * 255.0).round().clamp(0.0, 255.0) as u32
        }
    };
    Some((chan(hue + 1.0 / 3.0) << 16) | (chan(hue) << 8) | chan(hue - 1.0 / 3.0))
}

/// Canonical mascot ID for a stored avatar value (`B1` or Orca's `mascot:B1`).
#[must_use]
pub fn mascot_id(raw: &str) -> Option<&'static str> {
    let raw = raw.trim();
    let id = raw.strip_prefix("mascot:").unwrap_or(raw);
    MASCOT_IDS
        .iter()
        .copied()
        .find(|m| m.eq_ignore_ascii_case(id))
}

/// `#rrggbb` → `0xRRGGBB`.
#[must_use]
pub fn parse_color(raw: &str) -> Option<u32> {
    let hex = raw.trim().strip_prefix('#')?;
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// `0xRRGGBB` → lowercase `#rrggbb`, the form Orca stores.
#[must_use]
pub fn format_color(rgb: u32) -> String {
    format!("#{rgb:06x}")
}

/// WCAG relative luminance below 0.18 turns on the mascot's outline so it reads on dark ink.
#[must_use]
pub fn is_dark(rgb: u32) -> bool {
    fn channel(c: u32) -> f32 {
        #[expect(clippy::cast_precision_loss, reason = "8-bit channel")]
        let v = (c & 0xff) as f32 / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    let l = 0.0722f32.mul_add(
        channel(rgb),
        0.2126f32.mul_add(channel(rgb >> 16), 0.7152 * channel(rgb >> 8)),
    );
    l < 0.18
}

/// The mascot SVG with its background and outline filled in, ready for resvg.
#[must_use]
pub fn mascot_svg(id: &str, background: u32) -> Option<String> {
    let index = MASCOT_IDS.iter().position(|m| *m == id)?;
    let src = std::str::from_utf8(MASCOT_SVGS[index]).ok()?;
    let rim = if is_dark(background) { "1" } else { "0" };
    Some(
        src.replace("var(--avatar-bg,#b3dbf4)", &format_color(background))
            .replace("var(--avatar-rim,0)", rim)
            // resvg rasterises at the intrinsic size; 256px keeps the 112px picker sharp on Retina.
            .replacen(
                r#"viewBox="0 0 128 128">"#,
                r#"width="256" height="256" viewBox="0 0 128 128">"#,
                1,
            ),
    )
}

/// Full-colour mascot image, cached per mascot and background.
#[must_use]
pub fn mascot_image(id: &'static str, background: u32) -> Option<Arc<Image>> {
    type Cache = Mutex<HashMap<(&'static str, u32), Arc<Image>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Cache::default);
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(image) = cache.get(&(id, background)) {
        return Some(image.clone());
    }
    let image = Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        mascot_svg(id, background)?.into_bytes(),
    ));
    cache.insert((id, background), image.clone());
    Some(image)
}

/// Desaturates all `#rrggbb` colors in an SVG string using Rec. 601 luma, for Orca's
/// `grayscale` all-clear avatar (`eefd93a3` / `#46740`).
fn desaturate_svg_hex_colors(svg: &str) -> String {
    let bytes = svg.as_bytes();
    let mut out = String::with_capacity(svg.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'#'
            && idx + 7 <= bytes.len()
            && bytes[idx + 1..idx + 7].iter().all(u8::is_ascii_hexdigit)
            && !bytes.get(idx + 7).is_some_and(u8::is_ascii_hexdigit)
            && let Some(hex) = svg.get(idx + 1..idx + 7)
            && let Ok(rgb) = u32::from_str_radix(hex, 16)
        {
            let red = (rgb >> 16) & 0xff;
            let green = (rgb >> 8) & 0xff;
            let blue = rgb & 0xff;
            let luma = (299 * red + 587 * green + 114 * blue + 500) / 1000;
            out.push_str(&format_color((luma << 16) | (luma << 8) | luma));
            idx += 7;
            continue;
        }
        let Some(ch) = svg.get(idx..).and_then(|s| s.chars().next()) else {
            break;
        };
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

/// Grayscale mascot image, cached per mascot and background.
#[must_use]
pub fn mascot_image_grayscale(id: &'static str, background: u32) -> Option<Arc<Image>> {
    type Cache = Mutex<HashMap<(&'static str, u32), Arc<Image>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Cache::default);
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(image) = cache.get(&(id, background)) {
        return Some(image.clone());
    }
    let svg = desaturate_svg_hex_colors(&mascot_svg(id, background)?);
    let image = Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()));
    cache.insert((id, background), image.clone());
    Some(image)
}

fn grayscale_upload_image(upload: &Arc<Image>) -> Option<Arc<Image>> {
    let img = image::load_from_memory(upload.bytes()).ok()?.grayscale();
    let mut png = Vec::new();
    img.to_rgba8()
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
}

/// What a bot's avatar shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Avatar {
    Mascot {
        id: &'static str,
        background: u32,
    },
    /// `avatar.png` in the bot folder, fetched with `GetBotAvatar`.
    Upload,
    Emoji(String),
    Initial(String),
}

impl Avatar {
    /// Reads Krill's stored fields (`avatar`, `avatar_color`, `emoji`) the way Orca does.
    #[must_use]
    pub fn from_fields(
        avatar: Option<&str>,
        avatar_color: Option<&str>,
        emoji: Option<&str>,
        name: &str,
    ) -> Self {
        let avatar = avatar.map(str::trim).filter(|a| !a.is_empty());
        if let Some(id) = avatar.and_then(mascot_id) {
            let background = avatar_color
                .and_then(parse_color)
                .unwrap_or(DEFAULT_BACKGROUND);
            return Self::Mascot { id, background };
        }
        if let Some(a) = avatar {
            if a == UPLOAD_AVATAR || a.starts_with("upload:") {
                return Self::Upload;
            }
            // Orca keeps any other avatar string as an emoji.
            if a.chars().count() <= 4 {
                return Self::Emoji(a.to_owned());
            }
        }
        if let Some(e) = emoji.map(str::trim).filter(|e| !e.is_empty()) {
            return Self::Emoji(e.to_owned());
        }
        Self::Initial(initial(name))
    }

    /// Reads a [`krill_proto::bots::BotConfig`].
    #[must_use]
    pub fn of(bot: &krill_proto::bots::BotConfig) -> Self {
        Self::from_fields(
            bot.avatar.as_deref(),
            bot.avatar_color.as_deref(),
            bot.emoji.as_deref(),
            &bot.name,
        )
    }
}

/// Draws `avatar` in a `size` square with `radius` corners. `upload` is the decoded
/// `avatar.png` when the avatar is [`Avatar::Upload`]; without it the initial stands in.
#[must_use]
pub fn render(
    avatar: &Avatar,
    upload: Option<Arc<Image>>,
    fallback_name: &str,
    size: Pixels,
    radius: Pixels,
    p: &ui::Palette,
) -> AnyElement {
    render_inner(avatar, upload, fallback_name, size, radius, false, false, p)
}

/// Draws `avatar` desaturated and dimmed (`grayscale opacity-50 dark:opacity-40`) for Orca's
/// `BotItemsAllClear` (`eefd93a3` / `#46740`).
#[must_use]
pub fn render_grayscale(
    avatar: &Avatar,
    upload: Option<Arc<Image>>,
    fallback_name: &str,
    size: Pixels,
    radius: Pixels,
    dark: bool,
    p: &ui::Palette,
) -> AnyElement {
    render_inner(avatar, upload, fallback_name, size, radius, true, dark, p)
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared color and grayscale avatar renderer"
)]
fn render_inner(
    avatar: &Avatar,
    upload: Option<Arc<Image>>,
    fallback_name: &str,
    size: Pixels,
    radius: Pixels,
    grayscale: bool,
    dark: bool,
    p: &ui::Palette,
) -> AnyElement {
    let alpha = if !grayscale {
        1.0
    } else if dark {
        0.4
    } else {
        0.5
    };
    let image = match avatar {
        Avatar::Mascot { id, background } => {
            if grayscale {
                mascot_image_grayscale(id, *background)
            } else {
                mascot_image(id, *background)
            }
        }
        Avatar::Upload => {
            if grayscale {
                upload.and_then(|img| grayscale_upload_image(&img).or(Some(img)))
            } else {
                upload
            }
        }
        Avatar::Emoji(_) | Avatar::Initial(_) => None,
    };
    if let Some(image) = image {
        let el = gpui::img(image)
            .flex_none()
            .size(size)
            .rounded(radius)
            .object_fit(ObjectFit::Cover);
        return if grayscale {
            div()
                .flex_none()
                .opacity(alpha)
                .child(el)
                .into_any_element()
        } else {
            el.into_any_element()
        };
    }
    let glyph = match avatar {
        Avatar::Emoji(g) | Avatar::Initial(g) => g.clone(),
        Avatar::Mascot { .. } | Avatar::Upload => initial(fallback_name),
    };
    let text = if size >= gpui::px(56.) {
        ui::TEXT_2XL
    } else if size >= gpui::px(32.) {
        ui::TEXT_LG
    } else {
        ui::TEXT_XS
    };
    div()
        .flex_none()
        .size(size)
        .rounded(radius)
        .bg(p.muted)
        .border_1()
        .border_color(p.border)
        .when(grayscale, |d| d.opacity(alpha))
        .flex()
        .items_center()
        .justify_center()
        .text_size(text)
        // Orca BotAvatarTile: `font-semibold text-foreground` on the muted tile.
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if grayscale {
            p.muted_foreground
        } else {
            p.foreground
        })
        .child(glyph)
        .into_any_element()
}

/// The name's first letter, upper-cased (Orca's no-picture avatar).
fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_avatar_values_map_to_what_orca_draws() {
        assert_eq!(
            Avatar::from_fields(Some("B1"), None, None, "x"),
            Avatar::Mascot {
                id: "B1",
                background: DEFAULT_BACKGROUND
            }
        );
        assert_eq!(
            Avatar::from_fields(Some("mascot:q1"), Some("#1f2126"), None, "x"),
            Avatar::Mascot {
                id: "Q1",
                background: 0x1f_2126
            }
        );
        assert_eq!(
            Avatar::from_fields(Some("avatar.png"), None, None, "x"),
            Avatar::Upload
        );
        assert_eq!(
            Avatar::from_fields(Some("upload:3"), None, None, "x"),
            Avatar::Upload
        );
        assert_eq!(
            Avatar::from_fields(None, None, Some("🦀"), "x"),
            Avatar::Emoji("🦀".into())
        );
        assert_eq!(
            Avatar::from_fields(None, None, None, "krill bot"),
            Avatar::Initial("K".into())
        );
        // A colour that isn't `#rrggbb` (an old tab colour name) falls back to Sky.
        assert_eq!(
            Avatar::from_fields(Some("A1"), Some("blue"), None, "x"),
            Avatar::Mascot {
                id: "A1",
                background: DEFAULT_BACKGROUND
            }
        );
    }

    #[test]
    fn mascot_svg_fills_background_and_turns_rim_on_for_dark_ink() {
        for id in MASCOT_IDS {
            let light = mascot_svg(id, 0xff_ffff).unwrap();
            assert!(!light.contains("var(--"), "{id} keeps a CSS variable");
            assert!(light.contains("fill:#ffffff"), "{id} background");
            assert!(light.contains("opacity:0"), "{id} rim stays off on light");
            assert!(light.contains(r#"width="256""#), "{id} raster size");
            let dark = mascot_svg(id, 0x1f_2126).unwrap();
            assert!(dark.contains("opacity:1"), "{id} rim on for Ink");
        }
        assert!(is_dark(0x3a_3f4b) && is_dark(0x1f_2126));
        assert!(!is_dark(0x8f_a3b8) && !is_dark(DEFAULT_BACKGROUND));
    }

    #[test]
    fn bundled_mascots_match_their_recorded_hashes() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("mascots/sources.json")).unwrap();
        let files = manifest["files"].as_array().unwrap();
        assert_eq!(files.len(), MASCOT_IDS.len());
        for (entry, (id, bytes)) in files.iter().zip(MASCOT_IDS.iter().zip(MASCOT_SVGS)) {
            assert_eq!(entry["file"], format!("{id}.svg"));
            let hex = crate::updater::ed25519::sha256_hex(bytes);
            assert_eq!(
                entry["sha256"], hex,
                "{id}.svg changed without updating sources.json"
            );
        }
        assert!(PICKER_MASCOTS.iter().all(|m| *m != "S2"));
    }

    #[test]
    fn pictures_become_a_512_px_centre_square_png() {
        let encode = |w: u32, h: u32, format: image::ImageFormat| {
            // Left third red, middle green, right third blue: the crop must keep the middle.
            let img = image::RgbImage::from_fn(w, h, |x, _| match x * 3 / w {
                0 => image::Rgb([255, 0, 0]),
                1 => image::Rgb([0, 255, 0]),
                _ => image::Rgb([0, 0, 255]),
            });
            let mut out = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut out), format)
                .unwrap();
            out
        };
        for format in [image::ImageFormat::Jpeg, image::ImageFormat::Png] {
            let png = prepare_picture(&encode(600, 200, format)).unwrap();
            let back = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .unwrap()
                .to_rgba8();
            assert_eq!(back.dimensions(), (512, 512), "{format:?}");
            let centre = back.get_pixel(256, 256);
            assert!(
                centre[1] > 200 && centre[0] < 60 && centre[2] < 60,
                "{centre:?}"
            );
        }
        assert_eq!(
            prepare_picture(&encode(300, 100, image::ImageFormat::Png)),
            Err(PictureError::TooSmall)
        );
        assert_eq!(
            prepare_picture(b"definitely not a picture"),
            Err(PictureError::Unreadable)
        );
        assert_eq!(
            prepare_picture(&vec![0; PICTURE_MAX_BYTES + 1]),
            Err(PictureError::TooBig)
        );
    }
}
