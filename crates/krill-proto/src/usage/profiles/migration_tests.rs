use super::*;

mod migration_regression_tests {
    use super::*;

    #[test]
    fn legacy_migration_preserves_nested_data_and_fails_without_switching() {
        let home = std::env::temp_dir().join(format!("krill-legacy-copy-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let source = managed_root(&home).join("codex/account-with-a-very-long-legacy-home-name");
        std::fs::create_dir_all(source.join("sessions/day")).unwrap();
        std::fs::create_dir_all(source.join("rules")).unwrap();
        std::fs::write(source.join("auth.json"), "original credentials").unwrap();
        std::fs::write(
            source.join("sessions/day/conversation.jsonl"),
            "original conversation",
        )
        .unwrap();
        std::fs::write(source.join("rules/private.md"), "private rules").unwrap();
        let target = try_migrate_long_codex_profile(&home, "nested", &source).unwrap();
        assert_eq!(
            std::fs::read(target.join("rules/private.md")).unwrap(),
            b"private rules"
        );
        assert_eq!(
            std::fs::read(target.join("sessions/day/conversation.jsonl")).unwrap(),
            b"original conversation"
        );
        assert_eq!(
            try_migrate_long_codex_profile(&home, "nested", &source).unwrap(),
            target
        );
        std::fs::write(target.join("auth.json"), "new credentials").unwrap();
        let error = try_migrate_long_codex_profile(&home, "nested", &source).unwrap_err();
        assert!(error.to_string().contains("both homes preserved"));
        assert_eq!(
            std::fs::read(target.join("auth.json")).unwrap(),
            b"new credentials"
        );
        assert_eq!(
            std::fs::read(source.join("auth.json")).unwrap(),
            b"original credentials"
        );
        let external = home.join("shared-history");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("record"), "shared data").unwrap();
        std::os::unix::fs::symlink("../../../../../shared-history", source.join("external"))
            .unwrap();
        let with_link = try_migrate_long_codex_profile(&home, "links", &source).unwrap();
        assert_eq!(
            with_link.join("external").canonicalize().unwrap(),
            external.canonicalize().unwrap()
        );
        let blocked = short_codex_profile_dir(&home, "blocked");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("auth.json"), "someone else").unwrap();
        assert!(try_migrate_long_codex_profile(&home, "blocked", &source).is_err());
        assert_eq!(
            migrate_long_codex_profile_if_needed(&home, "blocked", &source),
            source
        );
        let socket_path = PathBuf::from(format!("/tmp/kcs-{}", std::process::id()));
        std::fs::remove_file(&socket_path).ok();
        let socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        std::fs::rename(socket_path, source.join("live.sock")).unwrap();
        assert!(try_migrate_long_codex_profile(&home, "incomplete", &source).is_err());
        assert!(!short_codex_profile_dir(&home, "incomplete").exists());
        drop(socket);
        std::fs::remove_dir_all(home).unwrap();
    }
}

mod independent_verifier {
    use super::*;
    #[test]
    fn independent_interrupted_registration_retry_must_not_switch_to_stale_copy() {
        let home = std::env::temp_dir().join(format!(
            "independent-migration-retry-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&home).ok();
        let source = managed_root(&home).join("codex/legacy-with-a-long-source-profile-directory");
        std::fs::create_dir_all(source.join("sessions")).unwrap();
        std::fs::write(source.join("auth.json"), "old auth").unwrap();
        let target = try_migrate_long_codex_profile(&home, "retry", &source).unwrap();
        // Simulate crash/save failure after target publication but before accounts.toml switches.
        std::fs::write(source.join("auth.json"), "new auth").unwrap();
        std::fs::write(
            source.join("sessions/new.jsonl"),
            "new session after failed registration",
        )
        .unwrap();
        let retry = try_migrate_long_codex_profile(&home, "retry", &source);
        let safe = retry.as_ref().map_or(true, |p| {
            std::fs::read(p.join("auth.json")).unwrap() == b"new auth"
                && p.join("sessions/new.jsonl").exists()
        });
        assert!(source.join("sessions/new.jsonl").exists());
        let actual = std::fs::read(target.join("auth.json")).unwrap();
        std::fs::remove_dir_all(home).unwrap();
        assert!(
            safe,
            "stale target accepted after source changed: {actual:?}"
        );
    }

    #[test]
    fn independent_simultaneous_source_write_fails_and_keeps_original() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let home =
            std::env::temp_dir().join(format!("independent-migration-live-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let source = managed_root(&home).join("codex/legacy-with-a-long-source-profile-directory");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("auth.json"), vec![0u8; 16 * 1024 * 1024]).unwrap();
        for n in 0..1000 {
            std::fs::write(source.join(format!("file-{n}")), b"data").unwrap();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let src = source.clone();
        let writer = std::thread::spawn(move || {
            use std::io::{Seek, Write};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(src.join("auth.json"))
                .unwrap();
            let mut n = 0u8;
            while !stopped.load(Ordering::Acquire) {
                f.rewind().unwrap();
                f.write_all(&[n]).unwrap();
                n = n.wrapping_add(1);
            }
        });
        let result = try_migrate_long_codex_profile(&home, "live", &source);
        stop.store(true, Ordering::Release);
        writer.join().unwrap();
        let source_ok = source.join("auth.json").is_file();
        let target_exists = short_codex_profile_dir(&home, "live").exists();
        let leftovers = std::fs::read_dir(short_codex_profile_dir(&home, "live").parent().unwrap())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("migrating-"));
        std::fs::remove_dir_all(home).unwrap();
        assert!(
            result.is_err() && source_ok && !target_exists && !leftovers,
            "live source published or rollback failed: {result:?}"
        );
    }
    #[test]
    fn retry_during_source_writes_preserves_newer_target_and_legacy_home() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let home =
            std::env::temp_dir().join(format!("krill-migration-retry-live-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let source = managed_root(&home).join("codex/legacy-with-a-long-source-profile-directory");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("auth.json"), vec![0; 16 * 1024 * 1024]).unwrap();
        let target = try_migrate_long_codex_profile(&home, "retry-live", &source).unwrap();
        std::fs::write(target.join("new-target-session.jsonl"), "modern session").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(std::sync::Barrier::new(2));
        let (stopped, started, src) = (stop.clone(), ready.clone(), source.clone());
        let writer = std::thread::spawn(move || {
            use std::io::{Seek, Write};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(src.join("auth.json"))
                .unwrap();
            file.write_all(b"active legacy").unwrap();
            started.wait();
            let mut n = 1u8;
            while !stopped.load(Ordering::Acquire) {
                file.rewind().unwrap();
                file.write_all(&[n]).unwrap();
                n = n.wrapping_add(1);
            }
        });
        ready.wait();
        let retry = try_migrate_long_codex_profile(&home, "retry-live", &source);
        stop.store(true, Ordering::Release);
        writer.join().unwrap();
        assert!(retry.is_err());
        assert!(source.join("auth.json").is_file());
        assert_eq!(
            std::fs::read(target.join("new-target-session.jsonl")).unwrap(),
            b"modern session"
        );
        assert_eq!(
            std::fs::read(target.join("auth.json")).unwrap(),
            vec![0; 16 * 1024 * 1024]
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
