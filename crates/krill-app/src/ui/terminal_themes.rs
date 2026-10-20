//! Orca 호환 30종 내장 터미널 테마 카탈로그·커스텀 테마·Ghostty/Warp 테마 가져오기(OWE-956).
//!
//! 출처: `.ref/orca/src/shared/terminal-themes/` (`defaults.ts`, `popular-dark-core.ts`,
//! `popular-dark-extended.ts`, `popular-light.ts`, `classic.ts`),
//! `.ref/orca/src/main/ghostty/`, `.ref/orca/src/main/warp-themes/`.

use std::path::{Path, PathBuf};

use gpui::Hsla;
use krill_proto::theme::{Rgb, Theme as TerminalTheme};
use serde::{Deserialize, Serialize};

use super::theme::{TERMINAL_DARK, TERMINAL_LIGHT, rgba_bytes, term_rgb};

/// 기본 다크 터미널 테마 이름(Orca `DEFAULT_TERMINAL_THEME_DARK`).
pub const DEFAULT_TERMINAL_THEME_DARK: &str = "Ghostty Default Style Dark";
/// 기본 라이트 터미널 테마 이름(Orca `DEFAULT_TERMINAL_THEME_LIGHT`).
pub const DEFAULT_TERMINAL_THEME_LIGHT: &str = "Builtin Tango Light";
/// 사용자 가져오기 테마 선택값 접두사(`custom:<id>`).
pub const CUSTOM_TERMINAL_THEME_PREFIX: &str = "custom:";
/// 사용자 가져오기 테마 최대 보관 개수(Orca `MAX_TERMINAL_CUSTOM_THEMES`).
pub const MAX_TERMINAL_CUSTOM_THEMES: usize = 200;

/// 테마 파일 읽기 상한(256 KiB).
const MAX_THEME_FILE_BYTES: u64 = 262_144;
/// Ghostty 설정 파일 읽기 상한(1 MiB).
const MAX_CONFIG_FILE_BYTES: u64 = 1_000_000;

/// 내장 터미널 테마 항목.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinTerminalTheme {
    pub name: &'static str,
    pub is_dark: bool,
    pub theme: TerminalTheme,
}

const fn term_hex(raw: u32) -> Rgb {
    let [_, r, g, b] = raw.to_be_bytes();
    term_rgb(r, g, b)
}

const fn builtin(
    name: &'static str,
    is_dark: bool,
    background: u32,
    foreground: u32,
    cursor: u32,
    ansi: [u32; 16],
) -> BuiltinTerminalTheme {
    BuiltinTerminalTheme {
        name,
        is_dark,
        theme: TerminalTheme {
            foreground: term_hex(foreground),
            background: term_hex(background),
            cursor: term_hex(cursor),
            ansi: [
                term_hex(ansi[0]),
                term_hex(ansi[1]),
                term_hex(ansi[2]),
                term_hex(ansi[3]),
                term_hex(ansi[4]),
                term_hex(ansi[5]),
                term_hex(ansi[6]),
                term_hex(ansi[7]),
                term_hex(ansi[8]),
                term_hex(ansi[9]),
                term_hex(ansi[10]),
                term_hex(ansi[11]),
                term_hex(ansi[12]),
                term_hex(ansi[13]),
                term_hex(ansi[14]),
                term_hex(ansi[15]),
            ],
        },
    }
}

/// Orca 내장 30종 터미널 테마 카탈로그(`.ref/orca/src/shared/terminal-themes/index.ts` 정본 순서).
pub const BUILTIN_TERMINAL_THEMES: &[BuiltinTerminalTheme] = &[
    // 1..=2: defaults.ts
    BuiltinTerminalTheme {
        name: DEFAULT_TERMINAL_THEME_DARK,
        is_dark: true,
        theme: TERMINAL_DARK,
    },
    BuiltinTerminalTheme {
        name: DEFAULT_TERMINAL_THEME_LIGHT,
        is_dark: false,
        theme: TERMINAL_LIGHT,
    },
    // 3..=10: popular-dark-core.ts
    builtin(
        "Dracula",
        true,
        0x0028_2a36,
        0x00f8_f8f2,
        0x00f8_f8f2,
        [
            0x0021_222c,
            0x00ff_5555,
            0x0050_fa7b,
            0x00f1_fa8c,
            0x00bd_93f9,
            0x00ff_79c6,
            0x008b_e9fd,
            0x00f8_f8f2,
            0x0062_72a4,
            0x00ff_6e6e,
            0x0069_ff94,
            0x00ff_ffa5,
            0x00d6_acff,
            0x00ff_92df,
            0x00a4_ffff,
            0x00ff_ffff,
        ],
    ),
    builtin(
        "One Dark",
        true,
        0x0028_2c34,
        0x00ab_b2bf,
        0x0052_8bff,
        [
            0x0028_2c34,
            0x00e0_6c75,
            0x0098_c379,
            0x00e5_c07b,
            0x0061_afef,
            0x00c6_78dd,
            0x0056_b6c2,
            0x00ab_b2bf,
            0x005c_6370,
            0x00e0_6c75,
            0x0098_c379,
            0x00e5_c07b,
            0x0061_afef,
            0x00c6_78dd,
            0x0056_b6c2,
            0x00ff_ffff,
        ],
    ),
    builtin(
        "Nord",
        true,
        0x002e_3440,
        0x00d8_dee9,
        0x00d8_dee9,
        [
            0x003b_4252,
            0x00bf_616a,
            0x00a3_be8c,
            0x00eb_cb8b,
            0x0081_a1c1,
            0x00b4_8ead,
            0x0088_c0d0,
            0x00e5_e9f0,
            0x004c_566a,
            0x00bf_616a,
            0x00a3_be8c,
            0x00eb_cb8b,
            0x0081_a1c1,
            0x00b4_8ead,
            0x008f_bcbb,
            0x00ec_eff4,
        ],
    ),
    builtin(
        "Monokai",
        true,
        0x0027_2822,
        0x00f8_f8f2,
        0x00f8_f8f0,
        [
            0x0027_2822,
            0x00f9_2672,
            0x00a6_e22e,
            0x00f4_bf75,
            0x0066_d9ef,
            0x00ae_81ff,
            0x00a1_efe4,
            0x00f8_f8f2,
            0x0075_715e,
            0x00f9_2672,
            0x00a6_e22e,
            0x00f4_bf75,
            0x0066_d9ef,
            0x00ae_81ff,
            0x00a1_efe4,
            0x00f9_f8f5,
        ],
    ),
    builtin(
        "Tokyo Night",
        true,
        0x001a_1b26,
        0x00c0_caf5,
        0x00c0_caf5,
        [
            0x0015_161e,
            0x00f7_768e,
            0x009e_ce6a,
            0x00e0_af68,
            0x007a_a2f7,
            0x00bb_9af7,
            0x007d_cfff,
            0x00a9_b1d6,
            0x0041_4868,
            0x00f7_768e,
            0x009e_ce6a,
            0x00e0_af68,
            0x007a_a2f7,
            0x00bb_9af7,
            0x007d_cfff,
            0x00c0_caf5,
        ],
    ),
    builtin(
        "Gruvbox Dark",
        true,
        0x0028_2828,
        0x00eb_dbb2,
        0x00eb_dbb2,
        [
            0x0028_2828,
            0x00cc_241d,
            0x0098_971a,
            0x00d7_9921,
            0x0045_8588,
            0x00b1_6286,
            0x0068_9d6a,
            0x00a8_9984,
            0x0092_8374,
            0x00fb_4934,
            0x00b8_bb26,
            0x00fa_bd2f,
            0x0083_a598,
            0x00d3_869b,
            0x008e_c07c,
            0x00eb_dbb2,
        ],
    ),
    builtin(
        "Catppuccin Mocha",
        true,
        0x001e_1e2e,
        0x00cd_d6f4,
        0x00f5_e0dc,
        [
            0x0045_475a,
            0x00f3_8ba8,
            0x00a6_e3a1,
            0x00f9_e2af,
            0x0089_b4fa,
            0x00f5_c2e7,
            0x0094_e2d5,
            0x00ba_c2de,
            0x0058_5b70,
            0x00f3_8ba8,
            0x00a6_e3a1,
            0x00f9_e2af,
            0x0089_b4fa,
            0x00f5_c2e7,
            0x0094_e2d5,
            0x00a6_adc8,
        ],
    ),
    builtin(
        "Solarized Dark",
        true,
        0x0000_2b36,
        0x0083_9496,
        0x0083_9496,
        [
            0x0007_3642,
            0x00dc_322f,
            0x0085_9900,
            0x00b5_8900,
            0x0026_8bd2,
            0x00d3_3682,
            0x002a_a198,
            0x00ee_e8d5,
            0x0000_2b36,
            0x00cb_4b16,
            0x0058_6e75,
            0x0065_7b83,
            0x0083_9496,
            0x006c_71c4,
            0x0093_a1a1,
            0x00fd_f6e3,
        ],
    ),
    // 11..=19: popular-dark-extended.ts
    builtin(
        "Material Dark",
        true,
        0x0021_2121,
        0x00ee_ffff,
        0x00ff_cc00,
        [
            0x0000_0000,
            0x00f0_7178,
            0x00c3_e88d,
            0x00ff_cb6b,
            0x0082_aaff,
            0x00c7_92ea,
            0x0089_ddff,
            0x00ff_ffff,
            0x0054_5454,
            0x00f0_7178,
            0x00c3_e88d,
            0x00ff_cb6b,
            0x0082_aaff,
            0x00c7_92ea,
            0x0089_ddff,
            0x00ff_ffff,
        ],
    ),
    builtin(
        "Ayu Dark",
        true,
        0x000a_0e14,
        0x00b3_b1ad,
        0x00e6_b450,
        [
            0x0001_060e,
            0x00ea_6c73,
            0x0091_b362,
            0x00f9_af4f,
            0x0053_bdfa,
            0x00fa_e994,
            0x0090_e1c6,
            0x00c7_c7c7,
            0x0068_6868,
            0x00f0_7178,
            0x00c2_d94c,
            0x00ff_b454,
            0x0059_c2ff,
            0x00ff_ee99,
            0x0095_e6cb,
            0x00ff_ffff,
        ],
    ),
    builtin(
        "Nightfox",
        true,
        0x0019_2330,
        0x00cd_cecf,
        0x00cd_cecf,
        [
            0x0039_3b44,
            0x00c9_4f6d,
            0x0081_b29a,
            0x00db_c074,
            0x0071_9cd6,
            0x009d_79d6,
            0x0063_cdcf,
            0x00df_dfe0,
            0x0057_5860,
            0x00d1_6983,
            0x008e_baa4,
            0x00e0_c989,
            0x0086_abdc,
            0x00ba_a1e2,
            0x007a_d5d6,
            0x00e4_e4e5,
        ],
    ),
    builtin(
        "Kanagawa",
        true,
        0x001f_1f28,
        0x00dc_d7ba,
        0x00c8_c093,
        [
            0x0009_0618,
            0x00c3_4043,
            0x0076_946a,
            0x00c0_a36e,
            0x007e_9cd8,
            0x0095_7fb8,
            0x006a_9589,
            0x00c8_c093,
            0x0072_7169,
            0x00e8_2424,
            0x0098_bb6c,
            0x00e6_c384,
            0x007f_b4ca,
            0x0093_8aa9,
            0x007a_a89f,
            0x00dc_d7ba,
        ],
    ),
    builtin(
        "Rose Pine",
        true,
        0x0019_1724,
        0x00e0_def4,
        0x0052_4f67,
        [
            0x0026_233a,
            0x00eb_6f92,
            0x0031_748f,
            0x00f6_c177,
            0x009c_cfd8,
            0x00c4_a7e7,
            0x00eb_bcba,
            0x00e0_def4,
            0x006e_6a86,
            0x00eb_6f92,
            0x0031_748f,
            0x00f6_c177,
            0x009c_cfd8,
            0x00c4_a7e7,
            0x00eb_bcba,
            0x00e0_def4,
        ],
    ),
    builtin(
        "Everforest Dark",
        true,
        0x002d_353b,
        0x00d3_c6aa,
        0x00d3_c6aa,
        [
            0x0047_5258,
            0x00e6_7e80,
            0x00a7_c080,
            0x00db_bc7f,
            0x007f_bbb3,
            0x00d6_99b6,
            0x0083_c092,
            0x00d3_c6aa,
            0x0047_5258,
            0x00e6_7e80,
            0x00a7_c080,
            0x00db_bc7f,
            0x007f_bbb3,
            0x00d6_99b6,
            0x0083_c092,
            0x00d3_c6aa,
        ],
    ),
    builtin(
        "Palenight",
        true,
        0x0029_2d3e,
        0x00bf_c7d5,
        0x00ff_cc00,
        [
            0x0029_2d3e,
            0x00f0_7178,
            0x00c3_e88d,
            0x00ff_cb6b,
            0x0082_aaff,
            0x00c7_92ea,
            0x0089_ddff,
            0x00d0_d0d0,
            0x0043_4758,
            0x00ff_8b92,
            0x00dd_ffa7,
            0x00ff_e585,
            0x009c_c4ff,
            0x00e1_acff,
            0x00a3_f7ff,
            0x00ff_ffff,
        ],
    ),
    builtin(
        "Horizon Dark",
        true,
        0x001c_1e26,
        0x00e0_e0e0,
        0x00e9_5378,
        [
            0x0016_161c,
            0x00e9_5678,
            0x0029_d398,
            0x00fa_b795,
            0x0026_bbd9,
            0x00ee_64ac,
            0x0059_e1e3,
            0x00d5_d8da,
            0x005b_5858,
            0x00ec_6a88,
            0x003f_daa4,
            0x00fb_c3a7,
            0x003f_c4de,
            0x00f0_75b5,
            0x006b_e4e6,
            0x00ff_ffff,
        ],
    ),
    builtin(
        "Night Owl",
        true,
        0x0001_1627,
        0x00d6_deeb,
        0x0080_a4c2,
        [
            0x0001_1627,
            0x00ef_5350,
            0x0022_da6e,
            0x00ad_db67,
            0x0082_aaff,
            0x00c7_92ea,
            0x0021_c7a8,
            0x00ff_ffff,
            0x0057_5656,
            0x00ef_5350,
            0x0022_da6e,
            0x00ff_eb95,
            0x0082_aaff,
            0x00c7_92ea,
            0x007f_dbca,
            0x00ff_ffff,
        ],
    ),
    // 20..=27: popular-light.ts
    builtin(
        "Solarized Light",
        false,
        0x00fd_f6e3,
        0x0065_7b83,
        0x0065_7b83,
        [
            0x0007_3642,
            0x00dc_322f,
            0x0085_9900,
            0x00b5_8900,
            0x0026_8bd2,
            0x00d3_3682,
            0x002a_a198,
            0x00ee_e8d5,
            0x0000_2b36,
            0x00cb_4b16,
            0x0058_6e75,
            0x0065_7b83,
            0x0083_9496,
            0x006c_71c4,
            0x0093_a1a1,
            0x00fd_f6e3,
        ],
    ),
    builtin(
        "One Light",
        false,
        0x00fa_fafa,
        0x0038_3a42,
        0x0052_6fff,
        [
            0x0038_3a42,
            0x00e4_5649,
            0x0050_a14f,
            0x00c1_8401,
            0x0040_78f2,
            0x00a6_26a4,
            0x0001_84bc,
            0x00a0_a1a7,
            0x004f_525e,
            0x00e0_6c75,
            0x0098_c379,
            0x00e5_c07b,
            0x0061_afef,
            0x00c6_78dd,
            0x0056_b6c2,
            0x00ff_ffff,
        ],
    ),
    builtin(
        "Catppuccin Latte",
        false,
        0x00ef_f1f5,
        0x004c_4f69,
        0x00dc_8a78,
        [
            0x005c_5f77,
            0x00d2_0f39,
            0x0040_a02b,
            0x00df_8e1d,
            0x001e_66f5,
            0x00ea_76cb,
            0x0017_9299,
            0x00ac_b0be,
            0x006c_6f85,
            0x00d2_0f39,
            0x0040_a02b,
            0x00df_8e1d,
            0x001e_66f5,
            0x00ea_76cb,
            0x0017_9299,
            0x00bc_c0cc,
        ],
    ),
    builtin(
        "GitHub Light",
        false,
        0x00ff_ffff,
        0x0024_292f,
        0x0009_69da,
        [
            0x0024_292f,
            0x00cf_222e,
            0x0011_6329,
            0x004d_2d00,
            0x0009_69da,
            0x0082_50df,
            0x001b_7c83,
            0x006e_7781,
            0x0057_606a,
            0x00a4_0e26,
            0x001a_7f37,
            0x0063_3c01,
            0x0021_8bff,
            0x00a4_75f9,
            0x0031_92aa,
            0x008c_959f,
        ],
    ),
    builtin(
        "Rose Pine Dawn",
        false,
        0x00fa_f4ed,
        0x0057_5279,
        0x00ce_cacd,
        [
            0x00f2_e9e1,
            0x00b4_637a,
            0x0028_6983,
            0x00ea_9d34,
            0x0056_949f,
            0x0090_7aa9,
            0x00d7_827e,
            0x0057_5279,
            0x0098_93a5,
            0x00b4_637a,
            0x0028_6983,
            0x00ea_9d34,
            0x0056_949f,
            0x0090_7aa9,
            0x00d7_827e,
            0x0057_5279,
        ],
    ),
    builtin(
        "Gruvbox Light",
        false,
        0x00fb_f1c7,
        0x003c_3836,
        0x003c_3836,
        [
            0x00fb_f1c7,
            0x00cc_241d,
            0x0098_971a,
            0x00d7_9921,
            0x0045_8588,
            0x00b1_6286,
            0x0068_9d6a,
            0x007c_6f64,
            0x0092_8374,
            0x009d_0006,
            0x0079_740e,
            0x00b5_7614,
            0x0007_6678,
            0x008f_3f71,
            0x0042_7b58,
            0x003c_3836,
        ],
    ),
    builtin(
        "Tokyo Night Light",
        false,
        0x00d5_d6db,
        0x0034_3b58,
        0x0034_3b58,
        [
            0x000f_0f14,
            0x008c_4351,
            0x0048_5e30,
            0x008f_5e15,
            0x0034_548a,
            0x005a_4a78,
            0x000f_4b6e,
            0x0034_3b58,
            0x0096_99a3,
            0x008c_4351,
            0x0048_5e30,
            0x008f_5e15,
            0x0034_548a,
            0x005a_4a78,
            0x000f_4b6e,
            0x0034_3b58,
        ],
    ),
    builtin(
        "Everforest Light",
        false,
        0x00fd_f6e3,
        0x005c_6a72,
        0x005c_6a72,
        [
            0x005c_6a72,
            0x00f8_5552,
            0x008d_a101,
            0x00df_a000,
            0x003a_94c5,
            0x00df_69ba,
            0x0035_a77c,
            0x00df_ddc8,
            0x005c_6a72,
            0x00f8_5552,
            0x008d_a101,
            0x00df_a000,
            0x003a_94c5,
            0x00df_69ba,
            0x0035_a77c,
            0x00df_ddc8,
        ],
    ),
    // 28..=30: classic.ts
    builtin(
        "Tango Dark",
        true,
        0x0000_0000,
        0x00ff_ffff,
        0x00ff_ffff,
        [
            0x0000_0000,
            0x00cc_0000,
            0x004e_9a06,
            0x00c4_a000,
            0x0034_65a4,
            0x0075_507b,
            0x0006_989a,
            0x00d3_d7cf,
            0x0055_5753,
            0x00ef_2929,
            0x008a_e234,
            0x00fc_e94f,
            0x0072_9fcf,
            0x00ad_7fa8,
            0x0034_e2e2,
            0x00ee_eeec,
        ],
    ),
    builtin(
        "Homebrew",
        true,
        0x0000_0000,
        0x0000_ff00,
        0x0023_ff18,
        [
            0x0000_0000,
            0x0099_0000,
            0x0000_a600,
            0x0099_9900,
            0x0000_00b2,
            0x00b2_00b2,
            0x0000_a6b2,
            0x00bf_bfbf,
            0x0066_6666,
            0x00e5_0000,
            0x0000_d900,
            0x00e5_e500,
            0x0000_00ff,
            0x00e5_00e5,
            0x0000_e5e5,
            0x00e5_e5e5,
        ],
    ),
    builtin(
        "Snazzy",
        true,
        0x0028_2a36,
        0x00ef_f0eb,
        0x0097_979b,
        [
            0x0028_2a36,
            0x00ff_5c57,
            0x005a_f78e,
            0x00f3_f99d,
            0x0057_c7ff,
            0x00ff_6ac1,
            0x009a_edfe,
            0x00f1_f1f0,
            0x0068_6868,
            0x00ff_5c57,
            0x005a_f78e,
            0x00f3_f99d,
            0x0057_c7ff,
            0x00ff_6ac1,
            0x009a_edfe,
            0x00ef_f0eb,
        ],
    ),
];

/// 터미널 [`Rgb`] 색상을 UI 스와치·미리보기용 불투명 [`Hsla`]로 바꾼다.
#[must_use]
pub const fn terminal_rgb_hsla(c: Rgb) -> Hsla {
    rgba_bytes(c.r, c.g, c.b, 0xff)
}

/// `#rrggbb`·`rrggbb`·`#rgb`·`rgb` 문자열을 [`Rgb`]로 파싱한다.
#[must_use]
pub fn parse_hex_rgb(raw: &str) -> Option<Rgb> {
    let trimmed = raw.trim().trim_matches(|c| c == '"' || c == '\u{27}');
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(hex.get(0..1)?, 16).ok()?;
            let g = u8::from_str_radix(hex.get(1..2)?, 16).ok()?;
            let b = u8::from_str_radix(hex.get(2..3)?, 16).ok()?;
            Some(term_rgb((r << 4) | r, (g << 4) | g, (b << 4) | b))
        }
        6 => {
            let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
            let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
            let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()?;
            Some(term_rgb(r, g, b))
        }
        _ => None,
    }
}

/// [`Rgb`]를 `#rrggbb` 소문자 문자열로 포맷한다.
#[must_use]
pub fn format_hex_rgb(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// 배경색 상대 휘도로 라이트 모드(`true`)인지 판정한다(Orca `luminance >= 0.55`).
#[must_use]
pub fn is_light_rgb(bg: Rgb) -> bool {
    let r = f32::from(bg.r) / 255.0;
    let g = f32::from(bg.g) / 255.0;
    let b = f32::from(bg.b) / 255.0;
    (0.2126 * r + 0.7152 * g + 0.0722 * b) >= 0.55
}

fn default_custom_source() -> String {
    "manual".to_owned()
}

fn default_custom_mode() -> String {
    "unknown".to_owned()
}

/// `settings.toml`의 `[[terminal_custom_themes]]` 항목(Ghostty·Warp·Orca 마이그레이션 호환).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomTerminalTheme {
    pub id: String,
    pub name: String,
    #[serde(default = "default_custom_source")]
    pub source: String,
    #[serde(default = "default_custom_mode")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub ansi: [String; 16],
}

impl CustomTerminalTheme {
    /// 설정 선택값(`custom:<id>`).
    #[must_use]
    pub fn selection_value(&self) -> String {
        format!("{CUSTOM_TERMINAL_THEME_PREFIX}{}", self.id)
    }

    /// 다크 테마인지 판정한다(`mode`가 명시되지 않았으면 배경 휘도로 추정).
    #[must_use]
    pub fn is_dark(&self) -> bool {
        if self.mode.eq_ignore_ascii_case("light") {
            return false;
        }
        if self.mode.eq_ignore_ascii_case("dark") {
            return true;
        }
        parse_hex_rgb(&self.background).is_none_or(|bg| !is_light_rgb(bg))
    }

    /// [`TerminalTheme`]으로 변환한다. `foreground`·`background`가 유효한 hex가 아니면 `None`.
    #[must_use]
    pub fn to_terminal_theme(&self) -> Option<TerminalTheme> {
        let fg = parse_hex_rgb(&self.foreground)?;
        let bg = parse_hex_rgb(&self.background)?;
        let fallback = if is_light_rgb(bg) {
            TERMINAL_LIGHT
        } else {
            TERMINAL_DARK
        };
        let cursor = parse_hex_rgb(&self.cursor).unwrap_or(fg);
        let mut ansi = fallback.ansi;
        for (idx, hex_str) in self.ansi.iter().enumerate() {
            if let Some(rgb) = parse_hex_rgb(hex_str) {
                ansi[idx] = rgb;
            }
        }
        Some(TerminalTheme {
            foreground: fg,
            background: bg,
            cursor,
            ansi,
        })
    }

    ///[`TerminalTheme`]에서 [`CustomTerminalTheme`]을 만든다.
    #[must_use]
    pub fn from_terminal_theme(
        id: String,
        name: String,
        source: &str,
        mode: Option<&str>,
        source_label: Option<String>,
        theme: TerminalTheme,
    ) -> Self {
        let inferred_mode = match mode {
            Some("light") => "light",
            Some("dark") => "dark",
            _ if is_light_rgb(theme.background) => "light",
            _ => "dark",
        };
        Self {
            id,
            name,
            source: source.to_owned(),
            mode: inferred_mode.to_owned(),
            source_label,
            foreground: format_hex_rgb(theme.foreground),
            background: format_hex_rgb(theme.background),
            cursor: format_hex_rgb(theme.cursor),
            ansi: theme.ansi.map(format_hex_rgb),
        }
    }
}

/// 레거시 별칭(`Ghostty Default StyleLight` 등)을 정본 내장 테마 이름으로 정규화한다.
#[must_use]
pub fn canonical_terminal_theme_name(name: &str) -> &str {
    let trimmed = name.trim();
    if trimmed.eq_ignore_ascii_case("Ghostty Default StyleLight")
        || trimmed.eq_ignore_ascii_case("Ghostty Default Style Light")
    {
        return DEFAULT_TERMINAL_THEME_LIGHT;
    }
    if let Some(found) = BUILTIN_TERMINAL_THEMES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(trimmed))
    {
        return found.name;
    }
    if let Some(stripped) = trimmed.strip_prefix("Builtin ")
        && let Some(found) = BUILTIN_TERMINAL_THEMES
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(stripped.trim()))
    {
        return found.name;
    }
    trimmed
}

/// 내장 30종 카탈로그에서 이름으로 테마를 찾는다(대소문자 및 `Builtin ` 접두사 호환).
#[must_use]
pub fn builtin_terminal_theme(name: &str) -> Option<&'static BuiltinTerminalTheme> {
    let canonical = canonical_terminal_theme_name(name);
    BUILTIN_TERMINAL_THEMES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(canonical))
}

/// 설정된 테마 식별자(`custom:<id>` 또는 내장 테마 이름)를 [`TerminalTheme`]으로 해석한다.
#[must_use]
pub fn resolve_terminal_theme(
    selection: &str,
    custom_themes: &[CustomTerminalTheme],
) -> Option<TerminalTheme> {
    let trimmed = selection.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(custom_id) = trimmed.strip_prefix(CUSTOM_TERMINAL_THEME_PREFIX)
        && let Some(found) = custom_themes
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(custom_id.trim()))
    {
        return found.to_terminal_theme();
    }
    if let Some(found) = custom_themes
        .iter()
        .find(|c| c.id.eq_ignore_ascii_case(trimmed) || c.name.eq_ignore_ascii_case(trimmed))
        && let Some(theme) = found.to_terminal_theme()
    {
        return Some(theme);
    }
    builtin_terminal_theme(trimmed).map(|b| b.theme)
}

/// 선택된 테마 식별자의 화면 표시용 이름을 돌려준다.
#[must_use]
pub fn terminal_theme_display_label(
    selection: &str,
    custom_themes: &[CustomTerminalTheme],
) -> String {
    let trimmed = selection.trim();
    if let Some(custom_id) = trimmed.strip_prefix(CUSTOM_TERMINAL_THEME_PREFIX)
        && let Some(found) = custom_themes
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(custom_id.trim()))
    {
        return found.name.clone();
    }
    if let Some(found) = builtin_terminal_theme(trimmed) {
        return found.name.to_owned();
    }
    if let Some(found) = custom_themes
        .iter()
        .find(|c| c.id.eq_ignore_ascii_case(trimmed) || c.name.eq_ignore_ascii_case(trimmed))
    {
        return found.name.clone();
    }
    trimmed.to_owned()
}

/// 테마 이름을 정규화된 ID 슬러그로 바꾼다.
#[must_use]
pub fn normalize_theme_slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.chars() {
        if ch.is_control() || ch == '\u{27}' || ch == '"' {
            continue;
        }
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == ':' || lower == '_' || lower == '-' {
            out.push(lower);
            last_dash = lower == '-';
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "theme".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// 커스텀 테마 목록에 새 테마들을 `id` 기준으로 병합하고 [`MAX_TERMINAL_CUSTOM_THEMES`] 상한을 유지한다.
/// 반환값은 새로 추가되거나 갱신된 테마 개수.
pub fn merge_custom_themes(
    existing: &mut Vec<CustomTerminalTheme>,
    incoming: impl IntoIterator<Item = CustomTerminalTheme>,
) -> usize {
    let mut changed = 0usize;
    for theme in incoming {
        if theme.id.trim().is_empty()
            || theme.name.trim().is_empty()
            || theme.to_terminal_theme().is_none()
        {
            continue;
        }
        if let Some(slot) = existing
            .iter_mut()
            .find(|c| c.id.eq_ignore_ascii_case(&theme.id))
        {
            if *slot != theme {
                *slot = theme;
                changed += 1;
            }
        } else {
            existing.push(theme);
            changed += 1;
        }
    }
    if existing.len() > MAX_TERMINAL_CUSTOM_THEMES {
        let drop_count = existing.len() - MAX_TERMINAL_CUSTOM_THEMES;
        existing.drain(0..drop_count);
    }
    changed
}

const WARP_COLOR_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

fn strip_yaml_comment(raw: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = raw.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\x27' && !in_double {
            in_single = !in_single;
        } else if b == b'"' && !in_single {
            in_double = !in_double;
        } else if b == b'#'
            && !in_single
            && !in_double
            && (i == 0 || bytes[i - 1].is_ascii_whitespace())
        {
            return raw.get(..i).unwrap_or(raw).trim();
        }
    }
    raw.trim()
}

fn unquote_scalar(raw: &str) -> &str {
    unquote(strip_yaml_comment(raw))
}

fn unquote(raw: &str) -> &str {
    let t = raw.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"'))
            || (t.starts_with('\u{27}') && t.ends_with('\u{27}')))
    {
        t.get(1..t.len() - 1).unwrap_or(t).trim()
    } else {
        t
    }
}

/// Warp YAML 테마 본문을 파싱해 [`CustomTerminalTheme`]으로 변환한다(Orca `parseWarpThemeYaml` 호환).
///
/// # Errors
/// YAML에 유효한 `background`·`foreground` 또는 최소 1개의 ANSI 색상이 없으면 사유를 돌려준다.
pub fn parse_warp_theme_yaml(
    content: &str,
    file_label: &str,
) -> Result<CustomTerminalTheme, String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Root,
        Background,
        Cursor,
        Accent,
        TerminalColors,
        Normal,
        Bright,
        Other,
    }

    let mut name: Option<String> = None;
    let mut background: Option<Rgb> = None;
    let mut foreground: Option<Rgb> = None;
    let mut cursor: Option<Rgb> = None;
    let mut accent: Option<Rgb> = None;
    let mut details: Option<String> = None;
    let mut normal: [Option<Rgb>; 8] = [None; 8];
    let mut bright: [Option<Rgb>; 8] = [None; 8];

    let mut section = Section::Root;
    let mut tc_indent = 0usize;
    let mut sub_indent = 0usize;

    for raw_line in content.lines() {
        let trimmed = strip_yaml_comment(raw_line);
        if trimmed.is_empty() || trimmed == "---" || trimmed == "..." {
            continue;
        }
        let indent = raw_line.len() - raw_line.trim_start().len();
        let Some((raw_key, raw_val)) = trimmed.split_once(':') else {
            continue;
        };
        let key = unquote_scalar(raw_key);
        let val = unquote_scalar(raw_val);

        if indent == 0 {
            section = Section::Root;
            match key {
                "name" if !val.is_empty() => name = Some(val.to_owned()),
                "background" => {
                    if let Some(rgb) = parse_hex_rgb(val) {
                        background = Some(rgb);
                    } else if val.is_empty() {
                        section = Section::Background;
                    }
                }
                "foreground" => {
                    if let Some(rgb) = parse_hex_rgb(val) {
                        foreground = Some(rgb);
                    }
                }
                "cursor" => {
                    if let Some(rgb) = parse_hex_rgb(val) {
                        cursor = Some(rgb);
                    } else if val.is_empty() {
                        section = Section::Cursor;
                    }
                }
                "accent" => {
                    if let Some(rgb) = parse_hex_rgb(val) {
                        accent = Some(rgb);
                    } else if val.is_empty() {
                        section = Section::Accent;
                    }
                }
                "details" if !val.is_empty() => details = Some(val.to_ascii_lowercase()),
                "terminal_colors" => {
                    section = Section::TerminalColors;
                    tc_indent = 0;
                }
                _ => {
                    if val.is_empty() {
                        section = Section::Other;
                    }
                }
            }
            continue;
        }

        match section {
            Section::Background if matches!(key, "top" | "bottom" | "left" | "right") => {
                if background.is_none() {
                    background = parse_hex_rgb(val);
                }
            }
            Section::Cursor if matches!(key, "top" | "bottom" | "left" | "right") => {
                if cursor.is_none() {
                    cursor = parse_hex_rgb(val);
                }
            }
            Section::Accent if matches!(key, "top" | "bottom" | "left" | "right") => {
                if accent.is_none() {
                    accent = parse_hex_rgb(val);
                }
            }
            Section::TerminalColors | Section::Normal | Section::Bright => {
                if key == "normal" && val.is_empty() {
                    section = Section::Normal;
                    sub_indent = indent;
                    let _ = tc_indent;
                } else if key == "bright" && val.is_empty() {
                    section = Section::Bright;
                    sub_indent = indent;
                } else if indent > sub_indent
                    && let Some(idx) = WARP_COLOR_NAMES.iter().position(|&n| n == key)
                    && let Some(rgb) = parse_hex_rgb(val)
                {
                    if section == Section::Normal {
                        normal[idx] = Some(rgb);
                    } else if section == Section::Bright {
                        bright[idx] = Some(rgb);
                    }
                }
            }
            Section::Root
            | Section::Other
            | Section::Background
            | Section::Cursor
            | Section::Accent => {}
        }
    }

    let Some(bg) = background else {
        return Err("missing background color".to_owned());
    };
    let Some(fg) = foreground else {
        return Err("missing foreground color".to_owned());
    };
    let has_ansi = normal.iter().any(Option::is_some) || bright.iter().any(Option::is_some);
    if !has_ansi {
        return Err("missing terminal_colors ANSI palette".to_owned());
    }

    let fallback_name = Path::new(file_label)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported Theme");
    let clean_name: String = name
        .as_deref()
        .unwrap_or(fallback_name)
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned();
    let final_name = if clean_name.is_empty() {
        "Imported Theme".to_owned()
    } else {
        clean_name
    };

    let is_light = if is_light_rgb(bg) {
        true
    } else {
        matches!(details.as_deref(), Some("lighter"))
    };
    let base = if is_light {
        TERMINAL_LIGHT
    } else {
        TERMINAL_DARK
    };
    let mut ansi = base.ansi;
    for i in 0..8 {
        if let Some(c) = normal[i] {
            ansi[i] = c;
        }
        if let Some(c) = bright[i].or(normal[i]) {
            ansi[i + 8] = c;
        }
    }
    let id = format!("warp:{}", normalize_theme_slug(&final_name));
    Ok(CustomTerminalTheme::from_terminal_theme(
        id,
        final_name,
        "warp",
        Some(if is_light { "light" } else { "dark" }),
        Some(file_label.to_owned()),
        TerminalTheme {
            foreground: fg,
            background: bg,
            cursor: cursor.or(accent).unwrap_or(fg),
            ansi,
        },
    ))
}

/// Warp 테마 디렉터리 자동 탐색 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WarpImportReport {
    pub themes: Vec<CustomTerminalTheme>,
    pub scanned_dirs: usize,
    pub skipped_files: usize,
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    }
}

/// 플랫폼별 Warp 테마 디렉터리 후보 목록(Orca `getWarpThemeDirectories` 호환).
#[must_use]
pub fn warp_theme_directories() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    #[cfg(target_os = "macos")]
    {
        for ch in [
            ".warp",
            ".warp-preview",
            ".warp-oss",
            ".warp-dev",
            ".warp-local",
            ".warp-integration",
        ] {
            dirs.push(home.join(ch).join("themes"));
        }
        dirs.push(
            home.join(".local")
                .join("share")
                .join("warp-terminal")
                .join("themes"),
        );
        dirs.push(PathBuf::from(
            "/Applications/Warp.app/Contents/Resources/themes",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local").join("share"));
        for ch in [
            "warp-terminal",
            "warp-terminal-preview",
            "warp-oss",
            "warp-terminal-dev",
        ] {
            dirs.push(data_home.join(ch).join("themes"));
        }
        dirs.push(home.join(".warp").join("themes"));
    }
    #[cfg(target_os = "windows")]
    {
        let app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.clone());
        for ch in ["Warp", "WarpPreview", "WarpOss", "WarpDev"] {
            dirs.push(app_data.join("warp").join(ch).join("data").join("themes"));
        }
    }
    dirs
}

/// 지정한 디렉터리 목록에서 Warp YAML(`.yaml`·`.yml`) 테마를 스캔해 가져온다.
#[must_use]
pub fn scan_warp_themes_in_dirs(dirs: &[PathBuf]) -> WarpImportReport {
    let mut report = WarpImportReport::default();
    for dir in dirs {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            continue;
        };
        report.scanned_dirs += 1;
        let mut entries: Vec<PathBuf> = read_dir
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                    })
            })
            .collect();
        entries.sort();
        for path in entries {
            let Ok(meta) = std::fs::metadata(&path) else {
                report.skipped_files += 1;
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_THEME_FILE_BYTES {
                report.skipped_files += 1;
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                report.skipped_files += 1;
                continue;
            };
            let label = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("warp.yaml");
            match parse_warp_theme_yaml(&body, label) {
                Ok(theme) => {
                    report.themes.push(theme);
                    if report.themes.len() >= MAX_TERMINAL_CUSTOM_THEMES {
                        return report;
                    }
                }
                Err(_) => {
                    report.skipped_files += 1;
                }
            }
        }
    }
    report
}

/// 시스템 기본 Warp 테마 폴더들을 스캔해 가져온다.
#[must_use]
pub fn discover_and_import_warp_themes() -> WarpImportReport {
    scan_warp_themes_in_dirs(&warp_theme_directories())
}

/// Ghostty 설정·테마 줄 단위 파서(Orca `parseGhosttyConfig` 호환).
#[must_use]
pub fn parse_ghostty_pairs(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_k, raw_v)) = line.split_once('=') else {
            continue;
        };
        let key = raw_k.trim();
        if key.is_empty() {
            continue;
        }
        // Ghostty는 줄 끝 주석이 없고 `#rrggbb` 값을 따옴표 없이 쓰므로 YAML 주석 규칙을 적용하지 않는다.
        let val = unquote(raw_v);
        out.push((key.to_owned(), val.to_owned()));
    }
    out
}

/// Ghostty 테마(또는 색상 키가 든 설정) 문자열을 [`CustomTerminalTheme`]으로 변환한다.
#[must_use]
pub fn parse_ghostty_theme(
    content: &str,
    name: &str,
    source_label: Option<String>,
) -> Option<CustomTerminalTheme> {
    let pairs = parse_ghostty_pairs(content);
    parse_ghostty_theme_from_pairs(&pairs, name, source_label)
}

#[must_use]
pub fn parse_ghostty_theme_from_pairs(
    pairs: &[(String, String)],
    name: &str,
    source_label: Option<String>,
) -> Option<CustomTerminalTheme> {
    let mut bg: Option<Rgb> = None;
    let mut fg: Option<Rgb> = None;
    let mut cursor: Option<Rgb> = None;
    let mut palette: [Option<Rgb>; 16] = [None; 16];
    let mut palette_count = 0usize;

    for (k, v) in pairs {
        match k.as_str() {
            "background" => bg = parse_hex_rgb(v).or(bg),
            "foreground" => fg = parse_hex_rgb(v).or(fg),
            "cursor-color" => cursor = parse_hex_rgb(v).or(cursor),
            "palette" => {
                if let Some((idx_str, hex_str)) = v.split_once('=')
                    && let Ok(idx) = idx_str.trim().parse::<usize>()
                    && idx < 16
                    && let Some(rgb) = parse_hex_rgb(hex_str)
                {
                    palette[idx] = Some(rgb);
                    palette_count += 1;
                }
            }
            _ => {}
        }
    }

    let bg = bg?;
    let fg = fg?;
    if palette_count == 0 && cursor.is_none() {
        // 최소 배경+전경이 있으면 허용하되 기본 ANSI 팔레트로 채운다.
    }
    let is_light = is_light_rgb(bg);
    let base = if is_light {
        TERMINAL_LIGHT
    } else {
        TERMINAL_DARK
    };
    let mut ansi = base.ansi;
    for (idx, item) in palette.into_iter().enumerate() {
        if let Some(c) = item {
            ansi[idx] = c;
        }
    }
    let clean_name = if name.trim().is_empty() {
        "Ghostty Theme"
    } else {
        name.trim()
    };
    let id = format!("ghostty:{}", normalize_theme_slug(clean_name));
    Some(CustomTerminalTheme::from_terminal_theme(
        id,
        clean_name.to_owned(),
        "ghostty",
        Some(if is_light { "light" } else { "dark" }),
        source_label,
        TerminalTheme {
            foreground: fg,
            background: bg,
            cursor: cursor.unwrap_or(fg),
            ansi,
        },
    ))
}

/// Ghostty 설정·테마 가져오기 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhosttyImportReport {
    pub found: bool,
    pub dark_theme: Option<String>,
    pub light_theme: Option<String>,
    pub use_separate_light_theme: Option<bool>,
    pub custom_themes: Vec<CustomTerminalTheme>,
    pub font_family: Option<String>,
    pub font_size: Option<u8>,
    pub opacity_pct: Option<u8>,
}

/// 플랫폼별 Ghostty 설정 파일 경로 목록(Orca `getGhosttyConfigPaths` 호환).
#[must_use]
pub fn ghostty_config_paths() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        dirs.push(xdg.join("ghostty"));
    } else {
        dirs.push(home.join(".config").join("ghostty"));
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(
            home.join("Library")
                .join("Application Support")
                .join("com.mitchellh.ghostty"),
        );
    }
    #[cfg(target_os = "windows")]
    if let Some(app_data) = std::env::var_os("APPDATA").map(PathBuf::from) {
        dirs.push(app_data.join("ghostty"));
    }
    let mut paths = Vec::with_capacity(dirs.len() * 2);
    for d in dirs {
        paths.push(d.join("config.ghostty"));
        paths.push(d.join("config"));
    }
    paths
}

/// 사용자 Ghostty 커스텀 테마 디렉터리(`~/.config/ghostty/themes`).
#[must_use]
pub fn ghostty_user_theme_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        vec![xdg.join("ghostty").join("themes")]
    } else {
        vec![home.join(".config").join("ghostty").join("themes")]
    }
}

/// 번들 포함 Ghostty 테마 검색 디렉터리(Orca `getGhosttyThemeSearchDirs` 호환).
#[must_use]
pub fn ghostty_all_theme_dirs() -> Vec<PathBuf> {
    let mut dirs = ghostty_user_theme_dirs();
    if let Some(res) = std::env::var_os("GHOSTTY_RESOURCES_DIR").map(PathBuf::from) {
        dirs.push(res.join("themes"));
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from(
            "/Applications/Ghostty.app/Contents/Resources/ghostty/themes",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        dirs.push(PathBuf::from("/usr/share/ghostty/themes"));
        dirs.push(PathBuf::from("/usr/local/share/ghostty/themes"));
    }
    dirs
}

fn resolve_named_ghostty_theme(
    theme_name: &str,
    theme_dirs: &[PathBuf],
    custom_out: &mut Vec<CustomTerminalTheme>,
) -> Option<String> {
    let trimmed = theme_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(b) = builtin_terminal_theme(trimmed) {
        return Some(b.name.to_owned());
    }
    // 상대 이름에 경로 구분자가 없으면 테마 디렉터리에서 찾는다.
    if !trimmed.contains('/') && !trimmed.contains('\\') && trimmed != "." && trimmed != ".." {
        for dir in theme_dirs {
            let candidate = dir.join(trimmed);
            if let Ok(meta) = std::fs::metadata(&candidate)
                && meta.is_file()
                && meta.len() <= MAX_THEME_FILE_BYTES
                && let Ok(body) = std::fs::read_to_string(&candidate)
                && let Some(custom) =
                    parse_ghostty_theme(&body, trimmed, Some("Ghostty".to_owned()))
            {
                let sel = custom.selection_value();
                merge_custom_themes(custom_out, [custom]);
                return Some(sel);
            }
        }
    }
    None
}

/// 지정한 설정 파일 경로와 테마 디렉터리에서 Ghostty 설정·테마를 가져온다.
#[must_use]
pub fn import_ghostty_from_paths(
    config_paths: &[PathBuf],
    user_theme_dirs: &[PathBuf],
    all_theme_dirs: &[PathBuf],
) -> GhosttyImportReport {
    let mut report = GhosttyImportReport::default();

    // 1. 사용자 `~/.config/ghostty/themes` 디렉터리의 커스텀 테마 파일을 스캔한다.
    for dir in user_theme_dirs {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = read_dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_THEME_FILE_BYTES {
                continue;
            }
            let Some(name) = path
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.starts_with('.'))
            else {
                continue;
            };
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(theme) = parse_ghostty_theme(&body, name, Some("Ghostty".to_owned())) {
                report.found = true;
                merge_custom_themes(&mut report.custom_themes, [theme]);
            }
        }
    }

    // 2. Ghostty 설정 파일을 순서대로 읽어 병합한다.
    let mut pairs = Vec::new();
    for path in config_paths {
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_CONFIG_FILE_BYTES {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        report.found = true;
        pairs.extend(parse_ghostty_pairs(&body));
    }

    if pairs.is_empty() {
        return report;
    }

    let mut raw_theme: Option<String> = None;
    for (k, v) in &pairs {
        match k.as_str() {
            "theme" if !v.is_empty() => raw_theme = Some(v.clone()),
            "font-family" if !v.is_empty() => report.font_family = Some(v.clone()),
            "font-size" => {
                if let Ok(sz) = v.parse::<f32>()
                    && sz.is_finite()
                    && (10.0..=24.0).contains(&sz)
                {
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "10.0..=24.0 범위 확인 후 반올림"
                    )]
                    let pt = sz.round() as u8;
                    report.font_size = Some(pt);
                }
            }
            "background-opacity" => {
                if let Ok(op) = v.parse::<f32>()
                    && op.is_finite()
                    && (0.0..=1.0).contains(&op)
                {
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "0.0..=1.0 * 100 반올림을 50..=100으로 clamp"
                    )]
                    let pct = (op * 100.0).round().clamp(50.0, 100.0) as u8;
                    report.opacity_pct = Some(pct);
                }
            }
            _ => {}
        }
    }

    // 3. `theme = ...` 해석 (`light:<name>,dark:<name>` 또는 단일 테마 이름).
    if let Some(theme_val) = raw_theme {
        let mut light_part: Option<&str> = None;
        let mut dark_part: Option<&str> = None;
        for part in theme_val.split(',').map(str::trim) {
            if let Some(l) = part.strip_prefix("light:") {
                light_part = Some(l.trim());
            } else if let Some(d) = part.strip_prefix("dark:") {
                dark_part = Some(d.trim());
            }
        }
        if light_part.is_some() || dark_part.is_some() {
            if let Some(d) = dark_part.and_then(|n| {
                resolve_named_ghostty_theme(n, all_theme_dirs, &mut report.custom_themes)
            }) {
                report.dark_theme = Some(d);
            }
            if let Some(l) = light_part.and_then(|n| {
                resolve_named_ghostty_theme(n, all_theme_dirs, &mut report.custom_themes)
            }) {
                report.light_theme = Some(l);
                report.use_separate_light_theme = Some(true);
            }
        } else if let Some(resolved) =
            resolve_named_ghostty_theme(&theme_val, all_theme_dirs, &mut report.custom_themes)
        {
            let is_light = resolve_terminal_theme(&resolved, &report.custom_themes)
                .is_some_and(|t| is_light_rgb(t.background));
            if is_light {
                report.light_theme = Some(resolved);
                report.use_separate_light_theme = Some(true);
            } else {
                report.dark_theme = Some(resolved);
            }
        }
    }

    // 4. 설정 파일에 직접 적힌 `background` + `foreground` 인라인 색상이 있으면 커스텀 테마로 가져온다.
    if let Some(inline_custom) =
        parse_ghostty_theme_from_pairs(&pairs, "Ghostty Config", Some("Ghostty config".to_owned()))
    {
        let sel = inline_custom.selection_value();
        let is_dark = inline_custom.is_dark();
        merge_custom_themes(&mut report.custom_themes, [inline_custom]);
        if is_dark {
            report.dark_theme = Some(sel);
        } else {
            report.light_theme = Some(sel);
            report.use_separate_light_theme = Some(true);
        }
    }

    report
}

/// 시스템 기본 경로에서 Ghostty 설정·테마를 탐색해 가져온다.
#[must_use]
pub fn discover_and_import_ghostty() -> GhosttyImportReport {
    import_ghostty_from_paths(
        &ghostty_config_paths(),
        &ghostty_user_theme_dirs(),
        &ghostty_all_theme_dirs(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_has_all_30_orca_themes_with_unique_names() {
        assert_eq!(BUILTIN_TERMINAL_THEMES.len(), 30);
        assert_eq!(BUILTIN_TERMINAL_THEMES[0].name, DEFAULT_TERMINAL_THEME_DARK);
        assert_eq!(BUILTIN_TERMINAL_THEMES[0].theme, TERMINAL_DARK);
        assert_eq!(
            BUILTIN_TERMINAL_THEMES[1].name,
            DEFAULT_TERMINAL_THEME_LIGHT
        );
        assert_eq!(BUILTIN_TERMINAL_THEMES[1].theme, TERMINAL_LIGHT);

        let mut names = std::collections::BTreeSet::new();
        for entry in BUILTIN_TERMINAL_THEMES {
            assert!(
                names.insert(entry.name),
                "duplicate builtin theme {}",
                entry.name
            );
            assert_eq!(
                builtin_terminal_theme(entry.name).map(|b| b.theme),
                Some(entry.theme)
            );
        }

        // Legacy Orca aliases resolve cleanly.
        assert_eq!(
            builtin_terminal_theme("Ghostty Default StyleLight").map(|b| b.theme),
            Some(TERMINAL_LIGHT)
        );
        assert_eq!(
            builtin_terminal_theme("Builtin Solarized Dark").map(|b| b.name),
            Some("Solarized Dark")
        );
        assert_eq!(
            builtin_terminal_theme("Builtin Tango Dark").map(|b| b.name),
            Some("Tango Dark")
        );
    }

    #[test]
    fn custom_theme_roundtrips_and_resolves_by_custom_prefix() {
        let yaml = r"
name: Cyber Neon
background: '#12131a'
foreground: '#e0e6ff'
cursor: '#ff00aa'
details: darker
terminal_colors:
  normal:
    black: '#101018'
    red: '#ff3366'
    green: '#00ff99'
    yellow: '#ffee00'
    blue: '#3399ff'
    magenta: '#cc66ff'
    cyan: '#00ffff'
    white: '#d0d6f0'
  bright:
    black: '#40445a'
    red: '#ff6688'
";
        let parsed = parse_warp_theme_yaml(yaml, "cyber_neon.yaml").unwrap();
        assert_eq!(parsed.id, "warp:cyber-neon");
        assert_eq!(parsed.name, "Cyber Neon");
        assert_eq!(parsed.source, "warp");
        assert!(parsed.is_dark());
        assert_eq!(parsed.selection_value(), "custom:warp:cyber-neon");

        let custom_list = vec![parsed.clone()];
        let resolved = resolve_terminal_theme("custom:warp:cyber-neon", &custom_list).unwrap();
        assert_eq!(resolved.background, term_rgb(0x12, 0x13, 0x1a));
        assert_eq!(resolved.foreground, term_rgb(0xe0, 0xe6, 0xff));
        assert_eq!(resolved.cursor, term_rgb(0xff, 0x00, 0xaa));
        assert_eq!(resolved.ansi[1], term_rgb(0xff, 0x33, 0x66));
        assert_eq!(resolved.ansi[9], term_rgb(0xff, 0x66, 0x88));
        // Unspecified bright green falls back to normal green.
        assert_eq!(resolved.ansi[10], term_rgb(0x00, 0xff, 0x99));
        assert_eq!(
            terminal_theme_display_label("custom:warp:cyber-neon", &custom_list),
            "Cyber Neon"
        );
    }

    #[test]
    fn ghostty_config_and_theme_import_parses_conditional_and_inline_colors() {
        let dir = std::env::temp_dir().join(format!("mk-ghostty-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let themes_dir = dir.join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();

        let custom_theme_file = themes_dir.join("MyCustomDark");
        std::fs::write(
            &custom_theme_file,
            "background = #111213\nforeground = #eeeeee\ncursor-color = #ffaa00\npalette = 0=#000000\npalette = 1=#ff0000\n",
        )
        .unwrap();

        let config_file = dir.join("config");
        std::fs::write(
            &config_file,
            "theme = light:Catppuccin Latte,dark:MyCustomDark\nfont-size = 15\nbackground-opacity = 0.85\n",
        )
        .unwrap();

        let report = import_ghostty_from_paths(
            std::slice::from_ref(&config_file),
            std::slice::from_ref(&themes_dir),
            std::slice::from_ref(&themes_dir),
        );
        assert!(report.found);
        assert_eq!(
            report.dark_theme.as_deref(),
            Some("custom:ghostty:mycustomdark")
        );
        assert_eq!(report.light_theme.as_deref(), Some("Catppuccin Latte"));
        assert_eq!(report.use_separate_light_theme, Some(true));
        assert_eq!(report.font_size, Some(15));
        assert_eq!(report.opacity_pct, Some(85));
        assert_eq!(report.custom_themes.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
