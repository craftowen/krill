use super::*;
use crate::pull_request::{Action, PrMetadata, Runner, Service, Status};
use gpui::{Entity, Modifiers, MouseButton, VisualTestContext};
use std::io;
use std::path::Path;
use std::sync::Mutex;

struct MutationGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
    executor: gpui::BackgroundExecutor,
}

struct Fake {
    current: Mutex<PrInspection>,
    inspections: Mutex<Vec<PathBuf>>,
    branch: Mutex<String>,
    merges: Mutex<Vec<(PathBuf, String, PrMergeMethod)>>,
    fail_merge: bool,
    mutation_gate: Mutex<Option<MutationGate>>,
    fail_inspect: Mutex<bool>,
    switch_during_inspection: Mutex<bool>,
}

fn inspection() -> PrInspection {
    PrInspection {
        status: Status::Open,
        number: Some(42),
        title: "Confirm target".into(),
        url: "https://github.com/acme/krill/pull/42".into(),
        metadata: Some(PrMetadata {
            base_ref_name: "dev".into(),
            head_ref_name: "feature/pr".into(),
            base_ref_oid: "b".repeat(40),
            head_ref_oid: "a".repeat(40),
        }),
        ..PrInspection::default()
    }
}

impl Fake {
    fn new(fail_merge: bool) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new(inspection()),
            inspections: Mutex::new(Vec::new()),
            branch: Mutex::new("feature/pr".into()),
            merges: Mutex::new(Vec::new()),
            fail_merge,
            mutation_gate: Mutex::new(None),
            fail_inspect: Mutex::new(false),
            switch_during_inspection: Mutex::new(false),
        })
    }
}

impl Runner for Fake {
    fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
        Ok(Status::Open)
    }
    fn inspect_pr(&self, folder: &Path, _: &str) -> io::Result<PrInspection> {
        self.inspections.lock().unwrap().push(folder.to_owned());
        if *self.fail_inspect.lock().unwrap() {
            return Err(io::Error::other("fake: inspection unavailable"));
        }
        if *self.switch_during_inspection.lock().unwrap() {
            *self.branch.lock().unwrap() = "other".into();
        }
        Ok(self.current.lock().unwrap().clone())
    }
    fn merge_context_branch(&self, _: &Path) -> io::Result<Option<String>> {
        Ok(Some(self.branch.lock().unwrap().clone()))
    }
    fn merge_pr(&self, folder: &Path, target: &str, method: PrMergeMethod) -> io::Result<()> {
        self.merges
            .lock()
            .unwrap()
            .push((folder.to_owned(), target.to_owned(), method));
        let gate = self.mutation_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered.send(()).unwrap();
            // Runner is synchronous. Drain UI work while its mutation is in flight.
            let wait = async {
                gate.release.await.unwrap();
            };
            let mut wait = std::pin::pin!(wait);
            gate.executor
                .scheduler_executor()
                .scheduler()
                .block(None, wait.as_mut(), None);
        }
        if self.fail_merge {
            Err(io::Error::other("fake: permission denied"))
        } else {
            Ok(())
        }
    }
}

fn setup<'a>(
    cx: &'a mut gpui::TestAppContext,
    fake: &Arc<Fake>,
) -> (Entity<Panel>, &'a mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(|cx| crate::i18n::set_language(crate::i18n::Language::Ko, cx));
    let service = Arc::new(Service::new(fake.clone()));
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut panel = Panel::unconnected(PathBuf::from("/fake/worktree"), cx);
        panel.set_pr_service(service);
        panel.set_checks_branch("feature/pr");
        panel.checks.inspection = Some(inspection());
        panel.tab = super::super::PanelTab::Checks;
        panel
    });
    cx.run_until_parked();
    (view, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn pr_merge_confirmation_shows_target_cancel_close_escape_never_mutate(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::new(false);
    let (_, cx) = setup(cx, &fake);
    for cancel in ["pr-merge-cancel", "pr-merge-close", "escape"] {
        click(cx, "pr-merge-squash");
        assert!(cx.debug_bounds("pr-merge-confirmation").is_some());
        for selector in [
            "pr-merge-number:PR #42 · Confirm target",
            "pr-merge-repository:저장소: https://github.com/acme/krill/pull/42",
            "pr-merge-refs:base: dev ← head: feature/pr",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
        assert!(fake.merges.lock().unwrap().is_empty());
        if cancel == "escape" {
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
        } else {
            click(cx, cancel);
        }
        assert!(cx.debug_bounds("pr-merge-confirmation").is_none());
        assert!(fake.merges.lock().unwrap().is_empty());
    }
    cx.update(|_, cx| crate::i18n::set_language(crate::i18n::Language::En, cx));
    click(cx, "pr-merge-rebase");
    assert!(
        cx.debug_bounds("pr-merge-method:Merge method: Rebase & Merge")
            .is_some()
    );
    click(cx, "pr-merge-cancel");
    assert!(fake.merges.lock().unwrap().is_empty());
}

#[gpui::test]
fn pr_merge_confirmation_consumed_once_and_uses_pinned_url_for_all_methods(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::new(false);
    let (view, cx) = setup(cx, &fake);
    for (selector, method) in [
        ("pr-merge-squash", PrMergeMethod::Squash),
        ("pr-merge-rebase", PrMergeMethod::Rebase),
        ("pr-merge-commit", PrMergeMethod::Merge),
    ] {
        click(cx, selector);
        cx.update(|_, cx| {
            view.update(cx, |panel, cx| {
                panel.confirm_merge(cx);
                panel.confirm_merge(cx);
            });
        });
        cx.run_until_parked();
        let calls = fake.merges.lock().unwrap();
        assert_eq!(
            calls.last(),
            Some(&(PathBuf::from("/fake/worktree"), inspection().url, method))
        );
    }
    assert_eq!(fake.merges.lock().unwrap().len(), 3);
}

#[gpui::test]
fn pr_merge_confirmation_rejects_local_context_and_remote_snapshot_changes(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::new(false);
    let (view, cx) = setup(cx, &fake);
    // UI 상의 대상 변경: 확인 이후 워크트리, 선택 브랜치, 스냅샷, 세대 중 하나라도 바뀌면 거부한다.
    for field in ["repo", "branch", "snapshot", "generation"] {
        cx.update(|window, cx| {
            view.update(cx, |panel, cx| {
                panel.repo = PathBuf::from("/fake/worktree");
                panel.checks.branch = "feature/pr".into();
                panel.checks.inspection = Some(inspection());
                panel.merge_pr(PrMergeMethod::Squash, window, cx);
                match field {
                    "repo" => panel.repo = PathBuf::from("/fake/other"),
                    "branch" => panel.checks.branch = "other".into(),
                    "snapshot" => panel.checks.inspection.as_mut().unwrap().number = Some(43),
                    _ => panel.checks.generation += 1,
                }
                panel.confirm_merge(cx);
            });
        });
        cx.run_until_parked();
        assert!(fake.merges.lock().unwrap().is_empty(), "{field}");
    }
    // UI가 아직 갱신되지 않았어도 서비스의 재검사로 거부한다.
    for field in [
        "during_inspect",
        "branch",
        "number",
        "url",
        "base",
        "head",
        "base_oid",
        "head_oid",
        "status",
    ] {
        *fake.switch_during_inspection.lock().unwrap() = false;
        *fake.current.lock().unwrap() = inspection();
        *fake.branch.lock().unwrap() = "feature/pr".into();
        cx.update(|window, cx| {
            view.update(cx, |panel, cx| {
                panel.checks.inspection = Some(inspection());
                panel.merge_pr(PrMergeMethod::Squash, window, cx);
            });
        });
        match field {
            "during_inspect" => *fake.switch_during_inspection.lock().unwrap() = true,
            "branch" => *fake.branch.lock().unwrap() = "other".into(),
            "number" => fake.current.lock().unwrap().number = Some(43),
            "url" => {
                fake.current.lock().unwrap().url = "https://github.com/other/repo/pull/42".into();
            }
            "base" => {
                fake.current
                    .lock()
                    .unwrap()
                    .metadata
                    .as_mut()
                    .unwrap()
                    .base_ref_name = "main".into();
            }
            "head" => {
                fake.current
                    .lock()
                    .unwrap()
                    .metadata
                    .as_mut()
                    .unwrap()
                    .head_ref_name = "other".into();
            }
            "base_oid" => {
                fake.current
                    .lock()
                    .unwrap()
                    .metadata
                    .as_mut()
                    .unwrap()
                    .base_ref_oid = "c".repeat(40);
            }
            "head_oid" => {
                fake.current
                    .lock()
                    .unwrap()
                    .metadata
                    .as_mut()
                    .unwrap()
                    .head_ref_oid = "c".repeat(40);
            }
            _ => fake.current.lock().unwrap().status = Status::Closed,
        }
        cx.update(|_, cx| view.update(cx, Panel::confirm_merge));
        cx.run_until_parked();
        assert!(fake.merges.lock().unwrap().is_empty(), "{field}");
        assert!(cx.debug_bounds("pr-merge-error").is_some());
    }
}

#[gpui::test]
fn pr_merge_confirmation_failure_keeps_reason_and_requires_new_confirmation(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::new(true);
    let (view, cx) = setup(cx, &fake);
    click(cx, "pr-merge-squash");
    click(cx, "pr-merge-confirm");
    cx.update(|_, cx| {
        view.update(cx, |panel, cx| {
            assert!(
                panel
                    .checks
                    .action_error
                    .as_ref()
                    .unwrap()
                    .contains("fake: permission denied")
            );
            assert!(!panel.checks.loading);
            panel.confirm_merge(cx);
        });
    });
    assert_eq!(fake.merges.lock().unwrap().len(), 1);
    assert!(cx.debug_bounds("pr-merge-error").is_some());
    click(cx, "pr-merge-squash");
    assert!(cx.debug_bounds("pr-merge-confirmation").is_some());
}

#[test]
fn pr_merge_confirmation_metadata_and_guarded_command() {
    let bytes = br#"[{"number":42,"title":"Confirm target","url":"https://github.com/acme/krill/pull/42","state":"OPEN","baseRefName":"dev","headRefName":"feature/pr","baseRefOid":"bbbb","headRefOid":"aaaa"}]"#;
    let pr = crate::pull_request::parse_checks_from_pr_json(bytes).unwrap();
    assert!(pr.mergeable_snapshot());
    let cmd = crate::pull_request::confirmed_merge_command(
        Path::new("/fake"),
        &pr,
        PrMergeMethod::Rebase,
    )
    .unwrap();
    let args: Vec<_> = cmd.get_args().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(
        args,
        [
            "pr",
            "merge",
            "https://github.com/acme/krill/pull/42",
            "--rebase",
            "--match-head-commit",
            "aaaa"
        ]
    );
    let mut incomplete = pr;
    incomplete.metadata = None;
    assert!(
        crate::pull_request::confirmed_merge_command(
            Path::new("/fake"),
            &incomplete,
            PrMergeMethod::Squash
        )
        .is_err()
    );
}

#[gpui::test]
fn pr_merge_confirmation_missing_metadata_and_inspect_failure_never_mutate(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::new(false);
    let (view, cx) = setup(cx, &fake);
    cx.update(|window, cx| {
        view.update(cx, |panel, cx| {
            panel.checks.inspection.as_mut().unwrap().metadata = None;
            panel.merge_pr(PrMergeMethod::Squash, window, cx);
            assert!(panel.checks.pending_merge.is_none());
            assert!(panel.checks.action_error.is_some());
            panel.checks.inspection = Some(inspection());
            panel.merge_pr(PrMergeMethod::Squash, window, cx);
        });
    });
    *fake.fail_inspect.lock().unwrap() = true;
    cx.update(|_, cx| view.update(cx, Panel::confirm_merge));
    cx.run_until_parked();
    assert!(fake.merges.lock().unwrap().is_empty());
    cx.update(|_, cx| {
        view.update(cx, |panel, _| {
            assert!(
                panel
                    .checks
                    .action_error
                    .as_ref()
                    .unwrap()
                    .contains("inspection unavailable")
            );
        });
    });
}

#[gpui::test]
fn pr_merge_confirmation_in_flight_hide_and_refresh_keep_single_submission(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::new(false);
    let (view, cx) = setup(cx, &fake);
    click(cx, "pr-merge-squash");
    cx.update(|window, cx| {
        view.update(cx, |panel, cx| {
            panel.confirm_merge(cx);
            assert!(panel.checks.merging);
            panel.set_visible(false, cx);
            panel.refresh_checks(cx);
            panel.merge_pr(PrMergeMethod::Rebase, window, cx);
            panel.confirm_merge(cx);
            assert!(panel.checks.merging);
            assert!(panel.checks.pending_merge.is_none());
        });
    });
    cx.run_until_parked();
    assert_eq!(fake.merges.lock().unwrap().len(), 1);
    cx.update(|_, cx| {
        view.update(cx, |panel, _| {
            assert!(!panel.checks.merging);
            assert!(!panel.checks.loading);
        });
    });
}

#[gpui::test]
fn pr_merge_confirmation_enter_submits_once(cx: &mut gpui::TestAppContext) {
    let fake = Fake::new(false);
    let (_, cx) = setup(cx, &fake);
    click(cx, "pr-merge-squash");
    cx.simulate_keystrokes("enter enter");
    cx.run_until_parked();
    assert_eq!(fake.merges.lock().unwrap().len(), 1);
    assert!(cx.debug_bounds("pr-merge-confirmation").is_none());
}

#[gpui::test]
fn pr_merge_in_flight_repo_round_trip_reconfirmation_executes_once(cx: &mut gpui::TestAppContext) {
    let fake = Fake::new(false);
    let (view, cx) = setup(cx, &fake);
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    *fake.mutation_gate.lock().unwrap() = Some(MutationGate {
        entered,
        release: wait,
        executor: cx.executor(),
    });
    let mut switched = cx.clone();
    let switching_view = view.clone();
    let switch = cx.spawn(async move |_| {
        entry.await.unwrap();
        switched.update(|window, cx| {
            switching_view.update(cx, |panel, cx| {
                panel.set_visible(false, cx);
                panel.set_repo("/fake/other".into(), cx);
                panel.set_repo("/fake/worktree".into(), cx);
                assert!(panel.checks.merging);
                panel.checks.branch = "feature/pr".into();
                panel.checks.inspection = Some(inspection());
                panel.merge_pr(PrMergeMethod::Rebase, window, cx);
                panel.confirm_merge(cx);
                assert!(panel.checks.merging);
                assert!(panel.checks.merge_task.is_some());
                assert!(panel.checks.pending_merge.is_none());
            });
        });
        release.send(()).unwrap();
    });
    cx.update(|window, cx| {
        view.update(cx, |panel, cx| {
            panel.merge_pr(PrMergeMethod::Squash, window, cx);
            panel.confirm_merge(cx);
        });
    });
    cx.run_until_parked();
    drop(switch);
    assert_eq!(fake.merges.lock().unwrap().len(), 1);
    view.read_with(cx, |panel, _| {
        assert!(!panel.checks.merging);
        assert!(!panel.checks.loading);
        assert!(panel.checks.action_error.is_none());
        assert!(panel.checks.fix_status.is_none());
    });
}

#[gpui::test]
fn pr_merge_in_flight_repo_switch_completion_clears_busy_without_stale_result(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::new(true);
    let (view, cx) = setup(cx, &fake);
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    *fake.mutation_gate.lock().unwrap() = Some(MutationGate {
        entered,
        release: wait,
        executor: cx.executor(),
    });
    let mut switched = cx.clone();
    let switching_view = view.clone();
    let other = PrInspection {
        number: Some(99),
        title: "Current repository".into(),
        url: "https://github.com/acme/other/pull/99".into(),
        ..inspection()
    };
    let current = other.clone();
    let current_fake = fake.clone();
    let switch = cx.spawn(async move |_| {
        entry.await.unwrap();
        *current_fake.current.lock().unwrap() = other.clone();
        switched.update(|window, cx| {
            switching_view.update(cx, |panel, cx| {
                panel.set_visible(false, cx);
                panel.set_repo("/fake/other".into(), cx);
                assert!(panel.checks.merging);
                // Show Checks without unrelated Git watchers; completion must refresh this repo.
                panel.visible = true;
                panel.checks.branch = "feature/pr".into();
                panel.checks.inspection = Some(PrInspection {
                    title: "Before current repository refresh".into(),
                    ..other
                });
                panel.checks.action_error = Some("current repository error".into());
                panel.merge_pr(PrMergeMethod::Rebase, window, cx);
                panel.confirm_merge(cx);
                assert!(panel.checks.merging);
                assert!(panel.checks.merge_task.is_some());
                assert!(panel.checks.pending_merge.is_none());
            });
        });
        release.send(()).unwrap();
    });
    cx.update(|window, cx| {
        view.update(cx, |panel, cx| {
            panel.merge_pr(PrMergeMethod::Squash, window, cx);
            panel.confirm_merge(cx);
        });
    });
    cx.run_until_parked();
    drop(switch);
    assert_eq!(fake.merges.lock().unwrap().len(), 1);
    assert_eq!(
        fake.inspections.lock().unwrap().last(),
        Some(&PathBuf::from("/fake/other"))
    );
    view.read_with(cx, |panel, _| {
        assert_eq!(panel.repo, PathBuf::from("/fake/other"));
        assert!(!panel.checks.merging);
        assert!(!panel.checks.loading);
        assert_eq!(panel.checks.inspection.as_ref(), Some(&current));
        assert_eq!(
            panel.checks.action_error.as_deref(),
            Some("current repository error")
        );
        assert!(panel.checks.fix_status.is_none());
    });
}

/// APP-F01: 원격 호스트 작업공간에서는 PR · CI 탭이 로컬 git·gh를 부르지 않는다 — 조회·초안 전환·병합이
/// 모두 닫히고 미지원 상태만 보이며, 전환 전에 시작된 로컬 조회 결과는 버린다. 로컬로 돌아오면 다시 조회한다.
#[gpui::test]
fn remote_host_disables_pr_checks_and_drops_stale_local_inspection(cx: &mut gpui::TestAppContext) {
    let fake = Fake::new(false);
    let (view, cx) = setup(cx, &fake);
    assert!(cx.debug_bounds("pr-merge-squash").is_some());
    let inspections = || fake.inspections.lock().unwrap().len();
    let before = inspections();

    // 로컬 조회를 시작한 채 원격으로 전환: 결과가 도착해도 원격 상태를 덮어쓰지 않는다.
    let sock =
        std::env::temp_dir().join(format!("krill-checks-remote-{}.sock", std::process::id()));
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            p.refresh_checks(cx);
            p.connect(crate::hosts::HostTarget::remote("fixture", sock), cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let p = view.read(cx);
        assert!(p.remote_host());
        assert!(p.checks.inspection.is_none());
        assert!(!p.checks.loading);
        assert!(p.checks.pending_merge.is_none());
    });
    assert!(cx.debug_bounds("pr-checks-remote-unsupported").is_some());
    assert!(cx.debug_bounds("pr-merge-squash").is_none());
    assert!(cx.debug_bounds("pr-checks-loading").is_none());
    let after_switch = inspections();

    // 원격에서는 조회·초안 전환·병합이 아무것도 실행하지 않는다.
    cx.update(|window, cx| {
        view.update(cx, |p, cx| {
            p.set_tab(super::super::PanelTab::Changes, cx);
            p.set_tab(super::super::PanelTab::Checks, cx);
            p.refresh_checks(cx);
            p.toggle_pr_draft(cx);
            p.merge_pr(PrMergeMethod::Squash, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        inspections(),
        after_switch,
        "remote host must not run gh/git inspection"
    );
    assert!(fake.merges.lock().unwrap().is_empty());
    cx.update(|_, cx| {
        let p = view.read(cx);
        assert!(p.checks.inspection.is_none());
        assert!(!p.checks.loading);
        assert!(p.checks.pending_merge.is_none());
        assert!(p.checks.fix_status.is_none());
    });
    assert!(cx.debug_bounds("pr-merge-confirmation").is_none());
    assert!(cx.debug_bounds("pr-checks-remote-unsupported").is_some());

    // 로컬 호스트로 돌아오면 다시 조회해 PR 동작이 돌아온다.
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            let paths = krill_proto::daemon::Paths {
                socket: PathBuf::from("/fake/local.sock"),
                holder: PathBuf::new(),
                krilld: PathBuf::new(),
            };
            let hub: Arc<dyn crate::diff::Source> = Arc::new(crate::diff::TestHubSource(
                std::sync::Mutex::new(krilld::git::watch::Hub::default()),
            ));
            p.connect_with(crate::hosts::HostTarget::local(paths), Some(hub), cx);
        });
    });
    cx.run_until_parked();
    assert!(inspections() > after_switch);
    assert!(!cx.update(|_, cx| view.read(cx).remote_host()));
    assert!(cx.update(|_, cx| view.read(cx).checks.inspection.is_some()));
    assert!(cx.debug_bounds("pr-merge-squash").is_some());
    assert!(before <= after_switch);
}
