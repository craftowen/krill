//! `krill search <query> [--agent <id>] [--path <worktree>] [--limit <n>] [--json]`(MK-181):
//! krilld의 AI Vault 멀티 에이전트 대화 전문 검색을 호출해 결과를 출력한다.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use krill_proto::agent_history::{
    Action, FEAT_AGENT_HISTORY, FEAT_AI_VAULT, MAX_ENTRIES, Response, SearchQuery, SearchSnapshot,
    VaultAgent,
};
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use serde::Serialize;

const USAGE: &str = "usage: krill search <query> [--agent <claude|codex|agy|opencode>] [--path <worktree>] [--limit <n>] [--json]";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedArgs {
    query: SearchQuery,
    json: bool,
}

#[derive(Debug, Serialize)]
struct JsonReport {
    hits: Vec<JsonHit>,
    truncated: bool,
    skipped: bool,
}

#[derive(Debug, Serialize)]
struct JsonHit {
    agent: &'static str,
    id: String,
    cwd: String,
    modified: u64,
    title: String,
    summary: String,
    snippet: String,
    role: String,
    score: u32,
    resume_command: String,
}

pub fn run(raw: &[OsString]) -> ExitCode {
    // 검색어 `help`와 구분하기 위해 `-h`/`--help`만 도움말로 본다. 베어 `help`는 검색어다.
    if matches!(raw.first().and_then(|a| a.to_str()), Some("-h" | "--help")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let Ok(args) = raw
        .iter()
        .cloned()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("arguments must be UTF-8");
        return ExitCode::from(2);
    };
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match execute(&parsed.query) {
        Ok(snapshot) => {
            if parsed.json {
                println!("{}", format_json(&snapshot));
            } else {
                print!("{}", format_text(&snapshot));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("search: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    if args.is_empty() {
        return Err(USAGE.to_owned());
    }
    let mut query_parts = Vec::new();
    let mut agents = Vec::new();
    let mut paths = Vec::new();
    let mut limit: u16 = 20;
    let mut json = false;
    let mut idx = 0usize;

    while let Some(arg) = args.get(idx) {
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.to_owned()),
            "--json" => {
                json = true;
                idx = idx.saturating_add(1);
            }
            "--agent" | "-a" => {
                let val = args
                    .get(idx.saturating_add(1))
                    .ok_or_else(|| format!("missing value for {arg}\n{USAGE}"))?;
                for part in val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let parsed = VaultAgent::parse(part).ok_or_else(|| {
                        format!("unknown agent '{part}' (expected claude, codex, agy, or opencode)")
                    })?;
                    if !agents.contains(&parsed) {
                        agents.push(parsed);
                    }
                }
                idx = idx.saturating_add(2);
            }
            "--path" | "-p" | "--cwd" => {
                let val = args
                    .get(idx.saturating_add(1))
                    .ok_or_else(|| format!("missing value for {arg}\n{USAGE}"))?;
                let resolved = resolve_filter_path(val);
                if !paths.contains(&resolved) {
                    paths.push(resolved);
                }
                idx = idx.saturating_add(2);
            }
            "--limit" | "-n" => {
                let val = args
                    .get(idx.saturating_add(1))
                    .ok_or_else(|| format!("missing value for {arg}\n{USAGE}"))?;
                let parsed: u16 = val
                    .parse()
                    .ok()
                    .filter(|&n| n >= 1 && usize::from(n) <= MAX_ENTRIES)
                    .ok_or_else(|| {
                        format!("invalid --limit '{val}' (must be 1..={MAX_ENTRIES})")
                    })?;
                limit = parsed;
                idx = idx.saturating_add(2);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'\n{USAGE}"));
            }
            word => {
                query_parts.push(word.to_owned());
                idx = idx.saturating_add(1);
            }
        }
    }

    let query_text = query_parts.join(" ");
    if query_text.trim().is_empty() && agents.is_empty() && paths.is_empty() {
        return Err(USAGE.to_owned());
    }

    Ok(ParsedArgs {
        query: SearchQuery {
            query: query_text,
            agents,
            paths,
            limit,
        },
        json,
    })
}

fn resolve_filter_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        return std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
            .to_string();
    }
    if let Ok(cwd) = std::env::current_dir() {
        let joined = cwd.join(p);
        return std::fs::canonicalize(&joined)
            .unwrap_or(joined)
            .display()
            .to_string();
    }
    trimmed.to_owned()
}

fn execute(query: &SearchQuery) -> Result<SearchSnapshot, String> {
    let paths = Paths::dev().map_err(|e| e.to_string())?;
    let mut hello = daemon::client_hello(ClientRole::Cli, "krill search");
    let required = FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_AI_VAULT;
    hello.features |= required;
    let mut conn = daemon::connect(&paths, &hello).map_err(|e| e.to_string())?;
    if conn.negotiated.features & required != required {
        return Err("krilld does not support AI Vault search; update the daemon".to_owned());
    }
    let timeout = Some(Duration::from_secs(15));
    conn.stream
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .set_write_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .write_all(
            &Request::AgentHistory(Action::Search(query.clone()))
                .encode()
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or("krilld disconnected")?;
    match Reply::decode(kind, &body).map_err(|e| e.to_string())? {
        Reply::AgentHistory(Response::Searched(snapshot)) => Ok(snapshot),
        Reply::Failed(e) => Err(e.to_string()),
        _ => Err("unexpected daemon reply".to_owned()),
    }
}

fn format_json(snapshot: &SearchSnapshot) -> String {
    let report = JsonReport {
        hits: snapshot
            .hits
            .iter()
            .map(|h| JsonHit {
                agent: h.agent.profile(),
                id: h.id.clone(),
                cwd: h.cwd.clone(),
                modified: h.modified,
                title: h.title.clone(),
                summary: h.summary.clone(),
                snippet: h.snippet.clone(),
                role: h.role.clone(),
                score: h.score,
                resume_command: h.agent.resume_command(&h.id),
            })
            .collect(),
        truncated: snapshot.truncated,
        skipped: snapshot.skipped,
    };
    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_owned())
}

fn format_text(snapshot: &SearchSnapshot) -> String {
    if snapshot.hits.is_empty() {
        return "no matching conversations found\n".to_owned();
    }
    let mut lines = Vec::new();
    for hit in &snapshot.hits {
        let agent = hit.agent.profile();
        let title = if hit.title.is_empty() {
            hit.id.as_str()
        } else {
            hit.title.as_str()
        };
        lines.push(format!("[{agent}] {title} ({})\n", hit.id));
        lines.push(format!("  cwd: {}\n", hit.cwd));
        if !hit.snippet.is_empty() {
            let role_prefix = if hit.role.is_empty() {
                String::new()
            } else {
                format!("{}: ", hit.role)
            };
            lines.push(format!("  match: {role_prefix}{}\n", hit.snippet));
        }
        lines.push(format!("  resume: {}\n", hit.agent.resume_command(&hit.id)));
    }
    if snapshot.truncated {
        lines.push("(results truncated)\n".to_owned());
    }
    lines.join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::agent_history::VaultHit;

    fn vec_str(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn help_returns_zero() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("-h")]), ExitCode::SUCCESS);
        // 베어 `help`는 검색어다. 빈 저장소에서는 결과가 없어도 SUCCESS다.
        assert_eq!(run(&[OsString::from("help")]), ExitCode::SUCCESS);
    }

    #[test]
    fn parse_args_supports_query_agent_path_limit_and_json() {
        let parsed = parse_args(&vec_str(&[
            "oauth",
            "refresh",
            "--agent",
            "claude,codex",
            "--path",
            "/tmp/worktree",
            "--limit",
            "15",
            "--json",
        ]))
        .unwrap();
        assert_eq!(parsed.query.query, "oauth refresh");
        assert_eq!(
            parsed.query.agents,
            vec![VaultAgent::Claude, VaultAgent::Codex]
        );
        assert_eq!(parsed.query.paths.len(), 1);
        assert_eq!(parsed.query.limit, 15);
        assert!(parsed.json);
    }

    #[test]
    fn parse_args_rejects_invalid_agent_or_limit() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&vec_str(&["--agent", "unknown", "bug"])).is_err());
        assert!(parse_args(&vec_str(&["--limit", "0", "bug"])).is_err());
        assert!(parse_args(&vec_str(&["--limit", "999", "bug"])).is_err());
    }

    #[test]
    fn format_text_and_json_include_snippet_and_resume_command() {
        let snap = SearchSnapshot {
            hits: vec![VaultHit {
                agent: VaultAgent::Agy,
                id: "33333333-2222-4333-8444-555555555555".into(),
                cwd: "/tmp/repo".into(),
                modified: 100,
                title: "Vault Panel UI".into(),
                summary: "Implemented scope filters".into(),
                snippet: "Added VaultScope pills".into(),
                role: "assistant".into(),
                score: 145,
            }],
            truncated: false,
            skipped: false,
        };
        let text = format_text(&snap);
        assert!(text.contains("[agy] Vault Panel UI"));
        assert!(text.contains("Added VaultScope pills"));
        assert!(text.contains("agy --conversation 33333333-2222-4333-8444-555555555555"));

        let json = format_json(&snap);
        assert!(json.contains("\"agent\": \"agy\""));
        assert!(json.contains(
            "\"resume_command\": \"agy --conversation 33333333-2222-4333-8444-555555555555\""
        ));
    }
}
