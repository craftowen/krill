use super::*;
use gpui::{EntityInputHandler as _, TestAppContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn item(id: u64, label: &str, needs_you: bool) -> Item {
    Item::new(SessionId(id), label.into(), needs_you)
}

fn ids(items: &[Item], query: &str) -> Vec<u64> {
    search(items, query)
        .into_iter()
        .map(|i| items[i].id.0)
        .collect()
}

#[test]
fn fuzzy_order_hangul_and_needs_you() {
    let items = [
        item(1, "claude 1 · minikrill · 작업 중", false),
        item(2, "codex 2 · 미니크릴 · 입력 대기", true),
        item(3, "claude 3 · mk-owe-422 · 완료", false),
        item(4, "claude 4 · main · 입력 대기", true),
    ];
    // 빈 검색어: 입력 대기 세션이 맨 위, 나머지는 원래 순서.
    assert_eq!(ids(&items, ""), [2, 4, 1, 3]);
    // 부분 수열·대소문자·공백 무시.
    assert_eq!(ids(&items, "MKO"), [3]);
    assert_eq!(ids(&items, "mini krill"), [1]);
    // 한글 음절 단위 퍼지, 연속 일치가 흩어진 일치보다 앞.
    assert_eq!(ids(&items, "미니"), [2]);
    assert_eq!(ids(&items, "대기"), [2, 4]);
    assert_eq!(ids(&items, "작중"), [1]);
    assert!(ids(&items, "없는말").is_empty());
    // 초성·조합 중 음절.
    assert_eq!(ids(&items, "ㅁㄴㅋ"), [2]);
    assert_eq!(ids(&items, "미ㄴ"), [2]);
    assert_eq!(ids(&items, "이"), [2, 4]);
    assert!(ids(&items, "읻").is_empty());
    // 연속 일치("main")가 흩어진 일치보다 높다.
    assert_eq!(ids(&items, "main")[0], 4);
}

fn thousand() -> Vec<Item> {
    let states = ["작업 중", "입력 대기", "승인 대기", "완료", "종료"];
    (0..1000u64)
        .map(|i| {
            let state = states[usize::try_from(i).unwrap() % states.len()];
            item(
                i,
                &format!("claude {i} · 워크트리-{i:04}-feature-branch · {state}"),
                i % 7 == 0,
            )
        })
        .collect()
}

/// 완료 조건: 1,000개 항목 응답 ≤ 50ms. 검색 함수만 잰다(디버그 빌드).
#[test]
fn thousand_items_search_under_50ms() {
    let items = thousand();
    for query in ["", "c", "워크", "0999", "feat대기", "zzz"] {
        let start = Instant::now();
        let hits = search(&items, query);
        let took = start.elapsed();
        eprintln!("search 1000 {query:?}: {} hits in {took:?}", hits.len());
        assert!(took < Duration::from_millis(50), "{query}: {took:?}");
    }
}

fn open(
    cx: &mut TestAppContext,
    items: Vec<Item>,
) -> (
    Entity<Palette>,
    Rc<RefCell<Vec<Event>>>,
    &mut gpui::VisualTestContext,
) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|window, cx| Palette::new(items, window, cx));
    cx.update(|window, cx| view.update(cx, |p, cx| p.focus(window, cx)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let got = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, e: &Event, _| got.borrow_mut().push(*e))
            .detach();
    });
    cx.run_until_parked();
    (view, events, cx)
}

fn shown(view: &Entity<Palette>, cx: &mut gpui::VisualTestContext) -> Vec<u64> {
    cx.update(|_, cx| view.read(cx).matches().map(|i| i.id.0).collect())
}

/// 한글 IME: 조합 중 글자(preedit)로 바로 거르고, 확정·이동·Enter로 고른다. Esc는 닫기.
#[gpui::test]
fn hangul_preedit_filters_and_keys_confirm(cx: &mut TestAppContext) {
    let items = vec![
        item(1, "claude 1 · 한글 작업", false),
        item(2, "codex 2 · 하늘", false),
        item(3, "claude 3 · 한강 대기", true),
        item(4, "claude 4 · main", false),
    ];
    let (view, events, cx) = open(cx, items);
    assert_eq!(shown(&view, cx), [3, 1, 2, 4]);

    let input = cx.update(|_, cx| view.read(cx).input().clone());
    cx.update(|window, cx| {
        input.update(cx, |s, cx| {
            s.replace_and_mark_text_in_range(None, "하", None, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(shown(&view, cx), [3, 1, 2]);
    cx.update(|window, cx| {
        input.update(cx, |s, cx| {
            s.replace_and_mark_text_in_range(None, "한", None, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(shown(&view, cx), [3, 1]);
    // 확정은 IME 입력 경로(insertText)로.
    cx.simulate_input("한");
    assert_eq!(cx.update(|_, cx| input.read(cx).value().to_string()), "한");
    assert_eq!(shown(&view, cx), [3, 1]);

    // OWE-862: ↑↓는 순환한다. 2행에서 down 3번은 0→1→0→1.
    cx.simulate_keystrokes("down down down enter");
    assert_eq!(*events.borrow(), [Event::Confirm(SessionId(1))]);
    // 단일 행 입력이 위·아래를 먹지 않았다(커서·값 그대로).
    assert_eq!(cx.update(|_, cx| input.read(cx).value().to_string()), "한");

    cx.simulate_keystrokes("escape");
    assert_eq!(events.borrow().last(), Some(&Event::Dismiss));
}

/// 1,000개 항목에서 키 입력 → 거른 목록이 다시 그려질 때까지(테스트 플랫폼, 디버그 빌드).
#[gpui::test]
fn thousand_items_keystroke_to_render_under_50ms(cx: &mut TestAppContext) {
    let (view, _, cx) = open(cx, thousand());
    let mut last = usize::MAX;
    for query in ["0", "09", "099", "0999"] {
        let start = Instant::now();
        cx.simulate_input(query.get(query.len() - 1..).unwrap());
        cx.run_until_parked();
        let took = start.elapsed();
        let hits = shown(&view, cx);
        eprintln!("palette 1000 {query:?}: {} hits in {took:?}", hits.len());
        assert!(hits.contains(&999) && hits.len() <= last, "{query}");
        last = hits.len();
        assert!(took < Duration::from_millis(50), "{query}: {took:?}");
    }
}

#[gpui::test]
fn jump_palette_filters_vault_hits_and_emits_resume_vault(cx: &mut TestAppContext) {
    let (view, events, cx) = open(cx, vec![item(1, "claude 1 · main · 작업 중", false)]);
    let vault_hits = vec![
        VaultHit {
            agent: VaultAgent::Codex,
            id: "22222222-2222-4333-8444-555555555555".into(),
            cwd: "/tmp/repo-a/wt-1".into(),
            modified: 20,
            title: "Zstd Scrollback".into(),
            summary: "Segment writer".into(),
            snippet: "Compressed 64KB segments".into(),
            role: "assistant".into(),
            score: 100,
        },
        VaultHit {
            agent: VaultAgent::OpenCode,
            id: "ses_oc_1".into(),
            cwd: "/tmp/repo-b".into(),
            modified: 10,
            title: "Payments Schema".into(),
            summary: "Foreign key".into(),
            snippet: "ALTER TABLE payments".into(),
            role: "user".into(),
            score: 80,
        },
    ];
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            p.set_scope_paths(
                Some("/tmp/repo-a/wt-1".into()),
                vec!["/tmp/repo-a/wt-1".into()],
                cx,
            );
            p.set_vault_hits(vault_hits, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).vault_matches().count()), 2);

    // Filter by Workspace scope
    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.set_scope(VaultScope::Workspace, cx));
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).vault_matches().count()), 1);

    // Filter by OpenCode agent in All scope
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            p.set_scope(VaultScope::All, cx);
            p.set_agent_filter(Some(VaultAgent::OpenCode), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).vault_matches().count()), 1);

    // One-click Resume on OpenCode vault hit (index 1)
    let btn = cx
        .debug_bounds("jump-vault-resume-1")
        .expect("jump vault resume button");
    cx.simulate_click(btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(events.borrow().last(), Some(&Event::ResumeVault(1)));
}

/// OWE-768: 열린 세션이 있을 때와 0개일 때 모두 방향키(↑/↓)·Enter로
/// AI Vault 섹션의 원하는 항목을 탐색·선택해 재개할 수 있고,
/// 팝오버를 미리 열지 않아도 팔레트 오픈·쿼리 입력 시 볼트 스캔·검색이 연동된다.
#[gpui::test]
fn jump_palette_keyboard_navigates_vault_items_and_syncs_history_search(cx: &mut TestAppContext) {
    use krill_proto::agent_history::{Action, Agent, Entry, Response, SearchSnapshot, Snapshot};
    use krill_proto::control::{Reply, Request};
    use std::sync::{Arc, Mutex};

    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let call: crate::agent_history::Call = Arc::new(move |r| {
        seen.lock().unwrap().push(r.clone());
        Ok(Reply::AgentHistory(match r {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: vec![
                    Entry {
                        agent: Agent::Claude,
                        id: "11111111-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/repo-a".into(),
                        modified: 30,
                    },
                    Entry {
                        agent: Agent::Codex,
                        id: "22222222-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/repo-b".into(),
                        modified: 20,
                    },
                ],
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(q)) => Response::Searched(SearchSnapshot {
                hits: vec![
                    VaultHit {
                        agent: VaultAgent::Claude,
                        id: "11111111-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/repo-a".into(),
                        modified: 30,
                        title: format!("First hit for {}", q.query),
                        summary: String::new(),
                        snippet: "first snippet".into(),
                        role: "assistant".into(),
                        score: 120,
                    },
                    VaultHit {
                        agent: VaultAgent::Agy,
                        id: "33333333-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/repo-b".into(),
                        modified: 20,
                        title: format!("Second hit for {}", q.query),
                        summary: String::new(),
                        snippet: "second snippet".into(),
                        role: "assistant".into(),
                        score: 100,
                    },
                ],
                truncated: false,
                skipped: false,
            }),
            _ => unreachable!(),
        }))
    });

    let (view, events, cx) = open(cx, vec![item(1, "claude 1 · main · 작업 중", false)]);
    let history = cx.update(|_, cx| cx.new(|_| crate::agent_history::Panel::new(call)));

    // AI Vault 팝오버를 한 번도 열지 않은 상태(`!history.is_open()`)에서 팔레트 연결:
    // 즉시 스캔이 실행되어 볼트 항목 2건이 적재된다.
    cx.update(|_, cx| {
        assert!(!history.read(cx).is_open());
        view.update(cx, |p, cx| p.connect_history(&history, cx));
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).vault_matches().count()), 2);

    // 열린 세션 1개(index 0) + 볼트 2개(index 1, 2):
    // down 두 번으로 2번째 볼트 항목(index 2)까지 이동 후 Enter로 선택.
    cx.simulate_keystrokes("down down enter");
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 2);
    assert_eq!(events.borrow().as_slice(), &[Event::ResumeVault(1)]);

    // 열린 세션과 일치하지 않는 검색어("snippet") 입력:
    // 백엔드 `Action::Search`가 연동되고, 열린 세션 매치가 0개여도
    // down/up/enter로 볼트 2번째 항목을 선택할 수 있다.
    events.borrow_mut().clear();
    cx.simulate_input("snippet");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).matches().count()), 0);
    assert_eq!(cx.update(|_, cx| view.read(cx).vault_matches().count()), 2);

    // OWE-862: ↑↓는 순환한다. 2행에서 down 3번은 0→1→0→1.
    cx.simulate_keystrokes("down down down enter");
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 1);
    assert_eq!(events.borrow().as_slice(), &[Event::ResumeVault(1)]);
}

fn worktree(id: &str, label: &str, session: Option<u64>) -> WorktreeEntry {
    WorktreeEntry::new(
        format!("/repo/{id}").into(),
        label.into(),
        Some("repo".into()),
        session.map(SessionId),
    )
}

#[test]
fn worktree_and_action_search_rank_and_filter() {
    let entries = vec![
        worktree("main", "main", Some(1)),
        worktree("feature-x", "feature-x", None),
    ];
    assert_eq!(worktree_search(&entries, ""), [0, 1]);
    assert_eq!(worktree_search(&entries, "feat"), [1]);
    assert_eq!(worktree_search(&entries, "main"), [0]);
    // 경로는 검색되지만 한글 쿼리와 라틴 경로가 섞여도 깨지지 않는다.
    assert_eq!(worktree_search(&entries, "repo").len(), 2);
    assert!(worktree_search(&entries, "없는워크트리").is_empty());

    let actions = vec![
        ActionEntry::new(ActionKind::NewWorktree, "새 워크트리 만들기".into()),
        ActionEntry::new(ActionKind::OpenSettings, "설정 열기".into()),
    ];
    assert_eq!(action_search(&actions, ""), [0, 1]);
    assert_eq!(action_search(&actions, "설정"), [1]);
    assert!(action_search(&actions, "없는액션").is_empty());
}

/// OWE-865: 섹션 순서(⌘J: 세션→워크트리→액션→빠른 명령, ⇧⌘P: 액션→빠른 명령→세션→워크트리)와
/// ⌘1~9 행 즉시 선택(`jump_to_row`)이 이벤트로 이어진다.
#[gpui::test]
fn palette_sections_and_digit_jump_emit_events(cx: &mut TestAppContext) {
    let (view, events, cx) = open(cx, vec![item(1, "claude 1 · main · 작업 중", false)]);
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            p.set_worktrees(
                vec![
                    worktree("main", "main", Some(1)),
                    worktree("empty", "empty", None),
                ],
                cx,
            );
            p.set_actions(
                vec![
                    ActionEntry::new(ActionKind::NewWorktree, "새 워크트리 만들기".into()),
                    ActionEntry::new(ActionKind::OpenSettings, "설정 열기".into()),
                ],
                cx,
            );
            p.set_quick_commands(
                vec![crate::commands::QuickCommand {
                    name: "상태".into(),
                    text: "git status".into(),
                }],
                cx,
            );
        });
    });
    cx.run_until_parked();
    // Jump 모드: 세션 1 + 워크트리 2 + 액션 2 + 빠른 명령 1.
    assert_eq!(cx.update(|_, cx| view.read(cx).row_count()), 6);

    // 1행(세션 있는 워크트리) → 세션 열기.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| assert!(p.jump_to_row(1, cx)));
    });
    assert_eq!(events.borrow().as_slice(), &[Event::Confirm(SessionId(1))]);
    events.borrow_mut().clear();

    // 2행(세션 없는 워크트리) → 새 세션 만들기.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| assert!(p.jump_to_row(2, cx)));
    });
    assert_eq!(events.borrow().as_slice(), &[Event::OpenWorktree(1)]);
    events.borrow_mut().clear();

    // 3행(첫 액션) → 빠른 액션 실행.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| assert!(p.jump_to_row(3, cx)));
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[Event::Action(ActionKind::NewWorktree)]
    );
    events.borrow_mut().clear();

    // 5행(빠른 명령) → 설정 인덱스 0 실행.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| assert!(p.jump_to_row(5, cx)));
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[Event::Action(ActionKind::QuickCommand(0))]
    );
    events.borrow_mut().clear();

    // 범위를 벗어난 행은 무시한다.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| assert!(!p.jump_to_row(6, cx)));
    });
    assert!(events.borrow().is_empty());

    // Commands 모드: 액션이 맨 앞으로 온다.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.set_mode(Mode::Commands, cx));
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Commands);
    cx.update(|_, cx| {
        view.update(cx, |p, cx| assert!(p.jump_to_row(0, cx)));
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[Event::Action(ActionKind::NewWorktree)]
    );
}

/// OWE-862: ↑↓ 순환(끝에서 반대쪽으로) — 위·아래 모두 감싼다.
#[gpui::test]
fn palette_step_wraps_around(cx: &mut TestAppContext) {
    let (view, events, cx) = open(cx, vec![item(1, "a", false), item(2, "b", false)]);
    // 0에서 위로 가면 마지막 행, 마지막에서 아래로 가면 첫 행.
    cx.simulate_keystrokes("up");
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 1);
    cx.simulate_keystrokes("down");
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 0);
    cx.simulate_keystrokes("enter");
    assert_eq!(*events.borrow(), [Event::Confirm(SessionId(1))]);
}

/// OWE-862: ⌘P 파일 모드는 파일용 빈 문구·플레이스홀더를 쓰고 세션만 보여준다.
#[gpui::test]
fn files_mode_uses_file_empty_text(cx: &mut TestAppContext) {
    let (view, _, cx) = open(cx, vec![item(1, "src/lib.rs", false)]);
    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.set_mode(Mode::Files, cx));
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Files);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).empty_text()),
        "일치하는 파일 없음"
    );
    // 일치 없음 → 빈 행 0개, 힌트·빈 문구 선택자가 있다.
    cx.simulate_input("없는파일zzz");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).row_count()), 0);
    assert!(cx.debug_bounds("jump-empty").is_some());
    assert!(cx.debug_bounds("jump-hint").is_some());
}

#[gpui::test]
fn jump_palette_worktrees_project_filters_and_cmd_digits(cx: &mut TestAppContext) {
    let mut s1 = item(1, "* Claude Code · minikrill · main · 작업 중", false);
    s1.title = "* Claude Code".into();
    s1.agent = "claude".into();
    s1.state = Some(AgentState::Working);
    s1.is_current_tab = true;
    s1.host_label = "Local Mac".into();
    s1.repo_name = "minikrill".into();
    s1.worktree_name = "main".into();

    let mut s2 = item(2, "zsh · orca · feat-jump · 완료", false);
    s2.title = "zsh".into();
    s2.state = Some(AgentState::Done);
    s2.is_current_worktree = true;
    s2.host_label = "Local Mac".into();
    s2.repo_name = "orca".into();
    s2.worktree_name = "feat-jump".into();

    let (view, events, cx) = open(cx, vec![s1, s2]);
    let worktrees = vec![
        WorktreeItem::new(
            "/tmp/minikrill".into(),
            Some(SessionId(1)),
            "minikrill".into(),
            "main".into(),
            "minikrill".into(),
            "Local Mac".into(),
            "1분 미만".into(),
            Some(AgentState::Working),
            false,
            true,
            true,
        ),
        WorktreeItem::new(
            "/tmp/orca-wt".into(),
            None,
            "feat-jump".into(),
            "feat/jump".into(),
            "orca".into(),
            "Local Mac".into(),
            "5분".into(),
            None,
            false,
            false,
            false,
        ),
    ];
    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.set_worktrees(worktrees, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).worktree_matches().count()),
        2
    );

    // 프로젝트 필터("orca") 적용 시 세션·작업 공간 모두 해당 프로젝트만 남는다.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.set_project_filter(Some("orca".into()), cx));
    });
    cx.run_until_parked();
    assert_eq!(shown(&view, cx), [2]);
    assert_eq!(
        cx.update(|_, cx| {
            view.read(cx)
                .worktree_matches()
                .map(|w| w.name.clone())
                .collect::<Vec<_>>()
        }),
        ["feat-jump"]
    );

    // 모두 지우기로 필터 해제.
    cx.update(|_, cx| {
        view.update(cx, super::Palette::clear_all_filters);
    });
    cx.run_until_parked();
    assert_eq!(shown(&view, cx), [1, 2]);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).worktree_matches().count()),
        2
    );

    // Cmd+2 단축키로 2번째 최근 세션 즉시 열기.
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(events.borrow().as_slice(), &[Event::Confirm(SessionId(2))]);

    // 아래 방향키로 `최근 작업 공간` 섹션의 2번째 워크트리(세션 미보유)까지 내려가 Enter → OpenWorktree(1).
    events.borrow_mut().clear();
    cx.simulate_keystrokes("down down enter");
    assert_eq!(events.borrow().as_slice(), &[Event::OpenWorktree(1)]);
}

#[test]
fn infer_agent_from_title_detects_brands_when_status_agent_is_empty() {
    use crate::ui::status::infer_agent_from_title;
    assert_eq!(infer_agent_from_title("", "* Claude Code"), "claude");
    assert_eq!(infer_agent_from_title("zsh", "✳ Claude Code"), "claude");
    assert_eq!(infer_agent_from_title("", "Codex CLI"), "codex");
    assert_eq!(infer_agent_from_title("", "Antigravity Agent"), "agy");
    assert_eq!(infer_agent_from_title("", "OpenCode"), "opencode");
    assert_eq!(infer_agent_from_title("claude", "zsh"), "claude");
    assert_eq!(infer_agent_from_title("", "zsh"), "");
}

#[gpui::test]
fn jump_palette_windowing_across_chats_and_worktrees(cx: &mut TestAppContext) {
    let sessions: Vec<Item> = (1..=10)
        .map(|i| item(i, &format!("s-{i}"), false))
        .collect();
    let (view, _events, cx) = open(cx, sessions);
    let worktrees: Vec<WorktreeEntry> = (0..10)
        .map(|i| worktree(&format!("wt-{i}"), &format!("wt-{i}"), None))
        .collect();
    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.set_worktrees(worktrees, cx));
    });
    cx.run_until_parked();

    // Move selection into Worktrees section (index 10 = first worktree, index 16 = 7th worktree > EMPTY_QUERY_WORKTREE_CAP).
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            p.selected = 16;
            cx.notify();
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        let p = view.read(cx);
        // Selecting a worktree must NOT jump chat_window to the end of chats.
        assert_eq!(p.chat_window(), (0, super::EMPTY_QUERY_RECENT_TAB_CAP, 4));
        // And the selected worktree (index 6 within Worktrees) must be inside worktree_window.
        let (wt_start, wt_end, _) = p.worktree_window();
        assert!((wt_start..wt_end).contains(&6));
        assert!(p.selected_list_child_index().is_some());
    });
}
