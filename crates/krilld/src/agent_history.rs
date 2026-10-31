//! Claude·Codex의 로컬 transcript 메타데이터를 요청 시에만 읽는다(MK-082).
//! 읽기 전용, 별도 프로세스·감시·타이머·캐시 없음. 심링크·subagent 기록은 따라가지 않는다.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use krill_proto::agent_history::{
    Agent, Entry, MAX_ENTRIES, MAX_TEXT, Snapshot, is_claude_mem_cwd, is_claude_mem_project_dir,
};
use serde_json::Value;

pub(crate) mod chat;
mod opencode_sqlite;
pub(crate) mod vault;

/// 탐색/IO 예산. 실제 성능 측정값이 아닌 구현 상한이다.
const MAX_NODES: usize = 20_000;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const FILE_BYTES: usize = 128 * 1024;
const SCAN_TIME: Duration = Duration::from_secs(2);

pub(crate) fn scan(state_dir: Option<&Path>) -> Snapshot {
    let home = krill_proto::os::home_dir();
    let root = |var, default| {
        std::env::var_os(var)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(default)))
    };
    let claude_roots: Vec<PathBuf> = root("CLAUDE_CONFIG_DIR", ".claude").into_iter().collect();
    let codex_roots: Vec<PathBuf> = root("CODEX_HOME", ".codex").into_iter().collect();

    let accounts = crate::usage::accounts::load_file(
        home.as_deref().unwrap_or_else(|| Path::new("")),
        state_dir,
    );
    scan_profiles(claude_roots, codex_roots, &accounts)
}

fn scan_profiles(
    mut claude_roots: Vec<PathBuf>,
    mut codex_roots: Vec<PathBuf>,
    accounts: &crate::usage::accounts::AccountsFile,
) -> Snapshot {
    for account in &accounts.accounts {
        let dir = account.config_dir();
        if !dir.is_absolute() {
            continue;
        }
        let path = dir.to_path_buf();
        if account.provider.eq_ignore_ascii_case("claude") {
            if !claude_roots.contains(&path) {
                claude_roots.push(path);
            }
        } else if account.provider.eq_ignore_ascii_case("codex") && !codex_roots.contains(&path) {
            codex_roots.push(path);
        }
    }

    scan_roots(&claude_roots, &codex_roots)
}

/// K-13: 비활성 계정의 대화를 재개할 때 활성 계정 프로필이 아니라 이 대화가 실제로
/// 들어 있는 프로필 디렉터리를 찾는다. 못 찾으면 `None`(호출자는 기존 기본 동작 유지).
/// - Claude: `<프로필>/projects/*/<sessionId>.jsonl` 존재 여부.
/// - Codex: `sessions` 안 `rollout-*.jsonl` 첫 수 KB에서 `session_meta` id 일치 여부(상한 있음).
pub(crate) fn profile_dir_for_entry(
    accounts: &crate::usage::accounts::AccountsFile,
    entry: &Entry,
) -> Option<PathBuf> {
    for account in &accounts.accounts {
        let dir = account.config_dir();
        if !dir.is_absolute() {
            continue;
        }
        let claude = account.provider.eq_ignore_ascii_case("claude");
        let codex = account.provider.eq_ignore_ascii_case("codex");
        if entry.agent == Agent::Claude && !claude {
            continue;
        }
        if entry.agent == Agent::Codex && !codex {
            continue;
        }
        if profile_contains_entry(dir, entry) {
            return Some(dir.to_path_buf());
        }
    }
    None
}

pub(crate) fn profile_dir_for_entry_in(
    accounts: &crate::usage::accounts::AccountsFile,
    entry: &Entry,
    host: &Path,
) -> Option<PathBuf> {
    // 기본 프로필 대화는 accounts.toml에 등록되지 않아도 스캔된다.
    // 활성 등록 계정의 env를 그대로 두면 그 기본 대화를 엉뚱한 프로필에서 재개한다.
    if profile_contains_entry(host, entry) {
        Some(host.to_path_buf())
    } else {
        profile_dir_for_entry(accounts, entry)
    }
}

fn profile_contains_entry(dir: &Path, entry: &Entry) -> bool {
    if entry.agent == Agent::Codex {
        return transcript_holds_id(&dir.join("sessions"), &entry.id);
    }
    let Ok(listing) = fs::read_dir(dir.join("projects")) else {
        return false;
    };
    listing.take(MAX_NODES).flatten().any(|group| {
        group.file_type().is_ok_and(|kind| kind.is_dir())
            && group.path().join(format!("{}.jsonl", entry.id)).is_file()
    })
}

/// Codex `rollout-*.jsonl`들에서 세션 id를 찾는다. 재개 요청 한 번당 상한(파일 256개·앞 4KiB) 안에서만 읽는다.
fn transcript_holds_id(sessions: &Path, id: &str) -> bool {
    let Ok(listing) = fs::read_dir(sessions) else {
        return false;
    };
    let mut checked = 0usize;
    let mut nodes = 0usize;
    let start = Instant::now();
    let mut stack: Vec<PathBuf> = listing
        .take(MAX_NODES)
        .filter_map(|item| item.ok().map(|i| i.path()))
        .collect();
    while let Some(path) = stack.pop() {
        if checked >= 256 || nodes >= MAX_NODES || start.elapsed() >= SCAN_TIME {
            return false;
        }
        nodes += 1;
        let kind = fs::symlink_metadata(&path).ok().map(|m| m.file_type());
        if kind.is_some_and(|k| k.is_symlink()) {
            continue;
        }
        if kind.is_some_and(|k| k.is_dir()) {
            if let Ok(inner) = fs::read_dir(&path) {
                stack.extend(
                    inner
                        .take(MAX_NODES.saturating_sub(nodes + stack.len()))
                        .filter_map(|item| item.ok().map(|i| i.path())),
                );
            }
            continue;
        }
        if !kind.is_some_and(|k| k.is_file()) {
            continue;
        }
        if path.extension().is_some_and(|s| s != "jsonl") {
            continue;
        }
        if path
            .file_name()
            .is_none_or(|n| !n.to_string_lossy().starts_with("rollout-"))
        {
            continue;
        }
        checked += 1;
        let head = (|| -> io::Result<Vec<u8>> {
            let file = File::open(&path)?;
            let mut head = Vec::new();
            file.take(4096).read_to_end(&mut head)?;
            Ok(head)
        })();
        if head.is_ok_and(|bytes| {
            bytes
                .split(|b| *b == b'\n')
                .any(|line| metadata_holds_id(line, id))
        }) {
            return true;
        }
    }
    false
}

/// 한 줄 안에 `"type":"session_meta"`와 `"id":"<id>"`가 모두 들어 있는지 본다.
fn metadata_holds_id(line: &[u8], id: &str) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("session_meta")
        && value
            .get("payload")
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            == Some(id)
}

pub(crate) fn valid(entry: &Entry) -> bool {
    let mut parts = entry.id.split('-');
    let uuid = [8, 4, 4, 4, 12].into_iter().all(|len| {
        parts
            .next()
            .is_some_and(|p| p.len() == len && p.bytes().all(|c| c.is_ascii_hexdigit()))
    }) && parts.next().is_none();
    uuid && !entry.cwd.is_empty()
        && entry.cwd.len() <= MAX_TEXT
        && Path::new(&entry.cwd).is_absolute()
        && !entry.cwd.contains('\0')
}

#[cfg(test)]
#[cfg(unix)]
fn scan_in(claude: Option<&Path>, codex: Option<&Path>) -> Snapshot {
    let claude_roots: Vec<PathBuf> = claude.into_iter().map(Path::to_path_buf).collect();
    let codex_roots: Vec<PathBuf> = codex.into_iter().map(Path::to_path_buf).collect();
    scan_roots(&claude_roots, &codex_roots)
}

fn scan_roots(claude_roots: &[PathBuf], codex_roots: &[PathBuf]) -> Snapshot {
    let mut scan = Scan {
        result: Snapshot {
            entries: Vec::new(),
            truncated: false,
            skipped: false,
        },
        entries: BTreeMap::new(),
        nodes: 0,
        bytes: 0,
        start: Instant::now(),
    };
    for root in claude_roots {
        scan.walk(&root.join("projects"), Agent::Claude, 2);
    }
    for root in codex_roots {
        scan.walk(&root.join("sessions"), Agent::Codex, 4);
    }
    let mut entries: Vec<_> = scan.entries.into_values().collect();
    entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    scan.result.truncated |= entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    scan.result.entries = entries;
    scan.result
}

struct Scan {
    result: Snapshot,
    entries: BTreeMap<(Agent, String), Entry>,
    nodes: usize,
    bytes: usize,
    start: Instant,
}

impl Scan {
    fn exhausted(&mut self) -> bool {
        let exhausted =
            self.nodes >= MAX_NODES || self.bytes >= MAX_BYTES || self.start.elapsed() >= SCAN_TIME;
        self.result.truncated |= exhausted;
        exhausted
    }

    fn walk(&mut self, path: &Path, agent: Agent, depth: usize) {
        if depth == 0 || self.exhausted() {
            return;
        }
        let dir = match fs::read_dir(path) {
            Ok(d) => d,
            Err(e) => {
                self.result.skipped |= e.kind() != io::ErrorKind::NotFound;
                return;
            }
        };
        // 탐색 예산 안에서 모든 후보를 읽고 최신순으로 고른다.
        for item in dir {
            if self.exhausted() {
                break;
            }
            self.nodes = self.nodes.saturating_add(1);
            let Ok(item) = item else {
                self.result.skipped = true;
                continue;
            };
            let Ok(kind) = item.file_type() else {
                self.result.skipped = true;
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let name = item.file_name();
            let name_str = name.to_string_lossy();
            let path = item.path();
            if kind.is_dir() {
                if agent == Agent::Claude
                    && (name_str == "subagents" || is_claude_mem_project_dir(&name_str))
                {
                    continue;
                }
                self.walk(&path, agent, depth.saturating_sub(1));
            } else if kind.is_file() && path.extension().is_some_and(|s| s == "jsonl") {
                if agent == Agent::Codex && !name_str.starts_with("rollout-") {
                    continue;
                }
                self.read(&path, agent);
            }
        }
    }

    fn read(&mut self, path: &Path, agent: Agent) {
        enum ReadOutcome {
            Found(Entry),
            Excluded,
            Missing,
        }
        let result = (|| -> io::Result<ReadOutcome> {
            let file = File::open(path)?;
            let modified = file
                .metadata()?
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let limit = u64::try_from(FILE_BYTES.min(MAX_BYTES.saturating_sub(self.bytes)))
                .unwrap_or(u64::MAX);
            let mut reader = BufReader::with_capacity(4096, file.take(limit));
            let mut line = Vec::new();
            // 메타데이터를 찾으면 즉시 멈춘다. 큰 transcript의 나머지 본문은 읽지 않는다.
            let result = loop {
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => break Ok(ReadOutcome::Missing),
                    Ok(_) => {
                        match parse_outcome(&line, agent, modified) {
                            Some(Ok(entry)) => break Ok(ReadOutcome::Found(entry)),
                            Some(Err(())) => break Ok(ReadOutcome::Excluded),
                            None => {}
                        }
                        line.clear();
                    }
                    Err(e) => break Err(e),
                }
            };
            let read = limit.saturating_sub(reader.get_ref().limit());
            self.bytes = self
                .bytes
                .saturating_add(usize::try_from(read).unwrap_or(usize::MAX));
            result
        })();
        match result {
            Ok(ReadOutcome::Found(entry)) => {
                let key = (entry.agent, entry.id.clone());
                if self
                    .entries
                    .get(&key)
                    .is_none_or(|old| old.modified < entry.modified)
                {
                    self.entries.insert(key, entry);
                }
                // 목록 상한만큼만 보관하고 오래된 후보는 즉시 버린다.
                if self.entries.len() > MAX_ENTRIES {
                    self.result.truncated = true;
                    let oldest = self
                        .entries
                        .iter()
                        .min_by_key(|(_, e)| e.modified)
                        .map(|(k, _)| k.clone());
                    if let Some(k) = oldest {
                        self.entries.remove(&k);
                    }
                }
            }
            Ok(ReadOutcome::Excluded) => {}
            _ => self.result.skipped = true,
        }
    }
}

#[cfg(all(test, unix))]
fn parse(bytes: &[u8], agent: Agent, modified: u64) -> Option<Entry> {
    parse_outcome(bytes, agent, modified).and_then(Result::ok)
}

fn parse_outcome(bytes: &[u8], agent: Agent, modified: u64) -> Option<Result<Entry, ()>> {
    // 마지막 잘린 줄과 손상된 줄은 건너뛴다. 전체 transcript를 읽거나 메시지를 보관하지 않는다.
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let (id, cwd) = match agent {
            Agent::Claude if value.get("isSidechain").and_then(Value::as_bool) != Some(true) => (
                value.get("sessionId").and_then(Value::as_str),
                value.get("cwd").and_then(Value::as_str),
            ),
            Agent::Codex if value.get("type").and_then(Value::as_str) == Some("session_meta") => {
                let payload = value.get("payload")?;
                // Codex subagent transcript를 사용자가 실행한 대화로 표시하지 않는다.
                if payload
                    .get("source")
                    .and_then(Value::as_object)
                    .is_some_and(|s| s.contains_key("subagent"))
                {
                    continue;
                }
                (
                    payload.get("id").and_then(Value::as_str),
                    payload.get("cwd").and_then(Value::as_str),
                )
            }
            _ => continue,
        };
        if let (Some(id), Some(cwd)) = (id, cwd) {
            if is_claude_mem_cwd(cwd) {
                return Some(Err(()));
            }
            let entry = Entry {
                agent,
                id: id.to_owned(),
                cwd: cwd.to_owned(),
                modified,
            };
            if valid(&entry) {
                return Some(Ok(entry));
            }
        }
    }
    None
}

#[cfg(test)]
mod multi_account_tests {
    use super::*;

    #[test]
    fn scan_roots_scans_registered_account_dirs_and_deduplicates_by_session_id() {
        let root = std::env::temp_dir().join(format!("mk082-multi-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();

        let claude_a = root.join("claude-a/projects/p");
        let claude_b = root.join("claude-b/projects/p");
        let codex_a = root.join("codex-a/sessions/2026/10/16");
        fs::create_dir_all(&claude_a).unwrap();
        fs::create_dir_all(&claude_b).unwrap();
        fs::create_dir_all(&codex_a).unwrap();

        let id_shared = "11111111-2222-4333-8444-555555555555";
        let id_second = "22222222-2222-4333-8444-555555555555";
        let id_codex = "33333333-2222-4333-8444-555555555555";
        // 이 OS의 절대 경로(JSON 문자열, Windows 역슬래시 이스케이프 포함).
        let cwd = |name: &str| serde_json::to_string(&root.join(name)).unwrap();
        let (cwd_a, cwd_b, cwd_c) = (cwd("a"), cwd("b"), cwd("c"));

        fs::write(
            claude_a.join("a.jsonl"),
            format!("{{\"sessionId\":\"{id_shared}\",\"cwd\":{cwd_a},\"isSidechain\":false}}\n"),
        )
        .unwrap();
        fs::write(
            claude_b.join("dup.jsonl"),
            format!("{{\"sessionId\":\"{id_shared}\",\"cwd\":{cwd_a},\"isSidechain\":false}}\n"),
        )
        .unwrap();
        fs::write(
            claude_b.join("b.jsonl"),
            format!("{{\"sessionId\":\"{id_second}\",\"cwd\":{cwd_b},\"isSidechain\":false}}\n"),
        )
        .unwrap();
        fs::write(
            codex_a.join("rollout-1.jsonl"),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id_codex}\",\"cwd\":{cwd_c}}}}}\n"
            ),
        )
        .unwrap();

        let state = root.join("state");
        fs::create_dir_all(&state).unwrap();
        let mut accounts = crate::usage::accounts::AccountsFile::default();
        for (id, provider, profile) in [
            ("a", "claude", "claude-a"),
            ("b", "Claude", "claude-b"),
            ("c", "codex", "codex-a"),
        ] {
            accounts
                .accounts
                .push(crate::usage::accounts::StoredAccount {
                    id: id.into(),
                    provider: provider.into(),
                    label: id.into(),
                    email: None,
                    profile_dir: root.join(profile).display().to_string(),
                });
        }
        accounts.active.insert("claude".into(), "b".into());
        crate::usage::accounts::save_file(&root, Some(&state), &accounts).unwrap();
        let registered = crate::usage::accounts::load_file(&root, Some(&state));
        let snap = scan_profiles(vec![root.join("claude-a")], Vec::new(), &registered);
        assert_eq!(snap.entries.len(), 3);

        fs::remove_dir_all(&root).ok();
    }
}

/// K-13: 비활성 계정 대화를 재개하면 그 대화가 들어 있는 프로필을 찾는다.
#[cfg(test)]
mod resume_profile_tests {
    use super::*;
    use crate::usage::accounts::{AccountsFile, StoredAccount};

    const CLAUDE_ID: &str = "11111111-2222-4333-8444-555555555555";
    const CODEX_ID: &str = "33333333-2222-4333-8444-555555555555";

    fn accounts(root: &Path) -> AccountsFile {
        let mut file = AccountsFile::default();
        for (id, provider, profile) in [("a", "claude", "claude-a"), ("b", "claude", "claude-b")] {
            file.accounts.push(StoredAccount {
                id: id.into(),
                provider: provider.into(),
                label: id.into(),
                email: None,
                profile_dir: root.join(profile).display().to_string(),
            });
        }
        file.accounts.push(StoredAccount {
            id: "c".into(),
            provider: "codex".into(),
            label: "c".into(),
            email: None,
            profile_dir: root.join("codex-a").display().to_string(),
        });
        file.active.insert("claude".into(), "a".into());
        file
    }

    fn entry(agent: Agent, id: &str) -> Entry {
        Entry {
            agent,
            id: id.into(),
            cwd: "/tmp".into(),
            modified: 0,
        }
    }

    #[test]
    fn cross_review_codex_profile_metadata_accepts_json_whitespace() {
        let root = std::env::temp_dir().join(format!("mk082-cross-json-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let path = root.join("codex-a/sessions");
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("rollout-test.jsonl"),
            format!("{{\"type\": \"session_meta\", \"payload\": {{\"id\": \"{CODEX_ID}\"}}}}\n"),
        )
        .unwrap();
        let actual = profile_dir_for_entry(&accounts(&root), &entry(Agent::Codex, CODEX_ID));
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(actual, Some(root.join("codex-a")));
    }

    #[test]
    fn cross_review_host_transcript_resumes_outside_active_registered_profile() {
        let root = std::env::temp_dir().join(format!("mk082-cross-host-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let host = root.join("host");
        for (agent, id) in [(Agent::Claude, CLAUDE_ID), (Agent::Codex, CODEX_ID)] {
            let path = match agent {
                Agent::Claude => host.join("projects/p").join(format!("{id}.jsonl")),
                Agent::Codex => host.join("sessions/rollout-host.jsonl"),
            };
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\"}}}}\n"),
            )
            .unwrap();
            assert_eq!(
                profile_dir_for_entry_in(&accounts(&root), &entry(agent, id), &host),
                Some(host.clone())
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resume_finds_transcript_owner_not_active_account() {
        let root = std::env::temp_dir().join(format!("mk082-resume-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        // 대화는 비활성 계정 b에만 있다.
        let night = root.join("claude-b/projects/p");
        fs::create_dir_all(&night).unwrap();
        fs::write(
            night.join(format!("{CLAUDE_ID}.jsonl")),
            format!("{{\"sessionId\":\"{CLAUDE_ID}\",\"cwd\":\"/tmp\",\"isSidechain\":false}}\n"),
        )
        .unwrap();
        fs::create_dir_all(root.join("claude-a/projects/p")).unwrap();
        let codex = root.join("codex-a/sessions/2026/10/16");
        fs::create_dir_all(&codex).unwrap();
        fs::write(
            codex.join("rollout-1.jsonl"),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{CODEX_ID}\",\"cwd\":\"/tmp/c\"}}}}\n"
            ),
        )
        .unwrap();

        let file = accounts(&root);
        assert_eq!(
            profile_dir_for_entry(&file, &entry(Agent::Claude, CLAUDE_ID)),
            Some(root.join("claude-b"))
        );
        assert_eq!(
            profile_dir_for_entry(&file, &entry(Agent::Codex, CODEX_ID)),
            Some(root.join("codex-a"))
        );
        // 없는 대화·다른 에이전트 파일은 못 찾는다(호출자는 기존 동작 유지).
        assert_eq!(
            profile_dir_for_entry(
                &file,
                &entry(Agent::Claude, "99999999-2222-4333-8444-555555555555")
            ),
            None
        );
        assert_eq!(
            profile_dir_for_entry(&file, &entry(Agent::Codex, CLAUDE_ID)),
            None
        );

        fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
