//! AI Vault(MK-181): Claude Code·Codex·Antigravity(`agy`)·`OpenCode` 멀티 에이전트 대화 기록 스캔 및 전문 검색.
//!
//! 요청 시에만 디스크를 읽으며 상주 워커·폴링·감시 프로세스를 두지 않는다.
//! 탐색 노드·바이트·시간 예산 안에서 대화 제목·요약·메시지 본문을 인덱싱하고,
//! 키워드·에이전트·경로 필터와 스니펫 추출·원클릭 재개 프로필 탐색을 수행한다.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::session::lock;

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
const MAX_CACHE_ENTRIES: usize = 2048;

#[derive(Debug, Clone)]
struct CachedVaultDoc {
    mtime_ns: u128,
    size: u64,
    doc: VaultDoc,
    has_primary_title: bool,
    partial: bool,
    skipped: bool,
}

static VAULT_CACHE: LazyLock<Mutex<BTreeMap<PathBuf, CachedVaultDoc>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

#[cfg(all(test, unix))]
static VAULT_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(test, unix))]
pub(crate) fn vault_cache_hits_for_test() -> usize {
    VAULT_CACHE_HITS.load(Ordering::Relaxed)
}

fn file_stamp(path: &Path) -> Option<(u128, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Some((mtime_ns, meta.len()))
}

fn file_mtime_ns(path: &Path) -> u128 {
    file_stamp(path).map_or(0, |(mtime_ns, _)| mtime_ns)
}

fn lookup_cached_doc(path: &Path, mtime_ns: u128, size: u64) -> Option<CachedVaultDoc> {
    let guard = VAULT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = guard.get(path)?;
    if entry.mtime_ns == mtime_ns && entry.size == size {
        #[cfg(all(test, unix))]
        VAULT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
        Some(entry.clone())
    } else {
        None
    }
}

/// 캐시가 붙드는 힙 총량 상한(데몬 메모리 예산 보호). 항목 수 상한과 별개로 바이트로 막는다.
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
static VAULT_CACHE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// jemalloc이 `bytes` 요청에 실제로 잡는 크기. 크기 등급은 2배 구간마다 4단계이고, 간격은 16바이트에서
/// 16KiB 페이지까지다(aarch64 macOS 실측: 585,120B 요청은 589,824B).
fn allocation(bytes: usize) -> usize {
    match bytes {
        0 => 0,
        1..=8 => 8,
        _ => bytes.next_multiple_of((bytes.next_power_of_two() / 8).clamp(16, 16 * 1024)),
    }
}

impl CachedVaultDoc {
    /// 맵 슬롯·경로 키·문자열·메시지 버퍼를 할당 크기로 센 항목의 힙. 짧은 메시지는 본문보다 구조가 크다.
    fn heap_bytes(&self, key: &Path) -> usize {
        let string = |s: &String| allocation(s.capacity());
        let doc = &self.doc;
        size_of::<(PathBuf, Self)>()
            + allocation(key.as_os_str().len())
            + string(&doc.id)
            + string(&doc.cwd)
            + string(&doc.title)
            + string(&doc.summary)
            + allocation(doc.messages.capacity() * size_of::<(String, String)>())
            + doc
                .messages
                .iter()
                .map(|(role, text)| string(role) + string(text))
                .sum::<usize>()
    }
}

fn store_cached_doc(path: &Path, entry: CachedVaultDoc) {
    store_cached_doc_capped(path, entry, MAX_CACHE_BYTES);
}

fn store_cached_doc_capped(path: &Path, entry: CachedVaultDoc, max_bytes: usize) {
    let mut guard = VAULT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // 락을 잡은 채로만 합계를 바꾼다.
    if let Some(old) = guard.remove(path) {
        VAULT_CACHE_BYTES.fetch_sub(old.heap_bytes(path), Ordering::Relaxed);
    }
    let bytes = entry.heap_bytes(path);
    if bytes > max_bytes {
        return;
    }
    while (guard.len() >= MAX_CACHE_ENTRIES
        || VAULT_CACHE_BYTES.load(Ordering::Relaxed) + bytes > max_bytes)
        && let Some(first_key) = guard.keys().next().cloned()
    {
        if let Some(evicted) = guard.remove(&first_key) {
            VAULT_CACHE_BYTES.fetch_sub(evicted.heap_bytes(&first_key), Ordering::Relaxed);
        }
    }
    VAULT_CACHE_BYTES.fetch_add(bytes, Ordering::Relaxed);
    guard.insert(path.to_path_buf(), entry);
}

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
    let roots = collect_roots(state_dir);
    search_in(&roots, query)
}

pub(super) fn collect_roots(state_dir: Option<&Path>) -> VaultRoots {
    let home = krill_proto::os::home_dir();
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
    roots
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
#[cfg(unix)]
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

pub(crate) fn agy_candidate_dirs(dir: &Path, include_sibling: bool) -> Vec<PathBuf> {
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

pub(super) fn opencode_candidate_bases(dir: &Path) -> Vec<PathBuf> {
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
                || cand
                    .join("conversations")
                    .join(format!("{}.db", target.id))
                    .is_file()
                || cand
                    .join("annotations")
                    .join(format!("{}.pbtxt", target.id))
                    .is_file()
        }),
        VaultAgent::OpenCode => {
            for path in super::opencode_sqlite::candidates(dir) {
                if super::opencode_sqlite::read(&path, Some(&target.id), false).is_ok_and(|batch| {
                    batch.sessions.iter().any(|session| {
                        target.cwd.is_empty()
                            || (krill_proto::agent_history::path_inside_or_equal(
                                &session.cwd,
                                &target.cwd,
                            ) && krill_proto::agent_history::path_inside_or_equal(
                                &target.cwd,
                                &session.cwd,
                            ))
                    })
                }) {
                    return true;
                }
            }
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
            .filter(|p| !p.is_empty())
            .cloned()
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
                // Native identity lookup needs the original spelling, particularly
                // in Windows directories with case sensitivity enabled.
                if !rest.is_empty() {
                    paths.push(rest.to_owned());
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
        self.paths.iter().any(|filter| {
            if krill_proto::agent_history::is_absolute_history_path(filter) {
                krill_proto::agent_history::path_inside_or_equal(cwd, filter)
            } else {
                normalize_path(cwd)
                    .to_lowercase()
                    .contains(&filter.to_lowercase())
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

/// Comparison only; never rewrite a transcript's stored filesystem path.
pub(crate) fn normalize_path(path: &str) -> String {
    krill_proto::agent_history::normalize_path_for_comparison(path)
}

fn one_line(s: &str) -> String {
    normalized_line(s, MAX_TEXT)
}

fn normalized_line(s: &str, limit: usize) -> String {
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
        if out.len() >= limit {
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
    // Search before applying the display cap: a hit may be beyond MAX_TEXT.
    let clean = normalized_line(text, usize::MAX);
    let chars: Vec<char> = clean.chars().collect();
    if chars.len() <= SNIPPET_CHARS {
        return clean;
    }
    let lower = clean.to_lowercase();
    let hit = terms
        .iter()
        .filter(|term| !term.is_empty())
        .filter_map(|term| {
            lower
                .find(term.as_str())
                .map(|start| (start, start + term.len()))
        })
        .min_by_key(|&(start, _)| start);
    // Unicode lowercasing can expand one source character (İ -> i + combining dot).
    // Map the lowercase byte range back to the original character range.
    let (match_start, match_end) = hit.map_or((0, 0), |(start, end)| {
        let mut offset = 0;
        let mut first = None;
        let mut last = 0;
        for (i, ch) in chars.iter().enumerate() {
            let next = offset + ch.to_lowercase().map(char::len_utf8).sum::<usize>();
            if next > start && offset < end {
                first.get_or_insert(i);
                last = i + 1;
            }
            offset = next;
            if offset >= end {
                break;
            }
        }
        (first.unwrap_or(0), last)
    });
    let width = SNIPPET_CHARS.max(match_end.saturating_sub(match_start));
    let context = width.saturating_sub(match_end.saturating_sub(match_start)) / 2;
    let start = match_start
        .saturating_sub(context)
        .min(chars.len().saturating_sub(width));
    let end = start.saturating_add(width).min(chars.len());
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
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        let modified = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let file_len = meta.len();
        self.truncated |= file_len > u64::try_from(FILE_BYTES).unwrap_or(u64::MAX);
        let limit =
            u64::try_from(FILE_BYTES.min(MAX_BYTES.saturating_sub(self.bytes))).unwrap_or(u64::MAX);
        if limit == 0 {
            self.truncated = true;
            return Ok(modified);
        }

        let mut line = Vec::new();
        if file_len <= limit || limit < 2 {
            let mut reader = BufReader::with_capacity(8192, file.take(limit));
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
            return Ok(modified);
        }

        self.truncated = true;
        // 파일 크기가 FILE_BYTES(512KiB) 예산을 초과할 때는 Head(절반) + Tail(절반)을 분할해 읽는다.
        // 앞부분의 세션 메타데이터(sessionId·cwd·첫 프롬프트)와 뒷부분의 최신 대화·customTitle·summary를 함께 보존한다.
        let head_budget = limit / 2;
        let tail_budget = limit.saturating_sub(head_budget);
        let head_read = {
            let mut head_reader = BufReader::with_capacity(8192, (&mut file).take(head_budget));
            loop {
                line.clear();
                match head_reader.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => on_line(&line),
                    Err(_) => {
                        self.skipped = true;
                        break;
                    }
                }
            }
            head_budget.saturating_sub(head_reader.get_ref().limit())
        };

        let tail_start = file_len.saturating_sub(tail_budget);
        let mut tail_read = 0u64;
        if tail_start > 0
            && file
                .seek(SeekFrom::Start(tail_start.saturating_sub(1)))
                .is_ok()
        {
            let mut prev = [0u8; 1];
            let starts_mid_line = file.read_exact(&mut prev).is_ok() && prev[0] != b'\n';
            let mut tail_reader = BufReader::with_capacity(8192, (&mut file).take(tail_budget));
            if starts_mid_line {
                line.clear();
                drop(tail_reader.read_until(b'\n', &mut line));
            }
            loop {
                line.clear();
                match tail_reader.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => on_line(&line),
                    Err(_) => {
                        self.skipped = true;
                        break;
                    }
                }
            }
            tail_read = tail_budget.saturating_sub(tail_reader.get_ref().limit());
        }

        let total_read = head_read.saturating_add(tail_read);
        self.bytes = self
            .bytes
            .saturating_add(usize::try_from(total_read).unwrap_or(usize::MAX));
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
            let mut candidates: Vec<(u128, String, PathBuf)> = Vec::new();
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
                    candidates.push((file_mtime_ns(&transcript), conv_id, transcript));
                }
            }
            candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            for (_, conv_id, transcript) in candidates {
                if self.exhausted() {
                    break;
                }
                self.parse_agy_file(root, &transcript, Some(&conv_id), &history);
            }
        }
        if sessions.is_dir() {
            self.walk_jsonl(&sessions, 2, false, |this, path| {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                this.parse_agy_file(root, path, Some(&stem), &history);
            });
        }
    }

    fn scan_opencode(&mut self, root: &Path) {
        for path in super::opencode_sqlite::candidates(root) {
            if self.exhausted() {
                break;
            }
            match super::opencode_sqlite::read(&path, None, true) {
                Ok(batch) => {
                    self.truncated |= batch.truncated;
                    self.skipped |= batch.skipped;
                    self.bytes = self.bytes.saturating_add(batch.bytes);
                    for session in batch.sessions {
                        let mut messages = Vec::new();
                        for value in session.messages {
                            let role = value.get("role").and_then(Value::as_str).unwrap_or("user");
                            extract_content_messages(role, &value, &mut messages);
                            if let Some(parts) = value.get("parts").and_then(Value::as_array) {
                                for part in parts {
                                    extract_content_messages(role, part, &mut messages);
                                }
                            }
                        }
                        self.insert(VaultDoc {
                            agent: VaultAgent::OpenCode,
                            id: session.id,
                            cwd: session.cwd,
                            title: session.title,
                            modified: session.modified,
                            summary: String::new(),
                            messages,
                        });
                    }
                }
                Err(_) => self.skipped = true,
            }
        }
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
        let mut files = Vec::new();
        self.collect_jsonl_files(path, depth, codex_rollout_only, &mut files);
        files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, file_path) in files {
            if self.exhausted() {
                break;
            }
            on_file(self, &file_path);
        }
    }

    fn collect_jsonl_files(
        &mut self,
        path: &Path,
        depth: usize,
        codex_rollout_only: bool,
        files: &mut Vec<(u128, PathBuf)>,
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
        let mut items: Vec<_> = dir.collect();
        if codex_rollout_only {
            items.sort_unstable_by(|a, b| {
                let a_name = a.as_ref().ok().map(fs::DirEntry::file_name);
                let b_name = b.as_ref().ok().map(fs::DirEntry::file_name);
                b_name.cmp(&a_name)
            });
        }
        for item in items {
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
                self.collect_jsonl_files(
                    &item.path(),
                    depth.saturating_sub(1),
                    codex_rollout_only,
                    files,
                );
            } else if kind.is_file() {
                let p = item.path();
                if p.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                if codex_rollout_only && !name_str.starts_with("rollout-") {
                    continue;
                }
                files.push((file_mtime_ns(&p), p));
            }
        }
    }

    fn parse_claude_file(&mut self, path: &Path) {
        let stamp = file_stamp(path);
        if let Some((mtime_ns, size)) = stamp
            && let Some(cached) = lookup_cached_doc(path, mtime_ns, size)
        {
            self.truncated |= cached.partial;
            self.skipped |= cached.skipped;
            self.insert(cached.doc);
            return;
        }

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
            if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
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
                        && !is_injected_user_context(c)
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
        let doc = VaultDoc {
            agent: VaultAgent::Claude,
            id,
            cwd,
            modified,
            title,
            summary: sum,
            messages,
        };
        if let Some((mtime_ns, size)) = stamp {
            store_cached_doc(
                path,
                CachedVaultDoc {
                    partial: self.truncated,
                    skipped: self.skipped,
                    mtime_ns,
                    size,
                    doc: doc.clone(),
                    has_primary_title: true,
                },
            );
        }
        self.insert(doc);
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
        let stamp = file_stamp(path);
        if let Some((mtime_ns, size)) = stamp
            && let Some(cached) = lookup_cached_doc(path, mtime_ns, size)
        {
            self.truncated |= cached.partial;
            self.skipped |= cached.skipped;
            let mut doc = cached.doc;
            if !cached.has_primary_title
                && let Some(t) = titles.get(&doc.id)
                && !t.trim().is_empty()
            {
                t.trim().clone_into(&mut doc.title);
            }
            self.insert(doc);
            return;
        }

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
                    if role == "developer" || role == "system" {
                        return;
                    }
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
                        && (role != "user" || !is_injected_user_context(msg))
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
                if role != "developer"
                    && role != "system"
                    && (role != "user" || !is_injected_user_context(msg))
                {
                    messages.push((role.to_owned(), msg.to_owned()));
                }
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
        let doc = VaultDoc {
            agent: VaultAgent::Codex,
            id,
            cwd,
            modified,
            title,
            summary: sum,
            messages,
        };
        if let Some((mtime_ns, size)) = stamp {
            store_cached_doc(
                path,
                CachedVaultDoc {
                    partial: self.truncated,
                    skipped: self.skipped,
                    mtime_ns,
                    size,
                    doc: doc.clone(),
                    has_primary_title: !meta_title.is_empty(),
                },
            );
        }
        self.insert(doc);
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
        agy_root: &Path,
        path: &Path,
        fallback_id: Option<&str>,
        history: &BTreeMap<String, (String, String)>,
    ) {
        let stamp = file_stamp(path);
        if let Some((mtime_ns, size)) = stamp
            && let Some(cached) = lookup_cached_doc(path, mtime_ns, size)
        {
            self.truncated |= cached.partial;
            self.skipped |= cached.skipped;
            let mut doc = cached.doc;
            if !cached.has_primary_title {
                if let Some(ann_title) = read_agy_annotation_title(agy_root, &doc.id) {
                    doc.title = ann_title;
                } else if let Some((_, hist_display)) = history.get(&doc.id)
                    && !hist_display.is_empty()
                {
                    doc.title.clone_from(hist_display);
                }
            }
            self.insert(doc);
            return;
        }

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
                && let Some(extracted_cwd) = extract_agy_workspace_from_record(&v)
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
                        let cleaned = krill_proto::git::clean_user_prompt(&req);
                        let candidate = if cleaned.is_empty() { &req } else { &cleaned };
                        first_user = truncate_chars(&one_line(candidate), TITLE_CHARS);
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
        if title.is_empty()
            && let Some(ann_title) = read_agy_annotation_title(agy_root, &id)
        {
            title = ann_title;
        }
        let has_primary_title = !title.is_empty();
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
        let doc = VaultDoc {
            agent: VaultAgent::Agy,
            id,
            cwd,
            modified,
            title: resolved_title,
            summary: resolved_summary,
            messages,
        };
        if let Some((mtime_ns, size)) = stamp {
            store_cached_doc(
                path,
                CachedVaultDoc {
                    partial: self.truncated,
                    skipped: self.skipped,
                    mtime_ns,
                    size,
                    doc: doc.clone(),
                    has_primary_title,
                },
            );
        }
        self.insert(doc);
    }

    fn walk_opencode_dir(&mut self, dir: &Path, storage_root: Option<&Path>, depth: usize) {
        let mut files = Vec::new();
        self.collect_opencode_files(dir, depth, &mut files);
        files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, path, is_json) in files {
            if self.exhausted() {
                break;
            }
            if is_json {
                self.parse_opencode_json(&path, storage_root);
            } else {
                self.parse_opencode_jsonl(&path);
            }
        }
    }

    fn collect_opencode_files(
        &mut self,
        dir: &Path,
        depth: usize,
        files: &mut Vec<(u128, PathBuf, bool)>,
    ) {
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
                self.collect_opencode_files(&path, depth.saturating_sub(1), files);
            } else if kind.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "json" {
                    files.push((file_mtime_ns(&path), path, true));
                } else if ext == "jsonl" {
                    files.push((file_mtime_ns(&path), path, false));
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
        let stamp = file_stamp(path);
        if let Some((mtime_ns, size)) = stamp
            && let Some(cached) = lookup_cached_doc(path, mtime_ns, size)
        {
            self.truncated |= cached.partial;
            self.skipped |= cached.skipped;
            self.insert(cached.doc);
            return;
        }

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
        let doc = VaultDoc {
            agent: VaultAgent::OpenCode,
            id,
            cwd,
            modified,
            title: resolved_title,
            summary,
            messages,
        };
        if let Some((mtime_ns, size)) = stamp {
            store_cached_doc(
                path,
                CachedVaultDoc {
                    partial: self.truncated,
                    skipped: self.skipped,
                    mtime_ns,
                    size,
                    doc: doc.clone(),
                    has_primary_title: true,
                },
            );
        }
        self.insert(doc);
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
    if role == "developer" || role == "system" {
        return;
    }
    let content = record
        .get("content")
        .or_else(|| record.get("text"))
        .or_else(|| record.get("message"));
    let Some(content) = content else {
        return;
    };
    if let Some(s) = content.as_str() {
        if !s.trim().is_empty() && (role != "user" || !is_injected_user_context(s)) {
            out.push((role.to_owned(), s.to_owned()));
        }
        return;
    }
    if let Some(arr) = content.as_array() {
        let mut text_parts = Vec::new();
        for block in arr {
            if let Some(s) = block.as_str() {
                if !s.trim().is_empty() && (role != "user" || !is_injected_user_context(s)) {
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
                && (role != "user" || !is_injected_user_context(t))
            {
                text_parts.push(t.to_owned());
            }
        }
        if !text_parts.is_empty() {
            out.push((role.to_owned(), text_parts.join("\n")));
        }
    }
}

/// 에이전트 런타임(`codex`·`claude`·`opencode`)이 `role: "user"` 메시지로 주입하는
/// 시스템 지침·환경 컨텍스트·스킬 본문·이미지 래퍼·알림 블록인지 판별한다.
pub(crate) fn is_injected_user_context(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.starts_with("# AGENTS.md instructions for ")
        || (trimmed.starts_with("# ")
            && trimmed.contains("\n<INSTRUCTIONS>")
            && trimmed.ends_with("</INSTRUCTIONS>"))
    {
        return true;
    }
    for prefix in [
        "<INSTRUCTIONS>",
        "<user_instructions>",
        "<instructions>",
        "<environment_context>",
        "<permissions instructions>",
        "<collaboration_mode>",
        "<skills_instructions>",
        "<multi_agent_role>",
        "<multi_agent_mode>",
        "<subagent_notification>",
        "<task-notification>",
        "<local-command-caveat>",
        "<local-command-stdout>",
        "<command-name>",
        "<command-message>",
        "<system-reminder>",
    ] {
        if trimmed.starts_with(prefix) {
            return true;
        }
    }
    if trimmed.starts_with("<skill>\n")
        || (trimmed.starts_with("<skill>") && trimmed.ends_with("</skill>"))
        || (trimmed.starts_with("<CONTEXT_SUMMARY>") && trimmed.ends_with("</CONTEXT_SUMMARY>"))
    {
        return true;
    }
    trimmed == "<image>"
        || trimmed == "</image>"
        || (trimmed.starts_with("<image ") && trimmed.ends_with('>'))
}

pub(super) fn extract_agy_user_request(content: &str) -> String {
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

/// Protobuf text-format 문자열 리터럴(`"..."`)의 C/octal/hex 이스케이프를 UTF-8로 복원한다.
fn unescape_pbtxt_string(quoted: &str) -> String {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(quoted);
    let mut iter = inner.bytes().peekable();
    let mut out = Vec::with_capacity(inner.len());
    while let Some(b) = iter.next() {
        if b == b'\\'
            && let Some(esc) = iter.next()
        {
            match esc {
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'\\' | b'"' | b'\'' => out.push(esc),
                b'0'..=b'7' => {
                    let mut val = u32::from(esc.saturating_sub(b'0'));
                    for _ in 0..2 {
                        if let Some(&next) = iter.peek()
                            && (b'0'..=b'7').contains(&next)
                        {
                            iter.next();
                            val = val
                                .saturating_mul(8)
                                .saturating_add(u32::from(next.saturating_sub(b'0')));
                        } else {
                            break;
                        }
                    }
                    if let Ok(decoded) = u8::try_from(val) {
                        out.push(decoded);
                    }
                }
                b'x' | b'X' => {
                    let h1 = iter.peek().copied().and_then(|c| (c as char).to_digit(16));
                    if let Some(d1) = h1 {
                        iter.next();
                        if let Some(d2) =
                            iter.peek().copied().and_then(|c| (c as char).to_digit(16))
                        {
                            iter.next();
                            if let Ok(decoded) =
                                u8::try_from(d1.saturating_mul(16).saturating_add(d2))
                            {
                                out.push(decoded);
                            }
                        } else if let Ok(decoded) = u8::try_from(d1) {
                            out.push(decoded);
                        }
                    } else {
                        out.push(esc);
                    }
                }
                other => out.push(other),
            }
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn truncate_live_title(s: &str) -> Option<String> {
    const ELLIPSIS: char = '…';
    let max_bytes = krill_proto::control::MAX_TITLE_TEXT;
    if s.len() <= max_bytes && s.chars().take(TITLE_CHARS + 1).count() <= TITLE_CHARS {
        return krill_proto::control::display_title(s);
    }
    let body_budget = max_bytes.saturating_sub(ELLIPSIS.len_utf8());
    let mut out = String::with_capacity(max_bytes);
    for (i, ch) in s.chars().enumerate() {
        if i >= TITLE_CHARS || out.len().saturating_add(ch.len_utf8()) > body_budget {
            out.push(ELLIPSIS);
            break;
        }
        out.push(ch);
    }
    krill_proto::control::display_title(&out)
}

/// Antigravity `annotations/<id>.pbtxt`의 `title:"..."` 줄을 파싱한다.
pub(crate) fn parse_pbtxt_title(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("title:") else {
            continue;
        };
        let rest = rest.trim();
        if !rest.starts_with('"') || !rest.ends_with('"') || rest.len() < 2 {
            continue;
        }
        let decoded =
            serde_json::from_str::<String>(rest).unwrap_or_else(|_| unescape_pbtxt_string(rest));
        let cleaned = krill_proto::git::clean_user_prompt(&decoded);
        let candidate = if cleaned.is_empty() {
            decoded.as_str()
        } else {
            cleaned.as_str()
        };
        let single = one_line(candidate);
        if single.chars().any(char::is_alphanumeric) && !single.starts_with('{') {
            return truncate_live_title(&single);
        }
    }
    None
}

pub(crate) fn read_agy_annotation_title(agy_root: &Path, conv_id: &str) -> Option<String> {
    if !crate::agent::control::valid_session_id(conv_id) {
        return None;
    }
    for cand in agy_candidate_dirs(agy_root, true) {
        let path = cand.join("annotations").join(format!("{conv_id}.pbtxt"));
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let mut buf = Vec::with_capacity(256);
        if file.take(4096).read_to_end(&mut buf).is_ok()
            && let Some(title) = parse_pbtxt_title(&buf)
        {
            return Some(title);
        }
    }
    None
}

pub(crate) fn agy_root_and_id_from_path(path: &Path) -> Option<(PathBuf, String)> {
    if let Some(parent_name) = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let conv_id = match parent_name {
            "presence" => file_name.strip_suffix(".lock"),
            "conversations" => file_name
                .strip_suffix(".db-wal")
                .or_else(|| file_name.strip_suffix(".db-shm"))
                .or_else(|| file_name.strip_suffix(".db")),
            "annotations" => file_name.strip_suffix(".pbtxt"),
            "sessions" => file_name.strip_suffix(".jsonl"),
            _ => None,
        };
        if let Some(conv_id) = conv_id
            && crate::agent::control::valid_session_id(conv_id)
        {
            return Some((path.parent()?.parent()?.to_path_buf(), conv_id.to_owned()));
        }
    }
    for ancestor in path.ancestors() {
        if ancestor
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("brain")
        {
            let conv_id = ancestor.file_name()?.to_str()?;
            if crate::agent::control::valid_session_id(conv_id) {
                return Some((
                    ancestor.parent()?.parent()?.to_path_buf(),
                    conv_id.to_owned(),
                ));
            }
        }
    }
    None
}

fn agy_root_and_id_from_vnodes(paths: &[PathBuf]) -> Option<(PathBuf, String)> {
    for p in paths {
        if p.extension().and_then(|e| e.to_str()) == Some("lock")
            && let Some(found) = agy_root_and_id_from_path(p)
        {
            return Some(found);
        }
    }
    for p in paths {
        if let Some(found) = agy_root_and_id_from_path(p) {
            return Some(found);
        }
    }
    None
}

fn discover_agy_from_pids(root_pid: u32, fg_pid: Option<u32>) -> Option<(PathBuf, String)> {
    let mut seeds = Vec::new();
    let mut cur = fg_pid.unwrap_or(root_pid);
    for _ in 0..16 {
        if cur <= 1 || seeds.contains(&cur) {
            break;
        }
        seeds.push(cur);
        let paths = crate::sys::open_vnode_paths(cur);
        if let Some(found) = agy_root_and_id_from_vnodes(&paths) {
            return Some(found);
        }
        if cur == root_pid {
            break;
        }
        let Ok(parent) = crate::sys::parent_pid(cur) else {
            break;
        };
        if parent == cur {
            break;
        }
        cur = parent;
    }
    if root_pid > 1 && !seeds.contains(&root_pid) {
        seeds.push(root_pid);
        let paths = crate::sys::open_vnode_paths(root_pid);
        if let Some(found) = agy_root_and_id_from_vnodes(&paths) {
            return Some(found);
        }
    }
    if !seeds.is_empty()
        && let Ok(pids) = crate::sys::all_pids()
    {
        let mut seen = seeds;
        let mut frontier = seen.clone();
        for _ in 0..3 {
            let mut next = Vec::new();
            for &pid in &pids {
                if pid <= 1 || seen.contains(&pid) {
                    continue;
                }
                if crate::sys::parent_pid(pid).is_ok_and(|ppid| frontier.contains(&ppid)) {
                    seen.push(pid);
                    next.push(pid);
                    let paths = crate::sys::open_vnode_paths(pid);
                    if let Some(found) = agy_root_and_id_from_vnodes(&paths) {
                        return Some(found);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
    }
    None
}

fn agy_transcript_for_conv(agy_root: &Path, conv_id: &str) -> Option<PathBuf> {
    let conv_dir = agy_root.join("brain").join(conv_id);
    let logs = conv_dir.join(".system_generated/logs");
    [
        logs.join("transcript.jsonl"),
        logs.join("transcript_full.jsonl"),
        conv_dir.join("transcript.jsonl"),
        agy_root.join("sessions").join(format!("{conv_id}.jsonl")),
        agy_root.join("conversations").join(format!("{conv_id}.db")),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

pub(crate) fn cached_live_agy_identity(
    session: krill_proto::SessionId,
) -> Option<(PathBuf, String, PathBuf)> {
    let cache_mutex = AGY_LIVE_TITLE_CACHE.get()?;
    let (agy_root, conv_id) = {
        let cache = lock(cache_mutex);
        let entry = cache.get(&session.0)?;
        (entry.agy_root.clone()?, entry.conv_id.clone()?)
    };
    if !crate::agent::control::valid_session_id(&conv_id) {
        return None;
    }
    let path = agy_transcript_for_conv(&agy_root, &conv_id)
        .unwrap_or_else(|| agy_root.join("conversations").join(format!("{conv_id}.db")));
    Some((agy_root, conv_id, path))
}

pub(crate) fn discover_live_agy_identity(
    root_pid: u32,
    fg_pid: Option<u32>,
) -> Option<(PathBuf, String, PathBuf)> {
    let (agy_root, conv_id) = discover_agy_from_pids(root_pid, fg_pid)?;
    let path = agy_transcript_for_conv(&agy_root, &conv_id)
        .unwrap_or_else(|| agy_root.join("conversations").join(format!("{conv_id}.db")));
    Some((agy_root, conv_id, path))
}

fn read_agy_first_user_prompt(agy_root: &Path, conv_id: &str) -> Option<String> {
    if !crate::agent::control::valid_session_id(conv_id) {
        return None;
    }
    for cand in agy_candidate_dirs(agy_root, true) {
        let conv_dir = cand.join("brain").join(conv_id);
        let logs = conv_dir.join(".system_generated/logs");
        for file_path in [
            logs.join("transcript.jsonl"),
            logs.join("transcript_full.jsonl"),
            conv_dir.join("transcript.jsonl"),
        ] {
            let Ok(file) = File::open(&file_path) else {
                continue;
            };
            let mut reader = BufReader::with_capacity(8192, file.take(64 * 1024));
            let mut line = Vec::new();
            for _ in 0..64 {
                line.clear();
                let Ok(n) = reader.read_until(b'\n', &mut line) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                let Ok(v) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                let source = v.get("source").and_then(Value::as_str).unwrap_or("");
                let rec_type = v.get("type").and_then(Value::as_str).unwrap_or("");
                let role = v.get("role").and_then(Value::as_str).unwrap_or("");
                if matches!(source, "USER_EXPLICIT" | "USER")
                    || matches!(rec_type, "USER_INPUT" | "REQUEST")
                    || role == "user"
                {
                    let content = v
                        .get("content")
                        .or_else(|| v.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let req = extract_agy_user_request(content);
                    let cleaned = krill_proto::git::clean_user_prompt(&req);
                    let candidate = if cleaned.is_empty() {
                        req.as_str()
                    } else {
                        cleaned.as_str()
                    };
                    let single = one_line(candidate);
                    if !single.is_empty() {
                        return truncate_live_title(&single);
                    }
                }
            }
        }
    }
    None
}

struct AgyLiveTitleEntry {
    agy_root: Option<PathBuf>,
    conv_id: Option<String>,
    title: Option<String>,
    pbtxt_mtime_ns: u128,
    checked_at: Instant,
}

static AGY_LIVE_TITLE_CACHE: OnceLock<Mutex<BTreeMap<u64, AgyLiveTitleEntry>>> = OnceLock::new();

/// 실행 중인 Antigravity(`agy`) 세션의 대화 제목·요약을 조회한다.
/// OSC 0/2 터미널 타이틀을 내보내지 않는 `agy`를 위해 `annotations/<id>.pbtxt`와 트랜스크립트 첫 프롬프트를 읽는다.
pub(crate) fn resolve_live_agy_title(
    session: krill_proto::SessionId,
    root_pid: u32,
    fg_pid: Option<u32>,
    exited: bool,
    stored: Option<&crate::agent::control::StoredLaunch>,
) -> (Option<String>, Option<(String, Option<PathBuf>)>) {
    let cache_mutex = AGY_LIVE_TITLE_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let needs_store_binding =
        stored.is_some_and(|s| s.session_id.is_none() || s.transcript_path.is_none());
    let cached_snapshot = {
        let cache = lock(cache_mutex);
        if let Some(entry) = cache.get(&session.0) {
            if !needs_store_binding
                && entry.checked_at.elapsed() < Duration::from_secs(2)
                && entry.title.is_some()
            {
                return (entry.title.clone(), None);
            }
            if !needs_store_binding && entry.checked_at.elapsed() < Duration::from_secs(1) {
                return (None, None);
            }
            Some((
                entry.agy_root.clone(),
                entry.conv_id.clone(),
                entry.title.clone(),
                entry.pbtxt_mtime_ns,
            ))
        } else {
            None
        }
    };

    let mut resolved_pair: Option<(PathBuf, String)> = None;
    let mut discovered_for_store: Option<(String, Option<PathBuf>)> = None;

    if let Some(s) = stored {
        if let Some(tp) = s.transcript_path.as_deref() {
            resolved_pair = agy_root_and_id_from_path(Path::new(tp));
        }
        if resolved_pair.is_none()
            && let Some(sid) = s
                .session_id
                .as_deref()
                .filter(|id| crate::agent::control::valid_session_id(id))
        {
            let mut bases = Vec::new();
            if let Some(acc) = &s.account
                && !acc.dir.is_empty()
            {
                bases.push(PathBuf::from(&acc.dir));
            }
            bases.push(crate::usage::accounts::default_home());
            'search: for base in bases {
                for cand in agy_candidate_dirs(&base, true) {
                    if cand
                        .join("annotations")
                        .join(format!("{sid}.pbtxt"))
                        .is_file()
                        || cand.join("brain").join(sid).is_dir()
                        || cand.join("presence").join(format!("{sid}.lock")).is_file()
                        || cand
                            .join("conversations")
                            .join(format!("{sid}.db"))
                            .is_file()
                        || cand.join("sessions").join(format!("{sid}.jsonl")).is_file()
                    {
                        resolved_pair = Some((cand, sid.to_owned()));
                        break 'search;
                    }
                }
            }
        }
    }

    if resolved_pair.is_none()
        && !exited
        && let Some((agy_root, conv_id)) = discover_agy_from_pids(root_pid, fg_pid)
    {
        resolved_pair = Some((agy_root, conv_id));
    }

    if resolved_pair.is_none()
        && let Some((Some(prev_root), Some(prev_id), _, _)) = &cached_snapshot
    {
        resolved_pair = Some((prev_root.clone(), prev_id.clone()));
    }

    if let Some((ref agy_root, ref conv_id)) = resolved_pair
        && stored.is_some_and(|s| {
            s.session_id.is_none()
                || s.transcript_path.is_none()
                || s.transcript_path
                    .as_deref()
                    .is_some_and(crate::agent::control::is_agy_conversation_db_path)
        })
    {
        let tp = agy_transcript_for_conv(agy_root, conv_id);
        if stored.is_some_and(|s| s.session_id.is_none()) || tp.is_some() {
            discovered_for_store = Some((conv_id.clone(), tp));
        }
    }

    let Some((agy_root, conv_id)) = resolved_pair else {
        let mut cache = lock(cache_mutex);
        if cache.len() >= 128 && !cache.contains_key(&session.0) {
            cache.pop_first();
        }
        cache.insert(
            session.0,
            AgyLiveTitleEntry {
                agy_root: None,
                conv_id: None,
                title: None,
                pbtxt_mtime_ns: 0,
                checked_at: Instant::now(),
            },
        );
        return (None, None);
    };

    let pbtxt_path = agy_root
        .join("annotations")
        .join(format!("{conv_id}.pbtxt"));
    let mtime_ns = file_mtime_ns(&pbtxt_path);
    if let Some((_, Some(prev_id), Some(prev_title), prev_mtime)) = &cached_snapshot
        && prev_id == &conv_id
        && (*prev_mtime == u128::MAX || (mtime_ns > 0 && *prev_mtime == mtime_ns))
    {
        let mut cache = lock(cache_mutex);
        if let Some(entry) = cache.get_mut(&session.0) {
            entry.checked_at = Instant::now();
        }
        return (Some(prev_title.clone()), discovered_for_store);
    }

    // Antigravity의 `annotations/<id>.pbtxt`는 한국어 대화도 항상 영어로 요약하므로
    // 사용자가 입력한 첫 프롬프트(첨부 경로·태그 제거본)를 우선 표시하고, 없을 때만 `.pbtxt`로 폴백한다.
    let prompt_title = read_agy_first_user_prompt(&agy_root, &conv_id);
    let recorded_mtime = if prompt_title.is_some() {
        u128::MAX
    } else {
        mtime_ns
    };
    let title = prompt_title.or_else(|| {
        if mtime_ns > 0 {
            read_agy_annotation_title(&agy_root, &conv_id)
        } else {
            None
        }
    });
    let recorded_mtime = if title.is_some() { recorded_mtime } else { 0 };
    let mut cache = lock(cache_mutex);
    if cache.len() >= 128 && !cache.contains_key(&session.0) {
        cache.pop_first();
    }
    cache.insert(
        session.0,
        AgyLiveTitleEntry {
            agy_root: Some(agy_root),
            conv_id: Some(conv_id),
            title: title.clone(),
            pbtxt_mtime_ns: recorded_mtime,
            checked_at: Instant::now(),
        },
    );
    (title, discovered_for_store)
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

pub(super) fn extract_agy_workspace_from_record(v: &Value) -> Option<String> {
    if let Some(c) = v
        .get("cwd")
        .or_else(|| v.get("workspace"))
        .and_then(Value::as_str)
        .map(|s| s.trim().trim_matches('"'))
        && c.starts_with('/')
    {
        return Some(c.to_owned());
    }
    let content = v
        .get("content")
        .or_else(|| v.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(cwd) = extract_agy_workspace_from_text(content) {
        return Some(cwd);
    }
    if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let Some(args) = call.get("args").or_else(|| call.get("arguments")) else {
                continue;
            };
            for key in ["Cwd", "cwd", "SearchDirectory", "DirectoryPath"] {
                if let Some(val) = args
                    .get(key)
                    .and_then(Value::as_str)
                    .map(|s| s.trim().trim_matches('"'))
                    && val.starts_with('/')
                {
                    return Some(val.to_owned());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod agy_title_tests {
    use super::*;

    #[test]
    fn parse_pbtxt_title_decodes_utf8_and_octal_escapes() {
        let raw = "last_user_view_time {\n  seconds: 1776937878\n}\ntitle: \"Fix \\354\\225\\210\\355\\213\\260\\352\\267\\270\\353\\236\\230\\353\\271\\204\\355\\213\\260 Title & \\\"Photo\\\"\"\n";
        assert_eq!(
            parse_pbtxt_title(raw.as_bytes()).as_deref(),
            Some("Fix 안티그래비티 Title & \"Photo\"")
        );
    }

    #[test]
    fn resolve_live_agy_title_prefers_cleaned_prompt_over_english_annotations_and_falls_back() {
        let root = std::env::temp_dir().join(format!("krill-agy-title-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let agy_root = root.join(".gemini/antigravity-cli");
        let conv_id = "c28a49fb-6183-4c21-b219-2d9e17531a04";
        let log_dir = agy_root
            .join("brain")
            .join(conv_id)
            .join(".system_generated/logs");
        fs::create_dir_all(&log_dir).unwrap();
        let transcript = log_dir.join("transcript.jsonl");

        let stored = crate::agent::control::StoredLaunch {
            profile: "agy".into(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: Some(conv_id.into()),
            transcript_path: Some(transcript.to_string_lossy().into_owned()),
            cwd: Vec::new(),
            hibernation_blocked: false,
            account: None,
        };

        let ann_dir = agy_root.join("annotations");
        fs::create_dir_all(&ann_dir).unwrap();
        fs::write(
            ann_dir.join(format!("{conv_id}.pbtxt")),
            "title: \"Fix Antigravity Session Titles & Mobile Photo Button\"\n",
        )
        .unwrap();

        let (ann_fallback, discovered) =
            resolve_live_agy_title(krill_proto::SessionId(9001), 0, None, false, Some(&stored));
        assert!(discovered.is_none());
        assert_eq!(
            ann_fallback.as_deref(),
            Some("Fix Antigravity Session Titles & Mobile Photo Button")
        );

        fs::write(
            &transcript,
            "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\n'/var/folders/nm/jz7t2dm539597y551gd3tzw40000gn/T/krill-paste/mobile-f14066a1.jpg' 안티그래비티도 제목인지 내용 요약 뜨게하고\\n</USER_REQUEST>\"}\n",
        )
        .unwrap();

        let (prompt_title, _) =
            resolve_live_agy_title(krill_proto::SessionId(9002), 0, None, false, Some(&stored));
        assert_eq!(
            prompt_title.as_deref(),
            Some("안티그래비티도 제목인지 내용 요약 뜨게하고")
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_live_agy_title_caps_multibyte_utf8_within_control_max_title_text() {
        use krill_proto::screen::Msg as _;

        let root =
            std::env::temp_dir().join(format!("krill-agy-title-utf8-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let agy_root = root.join(".gemini/antigravity-cli");
        let conv_id = "d3462946-8e91-41c8-a596-f077854ed0ac";
        let log_dir = agy_root
            .join("brain")
            .join(conv_id)
            .join(".system_generated/logs");
        fs::create_dir_all(&log_dir).unwrap();
        let transcript = log_dir.join("transcript.jsonl");
        let long_korean = "우리 서비스 광고 10만원 돌렸는데 매출이 0이야. ".repeat(8);
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\n{long_korean}\\n</USER_REQUEST>\"}}\n"
            ),
        )
        .unwrap();

        let stored = crate::agent::control::StoredLaunch {
            profile: "agy".into(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: Some(conv_id.into()),
            transcript_path: Some(transcript.to_string_lossy().into_owned()),
            cwd: Vec::new(),
            hibernation_blocked: false,
            account: None,
        };

        let (title, _) =
            resolve_live_agy_title(krill_proto::SessionId(9003), 0, None, false, Some(&stored));
        let title = title.expect("title resolved");
        assert!(title.len() <= krill_proto::control::MAX_TITLE_TEXT);
        assert!(title.ends_with('…'));

        let mut status = krill_proto::control::AgentStatus::UNKNOWN;
        status.title = Some(title);
        let reply =
            krill_proto::control::Reply::States(vec![krill_proto::control::SessionStatus {
                session: krill_proto::SessionId(9003),
                status,
            }]);
        let frame = reply.encode().expect("encode States");
        let decoded =
            krill_proto::control::Reply::decode(frame[4], frame.get(5..).expect("ipc body"))
                .expect("decode States must succeed for multibyte agy title");
        assert_eq!(decoded, reply);

        fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod excerpt_window_tests {
    use super::*;

    #[test]
    fn vault_excerpt_preserves_late_hit_and_balances_context() {
        let text = format!("{}needle{}", "앞".repeat(MAX_TEXT), "뒤".repeat(500));
        let snippet = extract_snippet(&text, &["needle".to_owned()]);
        assert!(snippet.contains("needle"));
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
        let prefix = snippet
            .split("needle")
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        assert!((60..80).contains(&prefix));
    }

    #[test]
    fn vault_excerpt_unicode_lowercase_expansion_and_long_hit() {
        let text = format!("{}TARGET{}", "İ".repeat(500), "한".repeat(500));
        assert!(extract_snippet(&text, &["target".to_owned()]).contains("TARGET"));
        let term = "검색".repeat(SNIPPET_CHARS);
        let text = format!("{}{}{}", "앞".repeat(200), term, "뒤".repeat(200));
        assert!(extract_snippet(&text, std::slice::from_ref(&term)).contains(&term));
        assert_eq!(extract_snippet("짧은 문장", &[]), "짧은 문장");
        assert!(extract_snippet(&"끝".repeat(300), &[]).ends_with('…'));
    }
}

#[cfg(test)]
mod path_scope_regressions {
    use super::*;
    #[test]
    fn query_paths_and_inline_drive_filters_use_directory_boundaries() {
        for query in [
            SearchQuery {
                paths: vec![r"C:\work\app".into()],
                ..Default::default()
            },
            SearchQuery {
                query: "path:C:/work/app".into(),
                ..Default::default()
            },
        ] {
            let parsed = ParsedQuery::parse(&query);
            assert!(parsed.matches_path(r"C:\WORK\app\sub"));
            assert!(!parsed.matches_path("c:/work/app-old"));
        }
        let parsed = ParsedQuery::parse(&SearchQuery {
            paths: vec!["/work/App".into()],
            ..Default::default()
        });
        assert!(!parsed.matches_path("/work/app"));
    }
    #[cfg(windows)]
    #[test]
    fn native_query_scope_keeps_case_sensitive_filesystem_identity() {
        let root =
            std::env::temp_dir().join(format!("krill-vault-query-case-{}", std::process::id()));
        let upper = root.join("Case");
        let lower = root.join("case");
        std::fs::create_dir_all(upper.join("sub")).unwrap();
        std::fs::create_dir_all(lower.join("sub")).unwrap();
        let same = same_file::is_same_file(&upper, &lower).unwrap();
        for requested in [&upper, &lower] {
            for query in [
                SearchQuery {
                    paths: vec![requested.display().to_string()],
                    ..Default::default()
                },
                SearchQuery {
                    query: format!("path:{}", requested.display()),
                    ..Default::default()
                },
            ] {
                let parsed = ParsedQuery::parse(&query);
                assert!(parsed.matches_path(&requested.join("sub").display().to_string()));
                let other = if requested == &upper { &lower } else { &upper };
                assert_eq!(
                    parsed.matches_path(&other.join("sub").display().to_string()),
                    same
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod cache_budget_tests {
    use super::*;

    fn entry(bytes: usize) -> CachedVaultDoc {
        CachedVaultDoc {
            mtime_ns: 1,
            size: 1,
            doc: VaultDoc {
                agent: VaultAgent::Claude,
                id: String::new(),
                cwd: String::new(),
                modified: 0,
                title: String::new(),
                summary: String::new(),
                messages: vec![(String::new(), "x".repeat(bytes))],
            },
            has_primary_title: true,
            partial: false,
            skipped: false,
        }
    }

    #[test]
    fn cache_is_bounded_by_bytes() {
        let (a, b, c) = (Path::new("/a"), Path::new("/b"), Path::new("/c"));
        store_cached_doc_capped(a, entry(600), 1000);
        let one = VAULT_CACHE_BYTES.load(Ordering::Relaxed);
        store_cached_doc_capped(b, entry(600), 1000);
        assert!(lookup_cached_doc(a, 1, 1).is_none(), "oldest entry evicted");
        assert!(lookup_cached_doc(b, 1, 1).is_some());
        store_cached_doc_capped(c, entry(2000), 1000);
        assert!(
            lookup_cached_doc(c, 1, 1).is_none(),
            "oversized doc not retained"
        );
        assert!(lookup_cached_doc(b, 1, 1).is_some());
        assert_eq!(
            VAULT_CACHE_BYTES.load(Ordering::Relaxed),
            one,
            "evicted entries give back exactly what they counted"
        );
    }

    /// 검색이 캐시에 남긴 실제 힙(jemalloc 스레드 카운터)이 예산 안이고, 회계가 그 힙을 따라간다.
    #[cfg(not(windows))]
    #[test]
    fn cache_budget_bounds_the_heap_it_retains() {
        use tikv_jemalloc_ctl::thread::{allocatedp, deallocatedp};
        let (allocated, deallocated) = (allocatedp::read().unwrap(), deallocatedp::read().unwrap());
        let heap = || i128::from(allocated.get()) - i128::from(deallocated.get());
        let root = std::env::temp_dir().join(format!("vault-cache-heap-{}", std::process::id()));
        // 42바이트 줄(한 글자 메시지)과 437바이트 줄 대화. 둘 다 캐시 예산보다 크다.
        for (content, files) in [(1, 16), (396, 20)] {
            fs::remove_dir_all(&root).ok();
            let dir = root.join("claude/projects/-tmp-heap");
            fs::create_dir_all(&dir).unwrap();
            let line = format!(
                "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}\n",
                "x".repeat(content)
            );
            for i in 0..files {
                let id = format!("{i:08x}-2222-4333-8444-555555555555");
                let mut body = format!("{{\"sessionId\":\"{id}\",\"cwd\":\"/tmp/heap\"}}\n");
                while body.len() + line.len() <= 500 * 1024 {
                    body.push_str(&line);
                }
                fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
            }
            let roots = VaultRoots {
                claude: vec![root.join("claude")],
                ..VaultRoots::default()
            };
            let clear = || {
                VAULT_CACHE.lock().unwrap().clear();
                VAULT_CACHE_BYTES.store(0, Ordering::Relaxed);
            };
            // 첫 검색은 지연 초기화되는 전역까지 할당하므로 비우고 다시 잰다.
            drop(search_in(&roots, &SearchQuery::default()));
            clear();
            let before = heap();
            let snapshot = search_in(&roots, &SearchQuery::default());
            assert!(
                !snapshot.truncated,
                "{content}: scan budget must not cut the corpus"
            );
            drop(snapshot);
            let retained = heap() - before;
            let counted = i128::try_from(VAULT_CACHE_BYTES.load(Ordering::Relaxed)).unwrap();
            let budget = i128::try_from(MAX_CACHE_BYTES).unwrap();
            assert!(
                retained <= budget && (retained - counted).abs() <= counted / 50,
                "{content}-byte messages: retained {retained} B, counted {counted} B, budget {budget} B"
            );
            clear();
        }
        fs::remove_dir_all(&root).ok();
    }
}
