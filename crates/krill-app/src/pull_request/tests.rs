use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn sliding_window_and_minimum_gap_cover_refresh_and_create() {
    let now = Instant::now();
    let mut budget = Budget::default();
    for i in 0..20 {
        let at = now + Duration::from_secs(i * 10);
        assert!(budget.reserve(at));
        assert!(!budget.reserve(at));
        assert!(!budget.reserve(at + Duration::from_secs(9)));
    }
    assert!(!budget.reserve(now + Duration::from_secs(299)));
    assert!(budget.reserve(now + WINDOW));
    assert!(!budget.reserve(now + WINDOW + Duration::from_secs(9)));
    assert!(budget.reserve(now + WINDOW + GAP));
}

struct Fake(AtomicUsize);
impl Runner for Fake {
    fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Status::Open)
    }
}

#[test]
fn all_branches_and_create_share_budget_and_branch_switch_drops_stale_badge() {
    let fake = Arc::new(Fake(AtomicUsize::new(0)));
    let service = Service::new(fake.clone());
    let now = Instant::now();
    let mut branches: BTreeMap<_, _> = (0..30)
        .map(|i| (PathBuf::from(format!("/repo/{i}")), format!("b-{i}")))
        .collect();
    let first = service.refresh(&branches, now);
    assert_eq!(fake.0.load(Ordering::SeqCst), 1);
    let path = first.keys().next().unwrap().clone();
    // OWE-850: user-initiated create/inspect is not blocked by background polling cooldown.
    assert!(service.create(&path, &branches[&path], now).is_ok());
    assert_eq!(fake.0.load(Ordering::SeqCst), 2);
    branches.insert(path.clone(), "switched".into());
    assert!(!service.refresh(&branches, now).contains_key(&path));
    for i in 1..20 {
        service.refresh(&branches, now + GAP * i);
    }
    assert_eq!(fake.0.load(Ordering::SeqCst), 21);
    // Background refresh is still capped at 20 per WINDOW, while manual create/inspect still works.
    assert_eq!(
        service
            .refresh(&branches, now + Duration::from_secs(299))
            .len(),
        19
    );
    assert_eq!(fake.0.load(Ordering::SeqCst), 21);
    assert!(
        service
            .create(&path, "switched", now + Duration::from_secs(299))
            .is_ok()
    );
    assert_eq!(fake.0.load(Ordering::SeqCst), 22);
    assert_eq!(service.refresh(&branches, now + WINDOW).len(), 20);
    assert_eq!(fake.0.load(Ordering::SeqCst), 23);
}

#[test]
fn oldest_due_branch_is_selected_and_stable_cache_is_reused() {
    let fake = Arc::new(Fake(AtomicUsize::new(0)));
    let service = Service::new(fake.clone());
    let now = Instant::now();
    let branches = BTreeMap::from([
        (PathBuf::from("/a"), "a".into()),
        (PathBuf::from("/b"), "b".into()),
    ]);
    service.refresh(&branches, now);
    let statuses = service.refresh(&branches, now + GAP);
    assert_eq!(statuses.len(), 2);
    service.refresh(&branches, now + GAP * 2);
    assert_eq!(fake.0.load(Ordering::SeqCst), 2);
    service.refresh(&branches, now + REFRESH);
    assert_eq!(fake.0.load(Ordering::SeqCst), 3);
}

#[test]
fn states_missing_cli_and_auth_failure_hide_badges_and_provide_guidance() {
    for (json, expected) in [
        (r"[]", Status::NoPr),
        (r#"[{"state":"OPEN","isDraft":false}]"#, Status::Open),
        (r#"[{"state":"OPEN","isDraft":true}]"#, Status::Draft),
        (r#"[{"state":"MERGED","isDraft":false}]"#, Status::Merged),
        (r#"[{"state":"CLOSED","isDraft":false}]"#, Status::Closed),
    ] {
        assert_eq!(parse(json.as_bytes()).unwrap(), expected);
    }
    for bytes in [
        b"{".as_slice(),
        br#"[{"state":"new","isDraft":false}]"#,
        br"{}",
    ] {
        assert!(parse(bytes).is_err());
    }
    for (kind, expected) in [
        (io::ErrorKind::NotFound, Status::Missing),
        (io::ErrorKind::Other, Status::Failed(Failure::Other)),
        (io::ErrorKind::TimedOut, Status::Failed(Failure::TimedOut)),
        // 권한 거부(spawn 실패 등)는 gh 인증 실패로 단정하지 않는다.
        (
            io::ErrorKind::PermissionDenied,
            Status::Failed(Failure::Other),
        ),
    ] {
        let status = status_error(&kind.into());
        assert_eq!(status, expected);
        assert!(status.badge().is_none());
        assert!(status.guidance().is_some());
    }
}

#[test]
fn failures_distinguish_auth_timeout_and_general_errors_without_leaking_details() {
    type Case = (io::Error, Failure, &'static str);
    let cases: [Case; 3] = [
        (
            io::Error::new(io::ErrorKind::PermissionDenied, GhAuthRequired),
            Failure::Auth,
            FAILED.get(),
        ),
        (
            io::Error::new(io::ErrorKind::TimedOut, "secret timeout"),
            Failure::TimedOut,
            text::TIMED_OUT.get(),
        ),
        (
            io::Error::other("secret response"),
            Failure::Other,
            text::GH_FAILED.get(),
        ),
    ];
    for (error, expected, message) in cases {
        let status = status_error(&error);
        assert_eq!(status, Status::Failed(expected));
        assert_eq!(status.guidance(), Some(message));
        assert_eq!(failure_text(&error), message);
        assert!(!message.contains("secret"));
    }
}

#[cfg(unix)]
#[test]
fn gh_exit_four_is_authentication_required_without_stderr_in_guidance() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "printf secret >&2; exit 4"]);
    let error = execute(&mut cmd, Duration::from_secs(1)).unwrap_err();
    assert_eq!(status_error(&error), Status::Failed(Failure::Auth));
    assert!(!error.to_string().contains("secret"));
}

#[test]
fn literal_head_and_web_creation_never_request_push_or_fork() {
    let folder = Path::new("/tmp/한글 ' $(touch never)");
    let branch = "feature/a;$(touch never)";
    for action in [Action::Refresh, Action::Create] {
        let cmd = command(folder, branch, action);
        assert_eq!(cmd.get_current_dir(), Some(folder));
        let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert!(args.windows(2).any(|p| p == ["--head", branch]));
        assert!(!args.contains(&"--fill"));
        assert_eq!(args.contains(&"--web"), action == Action::Create);
    }
}

#[test]
fn bounded_output_timeout_nonzero_and_missing_executable_are_local_and_reaped() {
    let mut ok = Command::new("/bin/sh");
    ok.args(["-c", "printf '[{\"state\":\"OPEN\",\"isDraft\":false}]'"]);
    assert_eq!(
        parse(&execute(&mut ok, Duration::from_secs(1)).unwrap()).unwrap(),
        Status::Open
    );
    let mut failed = Command::new("/bin/sh");
    failed.args(["-c", "exit 1"]);
    let error = execute(&mut failed, Duration::from_secs(1)).unwrap_err();
    assert_eq!(status_error(&error), Status::Failed(Failure::Other));
    // gh 문서의 종료 코드 4(인증 필요)만 로그인 안내로 분류한다. stderr는 읽지 않는다.
    let mut unauthenticated = Command::new("/bin/sh");
    unauthenticated.args(["-c", "echo secret-token >&2; exit 4"]);
    let error = execute(&mut unauthenticated, Duration::from_secs(1)).unwrap_err();
    assert_eq!(status_error(&error), Status::Failed(Failure::Auth));
    assert!(!error.to_string().contains("secret-token"));
    let mut missing = Command::new("/no-such-mk090-gh");
    assert_eq!(
        execute(&mut missing, Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    let mut flood = Command::new("/bin/sh");
    flood.args([
        "-c",
        "while :; do printf '012345678901234567890123456789'; done",
    ]);
    assert_eq!(
        execute(&mut flood, Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::Other
    );
    let mut hung = Command::new("/bin/sh");
    hung.args(["-c", "while :; do :; done"]);
    let start = Instant::now();
    assert_eq!(
        execute(&mut hung, Duration::from_millis(30))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}

proptest::proptest! {
    #[test]
    fn arbitrary_json_output_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
        drop(parse(&bytes));
        drop(parse_checks_from_pr_json(&bytes));
        drop(parse_review_threads_json(&bytes));
    }

    #[test]
    fn arbitrary_log_text_never_panics(raw in ".*") {
        drop(extract_error_log_tail(&raw));
        drop(strip_ansi_codes(&raw));
    }
}

#[test]
fn two_windows_keep_independent_branch_caches_with_one_global_budget() {
    let fake = Arc::new(Fake(AtomicUsize::new(0)));
    let service = Service::new(fake.clone());
    let now = Instant::now();
    let a = BTreeMap::from([(PathBuf::from("/a"), "a".into())]);
    let b = BTreeMap::from([(PathBuf::from("/b"), "b".into())]);
    assert_eq!(service.refresh(&a, now).len(), 1);
    assert!(service.refresh(&b, now).is_empty());
    assert_eq!(service.refresh(&b, now + GAP).len(), 1);
    assert_eq!(service.refresh(&a, now + GAP * 2).len(), 1);
    assert_eq!(fake.0.load(Ordering::SeqCst), 2);
}

#[test]
fn missing_cli_and_unauthenticated_runner_are_guidance_only() {
    type Case = (fn() -> io::Error, Status, &'static str);
    struct Unavailable(fn() -> io::Error);
    impl Runner for Unavailable {
        fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
            Err((self.0)())
        }
    }
    let cases: [Case; 4] = [
        (
            || io::ErrorKind::NotFound.into(),
            Status::Missing,
            MISSING.get(),
        ),
        (
            || io::Error::new(io::ErrorKind::PermissionDenied, GhAuthRequired),
            Status::Failed(Failure::Auth),
            FAILED.get(),
        ),
        (
            || io::ErrorKind::TimedOut.into(),
            Status::Failed(Failure::TimedOut),
            text::TIMED_OUT.get(),
        ),
        (
            || io::Error::other("gh failed"),
            Status::Failed(Failure::Other),
            text::GH_FAILED.get(),
        ),
    ];
    for (error, status, guidance) in cases {
        assert_eq!(status.guidance(), Some(guidance));
        let service = Service::new(Arc::new(Unavailable(error)));
        let path = PathBuf::from("/repo");
        let branches = BTreeMap::from([(path.clone(), "branch".into())]);
        let now = Instant::now();
        assert_eq!(service.refresh(&branches, now)[&path], status);
        assert!(status.badge().is_none());
        assert_eq!(service.create(&path, "branch", now + GAP), Err(guidance));
    }
}

#[test]
fn create_command_includes_title_and_body_and_parse_pr_draft_extracts_sections() {
    let folder = Path::new("/tmp/repo");
    let cmd = create_command(folder, "feat/pr", "feat: add sync", "## Summary\n- item 1");
    let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
    assert!(args.windows(2).any(|p| p == ["--head", "feat/pr"]));
    assert!(args.windows(2).any(|p| p == ["--title", "feat: add sync"]));
    assert!(
        args.windows(2)
            .any(|p| p == ["--body", "## Summary\n- item 1"])
    );

    let draft = parse_pr_draft(
        "TITLE: feat: git panel sync\nBODY:\n- Add Push and Pull\n- Add AI recovery",
        "feat/sync",
    );
    assert_eq!(draft.title, "feat: git panel sync");
    assert_eq!(draft.body, "- Add Push and Pull\n- Add AI recovery");

    let synth = synthesize_pr_draft(
        "feat/sync",
        "first commit\nsecond commit\n",
        "1 file changed",
    );
    assert_eq!(synth.title, "first commit");
    assert!(synth.body.contains("- first commit"));
    assert!(synth.body.contains("1 file changed"));
}

#[gpui::test]
fn pr_dialog_ai_generate_populates_fields_and_submit_creates_pr(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, MouseButton};

    #[derive(Default)]
    struct DraftRunner {
        created: Mutex<Vec<(String, String, String)>>,
    }
    impl Runner for DraftRunner {
        fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
            Ok(Status::NoPr)
        }
        fn create_pr(&self, _: &Path, branch: &str, title: &str, body: &str) -> io::Result<Status> {
            self.created.lock().unwrap().push((
                branch.to_owned(),
                title.to_owned(),
                body.to_owned(),
            ));
            Ok(Status::Open)
        }
        fn generate_draft(&self, _: &Path, branch: &str) -> io::Result<Draft> {
            Ok(Draft {
                title: format!("feat({branch}): auto PR title"),
                body: "## Summary\n- Auto generated PR body".into(),
            })
        }
    }

    cx.update(gpui_component::init);
    let runner = Arc::new(DraftRunner::default());
    let service = Arc::new(Service::new(runner.clone()));
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);

    let (dialog, cx) = cx.add_window_view(|window, cx| {
        Dialog::new(
            PathBuf::from("/repo"),
            "owe-738".into(),
            service,
            window,
            cx,
        )
    });
    cx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, event: &Event, _| {
            events_clone.lock().unwrap().push(event.clone());
        })
        .detach();
    });
    cx.run_until_parked();

    let click = |selector: &'static str, cx: &mut gpui::VisualTestContext| {
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
    };

    click("pr-ai-generate", cx);
    cx.run_until_parked();

    dialog.read_with(cx, |d, cx| {
        assert_eq!(d.title.read(cx).value(), "feat(owe-738): auto PR title");
        assert_eq!(
            d.body.read(cx).value(),
            "## Summary\n- Auto generated PR body"
        );
    });

    click("pr-dialog-submit", cx);
    cx.run_until_parked();

    assert_eq!(
        *runner.created.lock().unwrap(),
        [(
            "owe-738".into(),
            "feat(owe-738): auto PR title".into(),
            "## Summary\n- Auto generated PR body".into()
        )]
    );
    assert_eq!(*events.lock().unwrap(), [Event::Created, Event::Close]);
}

#[test]
fn parse_checks_from_pr_json_maps_check_runs_status_contexts_and_actions_ids() {
    assert_eq!(
        parse_checks_from_pr_json(b"[]").unwrap().status,
        Status::NoPr
    );
    let json = br#"[{
        "number": 739,
        "title": "feat: PR CI checks & AI fix queue",
        "url": "https://github.com/acme/krill/pull/739",
        "state": "OPEN",
        "isDraft": false,
        "statusCheckRollup": [
            {
                "__typename": "CheckRun",
                "name": "cargo fmt",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "detailsUrl": "https://github.com/acme/krill/actions/runs/1001/job/2001"
            },
            {
                "__typename": "CheckRun",
                "name": "cargo nextest",
                "status": "COMPLETED",
                "conclusion": "FAILURE",
                "detailsUrl": "https://github.com/acme/krill/actions/runs/1001/job/2002"
            },
            {
                "__typename": "CheckRun",
                "name": "macos-universal",
                "status": "IN_PROGRESS",
                "conclusion": ""
            },
            {
                "__typename": "CheckRun",
                "name": "nightly-fuzz",
                "status": "COMPLETED",
                "conclusion": "SKIPPED"
            },
            {
                "__typename": "StatusContext",
                "context": "Vercel Preview",
                "state": "FAILURE",
                "description": "Deployment failed",
                "targetUrl": "https://vercel.com/acme/krill/123"
            }
        ]
    }]"#;
    let pr = parse_checks_from_pr_json(json).unwrap();
    assert_eq!(pr.status, Status::Open);
    assert_eq!(pr.number, Some(739));
    assert_eq!(pr.title, "feat: PR CI checks & AI fix queue");
    assert_eq!(
        parse_repo_owner_and_name(&pr.url),
        Some(("acme".into(), "krill".into()))
    );
    assert_eq!(pr.count(CheckState::Failed), 2);
    assert_eq!(pr.count(CheckState::Pending), 1);
    assert_eq!(pr.count(CheckState::Passed), 1);
    assert_eq!(pr.count(CheckState::Skipped), 1);
    // Failed checks are sorted to the top.
    assert_eq!(pr.checks[0].state, CheckState::Failed);
    assert_eq!(pr.checks[1].state, CheckState::Failed);
    let nextest = pr
        .checks
        .iter()
        .find(|c| c.name == "cargo nextest")
        .unwrap();
    assert_eq!(nextest.run_id.as_deref(), Some("1001"));
    assert_eq!(nextest.job_id.as_deref(), Some("2002"));
}

#[test]
fn extract_error_log_tail_strips_ansi_and_preserves_early_error_plus_tail() {
    let mut raw = String::new();
    raw.push_str("\x1b[32mCompiling krill-app v0.1.0\x1b[0m\n");
    raw.push_str("test result: ok. 120 passed; 0 failed; 0 ignored\n");
    raw.push_str("\x1b[31merror[E0425]: cannot find value `missing_var` in this scope\x1b[0m\n");
    raw.push_str("  --> crates/krill-app/src/diff.rs:42:9\n");
    for i in 0..90 {
        std::fmt::Write::write_fmt(
            &mut raw,
            format_args!("info: intermediate build step {i}\n"),
        )
        .ok();
    }
    raw.push_str("##[error]Process completed with exit code 101.\n");

    let extracted = extract_error_log_tail(&raw);
    assert!(!extracted.contains("\x1b["));
    assert!(extracted.contains("error[E0425]: cannot find value `missing_var`"));
    assert!(extracted.contains("crates/krill-app/src/diff.rs:42:9"));
    assert!(extracted.contains("... [middle log omitted] ..."));
    assert!(extracted.contains("##[error]Process completed with exit code 101."));
}

#[test]
fn parse_review_threads_json_filters_resolved_and_collects_replies_and_bots() {
    let json = br#"{
        "data": {
            "repository": {
                "pullRequest": {
                    "reviewThreads": {
                        "nodes": [
                            {
                                "id": "PRRT_resolved",
                                "isResolved": true,
                                "isOutdated": false,
                                "path": "src/old.rs",
                                "line": 10,
                                "comments": {
                                    "nodes": [
                                        {
                                            "id": "PRRC_1",
                                            "body": "Already fixed",
                                            "author": { "login": "alice", "__typename": "User" }
                                        }
                                    ]
                                }
                            },
                            {
                                "id": "PRRT_open",
                                "isResolved": false,
                                "isOutdated": true,
                                "path": "crates/krill-app/src/pull_request.rs",
                                "line": null,
                                "originalLine": 88,
                                "comments": {
                                    "nodes": [
                                        {
                                            "id": "PRRC_2",
                                            "body": "Avoid unwrap on untrusted JSON",
                                            "diffHunk": "@@ -85,4 +85,6 @@\n+let v = parse().unwrap();",
                                            "createdAt": "2026-10-17T03:00:00Z",
                                            "url": "https://github.com/acme/krill/pull/739#discussion_r2",
                                            "author": { "login": "coderabbitai[bot]", "__typename": "Bot" }
                                        },
                                        {
                                            "id": "PRRC_3",
                                            "body": "Please propagate io::Error instead.",
                                            "createdAt": "2026-10-17T03:05:00Z",
                                            "url": "https://github.com/acme/krill/pull/739#discussion_r3",
                                            "author": { "login": "owen", "__typename": "User" }
                                        }
                                    ]
                                }
                            }
                        ]
                    }
                }
            }
        }
    }"#;
    let comments = parse_review_threads_json(json).unwrap();
    assert_eq!(comments.len(), 1);
    let c = &comments[0];
    assert_eq!(c.id, "PRRC_2");
    assert_eq!(c.thread_id.as_deref(), Some("PRRT_open"));
    assert_eq!(c.author, "coderabbitai[bot]");
    assert!(c.is_bot);
    assert!(c.is_outdated);
    assert!(!c.is_resolved);
    assert_eq!(c.location(), "crates/krill-app/src/pull_request.rs:88");
    assert_eq!(c.body, "Avoid unwrap on untrusted JSON");
    assert_eq!(c.replies.len(), 1);
    assert_eq!(c.replies[0].author, "owen");
    assert!(!c.replies[0].is_bot);
    assert_eq!(c.replies[0].body, "Please propagate io::Error instead.");
}

#[gpui::test]
fn pr_checks_tab_displays_failed_logs_and_sends_combined_fix_prompt_to_active_agent(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::{Modifiers, MouseButton};

    struct InspectRunner;
    impl Runner for InspectRunner {
        fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
            Ok(Status::Open)
        }

        fn inspect_pr(&self, _: &Path, branch: &str) -> io::Result<PrInspection> {
            Ok(PrInspection {
                metadata: None,
                status: Status::Open,
                number: Some(739),
                title: format!("feat({branch}): PR CI checks"),
                url: "https://github.com/acme/krill/pull/739".into(),
                checks: vec![
                    CheckItem {
                        name: "cargo nextest".into(),
                        state: CheckState::Failed,
                        description: Some("1 test failed".into()),
                        details_url: Some(
                            "https://github.com/acme/krill/actions/runs/100/job/200".into(),
                        ),
                        run_id: Some("100".into()),
                        job_id: Some("200".into()),
                        log_tail: Some(
                            "thread 'check_parser' panicked at src/lib.rs:55:9\ntest result: FAILED"
                                .into(),
                        ),
                    },
                    CheckItem {
                        name: "cargo fmt".into(),
                        state: CheckState::Passed,
                        description: None,
                        details_url: None,
                        run_id: None,
                        job_id: None,
                        log_tail: None,
                    },
                ],
                review_comments: vec![ReviewComment {
                    id: "PRRC_10".into(),
                    thread_id: Some("PRRT_10".into()),
                    author: "owen".into(),
                    is_bot: false,
                    body: "Please handle empty log tail gracefully.".into(),
                    path: "crates/krill-app/src/pull_request/checks.rs".into(),
                    line: Some(142),
                    original_line: Some(140),
                    diff_hunk: "@@ -140,3 +140,5 @@\n+let tail = extract_error_log_tail(raw);"
                        .into(),
                    created_at: "2026-10-17T04:00:00Z".into(),
                    url: "https://github.com/acme/krill/pull/739#discussion_r10".into(),
                    is_resolved: false,
                    is_outdated: false,
                    replies: vec![ReviewReply {
                        author: "review-bot[bot]".into(),
                        is_bot: true,
                        body: "Verified reproduction in CI.".into(),
                        created_at: "2026-10-17T04:01:00Z".into(),
                    }],
                }],
            })
        }
    }

    cx.update(gpui_component::init);
    let service = Arc::new(Service::new(Arc::new(InspectRunner)));
    let sent_prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sent_clone = Arc::clone(&sent_prompts);

    let (panel, cx) = cx.add_window_view(|_, cx| {
        let mut p = crate::diff::Panel::unconnected(PathBuf::from("/repo"), cx);
        p.set_pr_service(service);
        p.set_checks_branch("craftowen/m10-owe-739");
        p
    });
    let opened_files: Arc<Mutex<Vec<crate::diff::OpenFileAt>>> = Arc::new(Mutex::new(Vec::new()));
    let opened_clone = Arc::clone(&opened_files);
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &crate::diff::SendComments, _| {
            sent_clone.lock().unwrap().push(event.0.clone());
        })
        .detach();
        cx.subscribe(&panel, move |_, event: &crate::diff::OpenFileAt, _| {
            opened_clone.lock().unwrap().push(event.clone());
        })
        .detach();
    });
    cx.run_until_parked();

    let click = |selector: &'static str, cx: &mut gpui::VisualTestContext| {
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
    };

    click("diff-tab-checks", cx);
    cx.run_until_parked();

    assert!(cx.debug_bounds("pr-checks-header").is_some());
    assert!(cx.debug_bounds("pr-check-row-0").is_some());
    assert!(cx.debug_bounds("pr-check-log-tail-0").is_some());
    assert!(cx.debug_bounds("pr-comment-row-0").is_some());
    // OWE-850: only failed checks expose the AI fix selection checkbox.
    assert!(cx.debug_bounds("pr-check-select-0").is_some());
    assert!(cx.debug_bounds("pr-check-select-1").is_none());

    // OWE-850: clicking PR title/number or CI check opens external URL; clicking comment location opens file at line.
    click("pr-checks-title-link", cx);
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url(),
        Some("https://github.com/acme/krill/pull/739".into())
    );

    click("pr-check-link-0", cx);
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url(),
        Some("https://github.com/acme/krill/actions/runs/100/job/200".into())
    );

    click("pr-comment-location-0", cx);
    cx.run_until_parked();
    assert_eq!(
        opened_files.lock().unwrap().as_slice(),
        &[crate::diff::OpenFileAt {
            path: PathBuf::from("/repo/crates/krill-app/src/pull_request/checks.rs"),
            line: 142,
            column: 1,
        }]
    );

    click("pr-check-select-0", cx);
    click("pr-comment-select-0", cx);
    cx.run_until_parked();

    click("pr-fix-with-ai", cx);
    cx.run_until_parked();

    let prompts = sent_prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];
    assert!(prompt.contains("PR #739"));
    assert!(prompt.contains("cargo nextest"));
    assert!(prompt.contains("thread 'check_parser' panicked at src/lib.rs:55:9"));
    assert!(prompt.contains("crates/krill-app/src/pull_request/checks.rs:142"));
    assert!(prompt.contains("@owen"));
    assert!(prompt.contains("Please handle empty log tail gracefully."));
    assert!(prompt.contains("@review-bot[bot] [bot]: Verified reproduction in CI."));

    panel.update(cx, |p, cx| {
        p.comments_sent(crate::diff::CommentSource::ChecksFix, true, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("pr-fix-status").is_some());
}

#[test]
fn inspect_shares_budget_and_enforces_max_concurrent_slots() {
    use std::sync::{Condvar, mpsc};

    struct BlockingInspect {
        calls: AtomicUsize,
        started: Mutex<mpsc::Sender<()>>,
        gate: (Mutex<bool>, Condvar),
    }
    impl Runner for BlockingInspect {
        fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Status::Open)
        }

        fn inspect_pr(&self, _: &Path, _: &str) -> io::Result<PrInspection> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.lock().unwrap().send(()).unwrap();
            let (lock, cvar) = &self.gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
            Ok(PrInspection::default())
        }
    }

    let (started_tx, started_rx) = mpsc::channel();
    let runner = Arc::new(BlockingInspect {
        calls: AtomicUsize::new(0),
        started: Mutex::new(started_tx),
        gate: (Mutex::new(false), Condvar::new()),
    });
    let service = Arc::new(Service::new(runner.clone()));
    let now = Instant::now();
    let repo = PathBuf::from("/repo");
    let max_conc = u32::try_from(MAX_CONCURRENT).unwrap();

    // 1. Hold MAX_CONCURRENT (4) concurrent `inspect_at` slots.
    let mut handles = Vec::new();
    for i in 0..max_conc {
        let svc = Arc::clone(&service);
        let r = repo.clone();
        let at = now + GAP * i;
        handles.push(std::thread::spawn(move || svc.inspect_at(&r, "main", at)));
        started_rx.recv().unwrap();
    }

    // 2. While 4 slots are in-flight, a 5th `inspect_at` or `create` is rejected with PACED.
    assert_eq!(
        service.inspect_at(&repo, "main", now + GAP * max_conc),
        Err(PACED.get())
    );
    assert_eq!(
        service.create(&repo, "main", now + GAP * (max_conc + 1)),
        Err(PACED.get())
    );

    // 3. Release in-flight workers; slots are returned on Drop, and manual inspect/create work without 10s GAP cooldown.
    {
        let (lock, cvar) = &runner.gate;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }
    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(runner.calls.load(Ordering::SeqCst), MAX_CONCURRENT);

    let last_at = now + GAP * (max_conc - 1);
    assert!(
        service
            .inspect_at(&repo, "main", last_at + Duration::from_secs(1))
            .is_ok()
    );
    assert!(
        service
            .create(&repo, "main", last_at + Duration::from_secs(2))
            .is_ok()
    );
}

#[test]
fn non_github_and_local_only_repositories_skip_pr_polling_without_failure() {
    assert!(!has_github_remote_config(""));
    assert!(!has_github_remote_config(
        "remote.origin.url /tmp/local-bare-repo\nremote.upstream.url https://gitlab.com/acme/repo.git"
    ));
    assert!(has_github_remote_config(
        "remote.origin.url git@github.com:acme/krill.git"
    ));
    assert!(has_github_remote_config(
        "remote.origin.url https://ghe.corp.example.com/org/repo.git\nremote.origin.gh-resolved base"
    ));

    let root = std::env::temp_dir().join(format!("mk-owe850-local-repo-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&root));
    std::fs::create_dir_all(&root).unwrap();
    assert!(
        Command::new("git")
            .current_dir(&root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    assert!(!has_github_remote(&root));
    assert_eq!(
        Gh.run(&root, "main", Action::Refresh).unwrap(),
        Status::NoPr
    );
    assert_eq!(
        checks::inspect_with_gh(&root, "main").unwrap().status,
        Status::NoPr
    );
    drop(std::fs::remove_dir_all(&root));
}

#[test]
fn parse_repo_owner_and_name_supports_ghe_hosts_and_graphql_uses_raw_fields_for_numeric_names() {
    assert_eq!(
        parse_repo_owner_and_name("https://github.com/acme/krill/pull/739"),
        Some(("acme".into(), "krill".into()))
    );
    assert_eq!(
        parse_repo_owner_and_name("https://ghe.corp.example.com/org/2026/pull/42"),
        Some(("org".into(), "2026".into()))
    );
    assert_eq!(
        parse_repo_owner_and_name(
            "http://git.internal:8443/ghe-prefix/team/true/pulls/7?tab=checks#discussion_r1"
        ),
        Some(("team".into(), "true".into()))
    );
    assert_eq!(
        parse_repo_owner_and_name("https://github.com/acme/krill/issues/739"),
        None
    );
    assert_eq!(parse_repo_owner_and_name("https://github.com/pull/1"), None);
    assert_eq!(parse_repo_owner_and_name("not-a-url"), None);

    for (owner, name) in [("12345", "2026"), ("true", "false"), ("@org", "@repo")] {
        let cmd = graphql_review_threads_command(Path::new("/tmp/repo"), owner, name, 739);
        let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        let owner_arg = format!("owner={owner}");
        let name_arg = format!("name={name}");
        assert!(
            args.windows(2).any(|w| w == ["-f", owner_arg.as_str()]),
            "owner must use -f (--raw-field), got: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["-f", name_arg.as_str()]),
            "name must use -f (--raw-field), got: {args:?}"
        );
        assert!(
            !args.windows(2).any(|w| w == ["-F", owner_arg.as_str()]),
            "owner must not use -F (--field)"
        );
        assert!(
            !args.windows(2).any(|w| w == ["-F", name_arg.as_str()]),
            "name must not use -F (--field)"
        );
        assert!(
            args.windows(2).any(|w| w == ["-F", "prNumber=739"]),
            "prNumber must remain -F for GraphQL Int!"
        );
    }
}

#[test]
fn owe924_merge_and_draft_toggle_commands_and_service_updates_cache() {
    struct ActionRunner;
    impl Runner for ActionRunner {
        fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
            Ok(Status::Open)
        }
        fn merge_pr(&self, _: &Path, _: &str, _: PrMergeMethod) -> io::Result<()> {
            Ok(())
        }
        fn set_pr_draft(&self, _: &Path, _: &str, _: bool) -> io::Result<()> {
            Ok(())
        }
    }

    let folder = Path::new("/tmp/repo");
    for (method, expected_flag) in [
        (PrMergeMethod::Squash, "--squash"),
        (PrMergeMethod::Rebase, "--rebase"),
        (PrMergeMethod::Merge, "--merge"),
    ] {
        assert_eq!(method.flag(), expected_flag);
        let cmd = merge_command(folder, "feat/owe-924", method);
        let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args, ["pr", "merge", "feat/owe-924", expected_flag]);
    }

    let ready_cmd = ready_command(folder, "feat/owe-924", false);
    let ready_args: Vec<_> = ready_cmd.get_args().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(ready_args, ["pr", "ready", "feat/owe-924"]);

    let draft_cmd = ready_command(folder, "feat/owe-924", true);
    let draft_args: Vec<_> = draft_cmd.get_args().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(draft_args, ["pr", "ready", "feat/owe-924", "--undo"]);

    let service = Service::new(Arc::new(ActionRunner));
    let repo = PathBuf::from("/repo");
    assert_eq!(service.set_draft(&repo, "feat/owe-924", true), Ok(()));
    assert_eq!(service.set_draft(&repo, "feat/owe-924", false), Ok(()));
    assert_eq!(
        service.merge(&repo, "feat/owe-924", PrMergeMethod::Squash),
        Ok(())
    );
}
