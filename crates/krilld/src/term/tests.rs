//! MK-020: 응답 주체 표(비공개 MK-008 스파이크 기록 §2) 준수와 재생 게이트.

use std::path::PathBuf;

use krill_proto::theme::DEFAULT;

use super::*;
use crate::appearance::CellSize;

mod tui;
mod unicode_fuzz;

const SIZE: WinSize = WinSize { cols: 80, rows: 40 };

#[test]
fn ascii_scan_bypass_finishes_pending_mode_sequences() {
    let look = fresh();
    let mut engine = Engine::new(SIZE);
    engine.feed(b"\x1b[?20", Feed::Live, &look);
    engine.feed(b"31h", Feed::Live, &look);
    engine.feed(b"plain text\r\n", Feed::Live, &look);
    assert_eq!(
        engine.feed(b"\x1b[?2031$p", Feed::Live, &look),
        "\x1b[?2031;1$y"
    );
    engine.feed(b"\x1b[?2031", Feed::Live, &look);
    engine.feed(b"l", Feed::Live, &look);
    assert_eq!(
        engine.feed(b"\x1b[?2031$p", Feed::Live, &look),
        "\x1b[?2031;2$y"
    );
}

#[test]
fn ascii_scan_bypass_matches_raw_parser_across_chunks() {
    let look = fresh();
    let mut engine = Engine::new(SIZE);
    let mut parser = Parser::new();
    let mut scan = RawScan::default();
    for chunk in [
        b"plain\r\n".as_slice(),
        b"\x1b[?2031",
        b"h",
        b"\x1b[?2031$p",
        b"\x1b]7;file://localhost/",
        b"tmp\x07",
        b"\xe9\x9f",
        b"ascii after incomplete utf8",
        b"\x1bPpayload",
        b"more payload",
        b"\x1b\\",
        b"\x1b",
        b"c",
        b"\x00\x07\x18\x1a\x7f",
    ] {
        parser.advance(&mut scan, chunk);
        engine.feed(chunk, Feed::Live, &look);
        assert_eq!(engine.scheme.on, scan.on, "chunk={chunk:?}");
        assert_eq!(engine.scheme.cwd, scan.cwd, "chunk={chunk:?}");
        assert_eq!(
            engine.scan.mk_is_ground(),
            parser.mk_is_ground(),
            "chunk={chunk:?}"
        );
    }
}

/// 디스크에 아무것도 없는 상태(테마를 한 번도 받지 않음). 테스트는 `set_theme`을 부르지 않는다.
fn fresh() -> Appearance {
    Appearance::load(PathBuf::from("/nonexistent/mk020/theme"))
}

/// MK-008 §2 표의 질의 16종(+ 표 19~21의 캡처 없는 질의). 실제 에이전트처럼 DA1을 맨 끝에 둔다.
const QUERIES: &[&str] = &[
    "\x1b[6n",                                       // 2 CPR
    "\x1b[?u",                                       // 3 kitty 키보드
    "\x1b]11;?\x1b\\",                               // 4 배경색
    "\x1b]10;?\x07",                                 // 5 전경색
    "\x1b]12;?\x07",                                 // 6 커서색
    "\x1b]4;1;?\x07",                                // 7 ANSI 팔레트
    "\x1b]4;196;?\x07",                              // 7 (16~255는 계산)
    "\x1b[14t",                                      // 8 픽셀 크기
    "\x1b[?2026$p",                                  // 9 DECRQM
    "\x1b[?2031$p",                                  // 9 DECRQM 2031(krilld 훅)
    "\x1b[>q",                                       // 10 XTVERSION: 무응답
    "\x1bP+q4d73\x1b\\",                             // 11 XTGETTCAP: 무응답
    "\x1b]13;?\x07\x1b]17;?\x07\x1b]19;?\x07",       // 12 OSC 13~19: 무응답
    "\x1b[?996n", // 14 색 구성 질의: 무응답(선택 항목, 넣지 않음)
    "\x1b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\", // 15 kitty 그래픽: 무응답
    "\x1b]99;i=opentui-notifications:p=?;\x1b\\", // 16 OSC 99: 무응답
    "\x1b]1337;Capabilities\x1b\\", // 17 OSC 1337: 무응답
    "\x1b]7501;?\x07", // 18 Claude 전용: 무응답
    "\x1b]52;c;?\x07", // 19 OSC 52 읽기: 무응답(보안)
    "\x1b[>c",    // 20 DA2
    "\x1b[5n",    // 21 DSR 5
    "\x1b[c",     // 1 DA1(센티널)
];

/// 테마를 받기 전(내장 기본 테마), 셀 크기를 받기 전의 응답. 질의 순서 그대로다.
const REPLIES_DEFAULT: &str = concat!(
    "\x1b[1;1R",
    "\x1b[?0u",
    "\x1b]11;rgb:0606/1616/2020\x1b\\",
    "\x1b]10;rgb:dfdf/eded/f2f2\x07",
    "\x1b]12;rgb:d3d3/e5e5/eeee\x07",
    "\x1b]4;1;rgb:e5e5/7a7a/8b8b\x07",
    "\x1b]4;196;rgb:ffff/0000/0000\x07",
    "\x1b[?2026;2$y",
    "\x1b[?2031;2$y",
    "\x1b[>0;2600;1c",
    "\x1b[0n",
    "\x1b[?6c",
);

fn all_queries() -> String {
    QUERIES.concat()
}

#[test]
fn table_queries_with_builtin_default_theme() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    assert_eq!(
        e.feed(all_queries().as_bytes(), Feed::Live, &look),
        REPLIES_DEFAULT
    );
}

#[test]
fn pushed_theme_cell_size_and_program_colors() {
    let look = fresh();
    look.set_cell_size(CellSize {
        width: 9,
        height: 18,
    });
    let mut e = Engine::new(SIZE);
    let q = "\x1b]11;?\x07\x1b[14t\x1b[c";
    assert_eq!(
        e.feed(q.as_bytes(), Feed::Live, &look),
        "\x1b]11;rgb:0606/1616/2020\x07\x1b[4;720;720t\x1b[?6c"
    );
    // 프로그램이 OSC 11로 바꾼 배경색이 테마보다 먼저다.
    let r = e.feed(b"\x1b]11;#123456\x07\x1b]11;?\x07", Feed::Live, &look);
    assert_eq!(r, "\x1b]11;rgb:1212/3434/5656\x07");
}

#[test]
fn replay_answers_nothing() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    assert_eq!(e.feed(all_queries().as_bytes(), Feed::Replay, &look), "");
    // 재생이 남긴 2031 질의 큐도 비워졌다: 다음 실시간 질의는 지금 상태로 답한다.
    let r = e.feed(b"\x1b[?2031h\x1b[?2031$p", Feed::Live, &look);
    assert_eq!(r, "\x1b[?2031;1$y");
    // 재생 중 sync 구간 안에 들어간 질의도 답하지 않는다.
    assert_eq!(e.feed(b"\x1b[?2026h\x1b[c", Feed::Replay, &look), "");
    assert_eq!(e.end_sync(Feed::Replay, &look), "");
}

#[test]
fn scheme_mode_2031() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    assert_eq!(e.scheme_notice(&DEFAULT), None, "켜기 전에는 알림 없음");

    // sync 구간 안 질의는 구간이 끝날 때 답하지만, 상태는 질의 시점 값이다(켜기 전 = 2).
    let r = e.feed(
        b"\x1b[?2026h\x1b[?2031$p\x1b[?1004;2031h\x1b[?2031$p\x1b[?2026l",
        Feed::Live,
        &look,
    );
    assert_eq!(r, "\x1b[?2031;2$y\x1b[?2031;1$y");
    assert_eq!(e.scheme_notice(&DEFAULT).as_deref(), Some("\x1b[?997;1n"));
    let mut light = DEFAULT;
    light.background = Rgb {
        r: 0xfa,
        g: 0xfa,
        b: 0xfa,
    };
    assert_eq!(e.scheme_notice(&light).as_deref(), Some("\x1b[?997;2n"));

    e.feed(b"\x1b[?2031l", Feed::Live, &look);
    assert_eq!(e.scheme_notice(&DEFAULT), None);
    e.feed(b"\x1b[?2031h\x1bc", Feed::Live, &look);
    assert_eq!(e.scheme_notice(&DEFAULT), None, "RIS가 끈다");
}

#[test]
fn oversized_text_area_is_not_answered() {
    // 상한 1024행 × 64px은 u16을 넘는다. alacritty 포매터가 panic하지 않게 답하지 않는다.
    // (K-01: 엔진 행은 MAX_LINES=1024로 잘리므로 셀 크기로 오버플로를 만든다.)
    let look = fresh();
    look.set_cell_size(CellSize {
        width: 20,
        height: 64,
    });
    let mut e = Engine::new(WinSize {
        cols: 80,
        rows: 2000,
    });
    assert_eq!(e.feed(b"\x1b[14t\x1b[c", Feed::Live, &look), "\x1b[?6c");
}

#[test]
fn palette_cube_and_gray() {
    assert_eq!(color(&DEFAULT, 16), Some(Rgb { r: 0, g: 0, b: 0 }));
    assert_eq!(
        color(&DEFAULT, 231),
        Some(Rgb {
            r: 255,
            g: 255,
            b: 255
        })
    );
    assert_eq!(color(&DEFAULT, 232), Some(Rgb { r: 8, g: 8, b: 8 }));
    assert_eq!(
        color(&DEFAULT, 255),
        Some(Rgb {
            r: 238,
            g: 238,
            b: 238
        })
    );
    assert_eq!(color(&DEFAULT, 259), None);
    assert!(is_dark(DEFAULT.background));
}

/// OWE-490: 제목·벨·커서 모양·OSC 8 URI·줄바꿈·OSC 4 팔레트가 화면 캡처에 실린다.
#[test]
fn screen_carries_title_bell_cursor_links_wrap_and_palette() {
    let look = fresh();
    let mut e = Engine::new(WinSize { cols: 10, rows: 4 });
    let bytes = concat!(
        "\x1b]2;빌드 중\x07",                              // 제목
        "\x07\x07",                                        // 벨 둘
        "\x1b[5 q",                                        // DECSCUSR 5 = 깜박이는 막대
        "\x1b]8;;https://a.b/\x1b\\ab\x1b]8;;\x1b\\",      // 링크 2칸
        "\r\n0123456789xy",                                // 10칸을 넘겨 줄바꿈
        "\r\n\x1b]4;1;rgb:01/02/03\x07\x1b[31mR\x1b[32mG", // 1번 팔레트를 바꿈
    );
    e.feed(bytes.as_bytes(), Feed::Live, &look);
    let s = e.screen();
    assert_eq!(s.title, "빌드 중");
    assert_eq!(s.bells, 2);
    assert_eq!(s.cursor_style.shape, CursorShape::Bar);
    assert!(s.cursor_style.blink);
    assert_eq!(s.link_at(0, 0), Some("https://a.b/"));
    assert_eq!(s.link_at(0, 1), Some("https://a.b/"));
    assert_eq!(s.link_at(0, 2), None);
    assert!(s.rows[1][9].flags.contains(Flags::WRAPLINE));
    assert!(!s.rows[2].iter().any(|c| c.flags.contains(Flags::WRAPLINE)));
    assert_eq!(s.rows[3][0].fg, Color::Rgb(Rgb { r: 1, g: 2, b: 3 }));
    assert_eq!(s.rows[3][1].fg, Color::Indexed(2));

    // 재생한 바이트의 벨은 세지 않고, 제목 초기화(빈 OSC 2)는 비운다.
    e.feed(b"\x07\x1b]2;\x07", Feed::Replay, &look);
    let s = e.screen();
    assert_eq!(s.bells, 2);
    assert!(s.title.is_empty());

    // 긴 제목은 글자 경계에서 자른다.
    let long = format!("\x1b]2;{}\x07", "가".repeat(MAX_TITLE));
    e.feed(long.as_bytes(), Feed::Live, &look);
    let t = e.screen().title;
    assert!(t.len() <= MAX_TITLE && t.len() > MAX_TITLE - 3 && t.chars().all(|c| c == '가'));

    // MK-076: OSC 0/2 제목에 섞인 제어문자·양방향(bidi) 재정의 문자는 엔진 단계에서 제거한다.
    e.feed(
        "\x1b]0;정상\u{202e}exe.txt\u{202c}\t\n\x7f\u{0085}\u{2066}제목\u{2069}\x07".as_bytes(),
        Feed::Live,
        &look,
    );
    assert_eq!(e.title(), "정상exe.txt제목");
    assert_eq!(e.screen().title, "정상exe.txt제목");
}

/// OWE-495: 한글 화면을 1열로 줄여도 reflow가 끝나야 한다. 17열x5행에 한글/분해 자모를
/// 넣고 1열x1행으로 줄이면 alacritty 0.26 `shrink_columns`가 끝나지 않았다(1열에서는
/// 앞에 남은 wide 문자 하나가 매 반복에서 잘린 쪽 맨 앞으로 돌아가 같은 상태가 반복된다).
/// 엔진은 열 하한([`MIN_COLS`])으로 우회하므로 화면은 2열x1행이다.
#[test]
fn korean_screen_resize_to_one_column_finishes() {
    use std::time::Duration;
    let look = fresh();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut engine = Engine::new(WinSize { cols: 17, rows: 5 });
        engine.feed(
            "한글 가나다 힣 ㄱㅏ \u{1112}\u{1161}\u{11ab}\r\n".as_bytes(),
            Feed::Replay,
            &look,
        );
        engine.resize(WinSize { cols: 1, rows: 1 });
        let screen = engine.screen();
        done_tx.send((screen.cols, screen.lines)).ok();
    });
    let (cols, lines) = done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("1열 resize가 10초 안에 끝나야 한다 (OWE-495)");
    assert_eq!((cols, lines), (MIN_COLS, 1));
}

/// OWE-495: 열 하한. 0~1열로 만들거나 줄여도 엔진은 [`MIN_COLS`]열로 동작한다.
#[test]
fn engine_floors_narrow_widths_to_min_cols() {
    let look = fresh();
    for cols in [0, 1] {
        let mut engine = Engine::new(WinSize { cols, rows: 5 });
        assert_eq!(engine.screen().cols, MIN_COLS, "생성 cols={cols}");
        engine.feed("한\r\n".as_bytes(), Feed::Replay, &look);
        engine.resize(WinSize { cols, rows: 1 });
        assert_eq!(engine.screen().cols, MIN_COLS, "resize cols={cols}");
    }
}

/// MK-035: 강등은 핫 스크롤백만 버리고 화면·모드·제목·kitty 스택은 그대로 둔다. 강등 중엔 새 행을 쌓지 않고,
/// 강등을 풀면 다시 쌓는다.
#[test]
fn demote_drops_hot_history_keeps_screen_and_modes() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    let lines = "line\r\n".repeat(200);
    e.feed(
        format!("\x1b]2;빌드\x07\x1b[>1u\x1b[?1h\x1b[?2004h{lines}끝").as_bytes(),
        Feed::Live,
        &look,
    );
    assert!(e.history_len() > 0);
    let (screen, modes) = (e.screen(), e.input_modes());

    e.demote(&look);
    assert_eq!(e.history_len(), 0);
    assert_eq!(e.screen(), screen);
    assert_eq!(e.input_modes(), modes);
    assert_eq!(e.title(), "빌드");
    e.feed(lines.as_bytes(), Feed::Live, &look);
    assert_eq!(e.history_len(), 0, "parked engine keeps no new rows");
    assert_eq!(e.input_modes(), modes);

    e.promote(&look);
    e.feed(lines.as_bytes(), Feed::Live, &look);
    assert_eq!(e.history_len(), 200);
}

/// OWE-1114: 강등 중 출력은 묶음마다 grid를 자르지 않고 정리를 한 번 예약한다. 이어지는 출력이 예약을 미루지 않고,
/// 정리·강등·승격이 예약을 지운다. 새 행은 여전히 쌓지 않는다.
#[test]
fn demoted_output_defers_grid_trim_once() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    e.demote(&look);
    assert_eq!(e.trim_deadline(), None);
    let lines = "line\r\n".repeat(200);
    e.feed(lines.as_bytes(), Feed::Live, &look);
    let first = e.trim_deadline();
    assert!(first.is_some(), "demoted output schedules a trim");
    e.feed(lines.as_bytes(), Feed::Live, &look);
    assert_eq!(
        e.trim_deadline(),
        first,
        "more output keeps the first deadline"
    );
    assert_eq!(e.history_len(), 0);
    e.trim();
    assert_eq!(e.trim_deadline(), None);
    e.feed(lines.as_bytes(), Feed::Live, &look);
    e.demote(&look);
    assert_eq!(e.trim_deadline(), None, "demote truncates now");
    e.feed(lines.as_bytes(), Feed::Live, &look);
    e.promote(&look);
    assert_eq!(e.trim_deadline(), None, "promote keeps the cache");
    e.feed(lines.as_bytes(), Feed::Live, &look);
    assert_eq!(e.history_len(), 200);
}

/// 핫 스크롤백 상한은 [`HOT_LINES`]를 넘지 못하고, 강등을 풀면 만들 때의 상한으로 돌아간다.
#[test]
fn hot_history_is_capped_and_restored_on_promote() {
    let look = fresh();
    let mut big = Engine::with_history(SIZE, HOT_LINES * 2);
    big.feed("x\r\n".repeat(HOT_LINES * 2).as_bytes(), Feed::Live, &look);
    assert_eq!(big.history_len(), HOT_LINES);

    let mut small = Engine::with_history(SIZE, 50);
    small.demote(&look);
    small.promote(&look);
    small.feed("x\r\n".repeat(200).as_bytes(), Feed::Live, &look);
    assert_eq!(small.history_len(), 50);
}

/// 열린 sync 구간·미완성 제어열 안에서 강등해도 파서 꼬리를 잃지 않는다(같은 입력의 강등 안 한 엔진과 화면이 같다).
#[test]
fn demote_mid_sequence_keeps_parser_tail() {
    let look = fresh();
    for (head, tail) in [
        (&b"a\x1b[?2026hsync "[..], &b"body\x1b[?2026l"[..]),
        (b"b\x1b[3", b"1mred"),
        (b"c \xed\x95", b"\x9c"),
    ] {
        let (mut parked, mut plain) = (Engine::new(SIZE), Engine::new(SIZE));
        for (e, park) in [(&mut parked, true), (&mut plain, false)] {
            e.feed(head, Feed::Live, &look);
            if park {
                e.demote(&look);
            }
            e.feed(tail, Feed::Live, &look);
        }
        assert_eq!(parked.screen(), plain.screen(), "head={head:?}");
    }
}

/// MK-033: OSC 52 쓰기는 살아 있는 출력만 남기고, 읽기에는 답하지 않으며, 너무 큰 값은 버린다.
#[test]
fn osc52_write_only_live() {
    let look = fresh();
    let mut e = Engine::new(WinSize { cols: 10, rows: 4 });
    assert_eq!(e.clipboard(), (0, ""));
    // "hello"
    assert!(
        e.feed(b"\x1b]52;c;aGVsbG8=\x07", Feed::Live, &look)
            .is_empty()
    );
    assert_eq!(e.clipboard(), (1, "hello"));
    // 재생한 쓰기는 이미 한 번 썼다.
    e.feed(b"\x1b]52;c;d29ybGQ=\x07", Feed::Replay, &look);
    assert_eq!(e.clipboard(), (1, "hello"));
    // 읽기(질의)는 답하지 않고 값도 바꾸지 않는다.
    assert!(e.feed(b"\x1b]52;c;?\x07", Feed::Live, &look).is_empty());
    assert_eq!(e.clipboard(), (1, "hello"));
    // 상한을 넘는 쓰기는 버린다(base64 4글자 = 3바이트).
    let big = format!(
        "\x1b]52;c;{}\x07",
        "QUFB".repeat(krill_proto::clipboard::MAX_CLIPBOARD / 3 + 1)
    );
    e.feed(big.as_bytes(), Feed::Live, &look);
    assert_eq!(e.clipboard(), (1, "hello"));
    // 상한 이하는 받는다(위 거절이 파서 한도 때문이 아님을 확인).
    let max = krill_proto::clipboard::MAX_CLIPBOARD / 3 * 3;
    let fits = format!("\x1b]52;c;{}\x07", "QUFB".repeat(max / 3));
    e.feed(fits.as_bytes(), Feed::Live, &look);
    assert_eq!(e.clipboard().0, 2);
    assert_eq!(e.clipboard().1.len(), max);
}

/// MK-033 × MK-035: 강등(핫 스크롤백 해제) 중에도 OSC 52 쓰기는 남고 읽기는 막히며, 강등·복귀가 값을 지우지 않는다.
#[test]
fn osc52_survives_demote_and_promote() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    e.feed(b"\x1b]52;c;aGVsbG8=\x07", Feed::Live, &look);
    e.demote(&look);
    assert_eq!(e.clipboard(), (1, "hello"));
    assert!(e.feed(b"\x1b]52;c;?\x07", Feed::Live, &look).is_empty());
    e.feed(b"\x1b]52;c;bmV3\x07", Feed::Live, &look);
    assert_eq!(e.clipboard(), (2, "new"));
    e.promote(&look);
    assert_eq!(e.clipboard(), (2, "new"));
    assert!(e.feed(b"\x1b]52;c;?\x07", Feed::Live, &look).is_empty());
}

/// OWE-474: kitty 키보드 스택이 가득 차도 panic 없이 가장 오래된 모드를 버린다.
/// 수정 전에는 키보드 스택 대신 제목 스택에서 `remove(0)` → 빈 제목 스택에서 panic
/// (출력 바이트만으로 Term 종료, `DoS`), 제목이 저장돼 있으면 저장된 제목이 하나 사라진다.
#[test]
fn kitty_keyboard_stack_overflow_evicts_oldest() {
    let look = fresh();
    // 제목 스택이 비어 있다: 수정 전 코드는 여기서 panic 한다.
    let mut bare = Engine::new(SIZE);
    bare.feed(&b"\x1b[>1u".repeat(4097), Feed::Live, &look);
    assert_eq!(bare.feed(b"\x1b[?u", Feed::Live, &look), "\x1b[?1u");

    // 저장된 제목은 overflow 뒤에도 그대로 있어야 한다(수정 전에는 하나 사라진다).
    let mut e = Engine::new(SIZE);
    e.feed(b"\x1b]2;fixed\x07\x1b[22t", Feed::Live, &look);
    e.feed(&b"\x1b[>1u".repeat(4097), Feed::Live, &look);
    e.feed(b"\x1b[23t", Feed::Live, &look);
    assert_eq!(e.title(), "fixed");
    assert_eq!(e.feed(b"\x1b[?u", Feed::Live, &look), "\x1b[?1u");
}

/// K-01: 거대 창 크기는 와이어 상한으로 잘린다. 잘리지 않으면 재기동 때
/// `tcgetwinsize` 값을 그대로 Term에 넣어 수백 MB를 할당하고(3000×3000에서
/// footprint 14MB→526MB 실측) abort·jetsam 재기동 루프가 된다.
#[test]
fn giant_winsize_is_clamped_to_wire_max() {
    use krill_proto::screen::{MAX_COLS, MAX_LINES};
    assert_eq!(
        clamp_size(WinSize { cols: 0, rows: 0 }),
        WinSize {
            cols: MIN_COLS,
            rows: 1
        }
    );
    assert_eq!(
        clamp_size(WinSize { cols: 1, rows: 24 }),
        WinSize {
            cols: MIN_COLS,
            rows: 24
        }
    );
    assert_eq!(
        clamp_size(WinSize {
            cols: u16::MAX,
            rows: u16::MAX
        }),
        WinSize {
            cols: MAX_COLS,
            rows: MAX_LINES
        }
    );
    assert_eq!(
        clamp_size(WinSize { cols: 80, rows: 24 }),
        WinSize { cols: 80, rows: 24 }
    );
    // 상한을 넘는 크기(WIN 한계는 2048×1024). 수정 전에는 이 엔진이
    // (1000+1500)×2600 격자를 할당한다.
    let e = Engine::new(WinSize {
        cols: 2600,
        rows: 1500,
    });
    let text = e.screen_text();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.len() <= MAX_LINES as usize,
        "screen rows {} > MAX_LINES {MAX_LINES}",
        lines.len()
    );
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    assert!(
        widest <= MAX_COLS as usize,
        "screen cols {widest} > MAX_COLS {MAX_COLS}"
    );
}

/// OSC 7·OSC 9;9 cwd 보고: 청크 경계를 넘어도 잇고, 이 OS의 절대 경로가 아니면 버린다.
#[test]
fn shell_cwd_reports() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    assert_eq!(e.cwd(), None);
    #[cfg(unix)]
    let (url, expected, quoted) = ("file://host/tmp/a%20b", "/tmp/a b", "\"/tmp/c;d\"");
    #[cfg(windows)]
    let (url, expected, quoted) = (
        "file://host/C:/Users/a%20b",
        "C:/Users/a b",
        "\"C:\\Users\\c;d\"",
    );
    let osc7 = format!("\x1b]7;{url}\x07");
    let (head, tail) = osc7.split_at(7);
    e.feed(head.as_bytes(), Feed::Live, &look);
    e.feed(tail.as_bytes(), Feed::Live, &look);
    assert_eq!(e.cwd(), Some(Path::new(expected)));
    e.feed(
        format!("\x1b]9;9;{quoted}\x1b\\").as_bytes(),
        Feed::Live,
        &look,
    );
    let quoted = quoted.trim_matches('"');
    assert_eq!(e.cwd(), Some(Path::new(quoted)));
    // 상대 경로·원격 꼴·다른 OSC는 마지막 값을 바꾸지 않는다.
    e.feed(
        b"\x1b]9;9;\"relative\"\x07\x1b]7;file:\x07\x1b]2;title\x07",
        Feed::Live,
        &look,
    );
    assert_eq!(e.cwd(), Some(Path::new(quoted)));
}

#[test]
fn clear_viewport_keeps_prompt_line_and_modes_without_pty_input() {
    let look = fresh();
    let mut engine = Engine::new(SIZE);
    engine.feed(b"old output\r\nprompt> \x1b[?2004h", Feed::Live, &look);
    let modes = engine.input_modes();
    engine.clear_viewport();
    let screen = engine.screen();
    assert_eq!(screen.cursor.line, 0);
    assert_eq!(screen.cursor.column, 8);
    assert_eq!(
        screen
            .rows
            .first()
            .unwrap()
            .iter()
            .map(|cell| cell.ch)
            .collect::<String>(),
        "prompt>"
    );
    assert!(screen.rows.iter().skip(1).all(Vec::is_empty));
    assert_eq!(engine.input_modes(), modes);
    assert_eq!(engine.history_len(), 0);
}

#[test]
fn alternate_screen_and_scroll_modes_follow_dec_private_sequences() {
    use krill_proto::term_modes::Modes;
    let mut engine = Engine::new(SIZE);
    let look = fresh();
    assert!(!engine.input_modes().contains(Modes::ALT_SCREEN));
    assert!(engine.input_modes().contains(Modes::ALTERNATE_SCROLL));
    engine.feed(b"\x1b[?1049h", Feed::Live, &look);
    assert!(engine.input_modes().contains(Modes::ALT_SCREEN));
    engine.feed(b"\x1b[?1007l", Feed::Live, &look);
    assert!(!engine.input_modes().contains(Modes::ALTERNATE_SCROLL));
    engine.feed(b"\x1b[?1049l", Feed::Live, &look);
    assert!(!engine.input_modes().contains(Modes::ALT_SCREEN));
}

#[test]
fn dashboard_tail_matches_plain_capture_without_composing_cells() {
    let look = Appearance::load(PathBuf::new());
    let mut engine = Engine::new(WinSize { cols: 640, rows: 6 });
    let mut bytes = b"first\r\n\r\nsecond\r\n".to_vec();
    bytes.extend(std::iter::repeat_n(b' ', 520));
    bytes.extend_from_slice(b"late-column\r\nlast");
    engine.feed(&bytes, Feed::Replay, &look);
    let text = engine.screen_text();
    let mut expected: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(3)
        .map(|line| line.chars().take(500).collect::<String>())
        .collect();
    expected.reverse();
    assert_eq!(engine.screen_tail(3, 500), expected);
}

#[test]
fn replay_rebase_preserves_partial_vt_sync_and_inactive_history() {
    let look = Appearance::load(PathBuf::new());
    let size = WinSize { cols: 12, rows: 3 };
    let mut canonical = Engine::new(size);
    let mut view = Engine::new(size);
    for bytes in [
        &b"primary1\r\nprimary2\r\nprimary3\r\nprimary4\r\n"[..],
        &b"\x1b[?1049hALT\r\none\r\ntwo\r\n\x1b[?2026h\xe9"[..],
    ] {
        canonical.feed(bytes, Feed::Replay, &look);
        view.feed(bytes, Feed::Replay, &look);
        view.take_history();
    }
    canonical.resize(WinSize { cols: 24, rows: 7 });
    view.rebase_replay(&canonical);
    assert_eq!(view.screen(), canonical.screen());
    assert_eq!(view.input_modes(), canonical.input_modes());
    assert_eq!(view.capture_rewind(), canonical.capture_rewind());
    view.clear_history();
    for bytes in [
        &b"\x9b\xaa\r\n\x1b[?2026l\x1b[?1049l\x1b[38;2;"[..],
        &b"1;2;3mnormal-again\r\n"[..],
    ] {
        canonical.feed(bytes, Feed::Replay, &look);
        view.feed(bytes, Feed::Replay, &look);
        assert_eq!(view.screen(), canonical.screen());
        assert_eq!(view.capture_rewind(), canonical.capture_rewind());
    }
    // The canonical primary history survives alternate-screen resizing and is restored
    // even though the harvesting view previously drained those same rows.
    canonical.resize(WinSize { cols: 8, rows: 2 });
    view.rebase_replay(&canonical);
    assert_eq!(view.screen(), canonical.screen());
    assert_eq!(
        view.history_rows(0, HOT_LINES),
        canonical.history_rows(0, HOT_LINES)
    );
    assert!(view.history_len() <= HOT_LINES);
    assert!(canonical.history_len() <= HOT_LINES);
}

#[test]
fn snapshot_hot_rows_survives_demote_and_live_csi_3j_until_explicit_clear_or_resize() {
    let look = Appearance::load(PathBuf::new());
    let mut engine = Engine::new(WinSize { cols: 20, rows: 2 });
    engine.feed(b"row-1\r\nrow-2\r\nrow-3\r\nrow-4", Feed::Live, &look);
    let row_texts = |rows: Vec<Vec<Cell>>| -> Vec<String> {
        rows.into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    };
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["row-2", "row-1"]
    );

    // Parking drops live grid rows to 0 but keeps compressed hot snapshot for immediate scroll on reattach.
    engine.demote(&look);
    assert_eq!(engine.history_len(), 0);
    engine.promote(&look);
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["row-2", "row-1"]
    );

    // Child TUI redraw with CSI 3 J wipes grid history, but snapshot_hot_rows keeps prior hot rows
    // behind any newly scrolled rows in newest-first order.
    engine.feed(
        b"\x1b[2J\x1b[3J\x1b[Hafter-1\r\nafter-2\r\nafter-3",
        Feed::Live,
        &look,
    );
    assert_eq!(engine.history_len(), 1);
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["after-1", "row-2", "row-1"]
    );

    //Resize on attach (e.g. schedule_fit) keeps preserved hot rows available for immediate scroll.
    engine.resize(WinSize { cols: 24, rows: 2 });
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["after-1", "row-2", "row-1"]
    );

    // Explicit user clear_history drops preserved hot rows too.
    engine.clear_history();
    assert!(engine.snapshot_hot_rows().is_empty());
}

#[test]
fn parked_session_preserves_and_updates_hot_rows_across_background_output_and_resize() {
    let look = Appearance::load(PathBuf::new());
    let mut engine = Engine::new(WinSize { cols: 20, rows: 2 });
    engine.feed(b"row-1\r\nrow-2\r\nrow-3\r\nrow-4", Feed::Live, &look);
    let row_texts = |rows: Vec<Vec<Cell>>| -> Vec<String> {
        rows.into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    };

    engine.demote(&look);
    assert_eq!(engine.history_len(), 0);

    // In-place statusbar redraw while parked must not wipe parked_hot.
    engine.feed(
        b"\x1b[?2026h\x1b[?7l\r\x1b[1Arow-3\r\nrow-4 (4h 4m)\x1b[?7h\x1b[?2026l",
        Feed::Live,
        &look,
    );
    assert_eq!(engine.history_len(), 0);
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["row-2", "row-1"]
    );

    // Background scrolling output while parked updates parked_hot while keeping grid truncated.
    engine.feed(b"\r\nrow-5", Feed::Live, &look);
    assert_eq!(engine.history_len(), 0);
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["row-3", "row-2", "row-1"]
    );

    // Reattaching promotes and fits the window size without losing hot scrollback.
    engine.promote(&look);
    engine.resize(WinSize { cols: 25, rows: 2 });
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["row-3", "row-2", "row-1"]
    );
}

#[test]
fn csi_3j_in_replay_and_live_preserves_hot_rows() {
    let look = Appearance::load(PathBuf::new());
    let row_texts = |rows: Vec<Vec<Cell>>| -> Vec<String> {
        rows.into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    };

    for mode in [Feed::Live, Feed::Replay] {
        let mut engine = Engine::new(WinSize { cols: 20, rows: 2 });
        engine.feed(b"row-1\r\nrow-2\r\nrow-3\r\nrow-4", mode, &look);
        engine.feed(
            b"\x1b[2J\x1b[3J\x1b[Hafter-1\r\nafter-2\r\nafter-3",
            mode,
            &look,
        );
        assert_eq!(engine.history_len(), 1);
        assert_eq!(
            row_texts(engine.snapshot_hot_rows()),
            vec!["after-1", "row-2", "row-1"]
        );
        let (total, rows) = engine.history_rows(0, 10);
        assert_eq!(total, 3);
        assert_eq!(row_texts(rows), vec!["row-1", "row-2", "after-1"]);
    }
}

#[test]
fn single_chunk_csi_3j_and_ris_preserve_hot_rows_and_unparked_history_rows() {
    let look = Appearance::load(PathBuf::new());
    let row_texts = |rows: Vec<Vec<Cell>>| -> Vec<String> {
        rows.into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    };

    for mode in [Feed::Live, Feed::Replay] {
        let mut engine = Engine::new(WinSize { cols: 20, rows: 2 });
        engine.feed(
            b"row-1\r\nrow-2\r\nrow-3\r\nrow-4\x1b[2J\x1b[3J\x1b[Hmid-1\r\nmid-2\r\nmid-3\x1bcafter-1\r\nafter-2\r\nafter-3",
            mode,
            &look,
        );
        assert_eq!(
            row_texts(engine.snapshot_hot_rows()),
            vec!["after-1", "mid-1", "row-2", "row-1"]
        );
        let (total, sliced) = engine.history_rows(1, 2);
        assert_eq!(total, 4);
        assert_eq!(row_texts(sliced), vec!["row-2", "mid-1"]);

        // Parked sessions report 0 live history to history_rows while keeping snapshot_hot_rows.
        engine.demote(&look);
        assert_eq!(engine.history_rows(0, 10).0, 0);
        engine.promote(&look);
        assert_eq!(
            row_texts(engine.history_rows(0, 10).1),
            vec!["row-1", "row-2", "mid-1", "after-1"]
        );
        assert_eq!(
            row_texts(engine.take_history()),
            vec!["row-1", "row-2", "mid-1", "after-1"]
        );
        assert!(engine.snapshot_hot_rows().is_empty());
    }
}

#[test]
fn dec2026_sync_csi_2j_3j_preserves_hot_rows_across_single_and_split_feeds() {
    let look = Appearance::load(PathBuf::new());
    let row_texts = |rows: Vec<Vec<Cell>>| -> Vec<String> {
        rows.into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    };

    // Case 1: Single-chunk output + synced CSI 2 J + CSI 3 J + trailing synced block.
    let mut engine = Engine::new(WinSize { cols: 20, rows: 2 });
    engine.feed(
        b"row-1\r\nrow-2\r\nrow-3\r\nrow-4\x1b[?2026h\x1b[2J\x1b[3J\x1b[Hmid-1\r\nmid-2\x1b[?2026l\x1b[?2026h\r\nafter-1\x1b[?2026l",
        Feed::Live,
        &look,
    );
    assert_eq!(
        row_texts(engine.snapshot_hot_rows()),
        vec!["mid-1", "row-2", "row-1"]
    );
    assert_eq!(
        row_texts(engine.history_rows(0, 10).1),
        vec!["row-1", "row-2", "mid-1"]
    );

    // Case 2: Split-chunk CSI 2 J -> synced CSI 3 J -> pure ASCII chunk -> ESU.
    let mut split = Engine::new(WinSize { cols: 20, rows: 2 });
    split.feed(
        b"row-1\r\nrow-2\r\nrow-3\r\nrow-4\x1b[2J",
        Feed::Live,
        &look,
    );
    split.feed(b"\x1b[?2026h\x1b[3J\x1b[H", Feed::Live, &look);
    split.feed(b"live-1\r\nlive-2", Feed::Live, &look);
    split.feed(b"\x1b[?2026l", Feed::Live, &look);
    assert_eq!(row_texts(split.snapshot_hot_rows()), vec!["row-2", "row-1"]);

    // Case 3: Split-chunk CSI 2 J -> synced CSI 3 J -> pure ASCII chunk -> end_sync timeout.
    let mut timeout = Engine::new(WinSize { cols: 20, rows: 2 });
    timeout.feed(
        b"row-1\r\nrow-2\r\nrow-3\r\nrow-4\x1b[2J",
        Feed::Live,
        &look,
    );
    timeout.feed(b"\x1b[?2026h\x1b[3J\x1b[H", Feed::Live, &look);
    timeout.feed(b"live-1\r\nlive-2", Feed::Live, &look);
    timeout.end_sync(Feed::Live, &look);
    assert_eq!(
        row_texts(timeout.snapshot_hot_rows()),
        vec!["row-2", "row-1"]
    );
}
