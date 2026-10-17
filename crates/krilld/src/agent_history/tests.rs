use super::*;
use proptest::prelude::*;

const ID: &str = "11111111-2222-4333-8444-555555555555";

fn claude(id: &str, cwd: &str) -> String {
    format!("{{\"sessionId\":\"{id}\",\"cwd\":\"{cwd}\",\"isSidechain\":false}}\n")
}
fn codex(id: &str, cwd: &str) -> String {
    format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{cwd}\"}}}}\n")
}

#[test]
fn disk_scan_is_explicit_repeatable_read_only_and_excludes_subagents() {
    let root = std::env::temp_dir().join(format!("mk082-scan-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let c = root.join("claude/projects/project");
    let x = root.join("codex/sessions/2026/10/15");
    fs::create_dir_all(&c).unwrap();
    fs::create_dir_all(&x).unwrap();
    let first = c.join("main.jsonl");
    let content = format!("broken\n{}{{\"tail\":", claude(ID, "/tmp/한글 폴더"));
    fs::write(&first, &content).unwrap();
    fs::write(c.join("duplicate.jsonl"), &content).unwrap();
    fs::create_dir_all(c.join("subagents")).unwrap();
    fs::write(
        c.join("subagents/agent.jsonl"),
        claude("22222222-2222-4333-8444-555555555555", "/tmp/sub"),
    )
    .unwrap();
    fs::write(
        c.join("sidechain.jsonl"),
        claude(ID, "/tmp").replace("false", "true"),
    )
    .unwrap();
    fs::write(x.join("rollout-main.jsonl"), codex(ID, "/tmp/codex")).unwrap();
    fs::write(
        x.join("rollout-sub.jsonl"),
        codex(ID, "/tmp/sub").replace("\"cwd\"", "\"source\":{\"subagent\":\"review\"},\"cwd\""),
    )
    .unwrap();
    fs::write(x.join("rollout-invalid.jsonl"), b"{\"truncated\"").unwrap();
    std::os::unix::fs::symlink(&first, c.join("linked.jsonl")).unwrap();
    let got = scan_in(Some(&root.join("claude")), Some(&root.join("codex")));
    assert_eq!(got.entries.len(), 2);
    assert!(got.entries.iter().any(|e| e.cwd == "/tmp/한글 폴더"));
    assert!(got.skipped);
    assert!(!got.truncated);
    assert_eq!(fs::read_to_string(&first).unwrap(), content, "읽기 전용");
    fs::write(
        c.join("new.jsonl"),
        claude("33333333-2222-4333-8444-555555555555", "/new"),
    )
    .unwrap();
    assert_eq!(got.entries.len(), 2, "명시적 재요청 전에는 갱신 없음");
    assert_eq!(scan_in(Some(&root.join("claude")), None).entries.len(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scan_limits_and_invalid_ids_are_visible() {
    let root = std::env::temp_dir().join(format!("mk082-limits-{}", std::process::id()));
    let dir = root.join("projects/p");
    fs::create_dir_all(&dir).unwrap();
    for n in 0..=MAX_ENTRIES {
        fs::write(
            dir.join(format!("{n}.jsonl")),
            claude(&format!("{n:08x}-2222-4333-8444-555555555555"), "/tmp"),
        )
        .unwrap();
        File::open(dir.join(format!("{n}.jsonl")))
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + Duration::from_secs(u64::try_from(n).unwrap())),
            )
            .unwrap();
    }
    let data = scan_in(Some(&root), None);
    assert_eq!(data.entries.len(), MAX_ENTRIES);
    assert!(data.truncated);
    assert_eq!(
        data.entries.first().unwrap().modified,
        u64::try_from(MAX_ENTRIES).unwrap()
    );
    assert_eq!(data.entries.last().unwrap().modified, 1);
    for id in ["--help", "../../../etc/passwd", "", "not-a-uuid"] {
        assert!(parse(claude(id, "/tmp").as_bytes(), Agent::Claude, 0).is_none());
    }
    assert!(parse(claude(ID, "relative").as_bytes(), Agent::Claude, 0).is_none());
    let empty = scan_in(None, None);
    assert!(empty.entries.is_empty() && !empty.skipped && !empty.truncated);
    let mut scan = Scan {
        result: empty,
        entries: BTreeMap::new(),
        nodes: MAX_NODES,
        bytes: 0,
        start: Instant::now(),
    };
    scan.walk(&dir, Agent::Claude, 2);
    assert!(scan.result.truncated && scan.entries.is_empty());
    fs::remove_dir_all(root).unwrap();
}

proptest! {
    #[test]
    fn arbitrary_transcript_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        for agent in [Agent::Claude, Agent::Codex] {
            if let Some(entry) = parse(&bytes, agent, 0) { prop_assert!(valid(&entry)); }
        }
    }
}

#[test]
fn vault_search_indexes_claude_codex_agy_opencode_titles_summaries_and_bodies() {
    use krill_proto::agent_history::{SearchQuery, VaultAgent, VaultTarget};
    use vault::{VaultRoots, profile_dir_for_vault_target, search_in};

    let root = std::env::temp_dir().join(format!("mk181-vault-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();

    let claude_root = root.join("claude");
    let codex_root = root.join("codex");
    let agy_root = root.join("agy");
    let opencode_root = root.join("opencode");

    // 1. Claude transcript with customTitle, summary, user & assistant messages, tool_use
    let claude_proj = claude_root.join("projects/-tmp-repo-alpha");
    fs::create_dir_all(&claude_proj).unwrap();
    let claude_id = "11111111-2222-4333-8444-555555555555";
    fs::write(
        claude_proj.join(format!("{claude_id}.jsonl")),
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"11111111-2222-4333-8444-555555555555\",\"cwd\":\"/tmp/repo-alpha\",\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Fix authentication token refresh bug\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"11111111-2222-4333-8444-555555555555\",\"cwd\":\"/tmp/repo-alpha\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Updated OAuth2 refresh handler in auth_service.rs\"},{\"type\":\"tool_use\",\"name\":\"edit_file\",\"input\":{\"file\":\"auth_service.rs\"}}]}}\n",
            "{\"type\":\"custom-title\",\"customTitle\":\"OAuth2 Token Refresh Fix\",\"sessionId\":\"11111111-2222-4333-8444-555555555555\"}\n",
            "{\"type\":\"summary\",\"summary\":\"Resolved race condition during refresh token rotation\"}\n"
        ),
    )
    .unwrap();

    // 2. Codex transcript with session_index.jsonl title and response_item body
    let codex_sess = codex_root.join("sessions/2026/10/17");
    fs::create_dir_all(&codex_sess).unwrap();
    let codex_id = "22222222-2222-4333-8444-555555555555";
    fs::write(
        codex_root.join("session_index.jsonl"),
        format!("{{\"id\":\"{codex_id}\",\"thread_name\":\"Zstd Cold Scrollback Segment\"}}\n"),
    )
    .unwrap();
    fs::write(
        codex_sess.join("rollout-2026-10-17-codex.jsonl"),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"22222222-2222-4333-8444-555555555555\",\"cwd\":\"/tmp/repo-alpha/worktree-b\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Compress terminal scrollback with zstd dictionary\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Implemented zstd frame writer with 64KB segment rotation\"}]}}\n"
        ),
    )
    .unwrap();

    // 3. Antigravity (agy) transcript in brain/<id>/.system_generated/logs/transcript.jsonl + history.jsonl
    let agy_id = "33333333-2222-4333-8444-555555555555";
    let agy_logs = agy_root
        .join("brain")
        .join(agy_id)
        .join(".system_generated/logs");
    fs::create_dir_all(&agy_logs).unwrap();
    fs::write(
        agy_root.join("history.jsonl"),
        format!(
            "{{\"conversationId\":\"{agy_id}\",\"workspace\":\"/private/tmp/repo-beta\",\"display\":\"GPUI Vault Search Panel\"}}\n"
        ),
    )
    .unwrap();
    fs::write(
        agy_logs.join("transcript.jsonl"),
        concat!(
            "{\"step_index\":1,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>Build the GPUI AI Vault search popover with scope filters</USER_REQUEST>\"}\n",
            "{\"step_index\":2,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"content\":\"Added VaultScope pills and snippet highlighting to agent_history.rs\"}\n"
        ),
    )
    .unwrap();

    // 4. OpenCode session in storage/session/global/<id>.json + storage/message/<id>/msg_1.json
    let oc_id = "ses_987654321abcdef";
    let oc_sess_dir = opencode_root.join("storage/session/global");
    let oc_msg_dir = opencode_root.join("storage/message").join(oc_id);
    fs::create_dir_all(&oc_sess_dir).unwrap();
    fs::create_dir_all(&oc_msg_dir).unwrap();
    fs::write(
        oc_sess_dir.join(format!("{oc_id}.json")),
        format!(
            "{{\"id\":\"{oc_id}\",\"directory\":\"/tmp/repo-beta\",\"title\":\"SQLite Migration Script\",\"summary\":{{\"title\":\"Schema v4 migration\",\"body\":\"Added foreign key indexes to payments table\"}}}}"
        ),
    )
    .unwrap();
    fs::write(
        oc_msg_dir.join("msg_001.json"),
        "{\"id\":\"msg_001\",\"role\":\"user\",\"content\":\"Write idempotent migration for payments_idx\"}",
    )
    .unwrap();

    let roots = VaultRoots {
        claude: vec![claude_root.clone()],
        codex: vec![codex_root.clone()],
        agy: vec![agy_root.clone()],
        opencode: vec![opencode_root.clone()],
    };

    // Empty query returns all 4 conversations
    let all = search_in(&roots, &SearchQuery::default());
    assert_eq!(all.hits.len(), 4);

    // Search Claude by summary/body keyword
    let q_oauth = search_in(
        &roots,
        &SearchQuery {
            query: "auth_service.rs rotation".into(),
            ..SearchQuery::default()
        },
    );
    assert_eq!(q_oauth.hits.len(), 1);
    assert_eq!(q_oauth.hits[0].agent, VaultAgent::Claude);
    assert_eq!(q_oauth.hits[0].title, "OAuth2 Token Refresh Fix");
    assert!(q_oauth.hits[0].snippet.contains("auth_service.rs"));

    // Search Codex by title + body
    let q_zstd = search_in(
        &roots,
        &SearchQuery {
            query: "zstd 64KB".into(),
            ..SearchQuery::default()
        },
    );
    assert_eq!(q_zstd.hits.len(), 1);
    assert_eq!(q_zstd.hits[0].agent, VaultAgent::Codex);
    assert_eq!(q_zstd.hits[0].title, "Zstd Cold Scrollback Segment");

    // Search Agy by body + path filter (/tmp/repo-beta matches /private/tmp/repo-beta)
    let q_agy = search_in(
        &roots,
        &SearchQuery {
            query: "VaultScope".into(),
            paths: vec!["/tmp/repo-beta".into()],
            ..SearchQuery::default()
        },
    );
    assert_eq!(q_agy.hits.len(), 1);
    assert_eq!(q_agy.hits[0].agent, VaultAgent::Agy);
    assert_eq!(q_agy.hits[0].id, agy_id);

    // Search OpenCode with inline agent:opencode operator
    let q_oc = search_in(
        &roots,
        &SearchQuery {
            query: "agent:opencode payments_idx".into(),
            ..SearchQuery::default()
        },
    );
    assert_eq!(q_oc.hits.len(), 1);
    assert_eq!(q_oc.hits[0].agent, VaultAgent::OpenCode);
    assert_eq!(q_oc.hits[0].id, oc_id);
    assert!(q_oc.hits[0].snippet.contains("payments_idx"));

    // Verify profile_dir_for_vault_target finds isolated profile across all 4 agents
    let mut accounts = crate::usage::accounts::AccountsFile::default();
    for (id, provider, dir) in [
        ("c1", "claude", &claude_root),
        ("x1", "codex", &codex_root),
        ("a1", "agy", &agy_root),
        ("o1", "opencode", &opencode_root),
    ] {
        accounts
            .accounts
            .push(crate::usage::accounts::StoredAccount {
                id: id.into(),
                provider: provider.into(),
                label: id.into(),
                email: None,
                profile_dir: dir.display().to_string(),
            });
    }
    for (agent, id, expected) in [
        (VaultAgent::Claude, claude_id, &claude_root),
        (VaultAgent::Codex, codex_id, &codex_root),
        (VaultAgent::Agy, agy_id, &agy_root),
        (VaultAgent::OpenCode, oc_id, &opencode_root),
    ] {
        let target = VaultTarget {
            agent,
            id: id.into(),
            cwd: "/tmp".into(),
            modified: 0,
        };
        assert_eq!(
            profile_dir_for_vault_target(&accounts, &target, None),
            Some(expected.clone())
        );
    }

    fs::remove_dir_all(&root).ok();
}
