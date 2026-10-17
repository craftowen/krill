//! OWE-615 원인 1: 확정(`insertText`) 뒤 에코가 오기 전까지 확정 글자를 낙관적으로 유지한다.
//!
//! `"한"` 조합 중 다음 자모를 치면 IME가 `insertText("한")` → `setMarkedText("ㄱ")`을 연달아
//! 보내는데, 에코 전 화면에는 `"한"`이 없어 다음 프레임이 `"한"`을 지운 것처럼 보였다.
//! 확정 글자를 `origin`에 남겨 두고 이어지는 preedit·후보창은 그 폭만큼 오른쪽으로 민다.

use std::time::Duration;

use krill_term::ime::{CellPoint, Ime, PENDING_TTL};

fn origin() -> CellPoint {
    CellPoint { line: 0, column: 0 }
}

/// 연속 음절: `insertText("한")` 직후 에코 없는 프레임이 와도 `"한"`이 남고,
/// 새 preedit `"ㄱ"`은 그 다음 칸(2열)에 그려진다.
#[test]
fn committed_text_survives_pre_echo_frame_and_shifts_preedit() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    ime.set_marked("하", None);
    let out = ime.insert("한");
    assert_eq!(out.pty, "한".as_bytes());

    // 에코 전 화면(커서 그대로)에서는 확정 글자가 남는다.
    ime.set_cursor(origin(), true);
    let pending = ime.pending().expect("에코 전 확정 글자 유지");
    assert_eq!(
        (pending.origin, pending.text, pending.columns),
        (origin(), "한", 2)
    );

    // 이어지는 조합은 확정 글자 다음 칸부터.
    ime.set_marked("ㄱ", None);
    assert!(
        ime.pending().is_some(),
        "setMarkedText가 확정 대기를 지우지 않는다"
    );
    let overlay = ime.overlay().expect("조합 중 오버레이");
    assert_eq!(
        (overlay.origin, overlay.text),
        (CellPoint { line: 0, column: 2 }, "ㄱ")
    );
    assert_eq!(
        ime.candidate_anchor(0),
        CellPoint { line: 0, column: 2 },
        "후보창도 확정 글자 다음 칸"
    );
}

/// 에코(커서가 확정 자리까지 전진)가 오면 낙관적 글자를 지운다.
#[test]
fn echo_cursor_advance_clears_pending() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    assert_eq!(ime.insert("한").pty, "한".as_bytes());
    ime.set_marked("ㄱ", None);

    // 커서가 한 칸만 갔으면(부분 에코) 아직 유지.
    ime.set_cursor(CellPoint { line: 0, column: 1 }, true);
    assert!(ime.pending().is_some());
    // 확정 자리(2열)까지 가면 에코 도착으로 보고 지운다. preedit는 그대로.
    ime.set_cursor(CellPoint { line: 0, column: 2 }, true);
    assert_eq!(ime.pending(), None);
    let overlay = ime.overlay().expect("preedit는 에코와 무관");
    assert_eq!(overlay.origin, CellPoint { line: 0, column: 2 });
}

/// 커서가 숨겨진 동안(TUI)은 커서로 확인할 수 없어 만료 시간으로 지운다.
#[test]
fn hidden_cursor_keeps_pending_until_ttl() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    assert_eq!(ime.insert("한").pty, "한".as_bytes());
    let at = ime.pending_at().expect("확정 시각 기록");
    ime.set_cursor(CellPoint { line: 0, column: 0 }, false);
    assert!(ime.pending().is_some(), "숨은 커서는 무시한다");
    ime.expire_pending(at + Duration::from_millis(499));
    assert!(ime.pending().is_some(), "만료 전 유지");
    ime.expire_pending(at + PENDING_TTL);
    assert_eq!(ime.pending(), None, "만료 후 삭제");
}

/// 받아쓰기처럼 에코 없이 확정이 이어지면 같은 자리에 이어 붙인다.
#[test]
fn consecutive_inserts_without_echo_accumulate() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    assert_eq!(ime.insert("안녕").pty, "안녕".as_bytes());
    assert_eq!(ime.insert("하세요").pty, "하세요".as_bytes());
    let pending = ime.pending().expect("이어 붙인 확정 글자");
    assert_eq!((pending.origin, pending.text), (origin(), "안녕하세요"));
    assert_eq!(pending.columns, 10);
    // 에코는 끝까지 전진해야 지운다.
    ime.set_cursor(CellPoint { line: 0, column: 9 }, true);
    assert!(ime.pending().is_some());
    ime.set_cursor(
        CellPoint {
            line: 0,
            column: 10,
        },
        true,
    );
    assert_eq!(ime.pending(), None);
}

/// 위젯이 직접 확정한 글자(특수 키·붙여넣기)도 에코까지 유지하고, IME의 늦은 중복 확정은 버린다.
#[test]
fn widget_commit_keeps_pending_and_late_duplicate_is_dropped() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    ime.set_marked("한", None);
    let out = ime.commit_then(b"\r");
    assert!(out.discard_ime);
    assert_eq!(ime.pending().map(|p| p.text), Some("한"));
    // IME가 키 처리 뒤에 같은 글자를 늦게 확정해도 PTY에 다시 쓰지 않는다.
    let late = ime.insert("한");
    assert!(late.pty.is_empty() && !late.discard_ime);
}

/// `unmarkText` 확정도 에코까지 유지한다.
#[test]
fn unmark_keeps_pending() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    ime.set_marked("하", None);
    let out = ime.unmark();
    assert_eq!(out.pty, "하".as_bytes());
    assert_eq!(ime.pending().map(|p| p.text), Some("하"));
}

/// 다른 줄로 가면(개행 에코) 낙관적 글자를 지운다.
#[test]
fn newline_echo_clears_pending() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    assert_eq!(ime.insert("한").pty, "한".as_bytes());
    ime.set_cursor(CellPoint { line: 1, column: 0 }, true);
    assert_eq!(ime.pending(), None);
}

/// AS-11: 에코가 일부만 와도 preedit·후보창은 확정 글자 끝(대기 시작점 + 폭)에 붙는다.
/// 커서 + 대기 폭으로 계산하면 오른쪽으로 이중 이동한다.
#[test]
fn partial_echo_keeps_preedit_after_pending_text() {
    let mut ime = Ime::default();
    ime.set_cursor(origin(), true);
    assert_eq!(ime.insert("한글").pty, "한글".as_bytes());
    // 첫 글자만 에코(커서 2열). 대기 글자는 아직 살아 있다.
    ime.set_cursor(CellPoint { line: 0, column: 2 }, true);
    assert!(ime.pending().is_some());
    ime.set_marked("ㄱ", None);
    let overlay = ime.overlay().expect("조합 중 오버레이");
    assert_eq!(
        (overlay.origin, overlay.text),
        (CellPoint { line: 0, column: 4 }, "ㄱ"),
        "대기 글자 끝(0 + 4열)에 그린다"
    );
    assert_eq!(
        ime.candidate_anchor(0),
        CellPoint { line: 0, column: 4 },
        "후보창도 대기 글자 끝"
    );
}
