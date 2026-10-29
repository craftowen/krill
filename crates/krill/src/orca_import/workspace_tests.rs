//! 현재 Orca의 가상 작업 공간·계보·분할 칸 재개 기록 호환 회귀 검사.
use super::*;
use proptest::prelude::*;

const INSTANCE: &str = "2de7222e-4069-4b91-bb07-3e05a9808a9f";

#[test]
fn workspace_keys_resolve_to_directories_without_eating_literal_separators() {
    for raw in [
        "/Users/test/작업::자료",
        "repo::/Users/test/작업::자료",
        "worktree:repo::/Users/test/작업::자료",
    ] {
        assert_eq!(split_worktree_key(raw).1, "/Users/test/작업::자료");
    }
    let raw = format!("worktree:repo::/Users/test/작업::자료::workspace:{INSTANCE}");
    assert_eq!(
        split_worktree_key(&raw),
        (Some("repo".into()), "/Users/test/작업::자료".into())
    );
    assert_eq!(
        split_worktree_key("repo::/tmp/notes::workspace:literal").1,
        "/tmp/notes::workspace:literal"
    );
}

proptest! {
    #[test]
    fn workspace_key_parser_preserves_unicode_path(path in "[a-zA-Z0-9가-힣 _-]{1,64}") {
        let cwd = format!("/Users/test/{path}::literal");
        let key = format!("worktree:repo::{cwd}::workspace:{INSTANCE}");
        prop_assert_eq!(split_worktree_key(&key), (Some("repo".into()), cwd));
    }

    #[test]
    fn arbitrary_workspace_keys_do_not_panic(raw in any::<String>()) {
        let _ = split_worktree_key(&raw);
        let _ = resolve_workspace_key(&BTreeMap::new(), &raw);
    }
}

#[test]
fn sessions_use_current_panes_and_resolve_folder_and_instance_keys() {
    let root = std::env::temp_dir().join(format!("orca-pane-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let cwd = root.to_string_lossy().into_owned();
    let key = format!("repo::{cwd}::workspace:{INSTANCE}");
    let remote_key = format!("remote::{cwd}");
    let domains = BTreeMap::from([
        (
            "folderWorkspaces".into(),
            serde_json::json!([{"id":"notes", "folderPath":cwd}]),
        ),
        (
            "worktreeMeta".into(),
            serde_json::json!({(remote_key.as_str()): {"hostId":"ssh-server"}}),
        ),
        (
            "workspaceSession".into(),
            serde_json::json!({
                "tabsByWorktree": {
                    (key.as_str()): [
                        {"id":"current", "worktreeId":key, "aiVaultTitle":{"agent":"codex", "sessionId":"stale"}},
                        {"id":"fallback", "worktreeId":key, "aiVaultTitle":{"agent":"claude", "sessionId":"fallback"}},
                        {"id":"invalid-pane", "worktreeId":key, "aiVaultTitle":{"agent":"claude", "sessionId":"valid-fallback"}}
                    ],
                    "folder:notes": [{"id":"folder", "worktreeId":"folder:notes",
                        "aiVaultTitle":{"agent":"claude", "sessionId":"folder"}}],
                    (remote_key.as_str()): [{"id":"remote", "worktreeId":remote_key,
                        "aiVaultTitle":{"agent":"claude", "sessionId":"remote"}}]
                },
                "sleepingAgentSessionsByPaneKey": {
                    "current:a": {"tabId":"current", "worktreeId":key, "agent":"codex", "providerSession":{"id":"current-a"}},
                    "current:b": {"tabId":"current", "worktreeId":key, "agent":"claude", "providerSession":{"id":"current-b"}},
                    "invalid-pane:a": {"tabId":"invalid-pane", "worktreeId":key, "agent":"unsupported", "providerSession":{"id":"bad"}},
                    "closed:a": {"tabId":"closed", "worktreeId":key, "agent":"codex", "providerSession":{"id":"closed"}}
                }
            }),
        ),
    ]);
    let found = orca_open_sessions(&domains);
    let ids: BTreeSet<_> = found.iter().map(|(target, _)| target.id.as_str()).collect();
    assert_eq!(
        ids,
        BTreeSet::from([
            "current-a",
            "current-b",
            "fallback",
            "folder",
            "valid-fallback"
        ])
    );
    assert!(found.iter().all(|(target, _)| target.cwd == cwd));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manual_reimport_repairs_old_paths_and_current_lineage_without_duplicates() {
    let root = std::env::temp_dir().join(format!("orca-workspaces-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(root.join("repo/child")).unwrap();
    fs::create_dir_all(root.join("repo/legacy")).unwrap();
    let repo = fs::canonicalize(root.join("repo"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let child = Path::new(&repo)
        .join("child")
        .to_string_lossy()
        .into_owned();
    let legacy = Path::new(&repo)
        .join("legacy")
        .to_string_lossy()
        .into_owned();
    let removed = Path::new(&repo)
        .join("removed-child")
        .to_string_lossy()
        .into_owned();
    let bad = format!("{repo}::workspace:{INSTANCE}");
    let opts = ImportOptions {
        home: root.join("home"),
        state_dir: root.join("state"),
        orca_dir: root.join("orca"),
        domains: vec![ImportDomain::Projects],
        dry_run: false,
    };
    fs::create_dir_all(&opts.orca_dir).unwrap();
    let source = serde_json::json!({
        "repos": [{"id":"repo", "path":repo, "displayName":"Project", "kind":"git"}],
        "worktreeMeta": {
            (format!("repo::{repo}")): {"displayName":"Main"},
            (format!("repo::{bad}")): {"displayName":"Virtual", "isArchived":true},
            (format!("repo::{child}")): {"displayName":"Child", "comment":"continue here"},
            (format!("repo::{removed}")): {"displayName":"Removed"}
        },
        "worktreeLineageById": {
            (format!("repo::{child}")): {"worktreeId":format!("repo::{child}"), "parentWorktreeId":format!("repo::{repo}")},
            (format!("repo::{legacy}")): {"worktreePath":legacy, "parentWorktreePath":format!("repo::{repo}")}
        },
        "workspaceLineageByChildKey": {
            (format!("worktree:repo::{child}")): {
                "childWorkspaceKey":format!("worktree:repo::{child}"),
                "parentWorkspaceKey":format!("worktree:repo::{repo}::workspace:{INSTANCE}")
            }
        }
    }).to_string();
    fs::write(opts.orca_dir.join("orca-data.json"), &source).unwrap();
    let original = ProjectsStore {
        projects: vec![
            StoredProject {
                id: "repo".into(),
                path: repo.clone(),
                display_name: "Project".into(),
                kind: "git".into(),
                ..StoredProject::default()
            },
            StoredProject {
                id: "legacy".into(),
                path: bad.clone(),
                display_name: basename_str(&bad),
                kind: "folder".into(),
                ..StoredProject::default()
            },
        ],
        worktrees: BTreeMap::from([
            (bad.clone(), StoredWorktreeMeta::default()),
            (
                removed.clone(),
                StoredWorktreeMeta {
                    repo_id: Some("repo".into()),
                    ..StoredWorktreeMeta::default()
                },
            ),
        ]),
        ..ProjectsStore::default()
    };
    original.save(&opts.state_dir, None).unwrap();
    let before = fs::read(opts.state_dir.join(PROJECTS_FILE)).unwrap();
    let preview = run_import(&ImportOptions {
        dry_run: true,
        ..opts.clone()
    })
    .unwrap();
    assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);
    assert_eq!(
        fs::read(opts.state_dir.join(PROJECTS_FILE)).unwrap(),
        before
    );
    for _ in 0..2 {
        let report = run_import(&opts).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let saved = ProjectsStore::load(&opts.state_dir).unwrap();
        assert_eq!(saved.projects.len(), 1);
        assert_eq!(saved.projects[0].path, repo);
        assert_eq!(saved.worktrees.len(), 2);
        assert_eq!(saved.worktrees[&repo].display_name.as_deref(), Some("Main"));
        assert!(!saved.worktrees[&repo].archived);
        assert_eq!(saved.lineage.len(), 2);
        assert_eq!(saved.lineage[&child].repo_id.as_deref(), Some("repo"));
        assert_eq!(saved.lineage[&legacy].repo_id.as_deref(), Some("repo"));
        assert_eq!(
            saved.lineage[&legacy].parent_worktree_path.as_deref(),
            Some(repo.as_str())
        );
        assert_eq!(
            saved.lineage[&child].parent_worktree_path.as_deref(),
            Some(repo.as_str())
        );
        let checkpoints = WorktreeStore::load(&opts.state_dir).unwrap();
        assert_eq!(
            checkpoints.parent_of(&child).as_deref(),
            Some(repo.as_str())
        );
        assert_eq!(
            checkpoints.checkpoint(&child).comment.as_deref(),
            Some("continue here")
        );
        assert_eq!(
            fs::read_to_string(opts.orca_dir.join("orca-data.json")).unwrap(),
            source
        );
    }
    fs::remove_dir_all(root).unwrap();
}
