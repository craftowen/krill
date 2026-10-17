//! MK-093: `krill set --comment` · `krill worktree set --comment` CLI 통합 테스트.

use std::fmt::Write as _;
use std::fs;
use std::process::{Command, Stdio};

const MK: &str = env!("CARGO_BIN_EXE_krill");

#[test]
fn cli_set_and_clear_worktree_comment() {
    let dir = std::env::temp_dir().join(format!("mk-cli-set-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let repo = dir.join("repo");
    let sub = repo.join("crates/sub");
    let state = dir.join("state");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir_all(repo.join(".git")).unwrap();
    let canonical_repo = fs::canonicalize(&repo).unwrap();

    let out = Command::new(MK)
        .args(["set", "--comment", "PR #93 리뷰 대기\n두번째 줄"])
        .current_dir(&sub)
        .env("KRILL_STATE_DIR", &state)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let raw = fs::read_to_string(state.join("worktree-comments.toml")).unwrap();
    assert!(raw.contains(&canonical_repo.to_string_lossy().into_owned()));
    assert!(raw.contains("PR #93 리뷰 대기 두번째 줄"));

    let out = Command::new(MK)
        .args([
            "worktree",
            "set",
            "--worktree",
            &canonical_repo.to_string_lossy(),
            "--comment",
            "",
        ])
        .env("KRILL_STATE_DIR", &state)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let cleared = fs::read_to_string(state.join("worktree-comments.toml")).unwrap();
    assert!(!cleared.contains("PR #93 리뷰 대기"));
    fs::remove_dir_all(dir).ok();
}

#[test]
fn cap_evicts_oldest_by_time_not_lexicographic() {
    let dir = std::env::temp_dir().join(format!("mk-set-cap-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    // 1,024개 상한을 시각이 엇갈리게 채운다: 사전순 첫 키("/a0000")는 가장 새 시각,
    // "/victim"은 가장 오래된 시각으로 둔다.
    let mut comments = String::from("[comments]\n");
    let mut times = String::from("[updated_at]\n");
    for i in 0..1_024u32 {
        let key = if i == 512 {
            "/victim".to_owned()
        } else {
            format!("/a{i:04}")
        };
        let stamp = if i == 512 {
            1
        } else if i == 0 {
            9_999_999
        } else {
            1_000 + u64::from(i)
        };
        writeln!(comments, "\"{key}\" = \"c{i}\"").unwrap();
        writeln!(times, "\"{key}\" = {stamp}").unwrap();
    }
    fs::write(
        dir.join("worktree-comments.toml"),
        format!("{comments}{times}"),
    )
    .unwrap();
    let out = Command::new(MK)
        .args(["set", "--comment", "new", "--worktree", "/repo/new"])
        .env("KRILL_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let raw = fs::read_to_string(dir.join("worktree-comments.toml")).unwrap();
    assert!(raw.contains("/repo/new"), "{raw}");
    assert!(
        !raw.contains("\"/victim\""),
        "oldest by time must go: {raw}"
    );
    assert!(
        raw.contains("\"/a0000\""),
        "lexicographically-first but newest must stay"
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn regression_corrupt_comments_refuse_write_and_preserve_bytes() {
    for (name, bytes) in [
        ("syntax", b"[comments\nunfinished".to_vec()),
        ("utf8", vec![0xff]),
        ("oversize", vec![b' '; 256 * 1024 + 1]),
    ] {
        let dir =
            std::env::temp_dir().join(format!("mk-set-corrupt-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("worktree-comments.toml");
        fs::write(&file, &bytes).unwrap();
        let out = Command::new(MK)
            .args(["set", "--comment", "new", "--worktree", "/repo"])
            .env("KRILL_STATE_DIR", &dir)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{name}: {out:?}");
        assert_eq!(fs::read(&file).unwrap(), bytes);
        assert_eq!(
            fs::read(file.with_extension("toml.corrupt")).unwrap(),
            bytes
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
