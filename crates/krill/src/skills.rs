//! Agent Skills Registry (`krill skills`), Context Handoff (`krill handoff`, MK-115),
//! Worktree References (`krill reference`, MK-116), and Local Ports Scanner (`krill ports`, MK-117).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use krill_proto::daemon;
use krill_proto::orchestration::{
    PortEntry, WorktreeStore, builtin_skills, extract_local_urls, install_skill,
};

pub fn run_skills(raw: &[OsString]) -> ExitCode {
    run_inner("skills", raw, dispatch_skills)
}

pub fn run_handoff(raw: &[OsString]) -> ExitCode {
    run_inner("handoff", raw, dispatch_handoff)
}

pub fn run_reference(raw: &[OsString]) -> ExitCode {
    run_inner("reference", raw, dispatch_reference)
}

pub fn run_ports(raw: &[OsString]) -> ExitCode {
    run_inner("ports", raw, dispatch_ports)
}

fn run_inner(label: &str, raw: &[OsString], f: fn(&[String]) -> io::Result<()>) -> ExitCode {
    let Ok(args) = raw
        .iter()
        .cloned()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("{label}: arguments must be UTF-8");
        return ExitCode::from(2);
    };
    match f(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("{label}: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("{label}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch_skills(args: &[String]) -> io::Result<()> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(bad_arg(
            "usage: krill skills list [--json] | krill skills get <name> [--full] [--json] | krill skills install <name>|--skill <name> [--agent|--target claude|codex|agy|antigravity|opencode|cursor|all] [--global|--local <dir>|--dir <dir>] [--json]",
        ));
    };
    match sub.as_str() {
        "list" | "ls" => {
            let json = rest.iter().any(|a| a == "--json");
            let skills = builtin_skills();
            if json {
                print_json(&skills)?;
            } else {
                println!("NAME\tDESCRIPTION");
                for s in skills {
                    println!("{}\t{}", s.name, s.description);
                }
            }
            Ok(())
        }
        "get" | "show" => {
            let Some((name, tail)) = rest.split_first() else {
                return Err(bad_arg("missing skill <name>"));
            };
            let mut json = false;
            for arg in tail {
                match arg.as_str() {
                    "--json" => json = true,
                    "--full" => {}
                    _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
                }
            }
            let Some(skill) = builtin_skills().iter().find(|s| s.name == name.trim()) else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("skill not found: {name}"),
                ));
            };
            if json {
                print_json(skill)?;
            } else {
                println!("{}", skill.content.trim_end());
            }
            Ok(())
        }
        "install" => {
            let mut name = None;
            let mut target = "all".to_owned();
            let mut custom_dir = None;
            let mut json = false;
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--skill" => {
                        if name.is_some() {
                            return Err(bad_arg("skill name specified more than once"));
                        }
                        name = Some(it.next().ok_or_else(|| bad_arg("missing --skill"))?);
                    }
                    "--target" | "--agent" => {
                        target.clone_from(it.next().ok_or_else(|| bad_arg("missing --target"))?);
                    }
                    "--dir" | "--local" => {
                        custom_dir = Some(PathBuf::from(
                            it.next().ok_or_else(|| bad_arg("missing --dir"))?,
                        ));
                    }
                    "--global" => {}
                    "--json" => json = true,
                    _ if !arg.starts_with('-') && name.is_none() => name = Some(arg),
                    _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
                }
            }
            let name = name.ok_or_else(|| bad_arg("missing skill <name> or --skill <name>"))?;
            if !matches!(
                target.as_str(),
                "claude" | "codex" | "opencode" | "cursor" | "agy" | "antigravity" | "all"
            ) {
                return Err(bad_arg(
                    "--agent/--target must be claude|codex|opencode|cursor|agy|antigravity|all",
                ));
            }
            let dirs = if let Some(dir) = custom_dir {
                vec![dir]
            } else {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or_else(|| io::Error::other("HOME is not set"))?;
                match target.as_str() {
                    "claude" => vec![home.join(".claude/skills")],
                    "codex" => vec![home.join(".codex/skills")],
                    "opencode" => vec![home.join(".config/opencode/skills")],
                    "cursor" => vec![home.join(".cursor/skills")],
                    "agy" | "antigravity" => vec![home.join(".gemini/config/skills")],
                    "all" => vec![
                        home.join(".claude/skills"),
                        home.join(".codex/skills"),
                        home.join(".config/opencode/skills"),
                        home.join(".cursor/skills"),
                        home.join(".gemini/config/skills"),
                    ],
                    _ => return Err(bad_arg("invalid skill target")),
                }
            };
            let mut installed = Vec::new();
            for d in dirs {
                let path = install_skill(name, &d)?;
                installed.push(path.to_string_lossy().into_owned());
            }
            if json {
                print_json(&serde_json::json!({
                    "skill": name,
                    "installed": installed,
                }))?;
            } else {
                for p in installed {
                    println!("installed\t{name}\t{p}");
                }
            }
            Ok(())
        }
        _ => Err(bad_arg(format!("unknown skills subcommand: {sub}"))),
    }
}

fn dispatch_handoff(args: &[String]) -> io::Result<()> {
    let mut from_session = None;
    let mut to_agent = "codex".to_owned();
    let mut to_session = None;
    let mut prompt = "Continue work from the previous agent session.".to_owned();
    let mut lines = 60_usize;
    let mut cwd = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --from"))?;
                from_session = Some(
                    raw.parse::<u64>()
                        .map_err(|_| bad_arg("invalid --from session id"))?,
                );
            }
            "--to-agent" | "--agent" => {
                to_agent.clone_from(it.next().ok_or_else(|| bad_arg("missing --to-agent"))?);
            }
            "--to-session" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --to-session"))?;
                to_session = Some(
                    raw.parse::<u64>()
                        .map_err(|_| bad_arg("invalid --to-session id"))?,
                );
            }
            "--prompt" => {
                prompt.clone_from(it.next().ok_or_else(|| bad_arg("missing --prompt"))?);
            }
            "--lines" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --lines"))?;
                lines = raw
                    .parse::<usize>()
                    .map_err(|_| bad_arg("invalid --lines"))?;
            }
            "--cwd" | "--worktree" => {
                cwd = Some(PathBuf::from(
                    it.next().ok_or_else(|| bad_arg("missing --cwd"))?,
                ));
            }
            "--json" => json = true,
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }
    let from_id = from_session.ok_or_else(|| {
        bad_arg("usage: krill handoff --from <session-id> [--to-agent <agent>] [--to-session <id>] [--prompt <text>] [--lines <N>] [--json]")
    })?;

    // 원본 세션을 못 읽으면 빈 맥락으로 새 에이전트를 띄우지 않는다.
    let captured = crate::session::capture_session_text(from_id)
        .map_err(|e| io::Error::other(format!("read session {from_id}: {e}")))?;
    let all_lines: Vec<&str> = captured
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    let start = all_lines.len().saturating_sub(lines);
    let context_tail = all_lines.get(start..).unwrap_or_default().join("\n");

    let work_cwd = match cwd {
        Some(c) => c,
        None => std::env::current_dir()?,
    };
    let wt_root = crate::set::resolve_worktree(&work_cwd);
    let wt_key = wt_root.to_string_lossy().into_owned();
    let checkpoint = daemon::state_dir()
        .and_then(|d| WorktreeStore::load(&d).ok())
        .map(|s| s.checkpoint(&wt_key))
        .unwrap_or_default();

    let newly_spawned = to_session.is_none();
    let target_session = match to_session {
        Some(sid) => Some(sid),
        None => Some(
            crate::session::spawn_agent_session(&to_agent, &wt_root).map_err(io::Error::other)?,
        ),
    };

    let packet = format!(
        "[MK HANDOFF from session #{from_id} -> {to_agent}] Worktree: {wt_key} | Status: {} | Note: {}\nPrompt:\n{prompt}\nRecent terminal context:\n{}",
        checkpoint.workspace_status.as_deref().unwrap_or("none"),
        checkpoint.comment.as_deref().unwrap_or("-"),
        if context_tail.is_empty() {
            "(empty)"
        } else {
            &context_tail
        }
    );

    let delivered = target_session.map_or(Ok(()), |sid| {
        let target = crate::remote::Target::establish(None).map_err(|e| e.to_string())?;
        if newly_spawned {
            crate::session::wait_for_spawned_agent_ready(
                &target,
                sid,
                crate::session::BOOT_TIMEOUT,
            )?;
        }
        crate::session::send_session_line(&target, sid, &packet)
    });

    if json {
        print_json(&serde_json::json!({
            "from_session": from_id,
            "to_agent": to_agent,
            "to_session": target_session,
            "worktree": wt_key,
            "prompt": prompt,
            "captured_lines": all_lines.len().saturating_sub(start),
            "packet": packet,
        }))?;
    } else {
        println!(
            "handoff\tfrom={from_id}\tto_agent={to_agent}\tto_session={}\tlines={}",
            target_session.map_or_else(|| "-".to_owned(), |n| n.to_string()),
            all_lines.len().saturating_sub(start)
        );
    }
    match delivered {
        Ok(()) => Ok(()),
        Err(e) => {
            // 새로 띄운 세션으로 전달이 안 되면 고아 에이전트 세션을 남기지 않는다(C6).
            // 방금 만든 세션이라 정리해도 사용자 작업 손실이 없다. 남의 세션(`--to-session`)은 절대 닫지 않는다.
            if newly_spawned && let Some(sid) = target_session {
                let closed = crate::session::close_session(sid).is_ok();
                return Err(handoff_delivery_error(&to_agent, &e, Some((sid, closed))));
            }
            Err(handoff_delivery_error(&to_agent, &e, None))
        }
    }
}

/// 전달 실패 오류에 세션 안내를 붙인다. 새로 띄운 세션이면 정리 결과까지 명시한다.
fn handoff_delivery_error(agent: &str, err: &str, spawned: Option<(u64, bool)>) -> io::Error {
    match spawned {
        Some((sid, true)) => io::Error::other(format!(
            "handoff to {agent}: {err} (spawned session {sid} closed)"
        )),
        Some((sid, false)) => io::Error::other(format!(
            "handoff to {agent}: {err} (spawned session {sid} kept; resend with krill send {sid} -- <text>)"
        )),
        None => io::Error::other(format!("handoff to {agent}: {err}")),
    }
}

fn dispatch_reference(args: &[String]) -> io::Result<()> {
    let state_dir = daemon::state_dir().ok_or_else(|| io::Error::other("HOME is not set"))?;
    cmd_reference(&state_dir, args)
}

fn cmd_reference(state_dir: &Path, args: &[String]) -> io::Result<()> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(bad_arg(
            "usage: krill reference list [--session <id>] [--worktree <path>] [--json] | krill reference add <url> [url...] [--session <id>] [--worktree <path>] [--json] | krill reference remove <url> [--session <id>] [--worktree <path>] [--json] | krill reference find [query] [--json]",
        ));
    };

    match sub.as_str() {
        "list" | "ls" => {
            let (wt_opt, json, urls) = parse_reference_args(rest)?;
            if !urls.is_empty() {
                return Err(bad_arg("reference list does not accept URLs"));
            }
            let key = resolve_wt_key(wt_opt)?;
            let store = WorktreeStore::load(state_dir)?;
            let refs = store.references.get(&key).cloned().unwrap_or_default();
            if json {
                print_json(&serde_json::json!({
                    "worktree": key,
                    "references": refs,
                }))?;
            } else {
                for r in refs {
                    println!("{key}\t{r}");
                }
            }
            Ok(())
        }
        "add" => {
            let (wt_opt, json, urls) = parse_reference_args(rest)?;
            if urls.is_empty() {
                return Err(bad_arg("missing <url>"));
            }
            let key = resolve_wt_key(wt_opt)?;
            let refs = WorktreeStore::mutate(state_dir, |s| {
                let mut refs = Vec::new();
                for url in &urls {
                    refs = s.add_reference(&key, url)?;
                }
                Ok(refs)
            })?;
            if json {
                print_json(&serde_json::json!({
                    "worktree": key,
                    "references": refs,
                }))?;
            } else {
                for url in urls {
                    println!("added\t{key}\t{url}");
                }
            }
            Ok(())
        }
        "remove" | "rm" => {
            let (wt_opt, json, urls) = parse_reference_args(rest)?;
            let Some(url) = urls.first() else {
                return Err(bad_arg("missing <url>"));
            };
            if urls.len() != 1 {
                return Err(bad_arg("reference remove accepts one URL"));
            }
            let key = resolve_wt_key(wt_opt)?;
            let refs = WorktreeStore::mutate(state_dir, |s| Ok(s.remove_reference(&key, url)))?;
            if json {
                print_json(&serde_json::json!({
                    "worktree": key,
                    "references": refs,
                }))?;
            } else {
                println!("removed\t{key}\t{url}");
            }
            Ok(())
        }
        "find" | "search" => {
            let mut query = String::new();
            let mut json = false;
            for arg in rest {
                if arg == "--json" {
                    json = true;
                } else if query.is_empty() {
                    query.clone_from(arg);
                } else {
                    return Err(bad_arg(format!("unexpected argument {arg}")));
                }
            }
            let store = WorktreeStore::load(state_dir)?;
            let found = store.find_references(&query);
            if json {
                let items: Vec<_> = found
                    .into_iter()
                    .map(|(w, u)| serde_json::json!({ "worktree": w, "url": u }))
                    .collect();
                print_json(&items)?;
            } else {
                for (w, u) in found {
                    println!("{w}\t{u}");
                }
            }
            Ok(())
        }
        _ => Err(bad_arg(format!("unknown reference subcommand: {sub}"))),
    }
}

fn dispatch_ports(args: &[String]) -> io::Result<()> {
    let entries = collect_ports(args)?;
    if args.iter().any(|arg| arg == "--json") {
        print_json(&entries)?;
    } else {
        println!("SESSION\tPORT\tREACHABLE\tURL");
        for e in entries {
            println!(
                "{}\t{}\t{}\t{}",
                e.session_id,
                e.port,
                if e.reachable { "open" } else { "closed" },
                e.url
            );
        }
    }
    Ok(())
}

fn collect_ports(args: &[String]) -> io::Result<Vec<PortEntry>> {
    collect_ports_with(
        args,
        crate::session::capture_session_text,
        crate::session::list_session_ids,
    )
}

fn collect_ports_with(
    args: &[String],
    capture: impl Fn(u64) -> std::result::Result<String, String>,
    list: impl Fn() -> std::result::Result<Vec<u64>, String>,
) -> io::Result<Vec<PortEntry>> {
    let mut session_filter = None;
    let mut probe = true;
    let mut sample_text = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--session" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --session"))?;
                session_filter = Some(
                    raw.parse::<u64>()
                        .map_err(|_| bad_arg("invalid --session id"))?,
                );
            }
            "--no-probe" => probe = false,
            "--text" => {
                sample_text = Some(it.next().ok_or_else(|| bad_arg("missing --text"))?);
            }
            "--json" => {}
            _ => return Err(bad_arg(format!("unexpected argument {arg}"))),
        }
    }

    let mut entries: Vec<PortEntry> = Vec::new();
    if let Some(text) = sample_text {
        entries.extend(extract_local_urls(session_filter.unwrap_or(0), text, probe));
    } else if let Some(sid) = session_filter {
        // 세션을 지정했는데 못 읽으면 빈 표+성공으로 넘기지 않는다(C2).
        // `reference`·`set`과 같은 NotFound 계열 오류(종료 1)로 끝낸다.
        match capture(sid) {
            Ok(text) => entries.extend(extract_local_urls(sid, &text, probe)),
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("session {sid} not found: {e}"),
                ));
            }
        }
    } else if let Ok(sessions) = list() {
        for sid in sessions {
            if let Ok(text) = capture(sid) {
                entries.extend(extract_local_urls(sid, &text, probe));
            }
        }
    }

    Ok(entries)
}

fn parse_reference_args(args: &[String]) -> io::Result<(Option<PathBuf>, bool, Vec<String>)> {
    let mut wt = None;
    let mut session = None;
    let mut json = false;
    let mut urls = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--worktree" | "--cwd" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --worktree"))?;
                if !matches!(raw.as_str(), "" | "." | "active") {
                    wt = Some(PathBuf::from(raw));
                }
            }
            "--json" => json = true,
            "--session" => {
                let raw = it.next().ok_or_else(|| bad_arg("missing --session"))?;
                let sid = raw
                    .strip_prefix('s')
                    .unwrap_or(raw)
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| bad_arg("invalid --session id"))?;
                session = Some(sid);
            }
            _ if arg.starts_with('-') => {
                return Err(bad_arg(format!("unexpected argument {arg}")));
            }
            _ => urls.push(arg.clone()),
        }
    }
    if wt.is_none()
        && let Some(sid) = session
    {
        wt = Some(crate::session::resolve_session_cwd(sid)?);
    }
    Ok((wt, json, urls))
}

fn resolve_wt_key(wt_opt: Option<PathBuf>) -> io::Result<String> {
    let dir = match wt_opt {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    Ok(crate::set::resolve_worktree(&dir)
        .to_string_lossy()
        .into_owned())
}

fn print_json<T: serde::Serialize>(val: &T) -> io::Result<()> {
    let text = serde_json::to_string_pretty(val).map_err(io::Error::other)?;
    println!("{text}");
    Ok(())
}

fn bad_arg(msg: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn install_skills_with_positional_and_option_names() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-cli-skills-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        for (name, name_flag, target_flag, dir_flag, target) in [
            ("minikrill", None, "--target", "--dir", "claude"),
            (
                "orchestration",
                Some("--skill"),
                "--agent",
                "--local",
                "opencode",
            ),
            (
                "worktree-checkpoints",
                Some("--skill"),
                "--agent",
                "--local",
                "all",
            ),
            ("computer-use", None, "--agent", "--dir", "agy"),
        ] {
            let mut raw = args(&["install"]);
            if let Some(flag) = name_flag {
                raw.push(flag.to_owned());
            }
            raw.extend(args(&[name, target_flag, target, dir_flag]));
            raw.push(dir.to_string_lossy().into_owned());
            raw.extend(args(&["--global", "--json"]));
            dispatch_skills(&raw)?;
            let skill = builtin_skills()
                .iter()
                .find(|skill| skill.name == name)
                .unwrap();
            // Orca와 이름이 겹치는 스킬은 `mk-<name>`으로 설치된다.
            let (folder, content) = if matches!(name, "orchestration" | "computer-use") {
                (
                    format!("mk-{name}"),
                    skill.content.replacen(
                        &format!("name: {name}\n"),
                        &format!("name: mk-{name}\n"),
                        1,
                    ),
                )
            } else {
                (name.to_owned(), skill.content.to_owned())
            };
            assert_eq!(
                std::fs::read_to_string(dir.join(folder).join("SKILL.md"))?,
                format!("{content}\n<!-- minikrill-managed-skill -->\n")
            );
        }
        dispatch_skills(&args(&["get", "minikrill", "--full"]))?;
        dispatch_skills(&args(&["get", "minikrill", "--full", "--json"]))?;
        dispatch_skills(&args(&["get", "computer-use", "--full"]))?;
        dispatch_skills(&args(&["get", "computer-use", "--full", "--json"]))?;
        assert!(dispatch_skills(&args(&["install", "--skill"])).is_err());
        assert!(dispatch_skills(&args(&["install", "minikrill", "--agent", "unknown"])).is_err());
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn reference_commands_round_trip_in_isolated_state() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-cli-reference-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let worktree = dir.join("worktree");
        std::fs::create_dir_all(&worktree)?;
        let wt_arg = worktree.to_string_lossy().into_owned();
        let key = resolve_wt_key(Some(worktree))?;
        let state = dir.join("state");
        cmd_reference(
            &state,
            &args(&[
                "add",
                "https://example.com/one",
                "OWE-626",
                "https://example.com/two",
                "--worktree",
                &wt_arg,
                "--session",
                "1",
                "--json",
            ]),
        )?;
        cmd_reference(
            &state,
            &args(&[
                "add",
                "--session",
                "1",
                "https://example.com/one",
                "--worktree",
                &wt_arg,
            ]),
        )?;
        let store = WorktreeStore::load(&state)?;
        assert_eq!(store.references.get(&key).map(Vec::len), Some(3));
        assert_eq!(
            store.find_references("owe-626"),
            vec![(key.clone(), "OWE-626".to_owned())]
        );
        cmd_reference(
            &state,
            &args(&["list", "--worktree", &wt_arg, "--session", "1", "--json"]),
        )?;
        cmd_reference(&state, &args(&["find", "owe-626", "--json"]))?;
        cmd_reference(
            &state,
            &args(&[
                "remove",
                "--session",
                "1",
                "OWE-626",
                "--worktree",
                &wt_arg,
                "--json",
            ]),
        )?;
        assert!(
            WorktreeStore::load(&state)?
                .find_references("OWE-626")
                .is_empty()
        );
        for url in ["https://example.com/one", "https://example.com/two"] {
            cmd_reference(
                &state,
                &args(&["rm", url, "--worktree", &wt_arg, "--session", "1"]),
            )?;
        }
        assert!(!WorktreeStore::load(&state)?.references.contains_key(&key));
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn reference_add_rejects_invalid_batch_without_saving() -> io::Result<()> {
        let dir =
            std::env::temp_dir().join(format!("mk-cli-reference-invalid-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        assert!(cmd_reference(&dir, &args(&["add", "https://example.com/valid", ""])).is_err());
        assert!(WorktreeStore::load(&dir)?.references.is_empty());
        assert!(parse_reference_args(&args(&["--session"])).is_err());
        assert!(parse_reference_args(&args(&["--session", "invalid"])).is_err());
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn ports_session_filter_reports_missing_session() {
        let raw = args(&["--session", "999", "--no-probe"]);
        let err = collect_ports_with(&raw, |_| Err("no such session".to_owned()), || Ok(vec![]))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("999"), "{err}");
        // 전체 스캔 경로는 개별 실패를 건너뛴다(기존 동작 유지).
        let scan = args(&["--no-probe"]);
        let entries = collect_ports_with(
            &scan,
            |sid| {
                if sid == 1 {
                    Ok("ready http://localhost:3000/x".to_owned())
                } else {
                    Err("gone".to_owned())
                }
            },
            || Ok(vec![1, 2]),
        )
        .expect("scan keeps working sessions");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, 1);
    }

    #[test]
    fn handoff_delivery_error_names_spawned_session() {
        let closed = handoff_delivery_error("codex", "boom", Some((7, true)));
        assert!(closed.to_string().contains('7'), "{closed}");
        assert!(closed.to_string().contains("closed"), "{closed}");
        let kept = handoff_delivery_error("codex", "boom", Some((7, false)));
        assert!(kept.to_string().contains("krill send 7"), "{kept}");
        let foreign = handoff_delivery_error("codex", "boom", None);
        assert!(!foreign.to_string().contains("session"), "{foreign}");
    }

    #[test]
    fn ports_text_extracts_urls_without_daemon_or_probe() -> io::Result<()> {
        let raw = args(&[
            "--text",
            "Ready: http://localhost:3000/api http://127.0.0.1:5173 http://localhost:3000/api",
            "--session",
            "42",
            "--no-probe",
            "--json",
        ]);
        let entries = collect_ports(&raw)?;
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.session_id == 42));
        assert!(entries.iter().any(|entry| entry.port == 3000));
        assert!(entries.iter().any(|entry| entry.port == 5173));
        dispatch_ports(&raw)?;
        assert!(collect_ports(&args(&["--text"])).is_err());
        Ok(())
    }
}
