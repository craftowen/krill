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
    let report = receive.recv_timeout(Duration::from_secs(2)).unwrap();
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
