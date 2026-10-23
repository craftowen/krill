use super::super::history_probe::{has_writer, no_open_files};
use super::*;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

type ConflictCase = (&'static str, &'static [u8], u64, &'static [u8], u64, bool);

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let home =
        std::env::temp_dir().join(format!("krill-shared-history-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&home).ok();
    let host = home.join(".claude");
    let profile = home.join("profile");
    fs::create_dir_all(&host).unwrap();
    fs::create_dir_all(&profile).unwrap();
    (home, host, profile)
}

fn write(path: &Path, bytes: &[u8], time: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    File::open(path)
        .unwrap()
        .set_times(
            FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(time)),
        )
        .unwrap();
}

fn snapshot(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        let meta = path.symlink_metadata().unwrap();
        if meta.is_dir() {
            found.extend(snapshot(&path));
        } else if meta.is_file() {
            found.push((path.clone(), fs::read(&path).unwrap()));
        } else {
            found.push((
                path.clone(),
                fs::read_link(&path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            ));
        }
    }
    found.sort();
    found
}

#[test]
fn conflict_uses_size_then_mtime_and_preserves_every_version_idempotently() {
    let (home, host, profile) = fixture("conflict");
    let cases: &[ConflictCase] = &[
        ("larger", b"old", 50, b"longer", 10, true),
        ("smaller", b"longer", 10, b"new", 50, false),
        ("newer", b"old", 10, b"new", 50, true),
        ("older", b"old", 50, b"new", 10, false),
        ("tie", b"old", 50, b"new", 50, false),
    ];
    for (name, current, current_time, incoming, incoming_time, _) in cases {
        write(
            &host.join("projects/repo").join(name),
            current,
            *current_time,
        );
        write(
            &profile.join("projects/repo").join(name),
            incoming,
            *incoming_time,
        );
    }
    write(&profile.join("projects/repo/unique"), b"unique", 30);
    write(
        &profile.join("history.jsonl"),
        b"private prompt history",
        30,
    );
    let archive = archive_root(&host, &profile);
    share(&host, &profile, &|| true, &mut Report::default()).unwrap();
    for (name, current, _, incoming, _, wins) in cases {
        let relative = Path::new("projects/repo").join(name);
        assert_eq!(
            fs::read(host.join(relative)).unwrap(),
            if *wins { *incoming } else { *current }
        );
        assert_eq!(
            fs::read(archive.join("projects/original/repo").join(name)).unwrap(),
            *incoming
        );
        if *wins {
            assert_eq!(
                fs::read(archive.join("projects/displaced/repo").join(name)).unwrap(),
                *current
            );
        }
    }
    assert_eq!(
        fs::read(host.join("projects/repo/unique")).unwrap(),
        b"unique"
    );
    assert_eq!(
        fs::read_link(profile.join("history.jsonl")).unwrap(),
        host.join("history.jsonl")
    );
    let before = snapshot(&home);
    let mut report = Report::default();
    share(&host, &profile, &|| false, &mut report).unwrap();
    assert_eq!(
        report,
        Report::default(),
        "이미 이행한 프로필은 실행 중에도 재이행하지 않는다"
    );
    assert_eq!(snapshot(&home), before);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn live_writer_or_unavailable_probe_defers_without_touching_private_history() {
    let (home, host, profile) = fixture("busy");
    write(
        &profile.join("projects/repo/session.jsonl"),
        b"still writing",
        10,
    );
    let before = snapshot(&home);
    let mut report = Report::default();
    share(&host, &profile, &|| false, &mut report).unwrap();
    assert!(report.conflicts.contains(&"projects"));
    assert_eq!(snapshot(&home), before);
    share(&host, &profile, &|| true, &mut Report::default()).unwrap();
    assert!(profile.join("projects").is_symlink());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn process_probe_only_defers_the_selected_profile_and_treats_errors_as_unknown() {
    let profile = Path::new("/tmp/profile with spaces");
    assert!(has_writer(
        "claude CLAUDE_CONFIG_DIR=/tmp/profile with spaces PATH=/bin",
        profile,
        "CLAUDE_CONFIG_DIR"
    ));
    assert!(has_writer(
        "node CLAUDE_CONFIG_DIR=/tmp/profile with spaces PATH=/bin",
        profile,
        "CLAUDE_CONFIG_DIR"
    ));
    assert!(!has_writer(
        "claude CLAUDE_CONFIG_DIR=/tmp/another-account PATH=/bin",
        profile,
        "CLAUDE_CONFIG_DIR"
    ));
    assert!(has_writer(
        "claude --continue\nnode /pkg/@anthropic-ai/claude-code/cli.js",
        profile,
        "CLAUDE_CONFIG_DIR"
    ));
    assert!(no_open_files(Some(1), b"", b""));
    assert!(!no_open_files(Some(0), b"1234\n", b""));
    assert!(!no_open_files(Some(1), b"", b"permission denied"));
    assert!(!no_open_files(None, b"", b""));
}

#[test]
#[cfg(target_os = "macos")]
fn actual_probe_detects_an_open_profile_file_without_affecting_other_profiles() {
    let (home, host, profile) = fixture("open-file");
    write(&profile.join("projects/session.jsonl"), b"open writer", 10);
    let file = OpenOptions::new()
        .append(true)
        .open(profile.join("projects/session.jsonl"))
        .unwrap();
    assert!(!quiescent(&profile));
    assert!(
        quiescent(&host),
        "다른 계정의 열린 파일은 이행을 막지 않는다"
    );
    drop(file);
    assert!(quiescent(&profile));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn resumes_after_original_directory_was_archived_before_crash() {
    let (home, host, profile) = fixture("restart");
    let archive = archive_root(&host, &profile);
    write(
        &archive.join("projects/original/repo/session.jsonl"),
        b"recover",
        10,
    );
    share(&host, &profile, &|| true, &mut Report::default()).unwrap();
    assert_eq!(
        fs::read(profile.join("projects/repo/session.jsonl")).unwrap(),
        b"recover"
    );
    assert!(
        archive
            .join("projects/original/repo/session.jsonl")
            .is_file()
    );
    let before = snapshot(&home);
    share(&host, &profile, &|| true, &mut Report::default()).unwrap();
    assert_eq!(snapshot(&home), before);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn refuses_nested_symlinks_and_rolls_back_incompatible_destination() {
    let (home, host, profile) = fixture("symlink");
    let outside = home.join("outside");
    fs::create_dir(&outside).unwrap();
    write(&outside.join("keep"), b"external", 10);
    fs::create_dir(profile.join("projects")).unwrap();
    std::os::unix::fs::symlink(&outside, profile.join("projects/link")).unwrap();
    assert!(share(&host, &profile, &|| true, &mut Report::default()).is_err());
    assert!(profile.join("projects/link").is_symlink());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"external");
    fs::remove_file(profile.join("projects/link")).unwrap();
    write(&profile.join("projects/session"), b"original", 10);
    fs::write(host.join("projects"), b"incompatible").unwrap();
    assert!(share(&host, &profile, &|| true, &mut Report::default()).is_err());
    assert_eq!(
        fs::read(profile.join("projects/session")).unwrap(),
        b"original"
    );
    assert_eq!(fs::read(host.join("projects")).unwrap(), b"incompatible");
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn account_state_stays_private_and_two_profiles_use_the_same_history() {
    let (home, host, profile) = fixture("private");
    for name in super::super::EXCLUDED {
        fs::write(profile.join(name), name).unwrap();
    }
    let second = home.join("second");
    fs::create_dir(&second).unwrap();
    for path in [&profile, &second] {
        share(&host, path, &|| true, &mut Report::default()).unwrap();
        for name in LINKED.iter().copied().chain([HISTORY]) {
            assert_eq!(fs::read_link(path.join(name)).unwrap(), host.join(name));
        }
    }
    for name in super::super::EXCLUDED {
        assert!(!profile.join(name).is_symlink());
        assert_eq!(fs::read(profile.join(name)).unwrap(), name.as_bytes());
    }
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn resumes_after_current_was_displaced_and_keeps_both_copies() {
    let (home, host, profile) = fixture("displaced-crash");
    let archive = archive_root(&host, &profile);
    write(
        &archive.join("projects/original/repo/session.jsonl"),
        b"longer original",
        10,
    );
    write(
        &archive.join("projects/displaced/repo/session.jsonl"),
        b"old",
        10,
    );
    share(&host, &profile, &|| true, &mut Report::default()).unwrap();
    assert_eq!(
        fs::read(host.join("projects/repo/session.jsonl")).unwrap(),
        b"longer original"
    );
    assert_eq!(
        fs::read(archive.join("projects/displaced/repo/session.jsonl")).unwrap(),
        b"old"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn another_daemon_migration_lock_defers_and_does_not_move_the_source() {
    let (home, host, profile) = fixture("locked");
    write(&profile.join("projects/session.jsonl"), b"keep", 10);
    let lock = File::create(host.join(".krill-history-migration.lock")).unwrap();
    lock.lock().unwrap();
    assert!(share(&host, &profile, &|| true, &mut Report::default()).is_err());
    assert!(!profile.join("projects").is_symlink());
    assert_eq!(
        fs::read(profile.join("projects/session.jsonl")).unwrap(),
        b"keep"
    );
    drop(lock);
    share(&host, &profile, &|| true, &mut Report::default()).unwrap();
    assert!(profile.join("projects").is_symlink());
    fs::remove_dir_all(home).unwrap();
}

#[test]
#[cfg(target_os = "macos")]
fn actual_probe_detects_profile_environment_even_without_open_history_files() {
    let (home, _, profile) = fixture("environment");
    let ready = home.join("ready");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "usage::provision::claude_history::tests::profile_environment_probe_child",
        ])
        .env("CLAUDE_CONFIG_DIR", &profile)
        .env("KRILL_PROFILE_PROBE_READY", &ready)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let idle = quiescent(&profile);
    child.kill().ok();
    child.wait().unwrap();
    assert!(!idle, "fd를 닫은 채 유휴 상태인 프로필 프로세스도 보호한다");
    assert!(quiescent(&profile));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn profile_environment_probe_child() {
    if let Some(ready) = std::env::var_os("KRILL_PROFILE_PROBE_READY") {
        fs::write(ready, b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(5));
    }
}
