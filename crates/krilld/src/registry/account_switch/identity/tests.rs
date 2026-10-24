use super::*;
use std::fs;

#[test]
fn open_transcript_metadata_recovers_identity_and_source_without_project_guessing() {
    for provider in [ProviderKind::Codex, ProviderKind::Claude] {
        let root =
            std::env::temp_dir().join(format!("kas-process-{}-{provider:?}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("source");
        let path = home.join(if provider == ProviderKind::Codex {
            "sessions/2026/10/24/rollout-known.jsonl"
        } else {
            "projects/project/known.jsonl"
        });
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let record = if provider == ProviderKind::Codex {
            r#"{"type":"session_meta","payload":{"id":"known","cwd":"/same/project"}}"#
        } else {
            r#"{"sessionId":"known"}"#
        };
        fs::write(&path, record).unwrap();
        let unrelated = path.with_file_name("unrelated.jsonl");
        fs::write(&unrelated, record.replace("known", "unrelated")).unwrap();
        let found = from_paths(provider, [path.clone(), path.clone()])
            .unwrap()
            .unwrap();
        assert_eq!(found.id, "known");
        assert_eq!(found.home, home);
        assert_eq!(found.path, path);
        // An unrelated conversation in the same project is never searched.
        if provider == ProviderKind::Codex {
            let other = path.with_file_name("rollout-other.jsonl");
            fs::write(&other, record.replace("known", "other")).unwrap();
            assert!(from_paths(provider, [path.clone(), other]).is_err());
            fs::write(
                &path,
                r#"{"type":"session_meta","payload":{"id":"child","source":{"subagent":{}}}}"#,
            )
            .unwrap();
            assert!(from_paths(provider, [path.clone()]).unwrap().is_none());
        } else {
            assert!(from_paths(provider, [path.clone(), unrelated]).is_err());
            fs::write(&path, r#"{"sessionId":"child","isSidechain":true}"#).unwrap();
            assert!(from_paths(provider, [path.clone()]).unwrap().is_none());
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn agy_presence_beats_old_brain_handles_but_ambiguity_fails_closed() {
    let root = PathBuf::from("/tmp/krill-agy-identity/.gemini/antigravity-cli");
    let paths = [
        root.join("brain/old/scratch"),
        root.join("conversations/live.db"),
        root.join("presence/live.lock"),
    ];
    let found = from_paths(ProviderKind::Antigravity, paths.clone())
        .unwrap()
        .unwrap();
    assert_eq!(found.id, "live");
    assert_eq!(found.home, root);
    assert!(
        from_paths(
            ProviderKind::Antigravity,
            paths.into_iter().chain([root.join("presence/other.lock")])
        )
        .is_err()
    );
    assert!(
        from_paths(ProviderKind::Antigravity, [root.join("brain/old/scratch")])
            .unwrap()
            .is_none()
    );
    assert!(
        from_paths(
            ProviderKind::Antigravity,
            [
                root.join("conversations/one.db"),
                root.join("conversations/two.db")
            ]
        )
        .is_err()
    );
}
