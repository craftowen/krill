//! 초대 링크 QR 코드 격자 트레이트 및 gpui 모듈 격자 렌더러(MK-133b, OWE-669).
//!
//! `crates/krill-link/src/qr.rs`와 외부 크레이트 중복 의존을 피하기 위해 QR 행렬(`Vec<Vec<bool>>`)을
//! [`QrMatrixSource`] 트레이트로 주입받는다.

use gpui::{Div, Stateful, div, prelude::*, px};
use sha2::{Digest as _, Sha256};

use crate::ui::hex;

/// 기본 QR 격자 한 변 크기(Version 2 규격 25×25).
pub const DEFAULT_QR_SIZE: usize = 25;

/// 초대 링크 문자열을 QR 모듈 행렬(`true` = 어두운 칸, `false` = 밝은 칸)로 바꾸는 경계.
///
/// 실제 연결은 `real::RealQrMatrix`가 공개 krill-link 인코더를 호출한다.
pub trait QrMatrixSource: Send + Sync + std::fmt::Debug {
    fn encode(&self, link: &str) -> Result<Vec<Vec<bool>>, String>;
}

/// 모의 백엔드·미리보기용 결정적 QR 모듈 행렬 생성기.
// 테스트·미리보기 전용이며 실제 초대에는 RealQrMatrix를 사용한다.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultQrMatrix;

impl QrMatrixSource for DefaultQrMatrix {
    fn encode(&self, link: &str) -> Result<Vec<Vec<bool>>, String> {
        Ok(build_qr_matrix(link))
    }
}

/// 고정된 `Vec<Vec<bool>>` 행렬을 그대로 돌려주는 어댑터(테스트·직접 주입용).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticQrMatrix(pub Vec<Vec<bool>>);

impl QrMatrixSource for StaticQrMatrix {
    fn encode(&self, _link: &str) -> Result<Vec<Vec<bool>>, String> {
        Ok(self.0.clone())
    }
}

/// 초대 링크 문자열로부터 25×25 QR 모듈 격자를 결정적으로 생성한다.
#[must_use]
pub fn build_qr_matrix(link: &str) -> Vec<Vec<bool>> {
    let n = DEFAULT_QR_SIZE;
    let mut grid = vec![vec![false; n]; n];
    let mut reserved = vec![vec![false; n]; n];

    // 1. 세 모서리의 7×7 위치 탐지 패턴(Finder Pattern) + 1칸 여백(Separator).
    for (r0, c0) in [(0usize, 0usize), (0, n - 7), (n - 7, 0)] {
        place_finder(&mut grid, &mut reserved, r0, c0);
    }

    // 2. 타이밍 패턴(6행, 6열).
    for i in 8..(n - 8) {
        let dark = i % 2 == 0;
        if let Some(row) = grid.get_mut(6)
            && let Some(cell) = row.get_mut(i)
        {
            *cell = dark;
        }
        if let Some(row) = reserved.get_mut(6)
            && let Some(cell) = row.get_mut(i)
        {
            *cell = true;
        }
        if let Some(row) = grid.get_mut(i)
            && let Some(cell) = row.get_mut(6)
        {
            *cell = dark;
        }
        if let Some(row) = reserved.get_mut(i)
            && let Some(cell) = row.get_mut(6)
        {
            *cell = true;
        }
    }

    // 3. Version 2 정렬 패턴(Alignment Pattern, 중심 18,18 → 좌상단 16,16..=20,20).
    place_alignment(&mut grid, &mut reserved, 16, 16);

    // 4. 고정 어두운 모듈(Dark Module).
    if let Some(row) = grid.get_mut(n - 8)
        && let Some(cell) = row.get_mut(8)
    {
        *cell = true;
    }
    if let Some(row) = reserved.get_mut(n - 8)
        && let Some(cell) = row.get_mut(8)
    {
        *cell = true;
    }

    // 5. 나머지 데이터 영역은 링크의 SHA-256 다이제스트 비트로 결정적으로 채운다.
    let mut bit_idx = 0usize;
    let mut digest = Sha256::digest(link.as_bytes());
    let mut block_counter = 0u32;

    for r in 0..n {
        for c in 0..n {
            let is_res = reserved
                .get(r)
                .and_then(|row| row.get(c))
                .copied()
                .unwrap_or(false);
            if is_res {
                continue;
            }
            let byte_pos = (bit_idx / 8) % 32;
            if bit_idx > 0 && bit_idx.is_multiple_of(256) {
                block_counter = block_counter.wrapping_add(1);
                let mut hasher = Sha256::new();
                hasher.update(link.as_bytes());
                hasher.update(block_counter.to_le_bytes());
                digest = hasher.finalize();
            }
            let byte = digest.get(byte_pos).copied().unwrap_or(0);
            let bit = ((byte >> (bit_idx % 8)) & 1) == 1;
            if let Some(row) = grid.get_mut(r)
                && let Some(cell) = row.get_mut(c)
            {
                *cell = bit;
            }
            bit_idx = bit_idx.saturating_add(1);
        }
    }

    grid
}

fn place_finder(grid: &mut [Vec<bool>], reserved: &mut [Vec<bool>], r0: usize, c0: usize) {
    let n = grid.len();
    let r_start = r0.saturating_sub(1);
    let c_start = c0.saturating_sub(1);
    let r_end = (r0 + 8).min(n);
    let c_end = (c0 + 8).min(n);

    for r in r_start..r_end {
        for c in c_start..c_end {
            if let Some(row) = reserved.get_mut(r)
                && let Some(cell) = row.get_mut(c)
            {
                *cell = true;
            }
            let in_box = r >= r0 && r < r0 + 7 && c >= c0 && c < c0 + 7;
            let dark = if in_box {
                let dr = r - r0;
                let dc = c - c0;
                dr == 0
                    || dr == 6
                    || dc == 0
                    || dc == 6
                    || ((2..=4).contains(&dr) && (2..=4).contains(&dc))
            } else {
                false
            };
            if let Some(row) = grid.get_mut(r)
                && let Some(cell) = row.get_mut(c)
            {
                *cell = dark;
            }
        }
    }
}

fn place_alignment(grid: &mut [Vec<bool>], reserved: &mut [Vec<bool>], r0: usize, c0: usize) {
    for dr in 0..5 {
        for dc in 0..5 {
            let r = r0 + dr;
            let c = c0 + dc;
            if let Some(row) = reserved.get_mut(r)
                && let Some(cell) = row.get_mut(c)
            {
                *cell = true;
            }
            let dark = dr == 0 || dr == 4 || dc == 0 || dc == 4 || (dr == 2 && dc == 2);
            if let Some(row) = grid.get_mut(r)
                && let Some(cell) = row.get_mut(c)
            {
                *cell = dark;
            }
        }
    }
}

/// QR 행렬의 `(행 수, 열 수)`를 돌려준다.
#[must_use]
pub fn matrix_dimensions(matrix: &[Vec<bool>]) -> (usize, usize) {
    let rows = matrix.len();
    let cols = matrix.first().map_or(0, Vec::len);
    (rows, cols)
}

/// `Vec<Vec<bool>>` QR 행렬을 gpui 모듈 격자 요소로 그린다.
#[must_use]
pub fn render_qr_matrix(matrix: &[Vec<bool>], module_px: f32) -> Stateful<Div> {
    let matrix = matrix.to_vec();
    let n = matrix.len();
    let cell = module_px.max(2.0).min(256.0 / index_pixels(n.max(1)));
    let edge = (index_pixels(n) + 8.) * cell;
    div()
        .id("link-invite-qr")
        .debug_selector(|| "link-invite-qr".into())
        .bg(hex(0x00ff_ffff))
        .child(
            gpui::canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    for (row, cells) in matrix.iter().enumerate() {
                        for (col, dark) in cells.iter().enumerate() {
                            if *dark {
                                let origin = bounds.origin
                                    + gpui::point(
                                        px((index_pixels(col) + 4.) * cell),
                                        px((index_pixels(row) + 4.) * cell),
                                    );
                                window.paint_quad(gpui::fill(
                                    gpui::Bounds::new(origin, gpui::size(px(cell), px(cell))),
                                    hex(0x0000_0000),
                                ));
                            }
                        }
                    }
                },
            )
            .w(px(edge))
            .h(px(edge)),
        )
}

fn index_pixels(index: usize) -> f32 {
    f32::from(u16::try_from(index).unwrap_or(u16::MAX))
}
