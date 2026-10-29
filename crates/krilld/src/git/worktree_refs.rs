//! 워크트리 생성 창의 읽기 전용 ref 목록. 사용자 명령이나 옵션을 받지 않는다.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use krill_proto::control::{MAX_WORKTREE_REF_BYTES, MAX_WORKTREE_REFS, Reply};
use krill_proto::os::ffi::OsStrExt as _;

use super::GitQueue;

const TIMEOUT: Duration = Duration::from_secs(3);
// 실행 제한 이후 자식 종료 유예(2초)를 포함한다. 큐에서 늦게 꺼낸 요청은 spawn 전에 만료한다.
const REPLY_TIMEOUT: Duration = Duration::from_secs(6);

pub fn serve(repo: &[u8]) -> Reply {
    let result = super::queue().and_then(|queue| {
        query(queue, PathBuf::from(OsStr::from_bytes(repo)))
            .recv_timeout(REPLY_TIMEOUT)
            .map_err(|e| e.to_string())?
    });
    match result {
        Ok(refs) => Reply::WorktreeRefs(refs),
        Err(error) => Reply::WorktreeFailed(error),
    }
}

fn query(queue: &GitQueue, repo: PathBuf) -> mpsc::Receiver<Result<Vec<String>, String>> {
    let (tx, rx) = mpsc::channel();
    let deadline = Instant::now() + TIMEOUT;
    queue.enqueue(
        repo,
        0,
        Box::new(move |program, repo| {
            let args: Vec<OsString> = [
                "for-each-ref".to_owned(),
                "--format=%(refname:short)".to_owned(),
                format!("--count={MAX_WORKTREE_REFS}"),
                "refs/heads".to_owned(),
                "refs/remotes".to_owned(),
                "refs/tags".to_owned(),
            ]
            .into_iter()
            .map(Into::into)
            .collect();
            // 공통 runner가 출력 상한·환경 제거·자식 회수를 담당한다. guard는 큐 대기도 포함한다.
            let result = super::run_guarded(program, repo, &args, || {
                if Instant::now() >= deadline {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "git refs timed out",
                    ))
                } else {
                    Ok(())
                }
            })
            .map_err(|e| e.to_string())
            .and_then(|output| {
                if output.status.success() {
                    Ok(parse(&output.stdout))
                } else {
                    Err("git for-each-ref failed".to_owned())
                }
            });
            tx.send(result).ok();
        }),
    );
    rx
}

fn parse(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == b'\n')
        .take(MAX_WORKTREE_REFS)
        .filter_map(|line| std::str::from_utf8(line).ok())
        .map(|name| name.strip_suffix('\r').unwrap_or(name)) // CRLF의 구분자만 제거한다.
        .filter(|name| {
            !name.is_empty()
                && name.len() <= MAX_WORKTREE_REF_BYTES
                && !name.chars().any(|c| c.is_control() || c.is_whitespace())
        })
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::path::Path;
    use std::process::Command;

    struct Fixture {
        dir: PathBuf,
        repo: PathBuf,
        queue: GitQueue,
    }

    impl Fixture {
        fn new(tag: &str) -> io::Result<Self> {
            let dir = std::env::temp_dir().join(format!("krill-{tag}-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            let repo = dir.join("레포");
            std::fs::create_dir_all(&repo)?;
            let fixture = Self {
                dir,
                repo,
                queue: GitQueue::new()?,
            };
            fixture.git(&["init", "-q", "-b", "main"])?;
            fixture.git(&["commit", "-q", "--allow-empty", "-m", "init"])?;
            Ok(fixture)
        }

        fn git(&self, args: &[&str]) -> io::Result<()> {
            Self::git_in(&self.repo, args)
        }

        fn git_in(path: &Path, args: &[&str]) -> io::Result<()> {
            let output = Command::new("git")
                .args([
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "tag.gpgSign=false",
                    "-c",
                    "commit.gpgSign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .current_dir(path)
                .output()?;
            assert!(output.status.success(), "{output:?}");
            Ok(())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    #[test]
    fn worktree_refs_are_actual_local_remote_and_tag_refs() {
        let f = Fixture::new("refs-query").unwrap();
        f.git(&["branch", "한글-브랜치"]).unwrap();
        f.git(&["tag", "v1"]).unwrap();
        f.git(&["update-ref", "refs/remotes/upstream/release", "HEAD"])
            .unwrap();
        let refs = query(&f.queue, f.repo.clone()).recv().unwrap().unwrap();
        assert_eq!(refs, ["main", "upstream/release", "v1", "한글-브랜치"]);
        assert!(!refs.iter().any(|name| name == "origin/main"));
        let empty = f.dir.join("empty");
        std::fs::create_dir(&empty).unwrap();
        Fixture::git_in(&empty, &["init", "-q", "-b", "main"]).unwrap();
        assert!(query(&f.queue, empty).recv().unwrap().unwrap().is_empty());
        assert!(query(&f.queue, f.dir.clone()).recv().unwrap().is_err());
    }

    #[test]
    fn worktree_refs_parser_is_bounded_and_skips_unusable_names() {
        let bytes = b"main\nmain\n\xff\n\ninvalid name\nmain \nupstream/topic\r\nv1\n";
        assert_eq!(parse(bytes), ["main", "upstream/topic", "v1"]);
        assert!(parse("x".repeat(MAX_WORKTREE_REF_BYTES + 1).as_bytes()).is_empty());
        let mut bytes = String::new();
        for i in 0..MAX_WORKTREE_REFS + 10 {
            writeln!(bytes, "topic-{i}").unwrap();
        }
        assert_eq!(parse(bytes.as_bytes()).len(), MAX_WORKTREE_REFS);
    }

    #[test]
    fn worktree_refs_query_enforces_runner_output_budget() {
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::new("refs-output-budget").unwrap();
        let script = f.dir.join("large-output-git");
        std::fs::write(
            &script,
            "#!/bin/sh\nexec /usr/bin/head -c 4194305 /dev/zero\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let queue = GitQueue::with_program(script, 1).unwrap();
        let error = query(&queue, f.repo.clone())
            .recv_timeout(REPLY_TIMEOUT)
            .unwrap()
            .unwrap_err();
        assert!(error.contains("byte budget"), "{error}");
        drop(queue);
    }

    #[test]
    fn worktree_refs_query_times_out_and_reaps_child() {
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::new("refs-timeout").unwrap();
        let script = f.dir.join("slow-git");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let queue = GitQueue::with_program(script, 1).unwrap();
        assert!(
            query(&queue, f.repo.clone())
                .recv_timeout(REPLY_TIMEOUT)
                .unwrap()
                .is_err()
        );
        drop(queue);
    }
}
