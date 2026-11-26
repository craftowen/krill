use super::*;

fn home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("krill-skill-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&home).ok();
    fs::create_dir_all(&home).unwrap();
    home
}

#[test]
fn automatic_sync_updates_the_bundle_and_leaves_identical_files_untouched() {
    let home = home("automatic");
    let first = sync_builtin_skills_at(&home);
    assert!(first.failures.is_empty(), "{:?}", first.failures);
    assert_eq!(first.updated, first.installed.len());
    let mut stamps = Vec::new();
    for dir in skill_directories(&home, "all") {
        let entry = dir.join("krill-orchestration/SKILL.md");
        let body = fs::read_to_string(&entry).unwrap();
        assert!(body.starts_with("---\nname: krill-orchestration\n"));
        stamps.push((
            entry.clone(),
            fs::metadata(&entry).unwrap().modified().unwrap(),
        ));
        for (relative, bundled) in ORCHESTRATION_REFERENCES {
            let text = fs::read_to_string(entry.parent().unwrap().join(relative)).unwrap();
            assert_eq!(text, format!("{}\n{MARKER}\n", bundled.trim_end()));
        }
        let artifacts_dir = dir.join("artifacts");
        for (relative, bundled) in ARTIFACTS_REFERENCES {
            let text = fs::read_to_string(artifacts_dir.join(relative)).unwrap();
            assert_eq!(text, format!("{}\n{MARKER}\n", bundled.trim_end()));
        }
    }
    let second = sync_builtin_skills_at(&home);
    assert!(second.failures.is_empty());
    assert_eq!(second.updated, 0);
    assert_eq!(second.unchanged, first.installed.len());
    for (path, modified) in stamps {
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
    }
    let cli = home.join(".gemini/antigravity-cli/skills/krill-orchestration");
    fs::write(cli.join("SKILL.md"), format!("old version\n{MARKER}\n")).unwrap();
    fs::write(
        cli.join("references/worker-contract.md"),
        format!("old contract\n{MARKER}\n"),
    )
    .unwrap();
    let upgraded = sync_builtin_skills_at(&home);
    assert_eq!(upgraded.updated, 1);
    assert!(upgraded.failures.is_empty());
    assert!(
        fs::read_to_string(cli.join("SKILL.md"))
            .unwrap()
            .contains("# Krill orchestration")
    );
    assert!(
        fs::read_to_string(cli.join("references/worker-contract.md"))
            .unwrap()
            .contains("# Worker obligations")
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn foreign_reference_prevents_entrypoint_replacement_but_other_targets_still_sync() {
    let home = home("foreign-reference");
    let dir = home.join(".gemini/antigravity-cli/skills");
    let path = install_skill("krill-orchestration", &dir).unwrap();
    fs::write(&path, format!("old owned entrypoint\n{MARKER}\n")).unwrap();
    let reference = path.parent().unwrap().join("references/worker-contract.md");
    fs::write(&reference, "user authored contract").unwrap();
    let report = sync_builtin_skills_at(&home);
    assert!(!report.failures.is_empty());
    assert_eq!(
        fs::read_to_string(&reference).unwrap(),
        "user authored contract"
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        format!("old owned entrypoint\n{MARKER}\n")
    );
    assert!(
        home.join(".codex/skills/krill-orchestration/SKILL.md")
            .is_file()
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn concurrent_installers_publish_complete_matching_packages() {
    let home = home("concurrent");
    let threads: Vec<_> = (0..6)
        .map(|_| {
            let home = home.clone();
            std::thread::spawn(move || install_skill("orchestration", &home))
        })
        .collect();
    let mut completed = 0;
    for thread in threads {
        match thread.join().unwrap() {
            Ok(path) => {
                assert!(path.is_file());
                completed += 1;
            }
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
        }
    }
    assert!(completed > 0);
    install_skill("orchestration", &home).unwrap();
    for (path, body) in ORCHESTRATION_REFERENCES {
        assert_eq!(
            fs::read_to_string(home.join("krill-orchestration").join(path)).unwrap(),
            format!("{}\n{MARKER}\n", body.trim_end())
        );
    }
    assert!(
        fs::read_dir(home.join("krill-orchestration/references"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_busy_skill_lock_never_blocks_startup_and_retries_after_release() {
    use std::time::Duration;
    let home = home("busy-lock");
    let locked = home.join("locked");
    let other = home.join("other");
    fs::create_dir_all(&locked).unwrap();
    let guard = LockGuard::acquire(&locked.join(".krill-skills.lock")).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let dirs = vec![locked.clone(), other.clone()];
    let worker = std::thread::spawn(move || send.send(sync_builtin_skills(&dirs)).unwrap());
    // The lock stays held until `drop(guard)` below, so a sync that waited on it would
    // never return. The budget only bounds that hang; installing every skill into
    // `other` took over 2s on Windows CI runners.
    let report = receive.recv_timeout(Duration::from_secs(30)).unwrap();
    worker.join().unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.busy, std::slice::from_ref(&locked));
    assert_eq!(report.installed.len(), builtin_skills().len());
    assert!(!locked.join("krill-orchestration").exists());
    assert!(other.join("krill-orchestration/SKILL.md").is_file());
    drop(guard);
    let report = sync_builtin_skills(&[locked]);
    assert!(report.failures.is_empty() && report.busy.is_empty());
    assert_eq!(report.updated, builtin_skills().len());
    fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinks_at_roots_ancestors_references_and_locks_never_redirect_writes() {
    use std::os::unix::fs::symlink;
    let home = home("links");
    let outside = home.join("external");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("preserve"), "foreign").unwrap();
    symlink(&outside, home.join("base-link")).unwrap();
    assert!(install_skill("orchestration", &home.join("base-link")).is_err());
    assert!(install_skill("orchestration", &home.join("base-link/nested/skills")).is_err());
    let root = home.join("skills");
    let path = install_skill("orchestration", &root).unwrap();
    let refs = path.parent().unwrap().join("references");
    fs::remove_dir_all(&refs).unwrap();
    symlink(&outside, &refs).unwrap();
    assert!(install_skill("orchestration", &root).is_err());
    fs::remove_file(refs).unwrap();
    let lock = root.join(".krill-skills.lock");
    fs::remove_file(&lock).unwrap();
    symlink(outside.join("preserve"), &lock).unwrap();
    assert!(install_skill("orchestration", &root).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("preserve")).unwrap(),
        "foreign"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn pi_and_shared_agent_skill_roots_sync_builtin_bundle() {
    let home = home("pi-roots");
    let pi_dirs = skill_directories(&home, "pi");
    assert!(pi_dirs.contains(&home.join(".pi/agent/skills")));
    assert!(pi_dirs.contains(&home.join(".agents/skills")));
    let all_dirs = skill_directories(&home, "all");
    assert!(all_dirs.contains(&home.join(".pi/agent/skills")));
    assert!(all_dirs.contains(&home.join(".agents/skills")));
    let report = sync_builtin_skills(&pi_dirs);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(
        home.join(".pi/agent/skills/krill-orchestration/SKILL.md")
            .is_file()
    );
    assert!(
        home.join(".agents/skills/krill-orchestration/SKILL.md")
            .is_file()
    );
    let body = std::fs::read_to_string(home.join(".pi/agent/skills/krill-orchestration/SKILL.md"))
        .unwrap();
    assert!(body.contains("On pi, use the `bash` tool"));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn agent_desktop_alias_installs_computer_use_and_cleans_stale_references() {
    let home = home("agent-desktop-alias");
    assert_eq!(canonical_skill_name("agent-desktop"), "computer-use");
    assert_eq!(canonical_skill_name("krill-agent-desktop"), "computer-use");
    assert_eq!(canonical_skill_name("mk-computer-use"), "computer-use");
    assert!(skill_references("agent-desktop").is_empty());

    let stale_refs = home.join("krill-computer-use/references");
    fs::create_dir_all(&stale_refs).unwrap();
    fs::write(stale_refs.join("workflows.md"), format!("old\n{MARKER}\n")).unwrap();

    let path = install_skill("agent-desktop", &home).unwrap();
    assert_eq!(path, home.join("krill-computer-use/SKILL.md"));
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("---\nname: krill-computer-use\n"));
    assert!(body.contains("agent-desktop snapshot"));
    assert!(body.contains("agent-desktop skills get desktop"));
    assert!(!stale_refs.exists());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn krig_bundle_uses_syntax_valid_markers_and_skips_developer_symlinks() {
    let home = home("krig-bundle");
    let first = sync_builtin_skills_at(&home);
    assert!(first.failures.is_empty(), "{:?}", first.failures);

    let krig_dir = home.join(".claude/skills/krig");
    let has_hangul = |s: &str| {
        s.chars().any(|c| {
            matches!(
                c,
                '\u{AC00}'..='\u{D7A3}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}'
            )
        })
    };
    let skill_md = fs::read_to_string(krig_dir.join("SKILL.md")).unwrap();
    assert!(skill_md.ends_with(&format!("{MARKER}\n")));
    assert!(!has_hangul(&skill_md), "krig SKILL.md must be English");

    let openai_yaml = fs::read_to_string(krig_dir.join("agents/openai.yaml")).unwrap();
    assert!(openai_yaml.ends_with(&format!("{YAML_MARKER}\n")));
    assert!(!openai_yaml.contains(MARKER));
    assert!(
        !has_hangul(&openai_yaml),
        "krig openai.yaml must be English"
    );
    for (relative, content) in KRIG_FILES {
        let is_doc = std::path::Path::new(relative)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("yaml"));
        if is_doc {
            assert!(
                !has_hangul(content),
                "krig bundled doc {relative} must be English"
            );
        }
    }

    let route_mjs = fs::read_to_string(krig_dir.join("scripts/route.mjs")).unwrap();
    assert!(route_mjs.ends_with(&format!("{JS_MARKER}\n")));
    assert!(!route_mjs.contains(MARKER));
    assert!(krig_dir.join("playbooks/drive.md").is_file());
    assert!(krig_dir.join("references/standards-baseline.md").is_file());

    if let Some(node) = crate::agent::which("node", &std::env::var_os("PATH").unwrap_or_default()) {
        for script in [
            "scripts/route.mjs",
            "scripts/scan.mjs",
            "scripts/ship-check.mjs",
        ] {
            let status = std::process::Command::new(&node)
                .arg("--check")
                .arg(krig_dir.join(script))
                .status()
                .unwrap();
            assert!(status.success(), "{script} failed node --check");
        }
    }

    #[cfg(unix)]
    {
        let codex_skills = home.join(".codex/skills");
        let codex_krig = codex_skills.join("krig");
        let external = home.join("external-krig");
        fs::create_dir_all(&external).unwrap();
        fs::remove_dir_all(&codex_krig).unwrap();
        std::os::unix::fs::symlink(&external, &codex_krig).unwrap();

        let second = sync_builtin_skills_at(&home);
        assert!(second.failures.is_empty(), "{:?}", second.failures);
        assert!(
            fs::symlink_metadata(&codex_krig)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!codex_skills.join("krill-krig").exists());
        assert!(install_skill("krig", &codex_skills).is_err());
    }

    fs::remove_dir_all(home).unwrap();
}

#[test]
fn external_skill_ownership_skip_is_not_reported_as_failure() {
    let home = home("external-skip");
    let claude_skills = home.join(".claude/skills");
    let ext = claude_skills.join("krill-orchestration");
    fs::create_dir_all(&ext).unwrap();
    fs::write(ext.join("SKILL.md"), "user custom orchestration skill\n").unwrap();

    let report = sync_builtin_skills(std::slice::from_ref(&claude_skills));
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(
        report
            .skipped_external
            .iter()
            .any(|entry| entry.contains("orchestration")),
        "expected orchestration in skipped_external: {:?}",
        report.skipped_external
    );
    assert_eq!(
        fs::read_to_string(ext.join("SKILL.md")).unwrap(),
        "user custom orchestration skill\n"
    );
    assert!(install_skill("orchestration", &claude_skills).is_err());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn foreign_orca_skill_and_user_references_stay_already_current_without_locking() {
    let home = home("foreign-orca-current");
    let root = home.join("skills");
    let first = sync_builtin_skills(std::slice::from_ref(&root));
    assert!(first.failures.is_empty(), "{:?}", first.failures);
    assert_eq!(first.updated, builtin_skills().len());

    // Simulate Orca's own unprefixed `orchestration` and `computer-use` skills alongside
    // Krill's `krill-orchestration` / `krill-computer-use`, plus a user-authored file in
    // `krill-computer-use/references/`.
    for foreign in ["orchestration", "computer-use"] {
        let dir = root.join(foreign);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {foreign}\n---\nForeign Orca skill\n"),
        )
        .unwrap();
    }
    let user_ref_dir = root.join("krill-computer-use/references");
    fs::create_dir_all(&user_ref_dir).unwrap();
    let user_note = user_ref_dir.join("notes.md");
    fs::write(&user_note, "user-authored notes").unwrap();

    // Holding `.krill-skills.lock` proves `already_current` succeeds without touching the lock.
    let guard = LockGuard::acquire(&root.join(".krill-skills.lock")).unwrap();
    let second = sync_builtin_skills(std::slice::from_ref(&root));
    drop(guard);
    assert!(second.failures.is_empty(), "{:?}", second.failures);
    assert!(second.busy.is_empty(), "{:?}", second.busy);
    assert_eq!(second.updated, 0);
    assert_eq!(second.unchanged, builtin_skills().len());
    assert_eq!(
        fs::read_to_string(&user_note).unwrap(),
        "user-authored notes"
    );

    // Stale Krill-marked references and stale Krill-owned unprefixed copies still get cleaned.
    let stale_ref = user_ref_dir.join("old.md");
    fs::write(&stale_ref, format!("old\n{MARKER}\n")).unwrap();
    fs::write(
        root.join("orchestration/SKILL.md"),
        format!("old krill copy\n{MARKER}\n"),
    )
    .unwrap();
    let cleanup = sync_builtin_skills(std::slice::from_ref(&root));
    assert!(cleanup.failures.is_empty(), "{:?}", cleanup.failures);
    assert!(!stale_ref.exists());
    assert!(user_note.is_file());
    assert!(!root.join("orchestration/SKILL.md").exists());

    fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
#[test]
fn readonly_root_with_foreign_orca_skill_checks_every_remaining_skill() {
    use std::os::unix::fs::PermissionsExt as _;

    let home = home("readonly-orca-current");
    let root = home.join("skills");
    let initial = sync_builtin_skills(std::slice::from_ref(&root));
    assert!(initial.failures.is_empty(), "{:?}", initial.failures);

    let orca_dir = root.join("orchestration");
    fs::create_dir_all(&orca_dir).unwrap();
    fs::write(
        orca_dir.join("SKILL.md"),
        "---\nname: orchestration\n---\nForeign Orca skill\n",
    )
    .unwrap();
    let user_refs = root.join("krill-computer-use/references");
    fs::create_dir_all(&user_refs).unwrap();
    fs::write(user_refs.join("custom.md"), "user notes").unwrap();

    fs::remove_file(root.join(".krill-skills.lock")).ok();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();

    let report = sync_builtin_skills(std::slice::from_ref(&root));
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).ok();
    fs::remove_dir_all(home).ok();

    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.unchanged, builtin_skills().len());
}
