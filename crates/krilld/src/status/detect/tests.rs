use std::fmt::Write as _;

use krill_proto::SessionId;
use krill_proto::holder::WinSize;

use super::*;
use crate::appearance::Appearance;
use crate::status::Statuses;
use crate::term::{Engine, Feed};

const ID: SessionId = SessionId(49);

/// MK-006에서 실제 CLI로 녹화한 원본을 OSC 제목 경계마다 엔진에 넣는다. 재생 질의 응답은 항상 0건이다.
fn replay(recording: &[u8], chunk: usize) -> String {
    let look = Appearance::load("/nonexistent/mk049/theme".into());
    let mut engine = Engine::new(WinSize {
        cols: 120,
        rows: 40,
    });
    let st = Statuses::open(None);
    let mut detector = Detector::default();
    let now = Instant::now();
    let mut out = String::new();
    let mut before = (AgentState::Unknown, None, Confidence::Low);
    // 고정된 관찰 경계 사이 바이트 청크를 바꿔도 같은 상태가 나와야 한다.
    let mut start = 0;
    for end in recording
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| (b == 7).then_some(i + 1))
        .chain(std::iter::once(recording.len()))
    {
        for bytes in recording.get(start..end).unwrap().chunks(chunk) {
            assert!(engine.feed(bytes, Feed::Replay, &look).is_empty());
        }
        assert!(engine.end_sync(Feed::Replay, &look).is_empty());
        let status = st.get(ID);
        if let Some(e) = detector.observe(
            engine.title(),
            &engine.screen_text(),
            end as u64,
            &status,
            now,
        ) {
            st.inferred(ID, e, end as u64);
        }
        let status = st.get(ID);
        let current = (status.state, status.source, status.confidence);
        if current != before {
            writeln!(
                out,
                "{:?}/{:?}/{:?} badge={:?}",
                current.0,
                current.1,
                current.2,
                badge(&status)
            )
            .unwrap();
            before = current;
        }
        start = end;
    }
    out
}

#[test]
fn recorded_agents_golden() {
    let claude = include_bytes!("../../../tests/fixtures/claude-code.120x40.rec");
    let codex = include_bytes!("../../../tests/fixtures/codex.120x40.rec");
    for chunk in [1, 17, 1024] {
        assert_eq!(
            replay(claude, chunk),
            concat!(
                "Done/Some(Title)/Medium badge=Some(\"추정\")\n",
                "Working/Some(Title)/Medium badge=Some(\"추정\")\n",
                "Done/Some(Title)/Medium badge=Some(\"추정\")\n",
                "Working/Some(Title)/Medium badge=Some(\"추정\")\n",
            )
        );
        assert_eq!(
            replay(codex, chunk),
            concat!(
                "Working/Some(Title)/Medium badge=Some(\"추정\")\n",
                "Done/Some(Title)/Medium badge=Some(\"추정\")\n",
                "Working/Some(Title)/Medium badge=Some(\"추정\")\n",
                "Done/Some(Title)/Medium badge=Some(\"추정\")\n",
            )
        );
    }
}

#[test]
fn hook_wins_except_confirmed_dialog_dismissal() {
    let st = Statuses::open(None);
    let hook = crate::status::tests::event(
        1,
        10,
        "claude",
        r#"{"hook_event_name":"PermissionRequest"}"#,
    );
    st.hook(&[(ID, &hook)]).unwrap();
    let mut detector = Detector::default();
    let now = Instant::now();
    let dialog = "Do you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
    let e = detector
        .observe("✳ Claude Code", dialog, 1, &st.get(ID), now)
        .unwrap();
    st.inferred(ID, e, 20);
    assert_eq!(st.get(ID).source, Some(Source::Hook));
    // Esc 입력만으로는 상태를 바꾸지 않는다. 실제 redraw + quiet + 빈 프롬프트가 있어야 한다.
    assert!(detector.observe("", "❯", 2, &st.get(ID), now).is_none());
    let e = detector
        .observe("", "❯", 2, &st.get(ID), now + QUIET)
        .unwrap();
    st.inferred(ID, e, 30);
    let status = st.get(ID);
    assert_eq!(
        (
            status.state,
            status.source,
            status.confidence,
            badge(&status)
        ),
        (
            AgentState::Done,
            Some(Source::Screen),
            Confidence::Low,
            Some("추정")
        )
    );
    // 화면 시각보다 오래됐지만 PermissionRequest보다 새로운 hook은 여전히 우선한다.
    let work =
        crate::status::tests::event(2, 25, "claude", r#"{"hook_event_name":"UserPromptSubmit"}"#);
    assert_eq!(st.hook(&[(ID, &work)]).unwrap(), 1);
    assert_eq!(st.get(ID).state, AgentState::Working);
    st.exited(ID);
    st.inferred(ID, e, 40);
    assert_eq!(st.get(ID).state, AgentState::Exited);
    assert_eq!(badge(&st.get(ID)), None);
}

#[test]
fn screen_rules_need_quiet_and_menu_choices() {
    let mut detector = Detector::default();
    let now = Instant::now();
    for (rev, text, state) in [
        (
            1,
            "Do you trust this folder?\n❯ 1. Yes, I trust this folder",
            AgentState::Blocked,
        ),
        (
            2,
            "Would you like to run this command?\n1. Yes\n2. No",
            AgentState::Waiting,
        ),
        (
            3,
            "Do you trust the files in this folder?\n\n❯ No, exit\n  Yes, I trust this folder",
            AgentState::Blocked,
        ),
    ] {
        assert!(
            detector
                .observe("", text, rev, &AgentStatus::UNKNOWN, now)
                .is_none()
        );
        let e = detector
            .observe("", text, rev, &AgentStatus::UNKNOWN, now + QUIET)
            .unwrap();
        assert_eq!(
            (e.state, e.source, e.confidence),
            (state, Source::Screen, Confidence::Low)
        );
    }
    assert!(
        detector
            .observe(
                "unknown title",
                "explaining: trust this folder",
                4,
                &AgentStatus::UNKNOWN,
                now + QUIET
            )
            .is_none()
    );
}

#[test]
fn unknown_title_and_unseen_dialog_do_not_clear_hook_waiting() {
    let mut detector = Detector::default();
    let status = AgentStatus {
        state: AgentState::Waiting,
        source: Some(Source::Hook),
        ..AgentStatus::UNKNOWN
    };
    let now = Instant::now();
    assert!(
        detector
            .observe("random project", "❯", 1, &status, now)
            .is_none()
    );
    assert!(
        detector
            .observe("random project", "❯", 1, &status, now + QUIET)
            .is_none()
    );
}

/// 대화상자 없이 들어온 Claude waiting hook은 빈 입력줄이 유예 동안 이어지면 풀리고, 대화상자를 본 대기는 남는다.
#[test]
fn stray_claude_waiting_without_dialog_clears_after_grace() {
    for (title, cleared) in [
        ("✳ Claude Code", AgentState::Done),
        ("⠋ Claude Code", AgentState::Working),
    ] {
        let st = Statuses::open(None);
        let hook = crate::status::tests::event(
            1,
            10,
            "claude",
            r#"{"hook_event_name":"PermissionRequest"}"#,
        );
        st.hook(&[(ID, &hook)]).unwrap();
        let mut detector = Detector::default();
        let now = Instant::now();
        let idle = "────\n❯ \n────";
        // 유예 전에는 제목 근거가 나와도 hook waiting이 이긴다.
        for at in [now, now + QUIET] {
            if let Some(e) = detector.observe(title, idle, 1, &st.get(ID), at) {
                st.inferred(ID, e, 15);
            }
            assert_eq!(st.get(ID).state, AgentState::Waiting);
        }
        let e = detector
            .observe(title, idle, 1, &st.get(ID), now + STRAY_WAITING)
            .unwrap();
        st.inferred(ID, e, 20);
        let status = st.get(ID);
        assert_eq!(
            (status.state, status.source),
            (cleared, Some(Source::Screen))
        );
    }

    // 대화상자가 떠 있으면 아무리 오래 대기해도 그대로다.
    let st = Statuses::open(None);
    let hook = crate::status::tests::event(
        1,
        10,
        "claude",
        r#"{"hook_event_name":"PermissionRequest"}"#,
    );
    st.hook(&[(ID, &hook)]).unwrap();
    let mut detector = Detector::default();
    let now = Instant::now();
    let dialog = "Do you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
    for at in [now, now + STRAY_WAITING * 2] {
        if let Some(e) = detector.observe("✳ Claude Code", dialog, 1, &st.get(ID), at) {
            st.inferred(ID, e, 20);
        }
    }
    assert_eq!(st.get(ID).state, AgentState::Waiting);
}

/// 새 waiting hook이 도착한 뒤 이전 대화상자 소실 근거로 새 대기를 지우지 않는다.
#[test]
fn dismissal_does_not_clear_a_new_waiting_episode() {
    let st = Statuses::open(None);
    for (ns, payload) in [
        (10, "PermissionRequest"),
        (20, "UserPromptSubmit"),
        (30, "PermissionRequest"),
    ] {
        let hook = crate::status::tests::event(
            1,
            ns,
            "claude",
            &format!(r#"{{"hook_event_name":"{payload}"}}"#),
        );
        st.hook(&[(ID, &hook)]).unwrap();
    }
    st.inferred(
        ID,
        Evidence {
            state: AgentState::Done,
            source: Source::Screen,
            confidence: Confidence::Low,
            dismissed_since: Some(10),
        },
        40,
    );
    assert_eq!(st.get(ID).state, AgentState::Waiting);
    assert_eq!(st.get(ID).since_unix_ns, 30);
}

proptest::proptest! {
    #[test]
    fn arbitrary_text_never_panics_or_claims_high_confidence(title in ".{0,128}", screen in ".{0,256}") {
        let mut detector = Detector::default();
        let now = Instant::now();
        for at in [now, now + QUIET] {
            if let Some(e) = detector.observe(&title, &screen, 1, &AgentStatus::UNKNOWN, at) {
                proptest::prop_assert_ne!(e.confidence, Confidence::High);
                proptest::prop_assert_ne!(e.state, AgentState::Exited);
            }
        }
    }
}

#[test]
fn inferred_menu_clears_when_prompt_returns() {
    let st = Statuses::open(None);
    let mut detector = Detector::default();
    let now = Instant::now();
    let menu = "Do you want to proceed?\n❯ 1. Yes";
    detector.observe("", menu, 1, &st.get(ID), now);
    let e = detector
        .observe("", menu, 1, &st.get(ID), now + QUIET)
        .unwrap();
    st.inferred(ID, e, 10);
    assert_eq!(st.get(ID).state, AgentState::Waiting);
    detector.observe("", "❯", 2, &st.get(ID), now + QUIET);
    let e = detector
        .observe("", "❯", 2, &st.get(ID), now + QUIET * 2)
        .unwrap();
    st.inferred(ID, e, 20);
    assert_eq!(st.get(ID).state, AgentState::Done);
}

/// 서명 규칙으로 화면 지식을 교체하면 앱 업데이트 없이 판정이 바뀐다(MK-091).
#[test]
fn custom_rules_replace_screen_knowledge() {
    use krill_proto::agent::rules::ScreenRules;

    let mut rules = ScreenRules::default();
    rules.blocked_phrases.push("새 승인 문구".to_owned());
    rules.done_glyph = '★';
    let mut detector = Detector::with_rules(rules);
    let now = Instant::now();
    let menu = "새 승인 문구\n❯ 1. Yes";
    assert!(
        detector
            .observe("", menu, 1, &AgentStatus::UNKNOWN, now)
            .is_none()
    );
    let e = detector
        .observe("", menu, 1, &AgentStatus::UNKNOWN, now + QUIET)
        .unwrap();
    assert_eq!(
        (e.state, e.source, e.confidence),
        (AgentState::Blocked, Source::Screen, Confidence::Low)
    );
    // 바뀐 완료 글리프도 제목에서 바로 읽는다.
    let e = detector
        .observe("★ 새 작업", "…", 2, &AgentStatus::UNKNOWN, now + QUIET)
        .unwrap();
    assert_eq!(e.state, AgentState::Done);
}

#[test]
fn screen_menu_overrides_terminal_title_and_hook_working() {
    let st = Statuses::open(None);
    let mut detector = Detector::default();
    let now = Instant::now();
    let dialog = "Do you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";

    // 제목이 ✳ Claude Code(Done)이거나 ◐(Working)이어도 QUIET 뒤 화면 메뉴가 우선한다.
    assert_eq!(
        detector
            .observe("✳ Claude Code", dialog, 1, &st.get(ID), now)
            .unwrap()
            .state,
        AgentState::Done
    );
    let e = detector
        .observe("✳ Claude Code", dialog, 1, &st.get(ID), now + QUIET)
        .unwrap();
    assert_eq!(
        (e.state, e.source, e.confidence),
        (AgentState::Waiting, Source::Screen, Confidence::Low)
    );
    st.inferred(ID, e, 10);
    assert_eq!(st.get(ID).state, AgentState::Waiting);

    // Hook Working 상태에서도 전용 훅 없이 뜬 화면 승인 메뉴는 Waiting(Screen)으로 반영된다.
    let work = crate::status::tests::event(1, 20, "agy", r#"{"hook_event_name":"PreInvocation"}"#);
    st.hook(&[(ID, &work)]).unwrap();
    assert_eq!(
        (st.get(ID).state, st.get(ID).source),
        (AgentState::Working, Some(Source::Hook))
    );
    detector.observe("", dialog, 2, &st.get(ID), now + QUIET);
    let e = detector
        .observe("", dialog, 2, &st.get(ID), now + QUIET * 2)
        .unwrap();
    st.inferred(ID, e, 30);
    assert_eq!(
        (st.get(ID).state, st.get(ID).source),
        (AgentState::Waiting, Some(Source::Screen))
    );

    // 메뉴가 사라지고 명령 실행 화면(프롬프트 아님)으로 바뀌면 Working으로 복귀한다.
    detector.observe("", "Running cargo test...", 3, &st.get(ID), now + QUIET * 2);
    let e = detector
        .observe("", "Running cargo test...", 3, &st.get(ID), now + QUIET * 3)
        .unwrap();
    st.inferred(ID, e, 40);
    assert_eq!(st.get(ID).state, AgentState::Working);
}

#[test]
fn orca_title_waiting_working_and_done_glyphs() {
    let mut detector = Detector::default();
    let now = Instant::now();
    for (rev, title, expected) in [
        (1, "✋ Action Required (Gemini)", AgentState::Waiting),
        (2, "▲ Qoder permission", AgentState::Waiting),
        (3, "π ! My Project", AgentState::Waiting),
        (4, "✦ Thinking (krill)", AgentState::Working),
        (5, "⏲ Working (krill)", AgentState::Working),
        (6, "◇ Ready (krill)", AgentState::Done),
        (7, "Claude — Action Required", AgentState::Waiting),
        (8, "Antigravity: Permission Needed", AgentState::Waiting),
    ] {
        let e = detector
            .observe(title, "…", rev, &AgentStatus::UNKNOWN, now)
            .unwrap_or_else(|| panic!("expected evidence for {title}"));
        assert_eq!(
            (e.state, e.source, e.confidence),
            (expected, Source::Title, Confidence::Medium),
            "title={title}"
        );
    }
    // 에이전트 이름이 없는 일반 창 제목의 permission 단어는 오탐하지 않는다.
    assert!(
        detector
            .observe(
                "chmod permission denied",
                "…",
                9,
                &AgentStatus::UNKNOWN,
                now
            )
            .is_none()
    );
}

#[test]
fn codex_queued_question_survives_continuous_redraw_and_hook_working() {
    let st = Statuses::open(None);
    let mut detector = Detector::default();
    let now = Instant::now();
    let work = crate::status::tests::event(
        1,
        10,
        "codex",
        r#"{"hook_event_name":"PreToolUse","tool_name":"exec_command"}"#,
    );
    st.hook(&[(ID, &work)]).unwrap();
    let screen = "• Working (14m 03s • esc to interrupt)\n• Queued follow-up inputs\n  ? 1 question\n  ⇧← to answer\n\n› Ask Codex to do anything";
    // 스피너·경과 시간으로 revision이 매번 바뀌어도 질문이 계속 있으면 Waiting이다.
    for tick in 0..=6 {
        let e = detector
            .observe(
                "⠋ | Codex task",
                screen,
                tick + 1,
                &st.get(ID),
                now + Duration::from_millis(tick * 100),
            )
            .unwrap();
        st.inferred(ID, e, 20 + tick);
        assert_eq!(
            st.get(ID).state,
            if tick < 3 {
                AgentState::Working
            } else {
                AgentState::Waiting
            }
        );
    }
    assert_eq!(st.get(ID).source, Some(Source::Screen));
    // 병렬 도구 hook이 Working을 알려도 미응답 질문은 다음 관찰에서 다시 잡는다.
    let work = crate::status::tests::event(
        2,
        40,
        "codex",
        r#"{"hook_event_name":"PostToolUse","tool_name":"exec_command"}"#,
    );
    st.hook(&[(ID, &work)]).unwrap();
    let e = detector
        .observe("⠙ | Codex task", screen, 8, &st.get(ID), now + QUIET * 3)
        .unwrap();
    st.inferred(ID, e, 50);
    assert_eq!(st.get(ID).state, AgentState::Waiting);
    // 응답 뒤에는 작업 중으로 돌아간다. 일반 입력창만 있다고 Done으로 바꾸지 않는다.
    let e = detector
        .observe(
            "⠹ | Codex task",
            "• Working\n› Ask Codex to do anything",
            9,
            &st.get(ID),
            now + QUIET * 4,
        )
        .unwrap();
    st.inferred(ID, e, 60);
    assert_eq!(st.get(ID).state, AgentState::Working);
}

#[test]
fn codex_queued_question_must_remain_visible_without_a_title() {
    let mut detector = Detector::default();
    let now = Instant::now();
    let question = "• Queued follow-up inputs\n? 1 question\n⇧← to answer\n›";
    for (rev, ms, screen) in [
        (1, 0, question),
        (2, 200, "Working..."),
        (3, 400, question),
        (4, 500, question),
    ] {
        assert!(
            detector
                .observe(
                    "",
                    screen,
                    rev,
                    &AgentStatus::UNKNOWN,
                    now + Duration::from_millis(ms),
                )
                .is_none()
        );
    }
    let e = detector
        .observe(
            "",
            question,
            5,
            &AgentStatus::UNKNOWN,
            now + Duration::from_millis(700),
        )
        .unwrap();
    assert_eq!(
        (e.state, e.source, e.confidence),
        (AgentState::Waiting, Source::Screen, Confidence::Low)
    );
}
