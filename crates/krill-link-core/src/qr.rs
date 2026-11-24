//! 외부 의존성 없는 순수 Rust QR 코드(ISO/IEC 18004 Model 2, Byte 모드, ECC-L, 버전 1..=16) 생성 및
//! 유니코드 반블록(`█`, `▀`, `▄`, ` `) 터미널 렌더러.

use crate::Error;

#[derive(Debug, Clone, Copy)]
struct VersionSpec {
    version: usize,
    ec_per_block: usize,
    g1_blocks: usize,
    g1_data: usize,
    g2_blocks: usize,
    g2_data: usize,
    align: &'static [usize],
}

impl VersionSpec {
    const fn total_data_codewords(&self) -> usize {
        self.g1_blocks * self.g1_data + self.g2_blocks * self.g2_data
    }

    const fn header_bytes(&self) -> usize {
        // 4비트 모드(0100) + 길이(V1..=9는 8비트, V10..=16은 16비트)
        if self.version <= 9 { 2 } else { 3 }
    }

    const fn max_payload_bytes(&self) -> usize {
        self.total_data_codewords()
            .saturating_sub(self.header_bytes())
    }

    const fn dimension(&self) -> usize {
        17 + self.version * 4
    }
}

const SPECS_L: &[VersionSpec] = &[
    VersionSpec {
        version: 1,
        ec_per_block: 7,
        g1_blocks: 1,
        g1_data: 19,
        g2_blocks: 0,
        g2_data: 0,
        align: &[],
    },
    VersionSpec {
        version: 2,
        ec_per_block: 10,
        g1_blocks: 1,
        g1_data: 34,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 18],
    },
    VersionSpec {
        version: 3,
        ec_per_block: 15,
        g1_blocks: 1,
        g1_data: 55,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 22],
    },
    VersionSpec {
        version: 4,
        ec_per_block: 20,
        g1_blocks: 1,
        g1_data: 80,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 26],
    },
    VersionSpec {
        version: 5,
        ec_per_block: 26,
        g1_blocks: 1,
        g1_data: 108,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 30],
    },
    VersionSpec {
        version: 6,
        ec_per_block: 18,
        g1_blocks: 2,
        g1_data: 68,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 34],
    },
    VersionSpec {
        version: 7,
        ec_per_block: 20,
        g1_blocks: 2,
        g1_data: 78,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 22, 38],
    },
    VersionSpec {
        version: 8,
        ec_per_block: 24,
        g1_blocks: 2,
        g1_data: 97,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 24, 42],
    },
    VersionSpec {
        version: 9,
        ec_per_block: 30,
        g1_blocks: 2,
        g1_data: 116,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 26, 46],
    },
    VersionSpec {
        version: 10,
        ec_per_block: 18,
        g1_blocks: 2,
        g1_data: 68,
        g2_blocks: 2,
        g2_data: 69,
        align: &[6, 28, 50],
    },
    VersionSpec {
        version: 11,
        ec_per_block: 20,
        g1_blocks: 4,
        g1_data: 81,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 30, 54],
    },
    VersionSpec {
        version: 12,
        ec_per_block: 24,
        g1_blocks: 2,
        g1_data: 92,
        g2_blocks: 2,
        g2_data: 93,
        align: &[6, 32, 58],
    },
    VersionSpec {
        version: 13,
        ec_per_block: 26,
        g1_blocks: 4,
        g1_data: 107,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 34, 62],
    },
    VersionSpec {
        version: 14,
        ec_per_block: 30,
        g1_blocks: 3,
        g1_data: 115,
        g2_blocks: 1,
        g2_data: 116,
        align: &[6, 26, 46, 66],
    },
    VersionSpec {
        version: 15,
        ec_per_block: 22,
        g1_blocks: 5,
        g1_data: 87,
        g2_blocks: 1,
        g2_data: 88,
        align: &[6, 26, 48, 70],
    },
    VersionSpec {
        version: 16,
        ec_per_block: 24,
        g1_blocks: 5,
        g1_data: 98,
        g2_blocks: 1,
        g2_data: 99,
        align: &[6, 26, 50, 74],
    },
];

/// QR 코드 격자.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrMatrix {
    size: usize,
    modules: Vec<bool>,
}

impl QrMatrix {
    /// 한 변의 모듈 수.
    pub fn size(&self) -> usize {
        self.size
    }

    /// `(row, col)` 모듈이 검은색(`true`)인지 돌려준다.
    pub fn get(&self, row: usize, col: usize) -> bool {
        self.modules
            .get(row.saturating_mul(self.size).saturating_add(col))
            .copied()
            .unwrap_or(false)
    }

    /// 터미널 출력용 유니코드 반블록 문자열(조용한 영역 여백 2모듈 포함)로 렌더링한다.
    pub fn render_unicode(&self) -> String {
        const QUIET: usize = 2;
        let total = self.size + QUIET * 2;
        let mut out = String::new();
        let mut y = 0;
        while y < total {
            for x in 0..total {
                let top = is_dark_with_quiet(self, y, x, QUIET);
                let bottom = if y + 1 < total {
                    is_dark_with_quiet(self, y + 1, x, QUIET)
                } else {
                    false
                };
                // 어두운/밝은 터미널 테마 모두에서 스캔되도록 반전 없는 블록 문자를 쓴다.
                let ch = match (top, bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                };
                out.push(ch);
            }
            out.push('\n');
            y += 2;
        }
        out
    }
}

fn is_dark_with_quiet(qr: &QrMatrix, y: usize, x: usize, quiet: usize) -> bool {
    if y < quiet || x < quiet {
        return false;
    }
    let row = y - quiet;
    let col = x - quiet;
    if row >= qr.size || col >= qr.size {
        return false;
    }
    qr.get(row, col)
}

/// 문자열을 QR 코드로 인코딩해 유니코드 반블록 문자열로 돌려준다.
pub fn render_qr_unicode(text: &str) -> Result<String, Error> {
    let matrix = encode_qr(text.as_bytes())?;
    Ok(matrix.render_unicode())
}

/// 바이트 슬라이스를 QR 코드(`QrMatrix`)로 인코딩한다.
pub fn encode_qr(data: &[u8]) -> Result<QrMatrix, Error> {
    let spec = SPECS_L
        .iter()
        .find(|s| data.len() <= s.max_payload_bytes())
        .ok_or(Error::QrPayloadTooLarge(data.len()))?;

    let data_codewords = encode_data_codewords(data, spec);
    let all_codewords = interleave_with_ec(&data_codewords, spec);

    let size = spec.dimension();
    let total_cells = size * size;
    let mut modules = vec![false; total_cells];
    let mut is_function = vec![false; total_cells];

    place_function_patterns(&mut modules, &mut is_function, spec);
    place_data_bits(&mut modules, &is_function, size, &all_codewords);

    // 마스크 0..8 중 가장 패널티가 낮은 마스크를 선택한다.
    let mut best_mask = 0u8;
    let mut best_penalty = usize::MAX;
    let mut best_modules = modules.clone();
    for mask in 0u8..8 {
        let mut candidate = modules.clone();
        apply_mask(&mut candidate, &is_function, size, mask);
        write_format_bits(&mut candidate, size, mask);
        let penalty = penalty_score(&candidate, size);
        if penalty < best_penalty {
            best_penalty = penalty;
            best_mask = mask;
            best_modules = candidate;
        }
    }
    let _ = best_mask;

    Ok(QrMatrix {
        size,
        modules: best_modules,
    })
}

fn push_bits(bits: &mut Vec<bool>, val: usize, count: usize) {
    for i in (0..count).rev() {
        bits.push(((val >> i) & 1) != 0);
    }
}

fn encode_data_codewords(data: &[u8], spec: &VersionSpec) -> Vec<u8> {
    let mut bits = Vec::with_capacity(spec.total_data_codewords() * 8);
    // Byte mode indicator: 0100
    push_bits(&mut bits, 0b0100, 4);
    let count_bits = if spec.version <= 9 { 8 } else { 16 };
    push_bits(&mut bits, data.len(), count_bits);
    for &b in data {
        push_bits(&mut bits, usize::from(b), 8);
    }
    let capacity_bits = spec.total_data_codewords() * 8;
    let terminator = capacity_bits.saturating_sub(bits.len()).min(4);
    push_bits(&mut bits, 0, terminator);
    while !bits.len().is_multiple_of(8) {
        bits.push(false);
    }

    let mut codewords = Vec::with_capacity(spec.total_data_codewords());
    let mut idx = 0;
    while idx + 8 <= bits.len() {
        let mut byte = 0u8;
        for b in &bits[idx..idx + 8] {
            byte = (byte << 1) | u8::from(*b);
        }
        codewords.push(byte);
        idx += 8;
    }

    let pad_bytes = [0xECu8, 0x11u8];
    let mut pad_idx = 0;
    while codewords.len() < spec.total_data_codewords() {
        codewords.push(pad_bytes[pad_idx & 1]);
        pad_idx += 1;
    }
    codewords
}

fn gf_mul(mut x: u8, mut y: u8) -> u8 {
    let mut z = 0u8;
    for _ in 0..8 {
        if (y & 1) != 0 {
            z ^= x;
        }
        let hi = (x & 0x80) != 0;
        x <<= 1;
        if hi {
            x ^= 0x1D;
        }
        y >>= 1;
    }
    z
}

fn rs_generator_poly(degree: usize) -> Vec<u8> {
    let mut poly = vec![0u8; degree];
    if let Some(last) = poly.last_mut() {
        *last = 1;
    }
    let mut root = 1u8;
    for _ in 0..degree {
        for i in 0..degree {
            let next = if i + 1 < degree { poly[i + 1] } else { 0 };
            poly[i] = gf_mul(poly[i], root) ^ next;
        }
        root = gf_mul(root, 0x02);
    }
    poly
}

fn rs_compute_remainder(data: &[u8], generator: &[u8]) -> Vec<u8> {
    let mut result = vec![0u8; generator.len()];
    for &byte in data {
        let factor = byte ^ result.first().copied().unwrap_or(0);
        result.rotate_left(1);
        if let Some(last) = result.last_mut() {
            *last = 0;
        }
        for (r, &g) in result.iter_mut().zip(generator.iter()) {
            *r ^= gf_mul(g, factor);
        }
    }
    result
}

fn interleave_with_ec(data_codewords: &[u8], spec: &VersionSpec) -> Vec<u8> {
    let generator = rs_generator_poly(spec.ec_per_block);
    let total_blocks = spec.g1_blocks + spec.g2_blocks;
    let mut data_blocks = Vec::with_capacity(total_blocks);
    let mut ec_blocks = Vec::with_capacity(total_blocks);

    let mut offset = 0;
    for block_idx in 0..total_blocks {
        let block_len = if block_idx < spec.g1_blocks {
            spec.g1_data
        } else {
            spec.g2_data
        };
        let slice = data_codewords
            .get(offset..offset + block_len)
            .unwrap_or_default();
        offset += block_len;
        ec_blocks.push(rs_compute_remainder(slice, &generator));
        data_blocks.push(slice.to_vec());
    }

    let max_data_len = spec.g1_data.max(spec.g2_data);
    let mut out =
        Vec::with_capacity(spec.total_data_codewords() + total_blocks * spec.ec_per_block);

    for i in 0..max_data_len {
        for b in &data_blocks {
            if let Some(&cw) = b.get(i) {
                out.push(cw);
            }
        }
    }
    for i in 0..spec.ec_per_block {
        for b in &ec_blocks {
            if let Some(&cw) = b.get(i) {
                out.push(cw);
            }
        }
    }
    out
}

fn set_func(
    modules: &mut [bool],
    is_func: &mut [bool],
    size: usize,
    row: usize,
    col: usize,
    dark: bool,
) {
    if row < size && col < size {
        let idx = row * size + col;
        if let (Some(m), Some(f)) = (modules.get_mut(idx), is_func.get_mut(idx)) {
            *m = dark;
            *f = true;
        }
    }
}

fn draw_finder(modules: &mut [bool], is_func: &mut [bool], size: usize, top: usize, left: usize) {
    for dy in 0usize..7 {
        for dx in 0usize..7 {
            let dist = dy.abs_diff(3).max(dx.abs_diff(3));
            let dark = dist != 2;
            set_func(modules, is_func, size, top + dy, left + dx, dark);
        }
    }
}

fn draw_alignment(
    modules: &mut [bool],
    is_func: &mut [bool],
    size: usize,
    center_r: usize,
    center_c: usize,
) {
    for dy in 0usize..5 {
        for dx in 0usize..5 {
            let dist = dy.abs_diff(2).max(dx.abs_diff(2));
            let dark = dist != 1;
            set_func(
                modules,
                is_func,
                size,
                center_r + dy - 2,
                center_c + dx - 2,
                dark,
            );
        }
    }
}

fn place_function_patterns(modules: &mut [bool], is_func: &mut [bool], spec: &VersionSpec) {
    let size = spec.dimension();

    // 1. Finder patterns + white separators
    draw_finder(modules, is_func, size, 0, 0);
    draw_finder(modules, is_func, size, 0, size - 7);
    draw_finder(modules, is_func, size, size - 7, 0);

    for i in 0..8 {
        set_func(modules, is_func, size, 7, i, false);
        set_func(modules, is_func, size, i, 7, false);
        set_func(modules, is_func, size, 7, size - 8 + i, false);
        set_func(modules, is_func, size, i, size - 8, false);
        set_func(modules, is_func, size, size - 8, i, false);
        set_func(modules, is_func, size, size - 8 + i, 7, false);
    }

    // 2. Timing patterns
    for i in 8..size - 8 {
        let dark = i % 2 == 0;
        set_func(modules, is_func, size, 6, i, dark);
        set_func(modules, is_func, size, i, 6, dark);
    }

    // 3. Alignment patterns (skipping finder overlaps)
    for (ir, &r) in spec.align.iter().enumerate() {
        for (ic, &c) in spec.align.iter().enumerate() {
            let last = spec.align.len() - 1;
            if (ir == 0 && (ic == 0 || ic == last)) || (ir == last && ic == 0) {
                continue;
            }
            draw_alignment(modules, is_func, size, r, c);
        }
    }

    // 4. Dark module & reserved format info cells
    set_func(modules, is_func, size, size - 8, 8, true);
    for i in 0..9 {
        if i != 6 {
            set_func(modules, is_func, size, 8, i, false);
            set_func(modules, is_func, size, i, 8, false);
        }
    }
    for i in 0..8 {
        set_func(modules, is_func, size, 8, size - 1 - i, false);
        if i < 7 {
            set_func(modules, is_func, size, size - 1 - i, 8, false);
        }
    }
    set_func(modules, is_func, size, size - 8, 8, true);

    // 5. Version info (V >= 7)
    if spec.version >= 7 {
        let mut rem = spec.version;
        for _ in 0..12 {
            rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
        }
        let bits = (spec.version << 12) | rem;
        for i in 0..18 {
            let dark = ((bits >> i) & 1) != 0;
            let a = size - 11 + (i % 3);
            let b = i / 3;
            set_func(modules, is_func, size, a, b, dark);
            set_func(modules, is_func, size, b, a, dark);
        }
    }
}

fn place_data_bits(modules: &mut [bool], is_func: &[bool], size: usize, codewords: &[u8]) {
    let mut bit_idx = 0usize;
    let total_bits = codewords.len() * 8;
    let mut right_col = size - 1;
    let mut upward = true;

    loop {
        if right_col == 6 {
            right_col -= 1;
        }
        for step in 0..size {
            let row = if upward { size - 1 - step } else { step };
            for dx in 0..2 {
                let col = right_col - dx;
                let cell = row * size + col;
                if !is_func.get(cell).copied().unwrap_or(true) {
                    let dark = if bit_idx < total_bits {
                        let cw = codewords.get(bit_idx / 8).copied().unwrap_or(0);
                        ((cw >> (7 - (bit_idx % 8))) & 1) != 0
                    } else {
                        false
                    };
                    if let Some(m) = modules.get_mut(cell) {
                        *m = dark;
                    }
                    bit_idx += 1;
                }
            }
        }
        upward = !upward;
        if right_col < 2 {
            break;
        }
        right_col -= 2;
    }
}

fn apply_mask(modules: &mut [bool], is_func: &[bool], size: usize, mask: u8) {
    for r in 0..size {
        for c in 0..size {
            let idx = r * size + c;
            if is_func.get(idx).copied().unwrap_or(true) {
                continue;
            }
            let invert = match mask {
                0 => (r + c).is_multiple_of(2),
                1 => r.is_multiple_of(2),
                2 => c.is_multiple_of(3),
                3 => (r + c).is_multiple_of(3),
                4 => (r / 2 + c / 3).is_multiple_of(2),
                5 => (r * c) % 2 + (r * c) % 3 == 0,
                6 => ((r * c) % 2 + (r * c) % 3).is_multiple_of(2),
                _ => ((r + c) % 2 + (r * c) % 3).is_multiple_of(2),
            };
            if invert && let Some(m) = modules.get_mut(idx) {
                *m = !*m;
            }
        }
    }
}

fn write_format_bits(modules: &mut [bool], size: usize, mask: u8) {
    // ECC Level L format bits = 01
    let data = (0b01usize << 3) | usize::from(mask & 0x07);
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    let bits = ((data << 10) | rem) ^ 0x5412;

    let set = |mods: &mut [bool], r: usize, c: usize, val: bool| {
        if let Some(m) = mods.get_mut(r * size + c) {
            *m = val;
        }
    };

    // First copy around top-left finder
    for i in 0..6 {
        set(modules, i, 8, ((bits >> i) & 1) != 0);
    }
    set(modules, 7, 8, ((bits >> 6) & 1) != 0);
    set(modules, 8, 8, ((bits >> 7) & 1) != 0);
    set(modules, 8, 7, ((bits >> 8) & 1) != 0);
    for i in 9..15 {
        set(modules, 8, 14 - i, ((bits >> i) & 1) != 0);
    }

    // Second copy around top-right and bottom-left finders
    for i in 0..8 {
        set(modules, 8, size - 1 - i, ((bits >> i) & 1) != 0);
    }
    for i in 8..15 {
        set(modules, size - 15 + i, 8, ((bits >> i) & 1) != 0);
    }
    set(modules, size - 8, 8, true);
}

fn penalty_score(modules: &[bool], size: usize) -> usize {
    let mut penalty = 0usize;
    let get = |r: usize, c: usize| modules.get(r * size + c).copied().unwrap_or(false);

    // Rule 1: 5+ consecutive same-color modules in row/col
    for r in 0..size {
        let mut run_color = get(r, 0);
        let mut run_len = 1usize;
        for c in 1..size {
            let color = get(r, c);
            if color == run_color {
                run_len += 1;
                if run_len == 5 {
                    penalty += 3;
                } else if run_len > 5 {
                    penalty += 1;
                }
            } else {
                run_color = color;
                run_len = 1;
            }
        }
    }
    for c in 0..size {
        let mut run_color = get(0, c);
        let mut run_len = 1usize;
        for r in 1..size {
            let color = get(r, c);
            if color == run_color {
                run_len += 1;
                if run_len == 5 {
                    penalty += 3;
                } else if run_len > 5 {
                    penalty += 1;
                }
            } else {
                run_color = color;
                run_len = 1;
            }
        }
    }
    // Rule 2: 2x2 blocks
    for r in 0..size.saturating_sub(1) {
        for c in 0..size.saturating_sub(1) {
            let color = get(r, c);
            if get(r, c + 1) == color && get(r + 1, c) == color && get(r + 1, c + 1) == color {
                penalty += 3;
            }
        }
    }
    penalty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_encodes_pairing_link_and_renders_unicode() {
        let url = "krill://pair?t=1b892674bdc3318a7d1556033fbf24120a63746e94640aed35d5a77e5c9ce66d0100c0a8db64a88e03&k=0123456789abcdef0123456789abcdef";
        let qr = encode_qr(url.as_bytes()).unwrap();
        assert!(qr.size() >= 21);
        // 좌상단·우상단·좌하단 Finder 패턴 모서리 확인
        assert!(qr.get(0, 0));
        assert!(qr.get(0, 6));
        assert!(qr.get(6, 0));
        assert!(qr.get(3, 3));
        assert!(!qr.get(1, 1));
        assert!(qr.get(0, qr.size() - 1));
        assert!(qr.get(qr.size() - 1, 0));

        let s = qr.size();
        for i in 0..6 {
            assert_eq!(qr.get(i, 8), qr.get(8, s - 1 - i));
        }
        assert_eq!(qr.get(7, 8), qr.get(8, s - 7));
        assert_eq!(qr.get(8, 8), qr.get(8, s - 8));
        assert_eq!(qr.get(8, 7), qr.get(s - 7, 8));
        for i in 9..15 {
            assert_eq!(qr.get(8, 14 - i), qr.get(s - 15 + i, 8));
        }

        let rendered = qr.render_unicode();
        assert!(rendered.contains('█'));
        assert!(rendered.lines().count() >= 12);
    }

    #[test]
    fn qr_rejects_oversized_payload() {
        let huge = vec![b'a'; 700];
        assert!(encode_qr(&huge).is_err());
    }
}
