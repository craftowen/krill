//! MK-002 10개 케이스 IME 회귀 체크리스트(MK-028).
//!
//! 실제 IME 없이 macOS 2벌식 IME가 보내는 `NSTextInputClient` 메시지 순서를 재생한다.
//! 실제 IME 구동은 `spikes/mk-002/run-all.sh`(화면 잠금 해제 필요)가 맡고, 여기서는 그 메시지에
//! 대한 상태기계의 응답(PTY 바이트·오버레이·후보창 앵커)을 고정한다.
//! 케이스 번호는 docs/spikes/MK-002.md 표, 스크립트 토큰은 spikes/mk-002 `r*` 케이스와 같다.

use krill_term::ime::{CellPoint, Ime, Output};

/// 탭 하나: IME 상태 + 그 탭 PTY에 쓴 바이트.
#[derive(Default)]
struct Pane {
    ime: Ime,
    pty: Vec<u8>,
    discards: usize,
}

impl Pane {
    fn apply(&mut self, out: &Output) {
        self.pty.extend_from_slice(&out.pty);
        self.discards += usize::from(out.discard_ime);
    }

    /// 토큰: `mark:<글자>`(setMarkedText) `ins:<글자>`(insertText) `unmark`
    /// `ret`·`bs`(조합 중이면 먼저 확정하고 키) `paste:<텍스트>` `commit`(포커스 이탈·탭 전환·창 닫기·비활성화)
    /// `cmd-c`(IME·PTY 무관) `echo`(데몬 화면 에코 도착: 낙관적 확정 글자 확인).
    fn run(&mut self, script: &str) -> &mut Self {
        for tok in script.split(' ') {
            if tok == "echo" {
                self.ime.clear_pending();
                continue;
            }
            match tok.split_once(':') {
                Some(("mark", t)) => self.ime.set_marked(t, None),
                Some(("ins", t)) => {
                    let out = self.ime.insert(t);
                    self.apply(&out);
                }
                Some(("paste", t)) => {
                    let out = self.ime.commit_then(t.as_bytes());
                    self.apply(&out);
                }
                _ => {
                    let out = match tok {
                        "unmark" => self.ime.unmark(),
                        "ret" => self.ime.commit_then(b"\r"),
                        "bs" => self.ime.commit_then(b"\x7f"),
                        "sp" => self.ime.insert(" "),
                        "commit" => self.ime.commit(),
                        "cmd-c" => Output::default(),
                        other => unreachable!("모르는 토큰 {other}"),
                    };
                    self.apply(&out);
                }
            }
        }
        self
    }

    fn written(&self) -> String {
        String::from_utf8_lossy(&self.pty).into_owned()
    }
}

fn pane(script: &str) -> Pane {
    let mut p = Pane::default();
    p.run(script);
    p
}

/// 2벌식 "한글"·"안녕하세요" 조합 메시지.
const HANGUL: &str = "mark:ㅎ mark:하 mark:한 ins:한 mark:ㄱ mark:그 mark:글 ins:글";
const ANNYEONG: &str = "mark:ㅇ mark:아 mark:안 ins:안 mark:ㄴ mark:녀 mark:녕 ins:녕 \
                        mark:ㅎ mark:하 ins:하 mark:ㅅ mark:세 ins:세 mark:ㅇ mark:요";

#[test]
fn c01_hangul_then_space() {
    assert_eq!(pane(&format!("{HANGUL} sp")).written(), "한글 ");
}

#[test]
fn c02_enter_after_composition_in_either_order() {
    // IME가 insertText 다음 Enter를 보내는 기대 순서.
    assert_eq!(
        pane(&format!("{ANNYEONG} ins:요 ret")).written(),
        "안녕하세요\r"
    );
    // IME가 확정 없이 Enter만 보낸 경우: 위젯이 키보다 먼저 확정(spike r2).
    let p = pane(&format!("{ANNYEONG} ret"));
    assert_eq!(p.written(), "안녕하세요\r");
    assert_eq!(p.discards, 1);
    // IME가 Enter 뒤에 늦게 확정을 보내도 중복되지 않는다.
    assert_eq!(
        pane(&format!("{ANNYEONG} ret ins:요")).written(),
        "안녕하세요\r"
    );
    // `한`⏎`글`⏎ 재정렬 없음.
    assert_eq!(
        pane("mark:ㅎ mark:하 mark:한 ret mark:ㄱ mark:그 mark:글 ret").written(),
        "한\r글\r"
    );
}

#[test]
fn c03_input_source_switch() {
    // 영→한 전환 직후 첫 자모: 영문은 조합 없이 insertText.
    assert_eq!(
        pane("ins:a ins:b ins:c mark:ㅎ mark:하 mark:한 ins:한 sp").written(),
        "abc한 "
    );
    // 조합 중 한→영 전환: IME가 unmarkText 또는 insertText로 확정한다.
    assert_eq!(pane("mark:ㅎ mark:하 unmark ins:a").written(), "하a");
    assert_eq!(pane("mark:ㅎ mark:하 ins:하 ins:a").written(), "하a");
    // 전환 직후 자음 하나.
    assert_eq!(pane("mark:ㄱ ins:ㄱ sp").written(), "ㄱ ");
    // setMarkedText는 이어 붙이지 않고 통째로 바꾼다(자모 분리 없음, winit #3095).
    let mut ime = Ime::default();
    ime.set_marked("ㄱ", None);
    ime.set_marked("가", None);
    assert_eq!(ime.overlay().map(|o| o.text), Some("가"));
}

#[test]
fn c04_single_space_and_first_symbol() {
    assert_eq!(
        pane("mark:ㅎ mark:하 mark:한 ins:한 sp mark:ㄱ mark:그 mark:글 ins:글 sp sp").written(),
        "한 글  "
    );
    assert_eq!(
        pane("ins:. mark:ㅎ mark:하 mark:한 ins:한 ins:? ins:!").written(),
        ".한?!"
    );
    // 늦은 중복 확정은 바로 다음 한 번만 버린다. 그 뒤 같은 글자는 정상 입력이다.
    assert_eq!(pane("mark:한 commit ins:한 ins:한").written(), "한한");
}

#[test]
fn c05_hanja_candidate_anchor() {
    let mut ime = Ime::default();
    ime.set_cursor(
        CellPoint {
            line: 3,
            column: 10,
        },
        true,
    );
    ime.set_marked("한", Some(0..1));
    assert_eq!(
        ime.candidate_anchor(0),
        CellPoint {
            line: 3,
            column: 10
        }
    );
    assert_eq!(
        ime.candidate_anchor(1),
        CellPoint {
            line: 3,
            column: 12
        }
    );
    assert_eq!(ime.marked_range_utf16(), Some(0..1));
    assert_eq!(ime.selected_range_utf16(), 0..1);
    // 한자 선택은 조합 중 글자를 대체한다.
    assert_eq!(ime.insert("韓").pty, "韓".as_bytes());
    assert!(!ime.is_composing());
    assert_eq!(ime.selected_range_utf16(), 0..0);
}

#[test]
fn c06_commit_on_blur_tab_switch_close() {
    // 포커스 이탈·창 비활성화: 확정하고 IME 조합을 버리게 한다.
    let p = pane("mark:한 commit mark:ㄱ mark:글 ins:글");
    assert_eq!(p.written(), "한글");
    assert_eq!(p.discards, 1);
    // discardMarkedText가 안 먹어 IME가 같은 글자를 다시 확정해도 중복 없음.
    assert_eq!(
        pane("mark:한 commit ins:한 mark:ㄱ ins:글").written(),
        "한글"
    );
    // 탭 전환(Cmd+T): 탭마다 상태가 따로다.
    let mut tab0 = pane("mark:한 commit");
    let tab1 = pane("mark:글 ins:글");
    assert_eq!(
        (tab0.written().as_str(), tab1.written().as_str()),
        ("한", "글")
    );
    // 창 닫기: 조합이 없어도 panic·쓰기 없음(winit #4626).
    tab0.run("commit commit");
    assert_eq!((tab0.written().as_str(), tab0.discards), ("한", 1));
}

#[test]
fn c07_cmd_keys_keep_composition() {
    // Cmd+C는 조합을 건드리지 않는다.
    assert_eq!(pane("mark:한 cmd-c ins:한").written(), "한");
    // Cmd+V는 조합을 먼저 확정하고 붙인다.
    assert_eq!(pane("mark:한 paste:XY sp").written(), "한XY ");
    // Cmd+T = c06 탭 전환.
}

#[test]
fn c08_claude_code_hidden_cursor() {
    // TUI가 하드웨어 커서를 숨겨도 마지막으로 보인 커서에 오버레이·후보창을 붙인다.
    // OWE-615: 음절마다 에코가 와서 낙관적 확정 글자가 확인된다고 가정한다(에코 없이 치면
    // 확정 대기가 쌓여 오버레이가 뒤로 밀리는 게 정상이다).
    const HANGUL_ECHO: &str =
        "mark:ㅎ mark:하 mark:한 ins:한 echo mark:ㄱ mark:그 mark:글 ins:글 echo";
    const ANNYEONG_ECHO: &str = "mark:ㅇ mark:아 mark:안 ins:안 echo mark:ㄴ mark:녀 mark:녕 ins:녕 echo \
                         mark:ㅎ mark:하 ins:하 echo mark:ㅅ mark:세 ins:세 echo mark:ㅇ mark:요";
    let mut p = Pane::default();
    p.ime.set_cursor(
        CellPoint {
            line: 20,
            column: 4,
        },
        true,
    );
    p.ime.set_cursor(CellPoint { line: 0, column: 0 }, false);
    p.run(&format!("{HANGUL_ECHO} sp echo {ANNYEONG_ECHO}"));
    assert_eq!(
        p.ime.overlay().map(|o| o.origin),
        Some(CellPoint {
            line: 20,
            column: 4
        })
    );
    assert_eq!(
        p.ime.candidate_anchor(0),
        CellPoint {
            line: 20,
            column: 4
        }
    );
    p.run("ret");
    assert_eq!(p.written(), "한글 안녕하세요\r");
}

#[test]
fn c09_dictation_and_emoji_picker() {
    // 받아쓰기·문자 뷰어는 조합 없이 insertText로 온다.
    assert_eq!(
        pane("ins:안녕하세요 ins:❤️ ins:👨‍👩‍👧").written(),
        "안녕하세요❤️👨‍👩‍👧"
    );
}

#[test]
fn c10_preedit_overlay_on_cursor_cell() {
    let mut ime = Ime::default();
    ime.set_cursor(CellPoint { line: 2, column: 4 }, true);
    assert_eq!(ime.overlay(), None);
    ime.set_marked("ㅎ", None);
    let o = ime.overlay().expect("조합 중 오버레이");
    assert_eq!(
        (o.origin, o.text, o.columns),
        (CellPoint { line: 2, column: 4 }, "ㅎ", 2)
    );
    ime.set_marked("한", None);
    assert_eq!(ime.overlay().map(|o| o.columns), Some(2));
    // NFD 자모: 폭 0 중성·종성은 초성 칸에 붙는다.
    ime.set_marked("\u{1112}\u{1161}\u{11ab}a", None);
    let o = ime.overlay().expect("조합 중 오버레이");
    assert_eq!(o.columns, 3);
    assert_eq!(
        o.cells().map(|(col, _)| col).collect::<Vec<_>>(),
        [0, 0, 0, 2]
    );
    // 조합이 끝나면 오버레이가 사라진다(커서 다시 표시). preedit는 PTY에 가지 않는다.
    assert!(ime.insert("한").pty == "한".as_bytes() && ime.overlay().is_none());
}

#[test]
fn preedit_never_reaches_pty() {
    let mut ime = Ime::default();
    for t in ["ㅎ", "하", "한", "한ㄱ", ""] {
        ime.set_marked(t, None);
    }
    assert!(!ime.is_composing());
    assert_eq!(ime.commit(), Output::default());
}

/// W1 자모 분리(`ㅁㅗㅂㅏㅇㅣㄹ`): 조합 중 문서(= preedit) 조회가 `markedRange`의 글자를 돌려주고,
/// 2벌식 "모바일" 메시지가 음절 단위로만 확정된다.
#[test]
fn w1_marked_text_is_readable_and_syllables_commit_whole() {
    let mut p = Pane::default();
    p.run("mark:ㅁ mark:모");
    let marked = p.ime.marked_range_utf16().unwrap();
    assert_eq!(
        p.ime.text_for_range_utf16(marked),
        Some(("모".to_owned(), 0..1))
    );
    // 범위를 넘으면 preedit 안으로 잘라 실제 범위를 알린다. 빈 범위는 없다.
    assert_eq!(
        p.ime.text_for_range_utf16(0..9),
        Some(("모".to_owned(), 0..1))
    );
    assert_eq!(p.ime.text_for_range_utf16(1..1), None);
    p.run("ins:모 mark:ㅂ mark:바 ins:바 mark:ㅇ mark:이 mark:일 ins:일");
    assert_eq!(p.written(), "모바일");
    assert_eq!(
        p.ime.text_for_range_utf16(0..1),
        None,
        "확정 뒤 문서는 비어 있다"
    );
    // 서로게이트 쌍 가운데를 자르는 범위는 글자가 아니다.
    p.run("mark:😀");
    assert_eq!(p.ime.text_for_range_utf16(0..1), None);
    assert_eq!(
        p.ime.text_for_range_utf16(0..2),
        Some(("😀".to_owned(), 0..2))
    );
}
