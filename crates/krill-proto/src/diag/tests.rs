use super::*;

const SECRETS: &str = "\
key sk-ant-api03-AbC_def-123456789012345678901234567890
openai sk-proj-abcdefghijklmnop1234
gh ghp_abcdefghijklmnopqrstuvwxyz0123456789 and github_pat_11ABCDEFG0123456789_abcdefghijklmnop
slack xoxb-1234567890-abcdefghij aws AKIAABCDEFGHIJKLMNOP
Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig
GITHUB_TOKEN=gho_shortbutsecret
DB_PASSWORD = hunter2
{\"api_key\": \"plain-value-123\", \"client_secret\":\"s3cr3t\"}
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ
-----END OPENSSH PRIVATE KEY-----
path /Users/owen/work/repo/src/main.rs:10:5
-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEA truncated tail";

const LEAKS: &[&str] = &[
    "sk-ant-api03",
    "AbC_def",
    "sk-proj-abcdef",
    "ghp_abc",
    "github_pat_11",
    "xoxb-123",
    "AKIAABCD",
    "eyJhbGci",
    "gho_short",
    "hunter2",
    "plain-value-123",
    "s3cr3t",
    "b3BlbnNzaC1rZXk",
    "MIIEowIBAAKCAQEA",
    "owen",
];

#[test]
fn redacts_tokens_pem_and_home() {
    let out = redact(SECRETS);
    for leak in LEAKS {
        assert!(!out.contains(leak), "{leak} leaked:\n{out}");
    }
    assert_eq!(out.matches("[REDACTED PEM]").count(), 2, "{out}");
    assert!(out.contains("GITHUB_TOKEN=[REDACTED]"), "{out}");
    assert!(
        out.contains("/Users/[USER]/work/repo/src/main.rs:10:5"),
        "{out}"
    );
}

#[test]
fn keeps_ordinary_text() {
    let text = "panicked at crates/krilld/src/auth.rs:12:5:\nindex out of bounds: the len is 3 but the index is 7\nrisk-assessment task-runner skip=3";
    assert_eq!(redact(text), text);
}

fn temp_state(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mk-diag-{}-{name}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    d
}

#[test]
fn bundle_collects_redacted_logs_and_acknowledge_hides_them() {
    let state = temp_state("bundle");
    let dir = crash_dir(&state);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("crash-1-10-krill-app.txt"), SECRETS).unwrap();
    fs::write(dir.join("crash-2-11-krilld.txt"), "message: boom").unwrap();
    fs::write(dir.join("other.txt"), "not a crash log").unwrap();

    let logs = pending(&state);
    assert_eq!(logs.len(), 2);
    acknowledge(&logs[..1]);
    assert_eq!(pending(&state), logs[1..]);

    let path = write_bundle(&state).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("==== crash-1-10-krill-app.seen ===="),
        "{text}"
    );
    assert!(text.contains("==== crash-2-11-krilld.txt ===="), "{text}");
    assert!(!text.contains("not a crash log"));
    for leak in LEAKS {
        assert!(!text.contains(leak), "{leak} leaked:\n{text}");
    }
    #[cfg(unix)]
    {
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
    }
    fs::remove_dir_all(&state).ok();
}

#[test]
fn same_second_crashes_keep_both_reports() {
    // PH-04: 같은 프로세스가 같은 초에 두 번 panic해도 두 보고가 다 남아야 한다.
    // 파일명에 프로세스 내 일련번호가 있어 `create_new`가 겹치지 않는다.
    let state = temp_state("twice");
    let dir = crash_dir(&state);
    let prev = std::panic::take_hook();
    let d = dir.clone();
    std::panic::set_hook(Box::new(move |info| {
        write_crash(&d, "test", info).ok();
    }));
    for _ in 0..2 {
        drop(std::panic::catch_unwind(|| {
            panic!("PH-04 same-second crash")
        }));
    }
    std::panic::set_hook(prev);
    let logs = pending(&state);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert_ne!(logs[0].file_name(), logs[1].file_name());
    fs::remove_dir_all(&state).ok();
}

#[test]
fn crash_filename_orders_by_time_and_never_collides_in_process() {
    let mut names = std::collections::BTreeSet::new();
    for seq in 0..1000u64 {
        names.insert(crash_filename(1_760_000_000, 123, 42, "krilld", seq));
    }
    assert_eq!(names.len(), 1000);
    let name = crash_filename(1_760_000_000, 5, 42, "krilld", 7);
    assert!(name.starts_with("crash-"), "{name}");
    assert!(
        std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e == "txt"),
        "{name}"
    );
    assert!(
        crash_filename(1_760_000_000, 5, 42, "krilld", 7)
            < crash_filename(1_760_000_001, 5, 42, "krilld", 0),
        "이름순 = 시각순"
    );
}

#[test]
fn panic_hook_writes_crash_log() {
    let state = temp_state("hook");
    let dir = crash_dir(&state);
    let prev = std::panic::take_hook();
    let d = dir.clone();
    std::panic::set_hook(Box::new(move |info| {
        write_crash(&d, "test", info).unwrap();
    }));
    let caught = std::panic::catch_unwind(|| panic!("MK-067 hook test"));
    std::panic::set_hook(prev);
    assert!(caught.is_err());
    let logs = pending(&state);
    assert_eq!(logs.len(), 1);
    let text = fs::read_to_string(&logs[0]).unwrap();
    assert!(text.contains("process: test"), "{text}");
    assert!(text.contains("message: MK-067 hook test"), "{text}");
    assert!(
        text.contains("location: crates/krill-proto/src/diag/tests.rs"),
        "{text}"
    );
    assert!(text.contains("backtrace:"), "{text}");
    fs::remove_dir_all(&state).ok();
}
