//! 고정 QR Model 2 version 10-L. 모바일 QR URL(최대 271 UTF-8 바이트)용 순수 Rust 인코더.
#![expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "version 10 고정 57x57 격자·274 data/346 total codewords 경계는 아래 알고리즘과 fixture로 검증한다"
)]

pub const SIZE: usize = 57;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qr {
    cells: Vec<Vec<bool>>,
}
impl Qr {
    pub fn encode(value: &str) -> Result<Self, &'static str> {
        if value.len() > 271 {
            return Err("pairing URL exceeds QR capacity");
        }
        let mut bits = Vec::new();
        append(&mut bits, 4, 4);
        append(&mut bits, u32::try_from(value.len()).unwrap_or(0), 16);
        for b in value.bytes() {
            append(&mut bits, u32::from(b), 8);
        }
        append(&mut bits, 0, 4);
        while bits.len() % 8 != 0 {
            bits.push(false);
        }
        let mut data: Vec<u8> = bits
            .chunks(8)
            .map(|b| b.iter().fold(0, |n, bit| (n << 1) | u8::from(*bit)))
            .collect();
        let mut pad = 0;
        while data.len() < 274 {
            data.push(if pad % 2 == 0 { 0xec } else { 0x11 });
            pad += 1;
        }
        let lengths = [68, 68, 69, 69];
        let mut offset = 0;
        let mut blocks = Vec::new();
        for length in lengths {
            let block = data[offset..offset + length].to_vec();
            let ecc = reed_solomon(&block);
            blocks.push((block, ecc));
            offset += length;
        }
        let mut codewords = Vec::new();
        for i in 0..69 {
            for (d, _) in &blocks {
                if let Some(v) = d.get(i) {
                    codewords.push(*v);
                }
            }
        }
        for i in 0..18 {
            for (_, e) in &blocks {
                codewords.push(e[i]);
            }
        }
        let mut matrix = vec![vec![false; SIZE]; SIZE];
        let mut reserved = matrix.clone();
        let mut mark = |x: usize, y: usize, dark: bool| {
            matrix[y][x] = dark;
            reserved[y][x] = true;
        };
        for c in 0..SIZE {
            mark(6, c, c % 2 == 0);
            mark(c, 6, c % 2 == 0);
        }
        for (cx, cy) in [(3, 3), (SIZE - 4, 3), (3, SIZE - 4)] {
            for dy in -4_i32..=4 {
                for dx in -4_i32..=4 {
                    let x = i32::try_from(cx).unwrap_or(0) + dx;
                    let y = i32::try_from(cy).unwrap_or(0) + dy;
                    if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
                        && x < SIZE
                        && y < SIZE
                    {
                        let d = dx.abs().max(dy.abs());
                        mark(x, y, d != 2 && d != 4);
                    }
                }
            }
        }
        for (i, x) in [6, 28, 50].into_iter().enumerate() {
            for (j, y) in [6, 28, 50].into_iter().enumerate() {
                if (i == 0 && (j == 0 || j == 2)) || (i == 2 && j == 0) {
                    continue;
                }
                for dy in -2_i32..=2 {
                    for dx in -2_i32..=2 {
                        let xx = usize::try_from(x + dx).unwrap_or(0);
                        let yy = usize::try_from(y + dy).unwrap_or(0);
                        mark(xx, yy, dx.abs().max(dy.abs()) != 1);
                    }
                }
            }
        }
        // L + mask 0, BCH(15,5), XOR mask 0x5412.
        let format = 0x77c4_u32;
        for i in 0..=5 {
            mark(8, i, (format >> i) & 1 != 0);
        }
        mark(8, 7, (format >> 6) & 1 != 0);
        mark(8, 8, (format >> 7) & 1 != 0);
        mark(7, 8, (format >> 8) & 1 != 0);
        for i in 9..15 {
            mark(14 - i, 8, (format >> i) & 1 != 0);
        }
        for i in 0..8 {
            mark(SIZE - 1 - i, 8, (format >> i) & 1 != 0);
        }
        for i in 8..15 {
            mark(8, SIZE - 15 + i, (format >> i) & 1 != 0);
        }
        mark(8, SIZE - 8, true);
        let mut remainder = 10_u32;
        for _ in 0..12 {
            remainder = (remainder << 1) ^ ((remainder >> 11) * 0x1f25);
        }
        let version = (10 << 12) | remainder;
        for i in 0..18 {
            let dark = (version >> i) & 1 != 0;
            let a = SIZE - 11 + i % 3;
            let b = i / 3;
            mark(a, b, dark);
            mark(b, a, dark);
        }
        let mut index = 0;
        let mut right = SIZE - 1;
        let mut up = true;
        while right > 0 {
            if right == 6 {
                right -= 1;
            }
            for v in 0..SIZE {
                let y = if up { SIZE - 1 - v } else { v };
                for x in [right, right - 1] {
                    if !reserved[y][x] {
                        let bit = codewords
                            .get(index / 8)
                            .is_some_and(|b| (b >> (7 - index % 8)) & 1 != 0);
                        matrix[y][x] = bit ^ ((x + y) % 2 == 0);
                        index += 1;
                    }
                }
            }
            up = !up;
            right = right.saturating_sub(2);
        }
        if index != 2768 {
            return Err("invalid QR layout");
        }
        Ok(Self { cells: matrix })
    }
    pub fn dark(&self, x: usize, y: usize) -> bool {
        self.cells
            .get(y)
            .and_then(|r| r.get(x))
            .copied()
            .unwrap_or(false)
    }
    /// 네 칸 quiet zone을 포함한, 밝은 배경 터미널용 Unicode 반블록 QR.
    pub fn terminal(&self) -> String {
        let mut out = String::new();
        for y in (0..SIZE + 8).step_by(2) {
            for x in 0..SIZE + 8 {
                let at = |yy: usize| x >= 4 && yy >= 4 && self.dark(x - 4, yy - 4);
                out.push(match (at(y), at(y + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    _ => ' ',
                });
            }
            out.push('\n');
        }
        out
    }
}
fn append(bits: &mut Vec<bool>, value: u32, count: u32) {
    for shift in (0..count).rev() {
        bits.push((value >> shift) & 1 != 0);
    }
}
fn multiply(mut a: u8, mut b: u8) -> u8 {
    let mut result = 0;
    for _ in 0..8 {
        if b & 1 != 0 {
            result ^= a;
        }
        let carry = a & 0x80 != 0;
        a <<= 1;
        if carry {
            a ^= 0x1d;
        }
        b >>= 1;
    }
    result
}
fn reed_solomon(data: &[u8]) -> Vec<u8> {
    let mut divisor = [0; 18];
    divisor[17] = 1;
    let mut root = 1;
    for _ in 0..18 {
        for j in 0..18 {
            divisor[j] = multiply(divisor[j], root);
            if j + 1 < 18 {
                divisor[j] ^= divisor[j + 1];
            }
        }
        root = multiply(root, 2);
    }
    let mut out = vec![0; 18];
    for b in data {
        let factor = *b ^ out[0];
        out.rotate_left(1);
        out[17] = 0;
        for i in 0..18 {
            out[i] ^= multiply(divisor[i], factor);
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independently_decoded_fixture() {
        let q = Qr::encode("https://example.test/?pair=fixture#key=fixture").unwrap();
        let mut hash = 14_695_981_039_346_656_037_u64;
        for y in 0..SIZE {
            for x in 0..SIZE {
                hash = (hash ^ u64::from(q.dark(x, y))).wrapping_mul(1_099_511_628_211);
            }
        }
        assert_eq!(hash, 0x4f7e_be8c_1aea_5ab3);
    }
    #[test]
    fn qr_capacity_quiet_zone_and_finder_patterns() {
        for text in [
            "https://example.test/?pair=fixture#key=fixture".to_owned(),
            "a".repeat(271),
        ] {
            let q = Qr::encode(&text).unwrap();
            assert!(q.dark(0, 0));
            assert!(!q.dark(1, 1));
            assert!(q.dark(3, 3));
            assert_eq!(q.terminal().lines().count(), 33);
        }
        assert!(Qr::encode(&"a".repeat(272)).is_err());
    }
}
