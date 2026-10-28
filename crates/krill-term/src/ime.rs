//! 한글 IME 상태기계(MK-028, ARCHITECTURE §3 IME, T3 §3.3).
//!
//! 입력은 macOS `NSTextInputClient` 메시지 단위다(`setMarkedText:` → [`Ime::set_marked`],
//! `insertText:` → [`Ime::insert`], `unmarkText` → [`Ime::unmark`], `firstRectForCharacterRange:` →
//! [`Ime::candidate_anchor`]). gpui `EntityInputHandler`든 objc2 `AppKit` 뷰든 메시지를 그대로 옮기고,
//! 돌려받은 [`Output::pty`]만 PTY에 쓴다. 규칙:
//!
//! 1. preedit(조합 중 글자)는 PTY로 보내지 않는다. UI 상태로만 두고 커서 셀 위 오버레이로 그린다.
//! 2. 확정 텍스트만 PTY에 UTF-8로 쓴다.
//! 3. 조합 중 특수 키·붙여넣기는 조합을 먼저 확정한 뒤 보낸다(`한`⏎`글` 재정렬 없음).
//! 4. 포커스 이탈·탭 전환·창 닫기·창 비활성화 때 확정한다(버리지 않는다).
//! 5. 오버레이·후보창은 그리드 커서 셀에 붙인다. 숨은 커서도 따른다: Claude Code(2.1.308 실측)는 커서를
//!    숨긴 채 매 프레임 끝에 입력 칸으로 옮긴다. 마지막으로 *보인* 커서만 쓰면 시작 때 `CSI 2J`·`CSI H`로
//!    맨 위에 남은 자리에 조합 글자가 찍힌다.
//! 6. 확정(`insertText`) 뒤 데몬 화면 에코가 오기 전까지 확정 글자를 낙관적으로 유지한다.
//!    `"한"` 조합 중 다음 자모를 치면 IME가 `insertText("한")` → `setMarkedText("ㄱ")`을
//!    연달아 보내는데, 에코 전 화면에는 `"한"`이 없어 다음 프레임이 `"한"`을 지운 것처럼
//!    보인다(OWE-615 원인 1). 그래서 확정 텍스트를 `origin`에 남겨 두고, 이어지는
//!    preedit·후보창은 그 폭만큼 오른쪽으로 민다. 에코(커서 전진)나 만료 시간이 오면 지운다.

use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

/// 화면 셀 좌표(맨 위 줄·맨 왼쪽 칸이 0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellPoint {
    pub line: usize,
    pub column: usize,
}

/// 메시지 하나를 처리한 결과.
#[must_use]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// PTY에 이 순서 그대로 쓸 바이트. 비어 있으면 쓰지 않는다.
    pub pty: Vec<u8>,
    /// 위젯이 조합을 끊고 확정했다. 앱은 플랫폼 IME 쪽 조합도 버려야 한다(AppKit
    /// `NSTextInputContext discardMarkedText`). 안 버리면 IME가 같은 글자를 나중에 다시 보낸다.
    pub discard_ime: bool,
}

/// preedit 오버레이. 커서 셀(`origin`)부터 `columns`칸을 배경으로 덮고 글자에 밑줄을 친다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overlay<'a> {
    pub origin: CellPoint,
    pub text: &'a str,
    pub columns: usize,
}

impl Overlay<'_> {
    /// 글자마다 `origin`에서 몇 칸 떨어져 그릴지. 폭 0 문자는 앞 글자와 같은 칸이다.
    pub fn cells(&self) -> impl Iterator<Item = (usize, char)> + '_ {
        self.text.chars().scan((0, 0), |(next, last), ch| {
            let width = char_width(ch);
            if width > 0 {
                *last = *next;
                *next += width;
            }
            Some((*last, ch))
        })
    }
}

/// 확정 뒤 에코를 기다리는 낙관적(optimistic) 텍스트. 화면에 아직 없어도 `origin`에 그린다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending<'a> {
    pub origin: CellPoint,
    pub text: &'a str,
    pub columns: usize,
}

/// 확정 에코를 기다리는 최대 시간(설계 상한, 측정값 아님). 실측 에코는 로컬 key→present p95
/// 4.54~5.20ms(`docs/boost-2026-10-20-terminal-performance.md` §4), 모바일 터널 키 에코 p90 25ms
/// (`docs/boost-2026-10-19-mobile-speed.md`)로, 에코 차단선 33ms(ARCHITECTURE §8)의 15배를 둔다.
/// 넘으면 화면이 멈춘 것으로 보고 낙관적 글자를 지운다. 만료 기준은 마지막 확정 시각이다.
pub const PENDING_TTL: Duration = Duration::from_millis(500);

/// 터미널 위젯(탭) 하나의 IME 상태.
#[derive(Debug, Default)]
pub struct Ime {
    preedit: String,
    /// `setMarkedText:`의 selectedRange(preedit 안 UTF-16 범위).
    selected: Option<Range<usize>>,
    /// 마지막 그리드 커서(숨은 커서 포함, 규칙 5).
    cursor: CellPoint,
    /// 위젯이 직접 확정한 글자. 바로 다음 `insertText:`가 같은 글자면 IME의 늦은 확정이라 버린다.
    /// `discardMarkedText`가 안 먹거나 IME가 키 처리 뒤에 확정을 보내는 경우의 중복 방지.
    /// 같은 글자·`setMarkedText:` 없이 바로 다음일 때만 버린다(실기 조건: `docs/testing/ime-mk002.md`).
    committed: Option<String>,
    /// 에코 대기 중인 확정 글자(원인 1). 같은 줄 커서가 `origin + columns`까지 전진하거나
    /// [`PENDING_TTL`]이 지나면 지운다.
    pending_text: String,
    pending_origin: CellPoint,
    pending_columns: usize,
    pending_at: Option<Instant>,
}

impl Ime {
    /// `setMarkedText:` — 조합 중 글자를 통째로 바꾼다. PTY에는 아무것도 쓰지 않는다.
    /// 빈 문자열은 조합 취소(예: 마지막 자모를 지움)다. 에코 대기 중인 확정 글자는 유지한다
    /// (`insertText("한")` 바로 뒤의 `setMarkedText("ㄱ")`이 에코 전 프레임을 지우지 않게).
    pub fn set_marked(&mut self, text: &str, selected_utf16: Option<Range<usize>>) {
        text.clone_into(&mut self.preedit);
        self.selected = selected_utf16;
        self.committed = None;
    }

    /// `insertText:` — 확정 텍스트. 조합 중 글자를 대체한다(한자 변환 포함).
    /// 받아쓰기·이모지 피커 입력도 조합 없이 이 경로로 온다.
    /// 확정 글자는 에코가 올 때까지 낙관적으로 유지한다(원인 1).
    pub fn insert(&mut self, text: &str) -> Output {
        let late_duplicate =
            self.committed.take().is_some_and(|c| c == text) && self.preedit.is_empty();
        self.clear_preedit();
        if late_duplicate {
            return Output::default();
        }
        if !text.is_empty() {
            self.push_pending(text);
        }
        Output {
            pty: text.as_bytes().to_vec(),
            discard_ime: false,
        }
    }

    /// `unmarkText` — IME가 조합 중 글자를 그대로 확정하라고 한다. IME 쪽은 이미 끝났으니 버리지 않는다.
    pub fn unmark(&mut self) -> Output {
        self.committed = None;
        let text = std::mem::take(&mut self.preedit);
        self.selected = None;
        if !text.is_empty() {
            self.push_pending(&text);
        }
        Output {
            pty: text.into_bytes(),
            discard_ime: false,
        }
    }

    /// 포커스 이탈·탭 전환·창 닫기·창 비활성화 때 조합 중 글자를 확정한다.
    pub fn commit(&mut self) -> Output {
        self.commit_then(&[])
    }

    /// 특수 키(Enter·Backspace·Tab·Esc·화살표·Ctrl 조합)나 붙여넣기 바이트를 보낸다.
    /// 조합 중이면 확정을 키보다 먼저 쓴다(IME가 `insertText:`를 키보다 늦게 보내도 순서 유지).
    /// 위젯이 직접 확정한 글자는 에코가 올 때까지 낙관적으로 유지한다(원인 1).
    pub fn commit_then(&mut self, bytes: &[u8]) -> Output {
        let text = std::mem::take(&mut self.preedit);
        self.selected = None;
        let discard_ime = !text.is_empty();
        let mut pty = text.clone().into_bytes();
        pty.extend_from_slice(bytes);
        if discard_ime {
            self.push_pending(&text);
            self.committed = Some(text);
        }
        Output { pty, discard_ime }
    }

    /// 매 프레임 그리드 커서를 알려 준다. 숨은 커서도 오버레이·후보창 기준으로 따른다(규칙 5).
    /// 커서가 확정 자리(`origin + columns`)까지 전진했거나 다른 줄로 가면 낙관적 글자를 지운다.
    /// 표시 여부는 에코 판정에 영향을 주지 않는다. 숨은 커서도 입력 칸으로 전진하는 TUI가 있다.
    pub fn set_cursor(&mut self, at: CellPoint, _visible: bool) {
        self.cursor = at;
        self.confirm_pending(at);
    }

    /// 에코 만료: [`PENDING_TTL`]이 지난 낙관적 글자를 지운다. 매 프레임·타이머에서 부른다.
    pub fn expire_pending(&mut self, now: Instant) {
        if self
            .pending_at
            .is_some_and(|at| now.saturating_duration_since(at) >= PENDING_TTL)
        {
            self.clear_pending();
        }
    }

    /// 낙관적 확정 글자를 지금 지운다(세션 종료·포커스 이동 등 화면이 바뀌는 경우).
    pub fn clear_pending(&mut self) {
        self.pending_text.clear();
        self.pending_origin = CellPoint::default();
        self.pending_columns = 0;
        self.pending_at = None;
    }

    /// 에코 대기 중인 확정 글자. 화면에 아직 없어도 `origin`에 그린다.
    pub fn pending(&self) -> Option<Pending<'_>> {
        (!self.pending_text.is_empty()).then(|| Pending {
            origin: self.pending_origin,
            text: &self.pending_text,
            columns: self.pending_columns,
        })
    }

    /// 낙관적 글자를 남긴 시각(만료 판정·테스트용).
    pub fn pending_at(&self) -> Option<Instant> {
        self.pending_at
    }

    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    /// 조합 중이면 그리드 커서를 숨기고 이 오버레이를 그린다.
    /// 에코 대기 중인 확정 글자가 같은 줄에 있으면 그 다음 칸부터 그린다(원인 1).
    pub fn overlay(&self) -> Option<Overlay<'_>> {
        self.is_composing().then(|| Overlay {
            origin: self.shifted_origin(),
            text: &self.preedit,
            columns: self.preedit.chars().map(char_width).sum(),
        })
    }

    /// `firstRectForCharacterRange:` — 후보창을 붙일 셀. preedit 안 UTF-16 위치 `start`까지의 칸 수만큼 오른쪽.
    /// 에코 대기 중인 확정 글자가 같은 줄에 있으면 그 폭만큼 더 오른쪽이다(원인 1).
    pub fn candidate_anchor(&self, start_utf16: usize) -> CellPoint {
        let mut utf16 = 0;
        let columns: usize = self
            .preedit
            .chars()
            .take_while(|ch| {
                utf16 += ch.len_utf16();
                utf16 <= start_utf16
            })
            .map(char_width)
            .sum();
        let origin = self.shifted_origin();
        CellPoint {
            line: origin.line,
            column: origin.column + columns,
        }
    }

    /// Keep preedit and its caret inside the visible row, even after a resize.
    pub fn overlay_in_columns(&self, columns: usize) -> Option<Overlay<'_>> {
        self.overlay().map(|mut overlay| {
            overlay.origin.column = overlay
                .origin
                .column
                .min(columns.saturating_sub(overlay.columns.saturating_add(1)));
            overlay
        })
    }

    /// Candidate windows use the same shifted origin as the painted preedit.
    pub fn candidate_anchor_in_columns(&self, start_utf16: usize, columns: usize) -> CellPoint {
        let mut anchor = self.candidate_anchor(start_utf16);
        if let Some(overlay) = self.overlay_in_columns(columns) {
            let offset = anchor.column.saturating_sub(self.shifted_origin().column);
            anchor.column = overlay.origin.column.saturating_add(offset);
        }
        anchor.column = anchor.column.min(columns.saturating_sub(1));
        anchor
    }

    /// `markedRange` — 조합 중 글자의 UTF-16 범위.
    pub fn marked_range_utf16(&self) -> Option<Range<usize>> {
        self.is_composing().then(|| 0..utf16_len(&self.preedit))
    }

    /// `attributedSubstringForProposedRange:` — 클라이언트 문서는 preedit뿐이다(`markedRange`·`selectedRange`와
    /// 같은 좌표). 범위를 preedit 안으로 잘라 그 글자와 실제 범위를 돌려준다. 조합 중 범위에 `nil`을 주면
    /// `markedRange`가 있다고 하면서 그 글자는 없다고 하는 셈이라, macOS 한글 IME가 조합이 깨졌다고 보고
    /// 자모마다 새로 조합을 시작한다(W1 자모 분리 유력 원인, 실기 미확인).
    pub fn text_for_range_utf16(&self, range: Range<usize>) -> Option<(String, Range<usize>)> {
        let units: Vec<u16> = self.preedit.encode_utf16().collect();
        let start = range.start.min(units.len());
        let end = range.end.min(units.len());
        if start >= end {
            return None;
        }
        let text = String::from_utf16(units.get(start..end)?).ok()?;
        Some((text, start..end))
    }

    /// `selectedRange` — 선택이 없으면 preedit 끝의 캐럿.
    pub fn selected_range_utf16(&self) -> Range<usize> {
        let end = utf16_len(&self.preedit);
        self.selected.clone().unwrap_or(end..end)
    }

    fn clear_preedit(&mut self) {
        self.preedit.clear();
        self.selected = None;
    }

    /// 확정 글자를 에코 대기열에 남긴다. 에코 전에 확정이 이어지면(받아쓰기 연속 입력)
    /// 같은 자리에 이어 붙인다.
    fn push_pending(&mut self, text: &str) {
        let columns: usize = text.chars().map(char_width).sum();
        if self.pending_text.is_empty() {
            self.pending_origin = self.cursor;
        }
        self.pending_text.push_str(text);
        self.pending_columns += columns;
        self.pending_at = Some(Instant::now());
    }

    /// 커서가 확정 자리를 지나면 에코가 온 것으로 보고 지운다(표시 여부 무관).
    fn confirm_pending(&mut self, at: CellPoint) {
        if self.pending_text.is_empty() {
            return;
        }
        if at.line != self.pending_origin.line
            || at.column >= self.pending_origin.column + self.pending_columns
        {
            self.clear_pending();
        }
    }

    /// preedit·후보창의 기준 칸. 같은 줄 대기 글자 뒤다.
    /// AS-11: 에코가 일부만 와도(커서가 대기 중간) 기준은 대기 시작점 + 폭이다.
    /// 커서 + 폭으로 계산하면 오른쪽으로 이중 이동한다.
    fn shifted_origin(&self) -> CellPoint {
        if self.pending_text.is_empty() || self.pending_origin.line != self.cursor.line {
            return self.cursor;
        }
        CellPoint {
            line: self.cursor.line,
            column: self.pending_origin.column + self.pending_columns,
        }
    }
}

/// 그리드와 같은 기준(`alacritty_terminal`이 쓰는 `unicode-width`)의 셀 폭.
fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

#[cfg(test)]
mod edge_tests {
    use super::*;

    #[test]
    fn candidate_and_preedit_share_edge_fit_for_pending_wide_text_and_resize() {
        let mut ime = Ime::default();
        ime.set_cursor(CellPoint { line: 3, column: 7 }, true);
        assert_eq!(ime.insert("한").pty, "한".as_bytes());
        ime.set_marked("글😀", None);
        let overlay = ime.overlay_in_columns(8).unwrap();
        assert_eq!(overlay.columns, 4);
        assert_eq!(overlay.origin, CellPoint { line: 3, column: 3 });
        assert_eq!(ime.candidate_anchor_in_columns(0, 8), overlay.origin);
        assert_eq!(ime.candidate_anchor_in_columns(1, 8).column, 5);
        assert_eq!(ime.candidate_anchor_in_columns(3, 8).column, 7);
        assert_eq!(ime.candidate_anchor_in_columns(3, 2).column, 1);
        assert_eq!(ime.overlay_in_columns(2).unwrap().origin.column, 0);
        assert_eq!(ime.candidate_anchor_in_columns(3, 0).column, 0);
        assert_eq!(ime.candidate_anchor_in_columns(3, 0).line, 3);
        assert_eq!(ime.commit().pty, "글😀".as_bytes());
    }
}
