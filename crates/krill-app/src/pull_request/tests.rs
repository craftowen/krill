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
    assert_eq!(
        service.create(&path, &branches[&path], now),
        Err(PACED.get())
    );
    branches.insert(path.clone(), "switched".into());
    assert!(!service.refresh(&branches, now).contains_key(&path));
    for i in 1..20 {
        service.refresh(&branches, now + GAP * i);
    }
    assert_eq!(fake.0.load(Ordering::SeqCst), 20);
    assert_eq!(
        service.create(&path, "switched", now + Duration::from_secs(299)),
        Err(PACED.get())
    );
    assert!(service.create(&path, "switched", now + WINDOW).is_ok());
    assert_eq!(fake.0.load(Ordering::SeqCst), 21);
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
        (io::ErrorKind::Other, Status::Failed),
    ] {
        let status = status_error(&kind.into());
        assert_eq!(status, expected);
        assert!(status.badge().is_none());
        assert!(status.guidance().is_some());
    }
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
    assert!(execute(&mut failed, Duration::from_secs(1)).is_err());
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
    struct Unavailable(io::ErrorKind);
    impl Runner for Unavailable {
        fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
            Err(self.0.into())
        }
    }
    for (kind, status, guidance) in [
        (io::ErrorKind::NotFound, Status::Missing, MISSING.get()),
        (io::ErrorKind::Other, Status::Failed, FAILED.get()),
    ] {
        let service = Service::new(Arc::new(Unavailable(kind)));
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
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &crate::diff::SendComments, _| {
            sent_clone.lock().unwrap().push(event.0.clone());
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

    panel.update(cx, |p, cx| p.comments_sent(true, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("pr-fix-status").is_some());
}
