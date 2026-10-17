//! AI Vault(MK-181): Claude Code·Codex·Antigravity(`agy`)·`OpenCode` 멀티 에이전트 대화 기록 스캔 및 전문 검색.
//!
//! 요청 시에만 디스크를 읽으며 상주 워커·폴링·감시 프로세스를 두지 않는다.
//! 탐색 노드·바이트·시간 예산 안에서 대화 제목·요약·메시지 본문을 인덱싱하고,
//! 키워드·에이전트·경로 필터와 스니펫 추출·원클릭 재개 프로필 탐색을 수행한다.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use krill_proto::agent_history::{
    MAX_ENTRIES, MAX_TEXT, SearchQuery, SearchSnapshot, VaultAgent, VaultHit, VaultTarget,
};
use serde_json::Value;

const MAX_NODES: usize = 25_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const FILE_BYTES: usize = 512 * 1024;
const SCAN_TIME: Duration = Duration::from_secs(3);
const SNIPPET_CHARS: usize = 140;
const TITLE_CHARS: usize = 120;

#[derive(Debug, Clone, Default)]
pub(crate) struct VaultRoots {
    pub claude: Vec<PathBuf>,
    pub codex: Vec<PathBuf>,
    pub agy: Vec<PathBuf>,
    pub opencode: Vec<PathBuf>,
}

impl VaultRoots {
    fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
        if path.is_absolute() && !list.contains(&path) {
            list.push(path);
        }
    }
}

pub(crate) fn valid_target(target: &VaultTarget) -> bool {
    crate::agent::control::valid_session_id(&target.id)
        && !target.cwd.is_empty()
        && target.cwd.len() <= MAX_TEXT
        && Path::new(&target.cwd).is_absolute()
        && !target.cwd.contains('\0')
}

pub(crate) fn search(state_dir: Option<&Path>, query: &SearchQuery) -> SearchSnapshot {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let env_or_home = |var: &str, default_rel: &str| {
        std::env::var_os(var)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(default_rel)))
    };
    let mut roots = VaultRoots::default();
    if let Some(p) = env_or_home("CLAUDE_CONFIG_DIR", ".claude") {
        VaultRoots::push_unique(&mut roots.claude, p);
    }
    if let Some(p) = env_or_home("CODEX_HOME", ".codex") {
        VaultRoots::push_unique(&mut roots.codex, p);
    }
    if let Some(p) = std::env::var_os("ANTIGRAVITY_CLI_HOME")
        .or_else(|| std::env::var_os("AGY_HOME"))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    {
        VaultRoots::push_unique(&mut roots.agy, p);
    } else if let Some(h) = &home {
        VaultRoots::push_unique(&mut roots.agy, h.join(".gemini/antigravity-cli"));
        VaultRoots::push_unique(&mut roots.agy, h.join(".gemini/antigravity"));
    }
    if let Some(p) = std::env::var_os("OPENCODE_CONFIG_DIR")
        .or_else(|| std::env::var_os("OPENCODE_DATA_DIR"))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    {
        VaultRoots::push_unique(&mut roots.opencode, p);
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    {
        VaultRoots::push_unique(&mut roots.opencode, xdg.join("opencode"));
    }
    if let Some(h) = &home {
        VaultRoots::push_unique(&mut roots.opencode, h.join(".local/share/opencode"));
        VaultRoots::push_unique(&mut roots.opencode, h.join(".config/opencode"));
    }

    let accounts = crate::usage::accounts::load_file(
        home.as_deref().unwrap_or_else(|| Path::new("")),
        state_dir,
    );
    add_account_roots(&mut roots, &accounts, state_dir);
    search_in(&roots, query)
}

fn add_account_roots(
    roots: &mut VaultRoots,
    accounts: &crate::usage::accounts::AccountsFile,
    state_dir: Option<&Path>,
) {
    for account in &accounts.accounts {
        let dir = account.config_dir();
        if !dir.is_absolute() {
            continue;
        }
        let path = dir.to_path_buf();
        if account.provider.eq_ignore_ascii_case("claude") {
            VaultRoots::push_unique(&mut roots.claude, path);
        } else if account.provider.eq_ignore_ascii_case("codex") {
            VaultRoots::push_unique(&mut roots.codex, path);
        } else if account.provider.eq_ignore_ascii_case("agy")
            || account.provider.eq_ignore_ascii_case("antigravity")
        {
            VaultRoots::push_unique(&mut roots.agy, path);
        } else if account.provider.eq_ignore_ascii_case("opencode") {
            VaultRoots::push_unique(&mut roots.opencode, path);
        }
    }
    if let Some(state) = state_dir {
        let profiles = state.join("profiles");
        for (sub, target) in [
            ("claude", &mut roots.claude),
            ("codex", &mut roots.codex),
            ("agy", &mut roots.agy),
            ("opencode", &mut roots.opencode),
        ] {
            if let Ok(entries) = fs::read_dir(profiles.join(sub)) {
                for item in entries.take(64).flatten() {
                    if item.file_type().is_ok_and(|k| k.is_dir()) {
                        VaultRoots::push_unique(target, item.path());
                    }
                }
            }
        }
    }
}

/// 볼트 재개 시 해당 대화가 실제로 들어 있는 격리 프로필 디렉터리를 찾는다.
#[cfg(test)]
pub(crate) fn profile_dir_for_vault_target(
    accounts: &crate::usage::accounts::AccountsFile,
    target: &VaultTarget,
    host: Option<&Path>,
) -> Option<PathBuf> {
    profile_dir_for_vault_target_in(accounts, target, host, None)
}

pub(crate) fn profile_dir_for_vault_target_in(
    accounts: &crate::usage::accounts::AccountsFile,
    target: &VaultTarget,
    host: Option<&Path>,
    state_dir: Option<&Path>,
) -> Option<PathBuf> {
    if matches!(target.agent, VaultAgent::Claude | VaultAgent::Codex) {
        let entry = krill_proto::agent_history::Entry {
            agent: if target.agent == VaultAgent::Claude {
                krill_proto::agent_history::Agent::Claude
            } else {
                krill_proto::agent_history::Agent::Codex
            },
            id: target.id.clone(),
            cwd: target.cwd.clone(),
            modified: target.modified,
        };
        let found = match host {
            Some(h) => super::profile_dir_for_entry_in(accounts, &entry, h),
            None => super::profile_dir_for_entry(accounts, &entry),
        };
        if found.is_some() {
            return found;
        }
    } else {
        if let Some(h) = host
            && root_contains_target(h, target)
        {
            return Some(h.to_path_buf());
        }
        for account in &accounts.accounts {
            let dir = account.config_dir();
            if !dir.is_absolute() {
                continue;
            }
            let matches_provider = match target.agent {
                VaultAgent::Claude => account.provider.eq_ignore_ascii_case("claude"),
                VaultAgent::Codex => account.provider.eq_ignore_ascii_case("codex"),
                VaultAgent::Agy => {
                    account.provider.eq_ignore_ascii_case("agy")
                        || account.provider.eq_ignore_ascii_case("antigravity")
                }
                VaultAgent::OpenCode => account.provider.eq_ignore_ascii_case("opencode"),
            };
            if matches_provider && root_contains_target(dir, target) {
                return Some(dir.to_path_buf());
            }
        }
    }
    if let Some(state) = state_dir {
        let sub = match target.agent {
            VaultAgent::Claude => "claude",
            VaultAgent::Codex => "codex",
            VaultAgent::Agy => "agy",
            VaultAgent::OpenCode => "opencode",
        };
        if let Ok(entries) = fs::read_dir(state.join("profiles").join(sub)) {
            for item in entries.take(64).flatten() {
                if item.file_type().is_ok_and(|k| k.is_dir()) {
                    let path = item.path();
                    if root_contains_target(&path, target) {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

fn agy_candidate_dirs(dir: &Path, include_sibling: bool) -> Vec<PathBuf> {
    let mut dirs = vec![
        dir.to_path_buf(),
        dir.join(".gemini/antigravity-cli"),
        dir.join(".gemini/antigravity"),
        dir.join(".gemini"),
        dir.join("antigravity-cli"),
        dir.join("antigravity"),
    ];
    if include_sibling
        && dir.file_name().is_some_and(|n| n == "antigravity-cli")
        && let Some(parent) = dir.parent()
    {
        dirs.push(parent.join("antigravity"));
    }
    dirs
}

fn opencode_candidate_bases(dir: &Path) -> Vec<PathBuf> {
    let mut bases = Vec::new();
    for sub in [
        "storage/session",
        "session",
        "sessions",
        "data/opencode/storage/session",
        "data/opencode/session",
        "data/opencode/sessions",
        "data/storage/session",
        "data/session",
        "data/sessions",
    ] {
        bases.push(dir.join(sub));
    }
    if dir.ends_with(".config/opencode")
        && let Some(home) = dir.parent().and_then(Path::parent)
    {
        for sub in ["storage/session", "session", "sessions"] {
            bases.push(home.join(".local/share/opencode").join(sub));
        }
    }
    bases
}

fn root_contains_target(dir: &Path, target: &VaultTarget) -> bool {
    match target.agent {
        VaultAgent::Claude => super::profile_contains_entry(
            dir,
            &krill_proto::agent_history::Entry {
                agent: krill_proto::agent_history::Agent::Claude,
                id: target.id.clone(),
                cwd: target.cwd.clone(),
                modified: target.modified,
            },
        ),
        VaultAgent::Codex => super::profile_contains_entry(
            dir,
            &krill_proto::agent_history::Entry {
                agent: krill_proto::agent_history::Agent::Codex,
                id: target.id.clone(),
                cwd: target.cwd.clone(),
                modified: target.modified,
            },
        ),
        VaultAgent::Agy => agy_candidate_dirs(dir, true).into_iter().any(|cand| {
            cand.join("brain").join(&target.id).is_dir()
                || cand
                    .join("sessions")
                    .join(format!("{}.jsonl", target.id))
                    .is_file()
        }),
        VaultAgent::OpenCode => {
            for base in opencode_candidate_bases(dir) {
                if base.join(format!("{}.json", target.id)).is_file()
                    || base.join(format!("{}.jsonl", target.id)).is_file()
                {
                    return true;
                }
                if let Ok(listing) = fs::read_dir(&base) {
                    for group in listing.take(256).flatten() {
                        if group.file_type().is_ok_and(|k| k.is_dir())
                            && (group.path().join(format!("{}.json", target.id)).is_file()
                                || group.path().join(format!("{}.jsonl", target.id)).is_file())
                        {
                            return true;
                        }
                    }
                }
            }
            false
        }
    }
}

pub(crate) fn search_in(roots: &VaultRoots, query: &SearchQuery) -> SearchSnapshot {
    let parsed_query = ParsedQuery::parse(query);
    let mut indexer = Indexer {
        docs: BTreeMap::new(),
        nodes: 0,
        bytes: 0,
        start: Instant::now(),
        truncated: false,
        skipped: false,
    };

    let wants =
        |agent: VaultAgent| parsed_query.agents.is_empty() || parsed_query.agents.contains(&agent);
    if wants(VaultAgent::Claude) {
        for root in &roots.claude {
            indexer.scan_claude(root);
        }
    }
    if wants(VaultAgent::Codex) {
        for root in &roots.codex {
            indexer.scan_codex(root);
        }
    }
    if wants(VaultAgent::Agy) {
        for root in &roots.agy {
            indexer.scan_agy(root);
        }
    }
    if wants(VaultAgent::OpenCode) {
        for root in &roots.opencode {
            indexer.scan_opencode(root);
        }
    }

    let mut hits: Vec<VaultHit> = indexer
        .docs
        .into_values()
        .filter(|doc| parsed_query.matches_path(&doc.cwd))
        .filter_map(|doc| parsed_query.evaluate(doc))
        .collect();

    if parsed_query.terms.is_empty() {
        hits.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    } else {
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.modified.cmp(&a.modified))
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    let limit = if query.limit == 0 {
        MAX_ENTRIES
    } else {
        usize::from(query.limit).min(MAX_ENTRIES)
    };
    let truncated = indexer.truncated || hits.len() > limit;
    hits.truncate(limit);

    SearchSnapshot {
        hits,
        truncated,
        skipped: indexer.skipped,
    }
}

#[derive(Debug, Clone)]
struct VaultDoc {
    agent: VaultAgent,
    id: String,
    cwd: String,
    modified: u64,
    title: String,
    summary: String,
    messages: Vec<(String, String)>,
}

struct ParsedQuery {
    raw_lower: String,
    terms: Vec<String>,
    agents: BTreeSet<VaultAgent>,
    paths: Vec<String>,
}

impl ParsedQuery {
    fn parse(req: &SearchQuery) -> Self {
        let mut agents: BTreeSet<VaultAgent> = req.agents.iter().copied().collect();
        let mut paths: Vec<String> = req
            .paths
            .iter()
            .map(|p| normalize_path(p))
            .filter(|p| !p.is_empty())
            .collect();
        let mut terms = Vec::new();
        for token in req.query.split_whitespace() {
            if let Some(rest) = token.strip_prefix("agent:")
                && let Some(a) = VaultAgent::parse(rest)
            {
                agents.insert(a);
                continue;
            }
            if let Some(rest) = token
                .strip_prefix("path:")
                .or_else(|| token.strip_prefix("repo:"))
            {
                let norm = normalize_path(rest);
                if !norm.is_empty() {
                    paths.push(norm);
                }
                continue;
            }
            let lower = token.to_lowercase();
            if !lower.is_empty() {
                terms.push(lower);
            }
        }
        let raw_lower = terms.join(" ");
        Self {
            raw_lower,
            terms,
            agents,
            paths,
        }
    }

    fn matches_path(&self, cwd: &str) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        let doc_norm = normalize_path(cwd);
        self.paths.iter().any(|filter| {
            if filter.starts_with('/') {
                doc_norm == *filter
                    || doc_norm
                        .strip_prefix(filter.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            } else {
                doc_norm.to_lowercase().contains(&filter.to_lowercase())
            }
        })
    }

    fn evaluate(&self, doc: VaultDoc) -> Option<VaultHit> {
        let title = if doc.title.is_empty() {
            doc.id.clone()
        } else {
            doc.title.clone()
        };
        if self.terms.is_empty() {
            let snippet = if doc.summary.is_empty() {
                truncate_chars(&one_line(&title), SNIPPET_CHARS)
            } else {
                truncate_chars(&one_line(&doc.summary), SNIPPET_CHARS)
            };
            return Some(VaultHit {
                agent: doc.agent,
                id: doc.id,
                cwd: doc.cwd,
                modified: doc.modified,
                title: truncate_chars(&one_line(&title), TITLE_CHARS),
                summary: truncate_chars(&one_line(&doc.summary), SNIPPET_CHARS),
                snippet,
                role: String::new(),
                score: 0,
            });
        }

        let title_lower = title.to_lowercase();
        let summary_lower = doc.summary.to_lowercase();
        let cwd_lower = doc.cwd.to_lowercase();
        let id_lower = doc.id.to_lowercase();
        let msg_lowers: Vec<(&str, &str, String)> = doc
            .messages
            .iter()
            .map(|(r, t)| (r.as_str(), t.as_str(), t.to_lowercase()))
            .collect();

        // 모든 검색어 토큰이 문서 어딘가(제목·요약·본문·경로·ID)에 등장해야 한다.
        for term in &self.terms {
            let found = title_lower.contains(term)
                || summary_lower.contains(term)
                || cwd_lower.contains(term)
                || id_lower.contains(term)
                || msg_lowers.iter().any(|(_, _, lower)| lower.contains(term));
            if !found {
                return None;
            }
        }

        let mut score = 0u32;
        let mut title_hits = 0u32;
        let mut summary_hits = 0u32;
        for term in &self.terms {
            if title_lower.contains(term) {
                score = score.saturating_add(100);
                title_hits = title_hits.saturating_add(1);
            }
            if summary_lower.contains(term) {
                score = score.saturating_add(60);
                summary_hits = summary_hits.saturating_add(1);
            }
            if cwd_lower.contains(term) || id_lower.contains(term) {
                score = score.saturating_add(20);
            }
        }
        if self.terms.len() > 1 {
            if title_lower.contains(&self.raw_lower) {
                score = score.saturating_add(50);
            }
            if summary_lower.contains(&self.raw_lower) {
                score = score.saturating_add(30);
            }
        }

        let mut best_msg: Option<(&str, &str, u32)> = None;
        for (role, raw_text, lower) in &msg_lowers {
            let mut msg_score = 0u32;
            for term in &self.terms {
                if lower.contains(term) {
                    msg_score = msg_score.saturating_add(40);
                }
            }
            if msg_score > 0 {
                if self.terms.len() > 1 && lower.contains(&self.raw_lower) {
                    msg_score = msg_score.saturating_add(35);
                }
                if *role == "user" || *role == "assistant" {
                    msg_score = msg_score.saturating_add(5);
                }
                if best_msg.is_none_or(|(_, _, prev)| msg_score > prev) {
                    best_msg = Some((*role, *raw_text, msg_score));
                }
            }
        }

        if let Some((_, _, msg_score)) = best_msg {
            score = score.saturating_add(msg_score);
        }

        let (role, snippet) = if let Some((r, text, _)) = best_msg {
            (r.to_owned(), extract_snippet(text, &self.terms))
        } else if summary_hits > 0 {
            (
                "summary".to_owned(),
                extract_snippet(&doc.summary, &self.terms),
            )
        } else if title_hits > 0 {
            ("title".to_owned(), extract_snippet(&title, &self.terms))
        } else {
            (
                "path".to_owned(),
                truncate_chars(&one_line(&doc.cwd), SNIPPET_CHARS),
            )
        };

        Some(VaultHit {
            agent: doc.agent,
            id: doc.id,
            cwd: doc.cwd,
            modified: doc.modified,
            title: truncate_chars(&one_line(&title), TITLE_CHARS),
            summary: truncate_chars(&one_line(&doc.summary), SNIPPET_CHARS),
            snippet,
            role,
            score: score.max(1),
        })
    }
}

/// macOS `/private/tmp` ↔ `/tmp`, `/private/var` ↔ `/var` 심링크 차이와 끝 `/`를 정규화한다.
pub(crate) fn normalize_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix("/private/tmp")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("/tmp{rest}");
    }
    if let Some(rest) = trimmed.strip_prefix("/private/var")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("/var{rest}");
    }
    trimmed.to_owned()
}

fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TEXT));
    let mut last_space = false;
    for ch in s.chars() {
        if ch.is_control() {
            if matches!(ch, '\n' | '\r' | '\t') && !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        if ch.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
        if out.len() >= MAX_TEXT {
            break;
        }
    }
    out.trim().to_owned()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    if out.len() > MAX_TEXT {
        out.clear();
        for ch in s.chars() {
            if out.len() + ch.len_utf8() > MAX_TEXT {
                break;
            }
            out.push(ch);
        }
    }
    out
}

fn extract_snippet(text: &str, terms: &[String]) -> String {
    let clean = one_line(text);
    let chars: Vec<char> = clean.chars().collect();
    if chars.len() <= SNIPPET_CHARS {
        return clean;
    }
    let lower = clean.to_lowercase();
    let match_char_pos = terms
        .iter()
        .filter_map(|term| {
            lower
                .find(term.as_str())
                .and_then(|byte_idx| lower.get(..byte_idx))
                .map(|prefix| prefix.chars().count())
        })
        .min()
        .unwrap_or(0);

    let half = SNIPPET_CHARS / 3;
    let start = match_char_pos
        .saturating_sub(half)
        .min(chars.len().saturating_sub(SNIPPET_CHARS));
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    if let Some(slice) = chars.get(start..end) {
        for &c in slice {
            out.push(c);
        }
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

struct Indexer {
    docs: BTreeMap<(VaultAgent, String), VaultDoc>,
    nodes: usize,
    bytes: usize,
    start: Instant,
    truncated: bool,
    skipped: bool,
}

impl Indexer {
    fn exhausted(&mut self) -> bool {
        let done =
            self.nodes >= MAX_NODES || self.bytes >= MAX_BYTES || self.start.elapsed() >= SCAN_TIME;
        self.truncated |= done;
        done
    }

    fn insert(&mut self, doc: VaultDoc) {
        let target = VaultTarget {
            agent: doc.agent,
            id: doc.id.clone(),
            cwd: doc.cwd.clone(),
            modified: doc.modified,
        };
        if !valid_target(&target) {
            self.skipped = true;
            return;
        }
        let key = (doc.agent, doc.id.clone());
        if self
            .docs
            .get(&key)
            .is_none_or(|old| old.modified < doc.modified)
        {
            self.docs.insert(key, doc);
        }
    }

    fn read_lines(&mut self, path: &Path, mut on_line: impl FnMut(&[u8])) -> io::Result<u64> {
        let file = File::open(path)?;
        let modified = file
            .metadata()?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let limit =
            u64::try_from(FILE_BYTES.min(MAX_BYTES.saturating_sub(self.bytes))).unwrap_or(u64::MAX);
        let mut reader = BufReader::with_capacity(8192, file.take(limit));
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => on_line(&line),
                Err(_) => {
                    self.skipped = true;
                    break;
                }
            }
        }
        let read = limit.saturating_sub(reader.get_ref().limit());
        self.bytes = self
            .bytes
            .saturating_add(usize::try_from(read).unwrap_or(usize::MAX));
        Ok(modified)
    }

    fn scan_claude(&mut self, root: &Path) {
        self.walk_jsonl(&root.join("projects"), 2, false, |this, path| {
            this.parse_claude_file(path);
        });
    }

    fn scan_codex(&mut self, root: &Path) {
        let titles = self.read_codex_title_index(&root.join("session_index.jsonl"));
        self.walk_jsonl(&root.join("sessions"), 4, true, |this, path| {
            this.parse_codex_file(path, &titles);
        });
    }

    fn scan_agy(&mut self, root: &Path) {
        for cand in agy_candidate_dirs(root, false) {
            if self.exhausted() {
                break;
            }
            self.scan_agy_dir(&cand);
        }
    }

    fn scan_agy_dir(&mut self, root: &Path) {
        let brain = root.join("brain");
        let sessions = root.join("sessions");
        if !brain.is_dir() && !sessions.is_dir() {
            return;
        }
        let history = self.read_agy_history(root);
        if let Ok(dir) = fs::read_dir(&brain) {
            for item in dir {
                if self.exhausted() {
                    break;
                }
                self.nodes = self.nodes.saturating_add(1);
                let Ok(item) = item else {
                    self.skipped = true;
                    continue;
                };
                let Ok(kind) = item.file_type() else {
                    self.skipped = true;
                    continue;
                };
                if kind.is_symlink() || !kind.is_dir() {
                    continue;
                }
                let conv_id = item.file_name().to_string_lossy().into_owned();
                let conv_dir = item.path();
                let logs = conv_dir.join(".system_generated/logs");
                let candidate = [
                    logs.join("transcript.jsonl"),
                    logs.join("transcript_full.jsonl"),
                    conv_dir.join("transcript.jsonl"),
                ]
                .into_iter()
                .find(|p| p.is_file());
                if let Some(transcript) = candidate {
                    self.parse_agy_file(&transcript, Some(&conv_id), &history);
                }
            }
        }
        if sessions.is_dir() {
            self.walk_jsonl(&sessions, 2, false, |this, path| {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                this.parse_agy_file(path, Some(&stem), &history);
            });
        }
    }

    fn scan_opencode(&mut self, root: &Path) {
        for session_dir in opencode_candidate_bases(root) {
            if !session_dir.is_dir() {
                continue;
            }
            let storage_root = session_dir.parent().map(Path::to_path_buf);
            self.walk_opencode_dir(&session_dir, storage_root.as_deref(), 3);
        }
    }

    fn walk_jsonl(
        &mut self,
        path: &Path,
        depth: usize,
        codex_rollout_only: bool,
        mut on_file: impl FnMut(&mut Self, &Path),
    ) {
        self.walk_jsonl_inner(path, depth, codex_rollout_only, &mut on_file);
    }

    fn walk_jsonl_inner(
        &mut self,
        path: &Path,
        depth: usize,
        codex_rollout_only: bool,
        on_file: &mut impl FnMut(&mut Self, &Path),
    ) {
        if depth == 0 || self.exhausted() {
            return;
        }
        let dir = match fs::read_dir(path) {
            Ok(d) => d,
            Err(e) => {
                self.skipped |= e.kind() != io::ErrorKind::NotFound;
                return;
            }
        };
        for item in dir {
            if self.exhausted() {
                break;
            }
            self.nodes = self.nodes.saturating_add(1);
            let Ok(item) = item else {
                self.skipped = true;
                continue;
            };
            let Ok(kind) = item.file_type() else {
                self.skipped = true;
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let name = item.file_name();
            let name_str = name.to_string_lossy();
            if kind.is_dir() {
                if name_str == "subagents" {
                    continue;
                }
                self.walk_jsonl_inner(
                    &item.path(),
                    depth.saturating_sub(1),
                    codex_rollout_only,
                    on_file,
                );
            } else if kind.is_file() {
                let p = item.path();
                if p.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                if codex_rollout_only && !name_str.starts_with("rollout-") {
                    continue;
                }
                on_file(self, &p);
            }
        }
    }

    fn parse_claude_file(&mut self, path: &Path) {
        let mut id = String::new();
        let mut cwd = String::new();
        let mut custom_title = String::new();
        let mut ai_title = String::new();
        let mut first_user = String::new();
        let mut summary = String::new();
        let mut last_prompt = String::new();
        let mut first_assistant = String::new();
        let mut sidechain = false;
        let mut messages = Vec::new();
        let mut saw_json = false;

        let res = self.read_lines(path, |line| {
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            saw_json = true;
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                sidechain = true;
                return;
            }
            if id.is_empty()
                && let Some(s) = v
                    .get("sessionId")
                    .or_else(|| v.get("id"))
                    .and_then(Value::as_str)
            {
                s.trim().clone_into(&mut id);
            }
            if cwd.is_empty()
                && let Some(c) = v.get("cwd").and_then(Value::as_str)
            {
                c.clone_into(&mut cwd);
            }
            if let Some(t) = v
                .get("customTitle")
                .or_else(|| v.get("title"))
                .and_then(Value::as_str)
                && !t.trim().is_empty()
            {
                t.trim().clone_into(&mut custom_title);
            }
            if let Some(t) = v
                .get("aiTitle")
                .or_else(|| v.get("agentName"))
                .and_then(Value::as_str)
                && !t.trim().is_empty()
            {
                t.trim().clone_into(&mut ai_title);
            }
            if let Some(s) = v.get("summary").and_then(Value::as_str)
                && !s.trim().is_empty()
            {
                s.trim().clone_into(&mut summary);
            }
            if let Some(lp) = v.get("lastPrompt").and_then(Value::as_str)
                && !lp.trim().is_empty()
            {
                lp.trim().clone_into(&mut last_prompt);
            }
            let kind = v
                .get("type")
                .or_else(|| v.get("role"))
                .and_then(Value::as_str)
                .unwrap_or("");
            match kind {
                "user" => {
                    let msg_val = v.get("message").unwrap_or(&v);
                    extract_content_messages("user", msg_val, &mut messages);
                    if first_user.is_empty()
                        && let Some((_, text)) = messages.iter().rfind(|(r, _)| r == "user")
                    {
                        first_user = truncate_chars(&one_line(text), TITLE_CHARS);
                    }
                }
                "assistant" => {
                    let msg_val = v.get("message").unwrap_or(&v);
                    extract_content_messages("assistant", msg_val, &mut messages);
                    if first_assistant.is_empty()
                        && let Some((_, text)) = messages.iter().rfind(|(r, _)| r == "assistant")
                    {
                        first_assistant = truncate_chars(&one_line(text), SNIPPET_CHARS);
                    }
                }
                _ => {
                    if let Some(c) = v
                        .get("content")
                        .or_else(|| v.get("text"))
                        .and_then(Value::as_str)
                        && !c.trim().is_empty()
                    {
                        messages.push(("user".to_owned(), c.to_owned()));
                    }
                }
            }
        });

        let Ok(modified) = res else {
            self.skipped = true;
            return;
        };
        if sidechain {
            return;
        }
        if id.is_empty()
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && crate::agent::control::valid_session_id(stem)
        {
            stem.clone_into(&mut id);
        }
        if !saw_json || id.is_empty() || cwd.is_empty() {
            self.skipped = true;
            return;
        }
        let title = first_non_empty(&[&custom_title, &ai_title, &first_user, &summary, &id]);
        let sum = first_non_empty(&[&summary, &last_prompt, &first_assistant]);
        self.insert(VaultDoc {
            agent: VaultAgent::Claude,
            id,
            cwd,
            modified,
            title,
            summary: sum,
            messages,
        });
    }

    fn read_codex_title_index(&mut self, path: &Path) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        if !path.is_file() {
            return map;
        }
        if self
            .read_lines(path, |line| {
                let Ok(v) = serde_json::from_slice::<Value>(line) else {
                    return;
                };
                let id = v
                    .get("id")
                    .or_else(|| v.get("session_id"))
                    .and_then(Value::as_str);
                let title = v
                    .get("thread_name")
                    .or_else(|| v.get("title"))
                    .and_then(Value::as_str);
                if let (Some(id), Some(title)) = (id, title)
                    && !title.trim().is_empty()
                {
                    map.insert(id.to_owned(), title.trim().to_owned());
                }
            })
            .is_err()
        {
            self.skipped = true;
        }
        map
    }

    fn parse_codex_file(&mut self, path: &Path, titles: &BTreeMap<String, String>) {
        let mut id = String::new();
        let mut cwd = String::new();
        let mut meta_title = String::new();
        let mut first_user = String::new();
        let mut summary = String::new();
        let mut first_assistant = String::new();
        let mut subagent = false;
        let mut messages = Vec::new();
        let mut saw_json = false;

        let res = self.read_lines(path, |line| {
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            saw_json = true;
            let rec_type = v.get("type").and_then(Value::as_str).unwrap_or("");
            if rec_type == "session_meta" {
                let Some(payload) = v.get("payload") else {
                    return;
                };
                if payload
                    .get("source")
                    .and_then(Value::as_object)
                    .is_some_and(|s| s.contains_key("subagent"))
                {
                    subagent = true;
                    return;
                }
                if id.is_empty()
                    && let Some(s) = payload.get("id").and_then(Value::as_str)
                {
                    s.clone_into(&mut id);
                }
                if cwd.is_empty()
                    && let Some(c) = payload.get("cwd").and_then(Value::as_str)
                {
                    c.clone_into(&mut cwd);
                }
                if let Some(t) = payload
                    .get("title")
                    .or_else(|| payload.get("name"))
                    .and_then(Value::as_str)
                    && !t.trim().is_empty()
                {
                    t.trim().clone_into(&mut meta_title);
                }
                if let Some(s) = payload
                    .get("summary")
                    .or_else(|| payload.get("instructions"))
                    .and_then(Value::as_str)
                    && !s.trim().is_empty()
                {
                    s.trim().clone_into(&mut summary);
                }
                return;
            }
            if rec_type == "turn_context" {
                if cwd.is_empty()
                    && let Some(c) = v
                        .get("payload")
                        .and_then(|p| p.get("cwd"))
                        .and_then(Value::as_str)
                {
                    c.clone_into(&mut cwd);
                }
                return;
            }
            if rec_type == "response_item"
                && let Some(payload) = v.get("payload")
            {
                let p_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
                if p_type == "message" {
                    let role = payload
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("user");
                    extract_content_messages(role, payload, &mut messages);
                    if role == "user"
                        && first_user.is_empty()
                        && let Some((_, t)) = messages.iter().rfind(|(r, _)| r == "user")
                    {
                        first_user = truncate_chars(&one_line(t), TITLE_CHARS);
                    } else if role == "assistant"
                        && first_assistant.is_empty()
                        && let Some((_, t)) = messages.iter().rfind(|(r, _)| r == "assistant")
                    {
                        first_assistant = truncate_chars(&one_line(t), SNIPPET_CHARS);
                    }
                } else if matches!(p_type, "function_call" | "custom_tool_call") {
                    let name = payload
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    let args = payload
                        .get("arguments")
                        .or_else(|| payload.get("input"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let combined = format!("{name} {args}");
                    if !combined.trim().is_empty() {
                        messages.push(("tool".to_owned(), combined));
                    }
                } else if matches!(p_type, "function_call_output" | "custom_tool_call_output")
                    && let Some(out) = payload.get("output").and_then(Value::as_str)
                    && !out.trim().is_empty()
                {
                    messages.push(("tool".to_owned(), out.to_owned()));
                }
                return;
            }
            if rec_type == "event_msg"
                && let Some(payload) = v.get("payload")
            {
                let p_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
                if matches!(p_type, "user_message" | "agent_message") {
                    let role = if p_type == "user_message" {
                        "user"
                    } else {
                        "assistant"
                    };
                    if let Some(msg) = payload
                        .get("message")
                        .or_else(|| payload.get("text"))
                        .and_then(Value::as_str)
                        && !msg.trim().is_empty()
                    {
                        if role == "user" && first_user.is_empty() {
                            first_user = truncate_chars(&one_line(msg), TITLE_CHARS);
                        } else if role == "assistant" && first_assistant.is_empty() {
                            first_assistant = truncate_chars(&one_line(msg), SNIPPET_CHARS);
                        }
                        messages.push((role.to_owned(), msg.to_owned()));
                    }
                } else if p_type == "item_completed"
                    && let Some(item) = payload.get("item")
                {
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                    let role = if matches!(item_type, "UserMessage" | "user_message") {
                        "user"
                    } else if matches!(item_type, "AgentMessage" | "agent_message") {
                        "assistant"
                    } else {
                        ""
                    };
                    if !role.is_empty() {
                        extract_content_messages(role, item, &mut messages);
                    }
                }
                return;
            }
            // 단순 메시지 줄 폴백
            if let Some(msg) = v
                .get("content")
                .or_else(|| v.get("text"))
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                && !msg.trim().is_empty()
            {
                let role = v.get("role").and_then(Value::as_str).unwrap_or("user");
                messages.push((role.to_owned(), msg.to_owned()));
            }
        });

        let Ok(modified) = res else {
            self.skipped = true;
            return;
        };
        if subagent {
            return;
        }
        if !saw_json || id.is_empty() || cwd.is_empty() {
            self.skipped = true;
            return;
        }
        let indexed_title = titles.get(&id).map_or("", String::as_str);
        let title = first_non_empty(&[&meta_title, indexed_title, &first_user, &summary, &id]);
        let sum = first_non_empty(&[&summary, &first_assistant]);
        self.insert(VaultDoc {
            agent: VaultAgent::Codex,
            id,
            cwd,
            modified,
            title,
            summary: sum,
            messages,
        });
    }

    fn read_agy_history(&mut self, root: &Path) -> BTreeMap<String, (String, String)> {
        let mut map = BTreeMap::new();
        let history_path = root.join("history.jsonl");
        if history_path.is_file()
            && self
                .read_lines(&history_path, |line| {
                    let Ok(v) = serde_json::from_slice::<Value>(line) else {
                        return;
                    };
                    let id = v
                        .get("conversationId")
                        .or_else(|| v.get("id"))
                        .and_then(Value::as_str);
                    let ws = v
                        .get("workspace")
                        .or_else(|| v.get("cwd"))
                        .and_then(Value::as_str);
                    let display = v
                        .get("display")
                        .or_else(|| v.get("title"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if let (Some(id), Some(ws)) = (id, ws)
                        && !ws.trim().is_empty()
                    {
                        map.insert(
                            id.to_owned(),
                            (ws.trim().to_owned(), display.trim().to_owned()),
                        );
                    }
                })
                .is_err()
        {
            self.skipped = true;
        }
        map
    }

    fn parse_agy_file(
        &mut self,
        path: &Path,
        fallback_id: Option<&str>,
        history: &BTreeMap<String, (String, String)>,
    ) {
        let mut id = fallback_id.unwrap_or("").to_owned();
        let mut cwd = String::new();
        let mut title = String::new();
        let mut summary = String::new();
        let mut first_user = String::new();
        let mut first_assistant = String::new();
        let mut messages = Vec::new();
        let mut saw_json = false;

        let res = self.read_lines(path, |line| {
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            saw_json = true;
            if let Some(cid) = v
                .get("conversationId")
                .or_else(|| v.get("sessionId"))
                .or_else(|| v.get("id"))
                .and_then(Value::as_str)
                && !cid.trim().is_empty()
            {
                cid.trim().clone_into(&mut id);
            }
            if cwd.is_empty()
                && let Some(c) = v
                    .get("cwd")
                    .or_else(|| v.get("workspace"))
                    .and_then(Value::as_str)
                && !c.trim().is_empty()
            {
                c.trim().clone_into(&mut cwd);
            }
            if let Some(t) = v.get("title").and_then(Value::as_str)
                && !t.trim().is_empty()
            {
                t.trim().clone_into(&mut title);
            }
            if let Some(s) = v.get("summary").and_then(Value::as_str)
                && !s.trim().is_empty()
            {
                s.trim().clone_into(&mut summary);
            }

            let source = v.get("source").and_then(Value::as_str).unwrap_or("");
            let rec_type = v.get("type").and_then(Value::as_str).unwrap_or("");
            let role = v.get("role").and_then(Value::as_str).unwrap_or("");
            let content = v
                .get("content")
                .or_else(|| v.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");

            if cwd.is_empty()
                && let Some(extracted_cwd) = extract_agy_workspace_from_text(content)
            {
                cwd = extracted_cwd;
            }

            if matches!(source, "USER_EXPLICIT" | "USER")
                || matches!(rec_type, "USER_INPUT" | "REQUEST")
                || role == "user"
            {
                let req = extract_agy_user_request(content);
                if !req.is_empty() {
                    if first_user.is_empty() {
                        first_user = truncate_chars(&one_line(&req), TITLE_CHARS);
                    }
                    messages.push(("user".to_owned(), req));
                }
            } else if source == "MODEL" || rec_type == "PLANNER_RESPONSE" || role == "assistant" {
                if !content.trim().is_empty() {
                    if first_assistant.is_empty() {
                        first_assistant = truncate_chars(&one_line(content), SNIPPET_CHARS);
                    }
                    messages.push(("assistant".to_owned(), content.to_owned()));
                }
                if let Some(thinking) = v.get("thinking").and_then(Value::as_str)
                    && !thinking.trim().is_empty()
                {
                    messages.push(("assistant".to_owned(), thinking.to_owned()));
                }
            }
        });

        let Ok(modified) = res else {
            self.skipped = true;
            return;
        };
        if let Some((hist_cwd, hist_display)) = history.get(&id) {
            if cwd.is_empty() {
                cwd.clone_from(hist_cwd);
            }
            if title.is_empty() && !hist_display.is_empty() {
                title.clone_from(hist_display);
            }
        }
        if !saw_json || id.is_empty() || cwd.is_empty() {
            self.skipped = true;
            return;
        }
        let resolved_title = first_non_empty(&[&title, &first_user, &summary, &id]);
        let resolved_summary = first_non_empty(&[&summary, &first_assistant]);
        self.insert(VaultDoc {
            agent: VaultAgent::Agy,
            id,
            cwd,
            modified,
            title: resolved_title,
            summary: resolved_summary,
            messages,
        });
    }

    fn walk_opencode_dir(&mut self, dir: &Path, storage_root: Option<&Path>, depth: usize) {
        if depth == 0 || self.exhausted() {
            return;
        }
        let Ok(listing) = fs::read_dir(dir) else {
            return;
        };
        for item in listing {
            if self.exhausted() {
                break;
            }
            self.nodes = self.nodes.saturating_add(1);
            let Ok(item) = item else {
                self.skipped = true;
                continue;
            };
            let Ok(kind) = item.file_type() else {
                self.skipped = true;
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let path = item.path();
            if kind.is_dir() {
                self.walk_opencode_dir(&path, storage_root, depth.saturating_sub(1));
            } else if kind.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "json" {
                    self.parse_opencode_json(&path, storage_root);
                } else if ext == "jsonl" {
                    self.parse_opencode_jsonl(&path);
                }
            }
        }
    }

    fn parse_opencode_json(&mut self, path: &Path, storage_root: Option<&Path>) {
        let mut bytes = Vec::new();
        let Ok(modified) = File::open(path).and_then(|f| {
            let m = f
                .metadata()?
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let limit = u64::try_from(FILE_BYTES.min(MAX_BYTES.saturating_sub(self.bytes)))
                .unwrap_or(u64::MAX);
            f.take(limit).read_to_end(&mut bytes)?;
            Ok(m)
        }) else {
            self.skipped = true;
            return;
        };
        self.bytes = self.bytes.saturating_add(bytes.len());
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            self.skipped = true;
            return;
        };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let id = v
            .get("id")
            .or_else(|| v.get("sessionId"))
            .and_then(Value::as_str)
            .unwrap_or(stem)
            .trim()
            .to_owned();
        let cwd = v
            .get("directory")
            .or_else(|| v.get("cwd"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if id.is_empty() || cwd.is_empty() {
            self.skipped = true;
            return;
        }
        let mut title = v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        let mut summary = extract_summary_field(v.get("summary"));
        let mut messages = Vec::new();

        if let Some(arr) = v.get("messages").and_then(Value::as_array) {
            for item in arr {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                extract_content_messages(role, item, &mut messages);
                if summary.is_empty() {
                    summary = extract_summary_field(item.get("summary"));
                }
            }
        }

        if let Some(sr) = storage_root {
            self.read_opencode_message_dir(
                &sr.join("message").join(&id),
                sr,
                &mut title,
                &mut summary,
                &mut messages,
            );
        }

        let first_user = messages
            .iter()
            .find(|(r, _)| r == "user")
            .map(|(_, t)| truncate_chars(&one_line(t), TITLE_CHARS))
            .unwrap_or_default();
        let first_assistant = messages
            .iter()
            .find(|(r, _)| r == "assistant")
            .map(|(_, t)| truncate_chars(&one_line(t), SNIPPET_CHARS))
            .unwrap_or_default();

        let resolved_title = first_non_empty(&[&title, &first_user, &summary, &id]);
        let resolved_summary = first_non_empty(&[&summary, &first_assistant]);
        self.insert(VaultDoc {
            agent: VaultAgent::OpenCode,
            id,
            cwd,
            modified,
            title: resolved_title,
            summary: resolved_summary,
            messages,
        });
    }

    fn read_opencode_message_dir(
        &mut self,
        msg_dir: &Path,
        storage_root: &Path,
        title: &mut String,
        summary: &mut String,
        messages: &mut Vec<(String, String)>,
    ) {
        let Ok(entries) = fs::read_dir(msg_dir) else {
            return;
        };
        let mut files: Vec<PathBuf> = entries
            .take(128)
            .flatten()
            .filter(|e| {
                e.file_type().is_ok_and(|k| k.is_file())
                    && e.path().extension().is_some_and(|ext| ext == "json")
            })
            .map(|e| e.path())
            .collect();
        files.sort();
        for file in files {
            if self.exhausted() {
                break;
            }
            self.nodes = self.nodes.saturating_add(1);
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            self.bytes = self.bytes.saturating_add(text.len());
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let role = v.get("role").and_then(Value::as_str).unwrap_or("user");
            if title.is_empty() {
                let s = extract_summary_field(v.get("summary"));
                if !s.is_empty() {
                    title.clone_from(&s);
                }
            }
            if summary.is_empty() {
                *summary = extract_summary_field(v.get("summary"));
            }
            extract_content_messages(role, &v, messages);
            if let Some(msg_id) = v.get("id").and_then(Value::as_str) {
                let part_dir = storage_root.join("part").join(msg_id);
                if let Ok(parts) = fs::read_dir(part_dir) {
                    for part in parts.take(32).flatten() {
                        if let Ok(pt) = fs::read_to_string(part.path())
                            && let Ok(pv) = serde_json::from_str::<Value>(&pt)
                            && let Some(t) = pv
                                .get("text")
                                .or_else(|| pv.get("content"))
                                .and_then(Value::as_str)
                            && !t.trim().is_empty()
                        {
                            messages.push((role.to_owned(), t.to_owned()));
                        }
                    }
                }
            }
        }
    }

    fn parse_opencode_jsonl(&mut self, path: &Path) {
        let mut id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        let mut cwd = String::new();
        let mut title = String::new();
        let mut summary = String::new();
        let mut messages = Vec::new();
        let mut saw_json = false;

        let res = self.read_lines(path, |line| {
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            saw_json = true;
            if let Some(s) = v
                .get("id")
                .or_else(|| v.get("sessionId"))
                .and_then(Value::as_str)
                && !s.trim().is_empty()
            {
                s.trim().clone_into(&mut id);
            }
            if cwd.is_empty()
                && let Some(c) = v
                    .get("directory")
                    .or_else(|| v.get("cwd"))
                    .and_then(Value::as_str)
                && !c.trim().is_empty()
            {
                c.trim().clone_into(&mut cwd);
            }
            if let Some(t) = v.get("title").and_then(Value::as_str)
                && !t.trim().is_empty()
            {
                t.trim().clone_into(&mut title);
            }
            let sum = extract_summary_field(v.get("summary"));
            if !sum.is_empty() {
                summary = sum;
            }
            let role = v.get("role").and_then(Value::as_str).unwrap_or("user");
            extract_content_messages(role, &v, &mut messages);
        });

        let Ok(modified) = res else {
            self.skipped = true;
            return;
        };
        if !saw_json || id.is_empty() || cwd.is_empty() {
            self.skipped = true;
            return;
        }
        let first_user = messages
            .iter()
            .find(|(r, _)| r == "user")
            .map(|(_, t)| truncate_chars(&one_line(t), TITLE_CHARS))
            .unwrap_or_default();
        let resolved_title = first_non_empty(&[&title, &first_user, &summary, &id]);
        self.insert(VaultDoc {
            agent: VaultAgent::OpenCode,
            id,
            cwd,
            modified,
            title: resolved_title,
            summary,
            messages,
        });
    }
}

fn first_non_empty(candidates: &[&str]) -> String {
    candidates
        .iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn extract_summary_field(val: Option<&Value>) -> String {
    let Some(val) = val else {
        return String::new();
    };
    if let Some(s) = val.as_str() {
        return s.trim().to_owned();
    }
    let title = val
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let body = val.get("body").and_then(Value::as_str).unwrap_or("").trim();
    first_non_empty(&[body, title])
}

fn extract_content_messages(role: &str, record: &Value, out: &mut Vec<(String, String)>) {
    let content = record
        .get("content")
        .or_else(|| record.get("text"))
        .or_else(|| record.get("message"));
    let Some(content) = content else {
        return;
    };
    if let Some(s) = content.as_str() {
        if !s.trim().is_empty() {
            out.push((role.to_owned(), s.to_owned()));
        }
        return;
    }
    if let Some(arr) = content.as_array() {
        let mut text_parts = Vec::new();
        for block in arr {
            if let Some(s) = block.as_str() {
                if !s.trim().is_empty() {
                    text_parts.push(s.to_owned());
                }
                continue;
            }
            let b_type = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            if b_type == "tool_use" {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let input = block.get("input").map(Value::to_string).unwrap_or_default();
                out.push(("tool".to_owned(), format!("{name} {input}")));
            } else if b_type == "tool_result" {
                if let Some(s) = block.get("content").and_then(Value::as_str)
                    && !s.trim().is_empty()
                {
                    out.push(("tool".to_owned(), s.to_owned()));
                }
            } else if let Some(t) = block
                .get("text")
                .or_else(|| block.get("content"))
                .and_then(Value::as_str)
                && !t.trim().is_empty()
            {
                text_parts.push(t.to_owned());
            }
        }
        if !text_parts.is_empty() {
            out.push((role.to_owned(), text_parts.join("\n")));
        }
    }
}

fn extract_agy_user_request(content: &str) -> String {
    let opener = "<USER_REQUEST>";
    let closer = "</USER_REQUEST>";
    if let Some(start) = content.find(opener) {
        let after = start.saturating_add(opener.len());
        if let Some(tail) = content.get(after..) {
            if let Some(end) = tail.find(closer)
                && let Some(inner) = tail.get(..end)
            {
                return inner.trim().to_owned();
            }
            return tail.trim().to_owned();
        }
    }
    content.trim().to_owned()
}

fn extract_agy_workspace_from_text(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Command Working Directory:") {
            let p = rest.trim();
            if p.starts_with('/') {
                return Some(p.to_owned());
            }
        }
    }
    None
}
