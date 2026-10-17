use super::*;
use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext};
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    edits: Mutex<Vec<Edit>>,
    fail: Mutex<bool>,
    no_sync_feature: Mutex<bool>,
    sync: Mutex<(u64, u64)>,
    generated: Mutex<Option<Result<String, String>>>,
}
impl EditSource for Fake {
    fn edit(&self, _: &Path, edit: Edit) -> io::Result<()> {
        self.edits.lock().unwrap().push(edit);
        if *self.fail.lock().unwrap() {
            Err(io::Error::other("hook rejected"))
        } else {
            Ok(())
        }
    }

    fn supports_sync(&self) -> bool {
        !*self.no_sync_feature.lock().unwrap()
    }

    fn branch_sync(&self, _: &Path) -> (u64, u64) {
        *self.sync.lock().unwrap()
    }

    fn generate_commit_message(&self, _: &Path) -> io::Result<String> {
        match self.generated.lock().unwrap().clone() {
            Some(Ok(msg)) => Ok(msg),
            Some(Err(err)) => Err(io::Error::other(err)),
            None => Ok("feat: auto generated commit".into()),
        }
    }
}

#[gpui::test]
fn stage_unstage_commit_confirmation_failure_and_retry(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut p = Panel::unconnected("/repo".into(), cx);
        p.visible = false;
        p.editing.source = fake.clone();
        p.changes = Changes {
            files: vec![ChangedFile {
                path: b"file".into(),
                old_path: None,
                status: *b"MM",
                staged: None,
                unstaged: None,
            }],
            limited: false,
        };
        drop(p.render_editor(window, cx));
        p
    });
    cx.run_until_parked();
    click("git-stage-0", cx);
    cx.run_until_parked();
    click("git-unstage-0", cx);
    cx.run_until_parked();
    assert_eq!(
        *fake.edits.lock().unwrap(),
        [
            Edit::Stage {
                path: b"file".into(),
                old_path: None
            },
            Edit::Unstage {
                path: b"file".into(),
                old_path: None
            }
        ]
    );
    click("git-commit", cx);
    cx.run_until_parked();
    assert!(cx.pending_prompt().is_none());
    assert_eq!(fake.edits.lock().unwrap().len(), 2);
    panel.update_in(cx, |p, window, cx| {
        p.editing
            .input
            .as_ref()
            .unwrap()
            .update(cx, |input, cx| input.set_value("한글 커밋", window, cx));
    });
    cx.run_until_parked();
    click("git-commit", cx);
    assert_eq!(cx.pending_prompt().unwrap().0, text::CONFIRM.get());
    cx.simulate_prompt_answer(text::CANCEL.get());
    cx.run_until_parked();
    assert_eq!(fake.edits.lock().unwrap().len(), 2);
    *fake.fail.lock().unwrap() = true;
    panel.update_in(cx, Panel::confirm_commit);
    cx.simulate_prompt_answer(text::COMMIT.get());
    cx.run_until_parked();
    panel.read_with(cx, |p, cx| {
        assert_eq!(p.editing.note, "hook rejected");
        assert!(!p.editing.busy);
        assert_eq!(
            p.editing.input.as_ref().unwrap().read(cx).value(),
            "한글 커밋"
        );
    });
    *fake.fail.lock().unwrap() = false;
    panel.update_in(cx, Panel::confirm_commit);
    cx.simulate_prompt_answer(text::COMMIT.get());
    cx.run_until_parked();
    panel.read_with(cx, |p, cx| {
        assert!(
            p.editing
                .input
                .as_ref()
                .is_none_or(|input| input.read(cx).value().is_empty())
        );
    });
    assert_eq!(
        fake.edits.lock().unwrap().last(),
        Some(&Edit::Commit {
            message: "한글 커밋".into()
        })
    );
}

#[test]
fn unsupported_daemon_receives_no_mutation_bytes() {
    let mut wire = io::Cursor::new(Vec::new());
    let req = Request::GitEdit {
        repo: b"/repo".into(),
        edit: Edit::Commit {
            message: "m".into(),
        },
    };
    assert_eq!(
        exchange(FEAT_CONTROL, &mut wire, &req).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(wire.get_ref().is_empty());

    // A peer that negotiated FEAT_GIT_EDIT (1<<48) without FEAT_GIT_SYNC (1<<41)
    // must never receive Push, Pull, Sync, or CommitAndPush bytes on the wire.
    let legacy_edit_features = FEAT_CONTROL | FEAT_GIT_EDIT;
    for sync_edit in [
        Edit::Push,
        Edit::Pull,
        Edit::Sync,
        Edit::CommitAndPush {
            message: "feat: sync".into(),
        },
    ] {
        let mut sync_wire = io::Cursor::new(Vec::new());
        let sync_req = Request::GitEdit {
            repo: b"/repo".into(),
            edit: sync_edit,
        };
        assert_eq!(
            exchange(legacy_edit_features, &mut sync_wire, &sync_req)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(sync_wire.get_ref().is_empty());
    }
}

#[gpui::test]
fn peer_without_feat_git_sync_disables_sync_buttons_and_never_sends_sync_edits(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    *fake.no_sync_feature.lock().unwrap() = true;
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut p = Panel::unconnected("/repo".into(), cx);
        p.visible = false;
        p.editing.source = fake.clone();
        p.changes = Changes {
            files: vec![ChangedFile {
                path: b"src/lib.rs".into(),
                old_path: None,
                status: *b"MM",
                staged: None,
                unstaged: None,
            }],
            limited: false,
        };
        p.refresh_branch_sync(cx);
        drop(p.render_editor(window, cx));
        p
    });
    cx.run_until_parked();
    panel.read_with(cx, |p, _| {
        assert!(!p.editing.sync_supported());
    });

    panel.update_in(cx, |p, window, cx| {
        p.editing.input.as_ref().unwrap().update(cx, |input, cx| {
            input.set_value("feat: local commit only", window, cx);
        });
    });
    cx.run_until_parked();

    // Clicking disabled sync buttons or invoking sync actions directly must never send sync Edits.
    click("git-pull", cx);
    cx.run_until_parked();
    click("git-push", cx);
    cx.run_until_parked();
    click("git-sync", cx);
    cx.run_until_parked();
    click("git-commit-push", cx);
    cx.run_until_parked();
    assert!(cx.pending_prompt().is_none());
    panel.update(cx, |p, cx| {
        p.run_git_action(Edit::Pull, cx);
        p.run_git_action(Edit::Push, cx);
        p.run_git_action(Edit::Sync, cx);
    });
    cx.run_until_parked();
    assert!(fake.edits.lock().unwrap().is_empty());

    // Stage and Commit remain available with FEAT_GIT_EDIT alone.
    click("git-stage-0", cx);
    cx.run_until_parked();
    click("git-commit", cx);
    assert_eq!(cx.pending_prompt().unwrap().0, text::CONFIRM.get());
    cx.simulate_prompt_answer(text::COMMIT.get());
    cx.run_until_parked();
    assert_eq!(
        *fake.edits.lock().unwrap(),
        [
            Edit::Stage {
                path: b"src/lib.rs".into(),
                old_path: None,
            },
            Edit::Commit {
                message: "feat: local commit only".into(),
            },
        ]
    );
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds(selector).unwrap();
    cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
}

#[gpui::test]
fn repository_switch_cancels_pending_commit_confirmation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut p = Panel::unconnected("/old".into(), cx);
        p.visible = false;
        p.editing.source = fake.clone();
        p.changes.files.push(ChangedFile {
            path: b"file".into(),
            old_path: None,
            status: *b"M.",
            staged: None,
            unstaged: None,
        });
        drop(p.render_editor(window, cx));
        p
    });
    panel.update_in(cx, |p, window, cx| {
        p.editing
            .input
            .as_ref()
            .unwrap()
            .update(cx, |input, cx| input.set_value("message", window, cx));
        p.confirm_commit(window, cx);
    });
    assert!(cx.pending_prompt().is_some());
    panel.update(cx, |p, cx| p.set_repo("/new".into(), cx));
    cx.simulate_prompt_answer(text::COMMIT.get());
    cx.run_until_parked();
    assert!(fake.edits.lock().unwrap().is_empty());
    panel.read_with(cx, |p, _| assert!(!p.editing.busy));
}

#[gpui::test]
fn push_pull_sync_commit_and_push_and_sync_count_badges(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    *fake.sync.lock().unwrap() = (3, 2);
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut p = Panel::unconnected("/repo".into(), cx);
        p.visible = false;
        p.editing.source = fake.clone();
        p.changes = Changes {
            files: vec![ChangedFile {
                path: b"src/main.rs".into(),
                old_path: None,
                status: *b"M.",
                staged: None,
                unstaged: None,
            }],
            limited: false,
        };
        p.refresh_branch_sync(cx);
        drop(p.render_editor(window, cx));
        p
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("git-sync-counts").is_some());
    panel.read_with(cx, |p, _| {
        assert_eq!((p.editing.ahead, p.editing.behind), (3, 2));
    });

    click("git-pull", cx);
    cx.run_until_parked();
    click("git-push", cx);
    cx.run_until_parked();
    click("git-sync", cx);
    cx.run_until_parked();

    panel.update_in(cx, |p, window, cx| {
        p.editing.input.as_ref().unwrap().update(cx, |input, cx| {
            input.set_value("feat: sync all", window, cx);
        });
    });
    cx.run_until_parked();
    click("git-commit-push", cx);
    assert_eq!(
        cx.pending_prompt().unwrap().0,
        text::CONFIRM_COMMIT_AND_PUSH.get()
    );
    cx.simulate_prompt_answer(text::COMMIT_AND_PUSH.get());
    cx.run_until_parked();

    assert_eq!(
        *fake.edits.lock().unwrap(),
        [
            Edit::Pull,
            Edit::Push,
            Edit::Sync,
            Edit::CommitAndPush {
                message: "feat: sync all".into()
            }
        ]
    );
}

#[gpui::test]
fn ai_commit_message_generation_and_recovery_banner_prompt_injection(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    *fake.generated.lock().unwrap() = Some(Ok("feat(git): add recovery banner".into()));
    let sent_prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sent_clone = Arc::clone(&sent_prompts);

    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut p = Panel::unconnected("/repo/wt".into(), cx);
        p.visible = false;
        p.editing.source = fake.clone();
        p.changes = Changes {
            files: vec![ChangedFile {
                path: b"src/diff/edit.rs".into(),
                old_path: None,
                status: *b"M.",
                staged: None,
                unstaged: None,
            }],
            limited: false,
        };
        drop(p.render_editor(window, cx));
        p
    });
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &SendComments, _| {
            sent_clone.lock().unwrap().push(event.0.clone());
        })
        .detach();
    });
    cx.run_until_parked();

    // 1. Click AI sparkle button to auto-generate commit message.
    click("git-ai-commit", cx);
    cx.run_until_parked();
    panel.read_with(cx, |p, cx| {
        assert_eq!(
            p.editing.input.as_ref().unwrap().read(cx).value(),
            "feat(git): add recovery banner"
        );
    });

    // 2. Trigger a failed commit (e.g. pre-commit hook failure) and verify recovery banner appears.
    *fake.fail.lock().unwrap() = true;
    click("git-commit", cx);
    cx.simulate_prompt_answer(text::COMMIT.get());
    cx.run_until_parked();
    assert!(cx.debug_bounds("git-recovery-banner").is_some());
    assert!(cx.debug_bounds("git-recovery-fix").is_some());

    // 3. Click "Ask AI agent to fix" and verify recovery prompt is emitted via SendComments.
    click("git-recovery-fix", cx);
    cx.run_until_parked();
    let prompts = sent_prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("git commit"));
    assert!(prompts[0].contains("/repo/wt"));
    assert!(prompts[0].contains("hook rejected"));
    assert!(prompts[0].contains("src/diff/edit.rs"));
    drop(prompts);

    // 4. Simulate Shell confirming prompt delivery -> banner clears and status note updates.
    panel.update(cx, |p, cx| p.comments_sent(true, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("git-recovery-banner").is_none());
    panel.read_with(cx, |p, _| {
        assert_eq!(p.editing.note, text::RECOVERY_SENT.get());
    });
}

#[test]
fn parse_helpers_and_offline_repo_commit_message_synthesis() {
    assert_eq!(parse_branch_sync("4\t7\n"), (4, 7));
    assert_eq!(parse_branch_sync(""), (0, 0));
    assert_eq!(
        parse_commit_message("```text\nSUBJECT: fix: handle hook error\n```"),
        "fix: handle hook error"
    );
    assert_eq!(
        synthesize_commit_message("", "M  crates/krill-app/src/diff.rs\n"),
        "Update crates/krill-app/src/diff.rs"
    );
}
