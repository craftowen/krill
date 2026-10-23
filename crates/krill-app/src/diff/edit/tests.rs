use super::*;
use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext};
use std::process::Command;
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    edits: Mutex<Vec<Edit>>,
    discards: Mutex<Vec<Vec<u8>>>,
    fail: Mutex<bool>,
    no_sync_feature: Mutex<bool>,
    no_remote: Mutex<bool>,
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

    fn discard(&self, _: &Path, _: &Path, file: &ChangedFile) -> io::Result<()> {
        self.discards.lock().unwrap().push(file.path.clone());
        if *self.fail.lock().unwrap() {
            Err(io::Error::other("discard failed"))
        } else {
            Ok(())
        }
    }

    fn supports_sync(&self) -> bool {
        !*self.no_sync_feature.lock().unwrap()
    }

    fn has_remote(&self, _: &Path) -> bool {
        !*self.no_remote.lock().unwrap()
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
fn stage_unstage_discard_multiline_commit_cmd_enter_failure_and_retry(cx: &mut TestAppContext) {
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

    // Worktree tab shows Discard + Stage buttons and hides Unstage button.
    assert!(cx.debug_bounds("git-discard-0").is_some());
    assert!(cx.debug_bounds("git-stage-0").is_some());
    assert!(cx.debug_bounds("git-unstage-0").is_none());

    // Discard opens confirmation prompt; Cancel leaves discards empty, Confirm runs discard.
    click("git-discard-0", cx);
    assert!(cx.pending_prompt().is_some());
    cx.simulate_prompt_answer(text::CANCEL.get());
    cx.run_until_parked();
    assert!(fake.discards.lock().unwrap().is_empty());

    click("git-discard-0", cx);
    assert!(cx.pending_prompt().is_some());
    cx.simulate_prompt_answer(text::DISCARD.get());
    cx.run_until_parked();
    assert_eq!(*fake.discards.lock().unwrap(), [b"file".to_vec()]);

    click("git-stage-0", cx);
    cx.run_until_parked();

    // Switching to Staged (`comparison-1`) shows Unstage button and hides Stage + Discard buttons.
    click("comparison-1", cx);
    cx.run_until_parked();
    assert!(cx.debug_bounds("git-unstage-0").is_some());
    assert!(cx.debug_bounds("git-stage-0").is_none());
    assert!(cx.debug_bounds("git-discard-0").is_none());

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

    // Empty commit message shows validation note without OS prompt.
    click("git-commit", cx);
    cx.run_until_parked();
    assert!(cx.pending_prompt().is_none());
    assert_eq!(fake.edits.lock().unwrap().len(), 2);

    // Multiline commit message submits directly without an OS warning prompt.
    let multiline_msg = "한글 커밋 제목\n\n- 본문 첫 줄\n- 본문 둘째 줄";
    panel.update_in(cx, |p, window, cx| {
        p.editing.input.as_ref().unwrap().update(cx, |input, cx| {
            input.set_value(multiline_msg, window, cx);
            input.focus(window, cx);
        });
    });
    cx.run_until_parked();
    *fake.fail.lock().unwrap() = true;
    click("git-commit", cx);
    assert!(
        cx.pending_prompt().is_none(),
        "normal commit must not pop up an OS warning confirmation dialog"
    );
    cx.run_until_parked();
    panel.read_with(cx, |p, cx| {
        assert_eq!(p.editing.note, "hook rejected");
        assert!(!p.editing.busy);
        assert_eq!(
            p.editing.input.as_ref().unwrap().read(cx).value(),
            multiline_msg
        );
    });

    // Pressing Cmd+Enter inside the commit textarea submits the multiline commit directly.
    *fake.fail.lock().unwrap() = false;
    panel.update_in(cx, |p, window, cx| {
        p.editing
            .input
            .as_ref()
            .unwrap()
            .update(cx, |input, cx| input.focus(window, cx));
    });
    cx.simulate_keystrokes("cmd-enter");
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
            message: multiline_msg.into()
        })
    );
}

#[gpui::test]
fn commit_textarea_plain_enter_inserts_newline(cx: &mut TestAppContext) {
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
    panel.update_in(cx, |p, window, cx| {
        p.editing.input.as_ref().unwrap().update(cx, |input, cx| {
            input.focus(window, cx);
        });
    });
    cx.run_until_parked();
    // Type "hi" then plain Enter: inserts exactly one newline natively and
    // must not submit (no staged-commit edit recorded).
    cx.simulate_keystrokes("h i");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    panel.read_with(cx, |p, cx| {
        assert_eq!(
            p.editing
                .input
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .to_string(),
            "hi\n"
        );
    });
    assert!(fake.edits.lock().unwrap().is_empty());
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
    assert!(cx.pending_prompt().is_none());
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
fn repository_switch_cancels_pending_discard_confirmation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut p = Panel::unconnected("/old".into(), cx);
        p.visible = false;
        p.editing.source = fake.clone();
        p.changes.files.push(ChangedFile {
            path: b"file".into(),
            old_path: None,
            status: *b".M",
            staged: None,
            unstaged: None,
        });
        drop(p.render_editor(window, cx));
        p
    });
    panel.update_in(cx, |p, window, cx| {
        p.confirm_discard_file(0, window, cx);
    });
    assert!(cx.pending_prompt().is_some());
    panel.update(cx, |p, cx| p.set_repo("/new".into(), cx));
    cx.simulate_prompt_answer(text::DISCARD.get());
    cx.run_until_parked();
    assert!(fake.discards.lock().unwrap().is_empty());
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
    assert!(cx.pending_prompt().is_none());
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
    assert!(cx.pending_prompt().is_none());
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
    panel.update(cx, |p, cx| {
        p.comments_sent(crate::diff::CommentSource::GitRecovery, true, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("git-recovery-banner").is_none());
    panel.read_with(cx, |p, _| {
        assert_eq!(p.editing.note, text::RECOVERY_SENT.get());
    });
}

#[test]
fn discard_worktree_file_restores_tracked_changes_and_trashes_untracked_files() {
    let dir = std::env::temp_dir().join(format!(
        "krill-discard-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let repo = dir.join("repo");
    let home = dir.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&repo)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&repo)
        .status()
        .unwrap();

    std::fs::write(repo.join("tracked.rs"), "pub fn v1() {}\n").unwrap();
    Command::new("git")
        .args(["add", "tracked.rs"])
        .current_dir(&repo)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(&repo)
        .status()
        .unwrap();

    // Modify tracked file and create untracked file.
    std::fs::write(repo.join("tracked.rs"), "pub fn dirty() {}\n").unwrap();
    std::fs::write(repo.join("untracked.txt"), "scratch\n").unwrap();

    discard_worktree_file(
        &repo,
        &home,
        &ChangedFile {
            path: b"tracked.rs".to_vec(),
            old_path: None,
            status: *b".M",
            staged: None,
            unstaged: None,
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("tracked.rs")).unwrap(),
        "pub fn v1() {}\n"
    );

    discard_worktree_file(
        &repo,
        &home,
        &ChangedFile {
            path: b"untracked.txt".to_vec(),
            old_path: None,
            status: *b"??",
            staged: None,
            unstaged: None,
        },
    )
    .unwrap();
    assert!(!repo.join("untracked.txt").exists());
    assert_eq!(
        std::fs::read_to_string(home.join(".Trash/untracked.txt")).unwrap(),
        "scratch\n"
    );

    std::fs::remove_dir_all(&dir).ok();
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

#[test]
fn hunk_and_line_stage_unstage_discard_on_real_repo() {
    let dir = std::env::temp_dir().join(format!(
        "krill-hunk-line-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&dir)
        .status()
        .unwrap();

    let initial =
        "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\n";
    std::fs::write(dir.join("file.txt"), initial).unwrap();
    Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(&dir)
        .status()
        .unwrap();

    // Modify line 1, line 2 (hunk 0) and line 12 (hunk 1).
    let mut lines: Vec<String> = (1..=12).map(|i| format!("line{i}")).collect();
    lines[0] = "line1_mod".into();
    lines[1] = "line2_mod".into();
    lines[11] = "line12_mod".into();
    std::fs::write(dir.join("file.txt"), format!("{}\n", lines.join("\n"))).unwrap();

    let file = ChangedFile {
        path: b"file.txt".to_vec(),
        old_path: None,
        status: *b".M",
        staged: None,
        unstaged: None,
    };
    let wt_patch = super::super::raw_patch(&dir, &file, Comparison::Worktree).unwrap();
    let (_, hunks) = extract_patch_header_and_hunks(&wt_patch);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunk_index_for_line(&wt_patch, 2, true), Some(0));
    assert_eq!(hunk_index_for_line(&wt_patch, 12, true), Some(1));

    // 1. Stage only line 2 from hunk 0.
    let stage_line2 = build_line_patch(&wt_patch, false, 2, 2, true).unwrap();
    apply_git_patch(&dir, &stage_line2, true, false).unwrap();

    let staged_patch = super::super::raw_patch(&dir, &file, Comparison::Index).unwrap();
    assert!(staged_patch.contains("+line2_mod"));
    assert!(!staged_patch.contains("+line1_mod"));
    assert!(!staged_patch.contains("+line12_mod"));

    // 2. Unstage line 2 from Index.
    let unstage_line2 = build_line_patch(&staged_patch, true, 2, 2, true).unwrap();
    apply_git_patch(&dir, &unstage_line2, true, true).unwrap();
    let staged_after_unstage = super::super::raw_patch(&dir, &file, Comparison::Index).unwrap();
    assert!(staged_after_unstage.trim().is_empty());

    // 3. Stage hunk 0, discard hunk 1.
    let hunk0 = build_hunk_patch(&wt_patch, 0).unwrap();
    apply_git_patch(&dir, &hunk0, true, false).unwrap();
    let hunk1 = build_hunk_patch(&wt_patch, 1).unwrap();
    apply_git_patch(&dir, &hunk1, false, true).unwrap();

    let disk_after = std::fs::read_to_string(dir.join("file.txt")).unwrap();
    assert!(disk_after.contains("line1_mod\nline2_mod\n"));
    assert!(disk_after.contains("line12\n"));
    assert!(!disk_after.contains("line12_mod"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn line_patch_handles_no_newline_at_end_of_file_on_real_repo() {
    let dir = std::env::temp_dir().join(format!(
        "krill-no-nl-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
    ] {
        Command::new("git")
            .args(args)
            .current_dir(&dir)
            .status()
            .unwrap();
    }
    // Initial file WITHOUT trailing newline
    std::fs::write(dir.join("no_nl.txt"), "first\nmiddle\nlast").unwrap();
    Command::new("git")
        .args(["add", "no_nl.txt"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(&dir)
        .status()
        .unwrap();

    // Modify line 1 and line 3 (still no trailing newline)
    std::fs::write(dir.join("no_nl.txt"), "first_mod\nmiddle\nlast_mod").unwrap();
    let file = ChangedFile {
        path: b"no_nl.txt".to_vec(),
        old_path: None,
        status: *b".M",
        staged: None,
        unstaged: None,
    };
    let wt_patch = super::super::raw_patch(&dir, &file, Comparison::Worktree).unwrap();
    assert!(wt_patch.contains("\\ No newline at end of file"));

    // Stage only line 1 (leaving the no-newline line 3 unstaged)
    let stage_line1 = build_line_patch(&wt_patch, false, 1, 1, true).unwrap();
    apply_git_patch(&dir, &stage_line1, true, false).unwrap();

    // Now stage line 3 (which has \ No newline at end of file on both old and new)
    let wt_patch2 = super::super::raw_patch(&dir, &file, Comparison::Worktree).unwrap();
    let stage_line3 = build_line_patch(&wt_patch2, false, 3, 3, true).unwrap();
    apply_git_patch(&dir, &stage_line3, true, false).unwrap();

    let remaining = super::super::raw_patch(&dir, &file, Comparison::Worktree).unwrap();
    assert!(remaining.trim().is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn repo_without_remote_disables_sync_buttons_and_blocks_sync_edits(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    *fake.no_remote.lock().unwrap() = true;
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut p = Panel::unconnected("/repo".into(), cx);
        p.visible = false;
        p.editing.source = fake.clone();
        p.changes = Changes {
            files: vec![ChangedFile {
                path: b"src/lib.rs".into(),
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
    panel.read_with(cx, |p, _| {
        assert!(p.editing.sync_supported());
        assert_eq!(p.editing.has_remote, Some(false));
    });

    panel.update_in(cx, |p, window, cx| {
        p.editing.input.as_ref().unwrap().update(cx, |input, cx| {
            input.set_value("feat: local only", window, cx);
        });
    });
    cx.run_until_parked();

    click("git-pull", cx);
    cx.run_until_parked();
    click("git-push", cx);
    cx.run_until_parked();
    click("git-sync", cx);
    cx.run_until_parked();
    click("git-commit-push", cx);
    cx.run_until_parked();
    panel.update(cx, |p, cx| {
        p.run_git_action(Edit::Pull, cx);
        p.run_git_action(Edit::Push, cx);
        p.run_git_action(Edit::Sync, cx);
    });
    cx.run_until_parked();
    assert!(fake.edits.lock().unwrap().is_empty());
    panel.read_with(cx, |p, _| {
        assert_eq!(p.editing.note, text::NO_REMOTE.get());
    });

    click("git-commit", cx);
    cx.run_until_parked();
    assert_eq!(
        *fake.edits.lock().unwrap(),
        [Edit::Commit {
            message: "feat: local only".into(),
        }]
    );
}

#[gpui::test]
fn unknown_remote_blocks_sync_until_background_probe_and_repo_reset(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::default());
    let panel = cx.new(|cx| {
        let mut panel = Panel::unconnected("/old".into(), cx);
        panel.visible = false;
        panel.editing.source = fake.clone();
        assert_eq!(panel.editing.has_remote, None);
        panel.run_git_action(Edit::Push, cx);
        panel.refresh_branch_sync(cx);
        panel
    });
    assert!(fake.edits.lock().unwrap().is_empty());
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        assert_eq!(panel.editing.has_remote, Some(true));
        let generation = panel.editing.sync_generation;
        panel.set_repo("/new".into(), cx);
        assert_ne!(panel.editing.sync_generation, generation);
        assert_eq!(panel.editing.has_remote, None);
        panel.run_git_action(Edit::Push, cx);
    });
    cx.run_until_parked();
    assert!(fake.edits.lock().unwrap().is_empty());
}
