use super::*;
use std::fs;

#[cfg(windows)]
#[test]
fn codex_idle_resume_requires_a_live_owned_writer_lock() {
    let root = std::env::temp_dir().join(format!("krill writer 한글 {}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let home = root.join("account");
    let transcript = home.join("sessions/2026/10/28/rollout-known.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::create_dir(home.join("thread-writer-locks")).unwrap();
    fs::write(
        &transcript,
        r#"{"type":"session_meta","payload":{"id":"known"}}"#,
    )
    .unwrap();
    // Register before Codex creates its lock. A stale filename alone is insufficient.
    let watch = CodexResume::new(&transcript).unwrap();
    let me = std::process::id();
    assert!(watch.observe(me, None).unwrap().is_none());
    let guard = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(home.join("thread-writer-locks/known.lock"))
        .unwrap();
    assert!(watch.observe(me, None).unwrap().is_none());
    guard.lock().unwrap();
    let found = watch.observe(me, None).unwrap().unwrap();
    assert_eq!(found.id, "known");
    assert_eq!(found.path, transcript);
    assert!(watch.observe(u32::MAX, None).unwrap().is_none());
    let mut unrelated = krill_proto::os::background_command("cmd.exe")
        .args(["/d", "/c", "more"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(crate::sys::start_us(unrelated.id()).is_ok());
    assert!(watch.observe(unrelated.id(), None).unwrap().is_none());
    unrelated.kill().ok();
    unrelated.wait().ok();
    // Holding an unrelated thread's writer lock cannot confirm this conversation.
    let other = transcript.with_file_name("rollout-other.jsonl");
    fs::write(
        &other,
        r#"{"type":"session_meta","payload":{"id":"other"}}"#,
    )
    .unwrap();
    assert!(
        CodexResume::new(&other)
            .unwrap()
            .observe(me, None)
            .unwrap()
            .is_none()
    );
    drop(guard);
    assert!(watch.observe(me, None).unwrap().is_none());
    drop(watch);
    fs::remove_dir_all(root).unwrap();
}

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
    let wal_found = from_paths(
        ProviderKind::Antigravity,
        [
            root.join("conversations/wal-only.db-wal"),
            root.join("conversations/wal-only.db-shm"),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(wal_found.id, "wal-only");
    assert_eq!(wal_found.home, root);
    assert_eq!(wal_found.path, root.join("conversations/wal-only.db"));
}

#[test]
fn resume_retries_only_pending_metadata_and_disappearing_files() {
    let root = std::env::temp_dir().join(format!("kas-pending-header-{}", std::process::id()));
    let path = root.join("sessions/2026/10/24/rollout-known.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for pending in [
        "",
        r#"{"type":"session_meta","payload":{"id":"kn"#,
        // A complete record without identity followed by the metadata line still being written.
        "{\"type\":\"turn_context\"}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"kn",
    ] {
        fs::write(&path, pending).unwrap();
        assert_eq!(
            from_paths(ProviderKind::Codex, [path.clone()])
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(
            super::super::resume_identity(|| from_paths(ProviderKind::Codex, [path.clone()]))
                .unwrap()
                .is_none()
        );
    }
    fs::write(&path, r#"{"type":"session_meta","payload":{"id":"known"}}"#).unwrap();
    assert_eq!(
        super::super::resume_identity(|| from_paths(ProviderKind::Codex, [path.clone()]))
            .unwrap()
            .unwrap()
            .id,
        "known"
    );
    for invalid in [
        r#"{"type":"session_meta","payload":{"id":"../invalid"}}"#,
        r#"{"type":"session_meta","payload":{}}"#,
        "corrupt\n",
    ] {
        fs::write(&path, invalid).unwrap();
        assert_eq!(
            super::super::resume_identity(|| from_paths(ProviderKind::Codex, [path.clone()]))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    fs::remove_file(&path).unwrap();
    assert!(
        super::super::resume_identity(|| from_paths(ProviderKind::Codex, [path.clone()]))
            .unwrap()
            .is_none()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resumed_agy_ignores_unrelated_open_conversation_dbs() {
    let root = PathBuf::from("/tmp/krill-agy-resumed-identity/.gemini/antigravity-cli");
    let multi_db_paths = [
        root.join("conversations/other-a.db"),
        root.join("conversations/other-b.db-wal"),
        root.join("conversations/expected.db"),
    ];
    // Blind discovery must still reject ambiguous open conversations.
    assert_eq!(
        from_paths(ProviderKind::Antigravity, multi_db_paths.clone())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    // Resumed discovery knows the target conversation ID and picks it out cleanly.
    let found = from_paths_with(
        ProviderKind::Antigravity,
        multi_db_paths.clone(),
        Some("expected"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(found.id, "expected");
    assert_eq!(found.home, root);
    assert_eq!(found.path, root.join("conversations/expected.db"));
    // Even if another conversation holds a presence lock, resumed discovery matches expected.
    let with_other_lock = multi_db_paths
        .into_iter()
        .chain([root.join("presence/unrelated.lock")]);
    let found = from_paths_with(ProviderKind::Antigravity, with_other_lock, Some("expected"))
        .unwrap()
        .unwrap();
    assert_eq!(found.id, "expected");
    // If the expected conversation is not open yet, resumed discovery stays pending (None)
    // instead of aborting the handoff with InvalidData.
    assert!(
        from_paths_with(
            ProviderKind::Antigravity,
            [
                root.join("conversations/other-a.db"),
                root.join("conversations/other-b.db"),
            ],
            Some("expected"),
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn resumed_claude_ignores_unrelated_and_corrupt_transcripts() {
    let root = std::env::temp_dir().join(format!("kas-claude-resumed-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let home = root.join(".claude");
    let proj = home.join("projects/work");
    fs::create_dir_all(&proj).unwrap();
    let other = proj.join("other.jsonl");
    let corrupt = proj.join("corrupt.jsonl");
    let expected = proj.join("expected.jsonl");
    fs::write(&other, r#"{"sessionId":"other"}"#).unwrap();
    fs::write(&corrupt, "{\"type\":\"summary\"}\n").unwrap();
    fs::write(&expected, r#"{"sessionId":"expected"}"#).unwrap();

    // Unrelated valid transcripts still make blind discovery fail closed.
    assert_eq!(
        from_paths(ProviderKind::Claude, [other.clone(), expected.clone()])
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    // Resumed discovery with expected_id ignores unrelated/corrupt siblings and selects expected.
    let found = from_paths_with(
        ProviderKind::Claude,
        [other.clone(), corrupt.clone(), expected.clone()],
        Some("expected"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(found.id, "expected");
    assert_eq!(found.path, expected);
    assert_eq!(found.home, home);
    // Still rejects when expected.jsonl itself is invalid.
    assert_eq!(
        from_paths_with(
            ProviderKind::Claude,
            [other.clone(), corrupt.clone()],
            Some("corrupt"),
        )
        .err()
        .unwrap()
        .kind(),
        io::ErrorKind::InvalidData
    );
    // Prefer picks `expected` when open alongside unrelated siblings, and falls back to an
    // unambiguous mismatch when `expected` is absent so callers can reject stale IDs.
    let mismatch = from_paths_mode(
        ProviderKind::Claude,
        [other.clone()],
        ExpectedId::Prefer("missing"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(mismatch.id, "other");
    let preferred = from_paths_mode(
        ProviderKind::Claude,
        [other, corrupt, expected.clone()],
        ExpectedId::Prefer("expected"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(preferred.id, "expected");
    assert_eq!(
        from_paths_mode(
            ProviderKind::Antigravity,
            [PathBuf::from(
                "/tmp/krill-agy-prefer/.gemini/antigravity-cli/conversations/bound.db"
            )],
            ExpectedId::Prefer("wrong-conversation"),
        )
        .unwrap()
        .unwrap()
        .id,
        "bound"
    );
    fs::remove_dir_all(root).unwrap();
}
