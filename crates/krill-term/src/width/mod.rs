//! 이모지·ZWJ·VS16 폭 보정(MK-058, T3#15): 엔진 셀은 그대로 두고 셰이핑 단위만 묶는다.
//!
//! 엔진(`alacritty_terminal` 0.26)은 문자 단위 `unicode-width`로 칸을 잡는다. 그래서 VS16 이모지 표시
//! (`❤\u{FE0F}` → 1칸)와 ZWJ 클러스터(가족 `👨‍👩‍👧‍👦` → 8칸)는 기대 렌더 폭(문자열 기준 2칸)과 어긋난다.
//! 주소 지정(PTY 프로그램의 `wcwidth` 계산)은 엔진 셀이 기준이므로 셀을 옮기지 않는다(지휘자 결정 A').
//! 대신 같은 문자소(결합 단위)에 속한 칸들을 셰이핑 한 번으로 묶어 글리프가 쪼개지지 않게 한다:
//!
//! - ZWJ로 이어진 칸들은 한 묶음으로 셰이핑하고 묶음의 왼쪽 2칸에 그린다. 나머지 칸은 빈칸으로 둔다(겹침 없음).
//! - VS16 1칸 묶음은 1칸에 맞춰 그리되, 바로 오른쪽 칸이 빈칸이면 2칸으로 넘쳐 그려도 된다.
//!
//! 묶음 판정은 엔진이 0폭 문자를 앞 칸에 붙이는 방식과 같다: 앞 칸의 `zerowidth`에 ZWJ가 있으면 뒤 칸과 잇고,
//! 뒤 칸의 본문(`ch`)이 뒤에 붙는 간격 꾸밈(피부색 `U+1F3FB..=U+1F3FF`, 둘러싸는 `U+20E3`)이면 앞 칸과 잇는다.
//! 이 모듈은 `krill-proto` 화면 셀만 보고 엔진·gpui에 닿지 않는다(`krill-term`은 엔진 중립, CONVENTIONS T5).

use super::grid::{Cell, Flags};

/// ZWJ(뒤 칸과 잇는다).
pub const ZWJ: char = '\u{200D}';
/// VS16 이모지 표시 선택자(앞 글자를 이모지로 그린다).
pub const VS16: char = '\u{FE0F}';
/// 둘러싸는 결합 문자(키캡 `1\u{FE0F}\u{20E3}`). 앞 글자에 붙는다.
const ENCLOSING: char = '\u{20E3}';
/// 피부색 꾸밈(`👩` + `🏽`). 앞 글자에 붙어 함께 셰이핑해야 한다.
const MODIFIER_START: char = '\u{1F3FB}';
const MODIFIER_END: char = '\u{1F3FF}';

/// 셰이핑 한 번으로 묶을 엔진 칸 범위. `end`는 포함하지 않는 끝 열이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster {
    /// 첫 칸 열.
    pub start: u16,
    /// 끝 열(포함하지 않음). `start + 1`이면 묶지 않은 칸 하나다.
    pub end: u16,
    /// 참이면 1칸 묶음이지만 2칸으로 넘쳐 그려도 된다(바로 오른쪽 칸이 빈칸일 때만).
    pub overflow: bool,
}

impl Cluster {
    /// 묶음이 차지하는 엔진 칸 수.
    pub fn cols(self) -> u16 {
        self.end.saturating_sub(self.start)
    }
}

fn is_modifier(ch: char) -> bool {
    (MODIFIER_START..=MODIFIER_END).contains(&ch)
}

/// 뒤 칸이 앞 칸과 같은 문자소에 속해 함께 셰이핑해야 하면 참. 칸은 행 안에서 바로 이웃해야 한다.
fn joins(previous: &Cell, next: &Cell) -> bool {
    // 공백은 문자소 경계다(ZWJ 뒤 공백은 잇지 않는다).
    if next.ch == ' ' {
        return false;
    }
    // 앞 칸의 ZWJ는 뒤 칸과 잇는다(가족·깃발·직업 ZWJ).
    if previous.zerowidth.contains(&ZWJ) {
        return true;
    }
    // 뒤 칸 본문이 앞에 붙는 간격 꾸밈이면 잇는다(피부색·키캡 둘러싸개).
    is_modifier(next.ch) || next.ch == ENCLOSING
}

/// 1칸 묶음이라도 2칸으로 넘쳐 그릴 수 있는 이모지 표시(VS16)면 참. ZWJ가 있으면 ZWJ 묶음이 맡는다.
fn vs16_single(text: &str) -> bool {
    text.contains(VS16) && !text.contains(ZWJ)
}

/// 행의 칸들을 셰이핑 묶음으로 나눈다. 자리(`SPACER`) 칸은 묶음에 딸려 가고 따로 내지 않는다.
/// 빈칸 여부(`is_blank`)는 넘침 판정에만 쓴다.
/// AS-14: 65,535칸을 넘는 행도 panic 없이 `u16::MAX`에서 자른다(정상 krilld는 2048칸으로 자름).
pub fn clusters(row: &[Cell]) -> Vec<Cluster> {
    // 자리 칸을 건너뛴 (열, 칸) 목록. 열은 u32로 세고 `u16::MAX`를 넘는 칸은 버린다.
    let cells: Vec<(u16, &Cell)> = (0u32..)
        .zip(row)
        .filter(|(_, c)| !c.flags.contains(Flags::SPACER))
        .take_while(|(col, _)| u16::try_from(*col).is_ok())
        .filter_map(|(col, c)| u16::try_from(col).ok().map(|col| (col, c)))
        .collect();
    let mut out = Vec::with_capacity(cells.len());
    let mut i = 0;
    while i < cells.len() {
        let start = cells[i].0;
        let mut end = start.saturating_add(cell_cols(cells[i].1));
        let mut j = i;
        // 바로 이웃한 칸이 `joins`면 묶음을 늘린다.
        while let Some(next) = cells.get(j + 1)
            && next.0 == end
            && joins(cells[j].1, next.1)
        {
            end = end.saturating_add(cell_cols(next.1));
            j += 1;
        }
        // 넘침: 1칸 VS16 묶음 + 바로 오른쪽 칸이 빈칸이거나 행 끝(AS-16: 끝에는 가릴 칸이 없음).
        let single = j == i && end.saturating_sub(start) == 1;
        let mut text = String::new();
        text.push(cells[i].1.ch);
        text.extend(cells[i].1.zerowidth.iter());
        for (_, cell) in cells.iter().take(j + 1).skip(i + 1) {
            text.push(cell.ch);
            text.extend(cell.zerowidth.iter());
        }
        let overflow = single
            && vs16_single(&text)
            && cells
                .get(j + 1)
                .is_none_or(|n| n.0 == end && n.1.is_blank());
        // 상한에서 잘린 WIDE 시작 칸은 빈 묶음이 되니 버린다(AS-14).
        if end > start {
            out.push(Cluster {
                start,
                end,
                overflow,
            });
        }
        i = j + 1;
    }
    out
}

/// 칸 하나가 차지하는 엔진 열 수(2칸 글자면 2).
fn cell_cols(cell: &Cell) -> u16 {
    if cell.flags.contains(Flags::WIDE) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests;
