use super::*;

fn provision(home: &Path, profile: &Path) -> io::Result<Report> {
    super::history::provision(home, profile, &|| true)
}
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let home = std::env::temp_dir().join(format!("krill-agy-history-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&home).ok();
    let host = home.join(".gemini/antigravity-cli");
    let profile = home.join("profile");
    fs::create_dir_all(&host).unwrap();
    fs::create_dir_all(profile.join(".gemini/antigravity-cli")).unwrap();
    (
        home.canonicalize().unwrap(),
        host.canonicalize().unwrap(),
        profile.canonicalize().unwrap(),
    )
}

fn database(path: &Path, rows: &str) {
    let sql = format!(
        "CREATE TABLE conversation_summaries(conversation_id TEXT PRIMARY KEY,title TEXT NOT NULL,last_modified_time TEXT NOT NULL); INSERT INTO conversation_summaries VALUES {rows};"
    );
    assert!(
        Command::new("/usr/bin/sqlite3")
            .arg(path)
            .arg(sql)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn new_agy_profile_shares_conversations_and_summary_database_without_sharing_auth() {
    let (home, host, profile) = fixture("first");
    let local = profile.join(".gemini/antigravity-cli");
    fs::write(local.join("antigravity-oauth-token"), b"private token").unwrap();
    provision(&home, &profile).unwrap();
    assert_eq!(
        fs::read_link(local.join("conversations")).unwrap(),
        host.join("conversations")
    );
    assert_eq!(
        fs::read_link(local.join("conversation_summaries.db")).unwrap(),
        host.join("conversation_summaries.db")
    );
    assert_eq!(
        fs::read(local.join("antigravity-oauth-token")).unwrap(),
        b"private token"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn agy_existing_conversations_share_and_summaries_are_archived_for_runtime_rebuild() {
    let (home, host, profile) = fixture("merge");
    let local = profile.join(".gemini/antigravity-cli");
    fs::create_dir_all(local.join("conversations")).unwrap();
    fs::write(
        local.join("conversations/private.pb"),
        b"private conversation",
    )
    .unwrap();
    database(
        &host.join("conversation_summaries.db"),
        "('global','global','2026-10-20'),('same','old','2026-10-20')",
    );
    database(
        &local.join("conversation_summaries.db"),
        "('private','private','2026-10-23'),('same','new','2026-10-23')",
    );
    provision(&home, &profile).unwrap();
    assert!(local.join("conversations").is_symlink());
    assert_eq!(
        fs::read(host.join("conversations/private.pb")).unwrap(),
        b"private conversation"
    );
    let output = Command::new("/usr/bin/sqlite3").arg("-readonly").arg(host.join("conversation_summaries.db")).arg("SELECT group_concat(title,',') FROM (SELECT title FROM conversation_summaries ORDER BY conversation_id)").output().unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "global,old"
    );
    let archive = super::super::history_store::archive_root(&host, &local, "history-merged-backup");
    assert!(
        archive
            .join("summaries/conversation_summaries.db")
            .is_file()
    );
    let again = provision(&home, &profile).unwrap();
    assert_eq!(again, Report::default());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn sqlite_database_and_wal_choose_one_complete_version_and_keep_both_backups() {
    let (home, host, profile) = fixture("wal-group");
    let local = profile.join(".gemini/antigravity-cli");
    for path in [&host, &local] {
        fs::create_dir(path.join("conversations")).unwrap();
    }
    // DB와 WAL을 파일별로 고르면 서로 다른 버전이 섞인다. SHM 크기는 정본 판단에서 제외한다.
    for (name, current, incoming) in [
        (
            "incoming.db",
            b"old".as_slice(),
            b"larger incoming".as_slice(),
        ),
        (
            "incoming.db-wal",
            b"old wal".as_slice(),
            b"incoming wal".as_slice(),
        ),
        (
            "incoming.db-shm",
            b"old shm".as_slice(),
            b"incoming shm".as_slice(),
        ),
        (
            "current.db",
            b"current".as_slice(),
            b"larger private db".as_slice(),
        ),
        (
            "current.db-wal",
            b"very very very large current WAL".as_slice(),
            b"new".as_slice(),
        ),
    ] {
        fs::write(host.join("conversations").join(name), current).unwrap();
        fs::write(local.join("conversations").join(name), incoming).unwrap();
    }
    provision(&home, &profile).unwrap();
    assert_eq!(
        fs::read(host.join("conversations/incoming.db")).unwrap(),
        b"larger incoming"
    );
    assert_eq!(
        fs::read(host.join("conversations/incoming.db-wal")).unwrap(),
        b"incoming wal"
    );
    assert_eq!(
        fs::read(host.join("conversations/incoming.db-shm")).unwrap(),
        b"incoming shm"
    );
    assert_eq!(
        fs::read(host.join("conversations/current.db")).unwrap(),
        b"current"
    );
    assert_eq!(
        fs::read(host.join("conversations/current.db-wal")).unwrap(),
        b"very very very large current WAL"
    );
    let archive = super::super::history_store::archive_root(&host, &local, "history-merged-backup");
    assert_eq!(
        fs::read(archive.join("conversations/original/current.db")).unwrap(),
        b"larger private db"
    );
    assert_eq!(
        fs::read(archive.join("conversations/displaced/incoming.db/incoming.db-wal")).unwrap(),
        b"old wal"
    );
    assert_eq!(provision(&home, &profile).unwrap(), Report::default());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn busy_agy_profile_or_host_defers_history_and_summaries_together() {
    let (home, host, profile) = fixture("busy");
    let local = profile.join(".gemini/antigravity-cli");
    fs::create_dir(local.join("conversations")).unwrap();
    fs::write(local.join("conversations/private.pb"), b"keep private").unwrap();
    fs::write(local.join("conversation_summaries.db"), b"keep summaries").unwrap();
    let report = super::history::provision(&home, &profile, &|| false).unwrap();
    assert_eq!(
        report.conflicts,
        ["conversations", "conversation_summaries.db"]
    );
    assert_eq!(
        fs::read(local.join("conversations/private.pb")).unwrap(),
        b"keep private"
    );
    assert_eq!(
        fs::read(local.join("conversation_summaries.db")).unwrap(),
        b"keep summaries"
    );
    assert!(!host.join("conversations").exists());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn new_profile_links_without_waiting_for_the_existing_host_agent() {
    let (home, host, profile) = fixture("first-busy-host");
    let report = super::history::provision(&home, &profile, &|| false).unwrap();
    assert!(report.conflicts.is_empty());
    assert!(
        profile
            .join(".gemini/antigravity-cli/conversations")
            .is_symlink()
    );
    assert!(host.join("conversation_summaries.db").is_file());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn agy_process_probe_distinguishes_default_home_and_selected_profile() {
    use super::super::history_probe::has_writer;
    let root = Path::new("/tmp/account");
    for text in [
        "agy --gemini_dir=/tmp/account/.gemini HOME=/tmp/global PATH=/bin",
        "agy --gemini_dir /tmp/account/.gemini HOME=/tmp/global PATH=/bin",
        "agy HOME=/tmp/account PATH=/bin",
        "agy ANTIGRAVITY_CLI_HOME=/tmp/account HOME=/tmp/global PATH=/bin",
        "agy --continue",
    ] {
        assert!(has_writer(text, root, "ANTIGRAVITY_CLI_HOME"), "{text}");
    }
    assert!(!has_writer(
        "agy ANTIGRAVITY_CLI_HOME=/tmp/account-other HOME=/tmp/account-other PATH=/bin",
        root,
        "ANTIGRAVITY_CLI_HOME"
    ));
    assert!(!has_writer(
        "agy --gemini_dir=/tmp/account/.gemini-other HOME=/tmp/global PATH=/bin",
        root,
        "ANTIGRAVITY_CLI_HOME"
    ));
    assert!(!has_writer(
        "agy ANTIGRAVITY_CLI_HOME=/tmp/account/nested HOME=/tmp/global PATH=/bin",
        root,
        "ANTIGRAVITY_CLI_HOME"
    ));
    assert!(!has_writer(
        "agy --gemini_dir=/tmp/other/.gemini HOME=/tmp/account PATH=/bin",
        root,
        "ANTIGRAVITY_CLI_HOME"
    ));
    assert!(!has_writer(
        "agy HOME=/tmp/other PATH=/bin",
        root,
        "ANTIGRAVITY_CLI_HOME"
    ));
}

#[test]
fn committed_wal_records_survive_migration_and_sqlite_integrity_check() {
    use std::io::{BufRead as _, Write as _};
    use std::process::Stdio;
    let (home, host, profile) = fixture("sqlite-real-wal");
    let local = profile.join(".gemini/antigravity-cli");
    fs::create_dir(local.join("conversations")).unwrap();
    fs::create_dir(host.join("conversations")).unwrap();
    let source = local.join("conversations/real.db");
    let target = host.join("conversations/real.db");
    database(&target, "('old','old','2026-10-20')");
    let mut child = Command::new("/usr/bin/sqlite3")
        .arg(&source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE payload(value TEXT); INSERT INTO payload VALUES (hex(zeroblob(50000)));\n.print ready\n").unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(output.read_line(&mut line).unwrap() > 0);
        if line.trim() == "ready" {
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    drop(input);
    let wal = source.with_extension("db-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 0);
    let before = fs::read(&wal).unwrap();
    provision(&home, &profile).unwrap();
    assert_eq!(fs::read(target.with_extension("db-wal")).unwrap(), before);
    let result = Command::new("/usr/bin/sqlite3")
        .arg("-readonly")
        .arg(&target)
        .arg("PRAGMA integrity_check; SELECT length(value) FROM payload;")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap().trim(),
        "ok\n100000"
    );
    let archive = super::super::history_store::archive_root(&host, &local, "history-merged-backup");
    assert_eq!(
        fs::read(archive.join("conversations/original/real.db-wal")).unwrap(),
        before
    );
    fs::remove_dir_all(home).unwrap();
}
