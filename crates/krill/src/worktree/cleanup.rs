//! `krill worktree cleanup` & `krill worktree space`(MK-175, OWE-729).
//!
//! - `krill worktree space [--repo <path>] [--json]`:
//!   워크트리별 전체 디스크 사용량 및 주요 빌드 아티팩트(`target`, `node_modules`, `.next` 등) 용량 집계.
//! - `krill worktree cleanup [--repo <path>] [--pr merged|closed|merged-or-closed|any] [--merged] [--closed] [--idle-days <N>] [--exclude-dirty|--include-dirty|--force] [--worktree <path>]... [--dry-run] [--yes] [--json]`:
//!   조건별(PR Merged/Closed, 경과 일수 7/14/30일 이상, dirty 안전 제외) 워크트리 일괄 삭제 및 병합된 로컬 브랜치 정리.
//!   `--worktree <path>`(반복 가능)를 주면 필터 일치 후보 중 그 경로만 대상으로 한다(앱 클린업 모달의 선택 실행).
//!
//! `--json` 출력 스키마는 `krill_proto::worktree_space`에 고정돼 있다: `space` → `SpaceReport`,
//! `cleanup` → `CleanupReport`. `krill-app`(`settings/space.rs`)이 이 출력을 그대로 파싱하므로 필드를 바꾸면 앱도 함께 바꾼다.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::ExitCode;

use krill_proto::daemon::{self, Paths};
use krill_proto::worktree_space::{
    CleanupFilter, CleanupItemOutcome, CleanupPrFilter, CleanupReport, CleanupSummary, SpaceReport,
    WorktreeCleanupCandidate, filter_cleanup_candidates, format_bytes,
};

use super::space::{
    discover_cleanup_candidates, discover_repositories, execute_cleanup as run_cleanup_engine,
    normalize_repo_roots, now_unix_secs, scan_workspace_space,
};

const CLEANUP_USAGE: &str = "usage: krill worktree cleanup [--repo <path>] [--pr merged|closed|merged-or-closed|any] [--merged] [--closed] [--idle-days <7|14|30|N>] [--exclude-dirty|--include-dirty|--force] [--worktree <path>]... [--dry-run] [--yes] [--json]";
const SPACE_USAGE: &str = "usage: krill worktree space [--repo <path>] [--json]";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CleanupArgs {
    repos: Vec<PathBuf>,
    filter: CleanupFilter,
    /// 비어 있지 않으면 필터 일치 후보 중 이 경로들만 남긴다.
    worktrees: Vec<PathBuf>,
    dry_run: bool,
    json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SpaceArgs {
    repos: Vec<PathBuf>,
    json: bool,
}

pub fn run_cleanup(args: &[OsString]) -> ExitCode {
    let parsed = match parse_cleanup_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("{e}\n{CLEANUP_USAGE}");
            return ExitCode::from(2);
        }
    };
    let state_dir = daemon::state_dir();
    let repos = match resolve_repos(&parsed.repos, state_dir.as_deref()) {
        Ok(repos) => repos,
        Err(e) => {
            eprintln!("worktree cleanup: {e}");
            return ExitCode::FAILURE;
        }
    };

    let all_candidates = discover_cleanup_candidates(&repos, state_dir.as_deref(), now_unix_secs());
    let matched = select_candidates(&all_candidates, &parsed);

    let summary = if parsed.dry_run || matched.is_empty() {
        CleanupSummary::default()
    } else {
        let cwds = match crate::session::running_session_cwds() {
            Ok(cwds) => cwds,
            Err(e) => {
                eprintln!(
                    "worktree cleanup: cannot check for running sessions ({e}); nothing was removed"
                );
                return ExitCode::FAILURE;
            }
        };
        let (idle, busy) = split_active_sessions(&matched, &cwds);
        let paths = Paths::dev().ok();
        let mut summary = run_cleanup_engine(
            &idle,
            !parsed.filter.exclude_dirty,
            paths.as_ref(),
            state_dir.as_deref(),
        );
        for item in busy {
            summary.failed_count = summary.failed_count.saturating_add(1);
            summary.items.push(CleanupItemOutcome {
                path: item.path,
                branch: item.branch,
                removed: false,
                branch_deleted: false,
                freed_bytes: 0,
                warnings: Vec::new(),
                error: Some("worktree has a running session; close it first".to_owned()),
            });
        }
        summary
    };

    if parsed.json {
        let out = CleanupReport {
            filter: parsed.filter,
            dry_run: parsed.dry_run,
            candidates: matched,
            summary,
        };
        match serde_json::to_string_pretty(&out) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("worktree cleanup: {e}");
                return ExitCode::FAILURE;
            }
        }
        return if out.summary.failed_count == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    if matched.is_empty() {
        println!("no matching worktrees to clean up");
        return ExitCode::SUCCESS;
    }

    if parsed.dry_run {
        println!("dry-run: {} worktree(s) matched", matched.len());
        for c in &matched {
            println!(
                "  {} (branch: {}, pr: {}, idle: {}d, size: {})",
                c.path.display(),
                c.branch.as_deref().unwrap_or("detached"),
                c.pr_state.as_str(),
                c.idle_days,
                format_bytes(c.disk_bytes),
            );
        }
        return ExitCode::SUCCESS;
    }

    for item in &summary.items {
        if item.removed {
            println!(
                "removed {} ({})",
                item.path.display(),
                format_bytes(item.freed_bytes)
            );
            if let Some(branch) = &item.branch {
                println!(
                    "branch {branch} {}",
                    if item.branch_deleted {
                        "deleted"
                    } else {
                        "preserved"
                    }
                );
            }
            for warning in &item.warnings {
                eprintln!("warning: {warning}");
            }
        } else if let Some(err) = &item.error {
            eprintln!("failed {}: {err}", item.path.display());
        }
    }
    println!(
        "cleanup complete: {} removed, {} failed, freed {}",
        summary.removed_count,
        summary.failed_count,
        format_bytes(summary.freed_bytes)
    );
    if summary.failed_count == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub fn run_space(args: &[OsString]) -> ExitCode {
    let parsed = match parse_space_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("{e}\n{SPACE_USAGE}");
            return ExitCode::from(2);
        }
    };
    let state_dir = daemon::state_dir();
    let repos = match resolve_repos(&parsed.repos, state_dir.as_deref()) {
        Ok(repos) => repos,
        Err(e) => {
            eprintln!("worktree space: {e}");
            return ExitCode::FAILURE;
        }
    };
    let report: SpaceReport = scan_workspace_space(&repos);
    if parsed.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("worktree space: {e}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    println!(
        "total: {} across {} worktree(s) (build artifacts: {})",
        format_bytes(report.total_bytes),
        report.worktrees.len(),
        format_bytes(report.total_artifact_bytes)
    );
    for wt in &report.worktrees {
        let artifacts = if wt.artifacts.is_empty() {
            "none".to_owned()
        } else {
            wt.artifacts
                .iter()
                .map(|a| format!("{} {}", a.name, format_bytes(a.bytes)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "  {:<12} {:<24} {} [artifacts: {}]",
            format_bytes(wt.total_bytes),
            wt.branch.as_deref().unwrap_or(&wt.name),
            wt.path.display(),
            artifacts
        );
    }
    ExitCode::SUCCESS
}

/// 필터를 적용하고, `--worktree`가 있으면 그 경로(정규화 비교)만 남긴다.
fn select_candidates(
    all: &[WorktreeCleanupCandidate],
    parsed: &CleanupArgs,
) -> Vec<WorktreeCleanupCandidate> {
    let mut matched = filter_cleanup_candidates(all, &parsed.filter);
    if !parsed.worktrees.is_empty() {
        let wanted: Vec<PathBuf> = parsed
            .worktrees
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();
        matched.retain(|c| wanted.contains(&c.path) || parsed.worktrees.contains(&c.path));
    }
    matched
}

/// 실행 중인 세션의 cwd가 안에 있는 워크트리를 따로 나눈다. 지우면 그 세션의 상대 경로 쓰기가 실패한다.
fn split_active_sessions(
    matched: &[WorktreeCleanupCandidate],
    cwds: &[PathBuf],
) -> (Vec<WorktreeCleanupCandidate>, Vec<WorktreeCleanupCandidate>) {
    let cwds: Vec<PathBuf> = cwds
        .iter()
        .map(|cwd| std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.clone()))
        .collect();
    matched.iter().cloned().partition(|c| {
        let wt = std::fs::canonicalize(&c.path).unwrap_or_else(|_| c.path.clone());
        !cwds.iter().any(|cwd| cwd.starts_with(&wt))
    })
}

fn resolve_repos(
    explicit: &[PathBuf],
    state_dir: Option<&std::path::Path>,
) -> Result<Vec<PathBuf>, String> {
    if !explicit.is_empty() {
        let mut resolved = Vec::new();
        for p in explicit {
            let canon =
                std::fs::canonicalize(p).map_err(|e| format!("cannot resolve repository: {e}"))?;
            resolved.push(canon);
        }
        return Ok(normalize_repo_roots(&resolved));
    }
    if let Ok(cwd) = std::env::current_dir() {
        let normalized = normalize_repo_roots(&[cwd]);
        if !normalized.is_empty() {
            return Ok(normalized);
        }
    }
    Ok(discover_repositories(state_dir))
}

fn parse_cleanup_args(args: &[OsString]) -> Result<CleanupArgs, String> {
    let mut it = args.iter();
    let mut out = CleanupArgs {
        repos: Vec::new(),
        filter: CleanupFilter::default(),
        worktrees: Vec::new(),
        dry_run: false,
        json: false,
    };

    while let Some(raw) = it.next() {
        let arg = utf8(raw)?;
        if let Some(val) = arg.strip_prefix("--repo=") {
            out.repos.push(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--worktree=") {
            out.worktrees.push(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--pr=") {
            out.filter.pr_filter = CleanupPrFilter::parse(val)?;
        } else if let Some(val) = arg
            .strip_prefix("--idle-days=")
            .or_else(|| arg.strip_prefix("--days="))
            .or_else(|| arg.strip_prefix("--older-than="))
        {
            out.filter.min_idle_days = parse_days(val)?;
        } else {
            match arg {
                "--repo" => {
                    let val = utf8(it.next().ok_or("missing --repo value")?)?;
                    out.repos.push(PathBuf::from(val));
                }
                "--worktree" => {
                    let val = utf8(it.next().ok_or("missing --worktree value")?)?;
                    out.worktrees.push(PathBuf::from(val));
                }
                "--pr" => {
                    let val = utf8(it.next().ok_or("missing --pr value")?)?;
                    out.filter.pr_filter = CleanupPrFilter::parse(val)?;
                }
                "--merged" => out.filter.pr_filter = CleanupPrFilter::Merged,
                "--closed" => out.filter.pr_filter = CleanupPrFilter::Closed,
                "--idle-days" | "--days" | "--older-than" => {
                    let val = utf8(it.next().ok_or("missing --idle-days value")?)?;
                    out.filter.min_idle_days = parse_days(val)?;
                }
                "--exclude-dirty" => out.filter.exclude_dirty = true,
                "--include-dirty" | "--force" => out.filter.exclude_dirty = false,
                "--dry-run" | "-n" => out.dry_run = true,
                "--yes" | "-y" => {}
                "--json" => out.json = true,
                _ => return Err(format!("unexpected argument {arg:?}")),
            }
        }
    }
    Ok(out)
}

fn parse_space_args(args: &[OsString]) -> Result<SpaceArgs, String> {
    let mut it = args.iter();
    let mut out = SpaceArgs::default();
    while let Some(raw) = it.next() {
        let arg = utf8(raw)?;
        if let Some(val) = arg.strip_prefix("--repo=") {
            out.repos.push(PathBuf::from(val));
        } else {
            match arg {
                "--repo" => {
                    let val = utf8(it.next().ok_or("missing --repo value")?)?;
                    out.repos.push(PathBuf::from(val));
                }
                "--json" => out.json = true,
                _ => return Err(format!("unexpected argument {arg:?}")),
            }
        }
    }
    Ok(out)
}

fn parse_days(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim().trim_end_matches('d').trim_end_matches('D');
    trimmed
        .parse::<u64>()
        .map_err(|_| format!("invalid idle days {raw:?}; expected a non-negative integer"))
}

fn utf8(s: &OsStr) -> Result<&str, String> {
    s.to_str()
        .ok_or_else(|| "arguments must be UTF-8".to_owned())
}

#[cfg(test)]
fn execute_cleanup_with_state(
    args: &[OsString],
    state_dir: Option<&std::path::Path>,
    use_daemon: bool,
) -> Result<(CleanupArgs, CleanupReport), String> {
    let parsed = parse_cleanup_args(args)?;
    let repos = resolve_repos(&parsed.repos, state_dir)?;
    let all_candidates = discover_cleanup_candidates(&repos, state_dir, now_unix_secs());
    let matched = select_candidates(&all_candidates, &parsed);
    let summary = if parsed.dry_run || matched.is_empty() {
        CleanupSummary::default()
    } else {
        let paths = use_daemon.then(|| Paths::dev().ok()).flatten();
        run_cleanup_engine(
            &matched,
            !parsed.filter.exclude_dirty,
            paths.as_ref(),
            state_dir,
        )
    };
    let out = CleanupReport {
        filter: parsed.filter.clone(),
        dry_run: parsed.dry_run,
        candidates: matched,
        summary,
    };
    Ok((parsed, out))
}

#[cfg(test)]
fn execute_space_with_state(
    args: &[OsString],
    state_dir: Option<&std::path::Path>,
) -> Result<(SpaceArgs, SpaceReport), String> {
    let parsed = parse_space_args(args)?;
    let repos = resolve_repos(&parsed.repos, state_dir)?;
    let report = scan_workspace_space(&repos);
    Ok((parsed, report))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use krill_proto::worktree_space::{CleanupPrState, SECS_PER_DAY};

    #[test]
    fn worktrees_holding_a_running_session_are_split_off() {
        let root = std::env::temp_dir().join(format!("mk-cleanup-busy-{}", std::process::id()));
        let (busy, idle) = (root.join("busy"), root.join("idle"));
        fs::create_dir_all(busy.join("sub")).unwrap();
        fs::create_dir_all(&idle).unwrap();
        let candidate = |path: &Path| super::WorktreeCleanupCandidate {
            repo: root.clone(),
            repo_name: String::new(),
            path: path.to_path_buf(),
            name: String::new(),
            branch: None,
            is_main: false,
            locked: false,
            dirty: false,
            dirty_count: 0,
            merged_into_main: true,
            pr_state: CleanupPrState::Merged,
            pr_number: None,
            last_active_secs: 0,
            idle_days: 0,
            disk_bytes: 0,
            artifact_bytes: 0,
        };
        let (keep, skip) = super::split_active_sessions(
            &[candidate(&busy), candidate(&idle)],
            &[busy.join("sub"), root.join("elsewhere")],
        );
        assert_eq!(
            keep.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
            [idle]
        );
        assert_eq!(
            skip.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
            [busy]
        );
        fs::remove_dir_all(&root).ok();
    }

    use super::super::space::record_worktree_metadata;

    use super::*;

    #[test]
    fn cleanup_arg_parser_handles_filters_and_days_suffix() {
        let args = [
            "--repo",
            "/tmp/repo",
            "--pr",
            "merged",
            "--idle-days",
            "14d",
            "--dry-run",
            "--json",
        ]
        .map(OsString::from);
        let parsed = parse_cleanup_args(&args).unwrap();
        assert_eq!(parsed.repos, vec![PathBuf::from("/tmp/repo")]);
        assert_eq!(parsed.filter.pr_filter, CleanupPrFilter::Merged);
        assert_eq!(parsed.filter.min_idle_days, 14);
        assert!(parsed.filter.exclude_dirty);
        assert!(parsed.dry_run);
        assert!(parsed.json);

        let args2 = ["--closed", "--older-than=30", "--include-dirty"].map(OsString::from);
        let parsed2 = parse_cleanup_args(&args2).unwrap();
        assert_eq!(parsed2.filter.pr_filter, CleanupPrFilter::Closed);
        assert_eq!(parsed2.filter.min_idle_days, 30);
        assert!(!parsed2.filter.exclude_dirty);
    }

    #[test]
    fn cli_worktree_space_and_cleanup_end_to_end() {
        let git = |cwd: &Path, args: &[&str]| {
            let out = Command::new("git")
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };

        let root =
            std::env::temp_dir().join(format!("mk-cli-space-cleanup-unit-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let state = root.join("state");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();

        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("README.md"), vec![b'r'; 1_000]).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "initial"]);

        let wt_merged = root.join("wt-merged");
        let wt_closed = root.join("wt-closed");
        let wt_dirty = root.join("wt-dirty");
        for (wt, branch) in [
            (&wt_merged, "feat-merged"),
            (&wt_closed, "feat-closed"),
            (&wt_dirty, "feat-dirty"),
        ] {
            git(
                &repo,
                &["worktree", "add", "-b", branch, wt.to_str().unwrap()],
            );
        }

        // wt_closed에 미머지 커밋을 추가해 삭제 시 브랜치가 보존되는지 검증한다.
        fs::write(wt_closed.join("wip.txt"), "unmerged\n").unwrap();
        git(&wt_closed, &["add", "."]);
        git(&wt_closed, &["commit", "-m", "wip"]);

        // 빌드 아티팩트(target, node_modules, .next)를 생성한다.
        fs::create_dir_all(wt_merged.join("target/debug")).unwrap();
        fs::write(wt_merged.join("target/debug/app"), vec![b'a'; 12_000]).unwrap();
        fs::create_dir_all(wt_closed.join("node_modules/pkg")).unwrap();
        fs::write(
            wt_closed.join("node_modules/pkg/index.js"),
            vec![b'b'; 6_000],
        )
        .unwrap();
        fs::create_dir_all(wt_dirty.join(".next/cache")).unwrap();
        fs::write(wt_dirty.join(".next/cache/chunk"), vec![b'c'; 4_000]).unwrap();
        fs::write(wt_dirty.join("dirty.txt"), "uncommitted\n").unwrap();

        let now = now_unix_secs();
        record_worktree_metadata(
            &state,
            &wt_merged,
            Some(CleanupPrState::Merged),
            Some(11),
            Some(now.saturating_sub(10 * SECS_PER_DAY)),
        )
        .unwrap();
        record_worktree_metadata(
            &state,
            &wt_closed,
            Some(CleanupPrState::Closed),
            Some(12),
            Some(now.saturating_sub(32 * SECS_PER_DAY)),
        )
        .unwrap();
        record_worktree_metadata(
            &state,
            &wt_dirty,
            Some(CleanupPrState::Merged),
            Some(13),
            Some(now.saturating_sub(40 * SECS_PER_DAY)),
        )
        .unwrap();

        // 1. `krill worktree space --repo <repo> --json` 검증
        let space_args = ["--repo", repo.to_str().unwrap(), "--json"].map(OsString::from);
        let (_, space_report) = execute_space_with_state(&space_args, Some(&state)).unwrap();
        assert_eq!(space_report.worktrees.len(), 4);
        assert_eq!(space_report.worktrees[0].name, "wt-merged");
        assert_eq!(space_report.worktrees[0].artifact_bytes, 12_000);
        assert_eq!(space_report.total_artifact_bytes, 22_000);

        // `super::run` 진입점도 성공 종료 코드를 반환하는지 검증
        let top_space_args =
            ["space", "--repo", repo.to_str().unwrap(), "--json"].map(OsString::from);
        assert_eq!(super::super::run(&top_space_args), ExitCode::SUCCESS);

        // 앱 전체 후보 조회(`--pr any --idle-days 0 --include-dirty --dry-run`)와 `--worktree` 선택(OWE-744).
        let all_args = [
            "--repo",
            repo.to_str().unwrap(),
            "--pr",
            "any",
            "--idle-days",
            "0",
            "--include-dirty",
            "--dry-run",
            "--json",
        ]
        .map(OsString::from);
        let (_, all_out) = execute_cleanup_with_state(&all_args, Some(&state), false).unwrap();
        assert_eq!(all_out.candidates.len(), 3);
        let mut picked_args = all_args.to_vec();
        picked_args.extend(["--worktree", wt_merged.to_str().unwrap()].map(OsString::from));
        let (_, picked_out) =
            execute_cleanup_with_state(&picked_args, Some(&state), false).unwrap();
        assert_eq!(picked_out.candidates.len(), 1);
        assert_eq!(picked_out.candidates[0].name, "wt-merged");
        assert!(wt_merged.exists(), "dry-run must not remove");

        // OWE-758: 미추적(`??`) 파일이 남아 있는 동안은 안전 필터(`exclude_dirty: true`)가 삭제를 막아야 한다.
        let closed_args = [
            "--repo",
            repo.to_str().unwrap(),
            "--pr",
            "closed",
            "--idle-days",
            "30",
            "--json",
        ]
        .map(OsString::from);
        let (_, guarded_out) =
            execute_cleanup_with_state(&closed_args, Some(&state), false).unwrap();
        assert_eq!(guarded_out.summary.removed_count, 0);
        assert!(
            wt_closed.exists(),
            "untracked files must be protected by safety filter"
        );

        // 미추적 아티팩트 디렉터리를 정리한 뒤에는 안전 필터(`exclude_dirty: true`, `force: false`)에서 정상 삭제된다.
        fs::remove_dir_all(wt_closed.join("node_modules")).unwrap();
        fs::remove_dir_all(wt_merged.join("target")).unwrap();

        // 2. `krill worktree cleanup --repo <repo> --pr closed --idle-days 30` 실행:
        //    wt-closed만 삭제되고 미머지 브랜치 feat-closed는 보존된다.
        let (_, closed_out) =
            execute_cleanup_with_state(&closed_args, Some(&state), false).unwrap();
        assert!(!wt_closed.exists());
        assert!(wt_merged.exists());
        assert!(wt_dirty.exists());
        assert_eq!(closed_out.summary.removed_count, 1);
        assert_eq!(closed_out.summary.branches_preserved, 1);

        // 3. `krill worktree cleanup --repo <repo> --pr merged --idle-days 7` 실행:
        //    wt-dirty는 dirty 안전 필터로 제외되고, wt-merged만 삭제되며 feat-merged 브랜치도 삭제된다.
        let merged_args = [
            "--repo",
            repo.to_str().unwrap(),
            "--pr",
            "merged",
            "--idle-days",
            "7",
            "--json",
        ]
        .map(OsString::from);
        let (_, merged_out) =
            execute_cleanup_with_state(&merged_args, Some(&state), false).unwrap();
        assert!(!wt_merged.exists());
        assert!(
            wt_dirty.exists(),
            "dirty worktree must be excluded by default"
        );
        assert_eq!(merged_out.summary.removed_count, 1);
        assert_eq!(merged_out.summary.branches_deleted, 1);

        let branches_out = Command::new("git")
            .current_dir(&repo)
            .args(["branch", "--format=%(refname:short)"])
            .output()
            .unwrap();
        let branches: Vec<String> = String::from_utf8_lossy(&branches_out.stdout)
            .lines()
            .map(|s| s.trim().to_owned())
            .collect();
        assert!(!branches.iter().any(|b| b == "feat-merged"));
        assert!(branches.iter().any(|b| b == "feat-closed"));
        assert!(branches.iter().any(|b| b == "feat-dirty"));

        fs::remove_dir_all(&root).ok();
    }
}
