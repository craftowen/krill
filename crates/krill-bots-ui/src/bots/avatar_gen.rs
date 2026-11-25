// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Local AI avatar generation via installed image-capable agent CLIs (`codex`, `grok`, `agy`).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use super::avatar::{PICTURE_MAX_BYTES, prepare_picture};
use crate::i18n::catalog::bots as text;

/// Maximum characters allowed in the avatar generation description box.
pub const MAX_AVATAR_DESCRIPTION_CHARS: usize = 500;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const RUN_TIMEOUT: Duration = Duration::from_mins(4);
const CAPABILITY_TTL: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const PROMPT_FILE: &str = "prompt.txt";

const ANTIGRAVITY_IMAGE_STEER: &str = "Use the image-generator subagent for this and wait for it to finish. Do not copy, move or save the image yourself.";

/// Agents whose CLI can create an image in a one-off read-only run, in preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvatarImageAgent {
    Codex,
    Grok,
    Agy,
}

impl AvatarImageAgent {
    pub const ALL: [Self; 3] = [Self::Codex, Self::Grok, Self::Agy];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Agy => "agy",
        }
    }

    #[must_use]
    pub const fn binary(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Agy => "agy",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "grok" => Some(Self::Grok),
            "agy" | "antigravity" => Some(Self::Agy),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> String {
        super::rpc::runtime_display_label(self.id())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarAgentStatusKind {
    Ready,
    SignedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvatarImageAgentStatus {
    pub agent: AvatarImageAgent,
    pub status: AvatarAgentStatusKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AvatarGenerationCapability {
    pub agents: Vec<AvatarImageAgentStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarGenerationFailure {
    Unavailable,
    RealPerson,
    SignedOut,
    UsageLimit,
    NoImage,
    TimedOut,
    Failed,
    Canceled,
}

impl AvatarGenerationFailure {
    #[must_use]
    pub fn message(self, agent: Option<AvatarImageAgent>) -> String {
        let name = agent.map_or_else(String::new, AvatarImageAgent::label);
        match self {
            Self::RealPerson => text::AVATAR_GEN_ERR_REAL_PERSON.get().to_owned(),
            Self::SignedOut => text::AVATAR_GEN_ERR_SIGNED_OUT.format(&[("agent", &name)]),
            Self::UsageLimit => text::AVATAR_GEN_ERR_USAGE_LIMIT.format(&[("agent", &name)]),
            Self::NoImage => text::AVATAR_GEN_ERR_NO_IMAGE.format(&[("agent", &name)]),
            Self::TimedOut => text::AVATAR_GEN_ERR_TIMED_OUT.format(&[("agent", &name)]),
            Self::Unavailable => text::AVATAR_GEN_ERR_UNAVAILABLE.get().to_owned(),
            Self::Failed | Self::Canceled => {
                text::AVATAR_GEN_ERR_FAILED.format(&[("agent", &name)])
            }
        }
    }
}

/// Generated avatar validated and scaled to a 512x512 PNG.
#[derive(Debug, Clone)]
pub struct GeneratedAvatarImage {
    pub png_bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum AvatarRunResult {
    Ok {
        agent: AvatarImageAgent,
        image: GeneratedAvatarImage,
    },
    Err {
        error: AvatarGenerationFailure,
        agent: Option<AvatarImageAgent>,
    },
}

/// Cancellable handle for an in-flight local avatar generation subprocess.
#[derive(Debug, Clone, Default)]
pub struct AvatarCancelHandle {
    canceled: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<u32>>>,
}

impl AvatarCancelHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        let pid = *self
            .child_pid
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(pid) = pid {
            kill_pid(pid);
        }
    }

    #[must_use]
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }

    fn set_pid(&self, pid: Option<u32>) {
        *self
            .child_pid
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = pid;
    }
}

#[cfg(unix)]
fn kill_pid(pid: u32) {
    if let Ok(raw) = i32::try_from(pid)
        && raw > 0
        && let Some(ipid) = rustix::process::Pid::from_raw(raw)
    {
        // Kill the process group / child so helper subagents also exit cleanly.
        rustix::process::kill_process_group(ipid, rustix::process::Signal::TERM).ok();
        rustix::process::kill_process(ipid, rustix::process::Signal::KILL).ok();
    }
}

#[cfg(windows)]
fn kill_pid(pid: u32) {
    crate::platform::background_command("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
}

/// Chooses the opening agent: remembered preference while offered, else first ready, else first.
#[must_use]
pub fn pick_avatar_agent(
    agents: &[AvatarImageAgentStatus],
    preferred: Option<AvatarImageAgent>,
) -> Option<AvatarImageAgent> {
    if let Some(pref) = preferred
        && agents.iter().any(|entry| entry.agent == pref)
    {
        return Some(pref);
    }
    agents
        .iter()
        .find(|entry| entry.status == AvatarAgentStatusKind::Ready)
        .or_else(|| agents.first())
        .map(|entry| entry.agent)
}

static REAL_PERSON_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let raw = [
        r"(?i)(^|\b(of|like|as|depicting) )(me|myself|us)\b",
        r"(?i)\b(me|myself|us) as\b",
        r"(?i)\b(draw|make|paint|show|create) (me|myself|us)\s*[.!]?\s*$",
        r"(?i)\b(my|our) (face|photo|picture|selfie|likeness)\b",
        r"(?i)\b(my|our) (wife|husband|partner|boyfriend|girlfriend|son|daughter|kids?|child|children|mom|mother|dad|father|brother|sister|friend|boss|coworker|colleague)\b",
        r"(?i)\b(real|actual) (person|people|human)\b",
        r"(?i)\b(celebrity|celebrities|politician)\b",
    ];
    raw.iter().filter_map(|pat| Regex::new(pat).ok()).collect()
});

/// True when a description explicitly asks for a real, identifiable person.
#[must_use]
pub fn describes_real_person(description: &str) -> bool {
    let normalized = description.split_whitespace().collect::<Vec<_>>().join(" ");
    REAL_PERSON_PATTERNS
        .iter()
        .any(|pat| pat.is_match(&normalized))
}

/// Fixed avatar generation prompt with the owner's subject description.
#[must_use]
pub fn avatar_generation_prompt(description: &str) -> String {
    let subject = description.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "Use your image generation tool to create exactly one image, then stop. \
        It is a profile avatar for an AI assistant in a chat app, shown small and cropped to a circle, \
        so it must read instantly at thumbnail size. \
        Subject: {subject} \
        Make it a square 1:1 image with the subject centered and filling most of the frame, \
        on a simple uncluttered background that runs to the edges. \
        Use a stylized character illustration with bold readable shapes and a limited palette, \
        unless the subject names another art style. \
        Do not draw a circle, badge, border or frame. No text, letters, logos, watermarks or interface elements. \
        Never depict a real, identifiable person, including public figures; \
        if the subject asks for one, make no image and reply REFUSED. \
        Do not run shell commands or edit files."
    )
}

#[derive(Debug)]
struct CachedCapability {
    at: Instant,
    value: AvatarGenerationCapability,
}

static CAPABILITY_CACHE: OnceLockCache = OnceLockCache::new();

struct OnceLockCache(std::sync::OnceLock<Mutex<Option<CachedCapability>>>);

impl OnceLockCache {
    const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    fn slot(&self) -> &Mutex<Option<CachedCapability>> {
        self.0.get_or_init(|| Mutex::new(None))
    }

    fn get_fresh(&self) -> Option<AvatarGenerationCapability> {
        let guard = self.slot().lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .as_ref()
            .filter(|entry| entry.at.elapsed() <= CAPABILITY_TTL)
            .map(|entry| entry.value.clone())
    }

    fn set(&self, value: AvatarGenerationCapability) {
        let mut guard = self.slot().lock().unwrap_or_else(PoisonError::into_inner);
        *guard = Some(CachedCapability {
            at: Instant::now(),
            value,
        });
    }

    fn clear(&self) {
        let mut guard = self.slot().lock().unwrap_or_else(PoisonError::into_inner);
        *guard = None;
    }
}

/// Returns the most recently cached capability if still warm (`<= 30s`).
#[must_use]
pub fn cached_capability() -> Option<AvatarGenerationCapability> {
    CAPABILITY_CACHE.get_fresh()
}

/// Probes installed avatar-capable CLIs (`codex`, `grok`, `agy`), using a 30s TTL cache.
pub fn probe_capability() -> AvatarGenerationCapability {
    if let Some(cached) = CAPABILITY_CACHE.get_fresh() {
        return cached;
    }
    let path = resolved_cli_path();
    let cap = probe_capability_with_path(path.as_deref());
    CAPABILITY_CACHE.set(cap.clone());
    cap
}

/// Clears cached capability so the next open or run re-probes sign-in state.
pub fn invalidate_capability_cache() {
    CAPABILITY_CACHE.clear();
}

#[must_use]
pub fn resolved_cli_path() -> Option<OsString> {
    let shell = default_shell_path();
    crate::onboarding::login_path(&shell).or_else(|| std::env::var_os("PATH"))
}

fn default_shell_path() -> OsString {
    #[cfg(unix)]
    {
        std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"))
    }
    #[cfg(windows)]
    {
        OsString::new()
    }
}

fn probe_capability_with_path(path: Option<&OsStr>) -> AvatarGenerationCapability {
    let (tx, rx) = std::sync::mpsc::channel();
    for agent in AvatarImageAgent::ALL {
        let tx = tx.clone();
        let path_owned = path.map(OsStr::to_os_string);
        std::thread::spawn(move || {
            let status = match agent {
                AvatarImageAgent::Codex => probe_codex(path_owned.as_deref()),
                AvatarImageAgent::Grok => probe_grok(path_owned.as_deref()),
                AvatarImageAgent::Agy => probe_agy(path_owned.as_deref()),
            };
            tx.send((agent, status)).ok();
        });
    }
    drop(tx);

    let mut probed = [None, None, None];
    for (agent, status) in rx {
        let idx = match agent {
            AvatarImageAgent::Codex => 0,
            AvatarImageAgent::Grok => 1,
            AvatarImageAgent::Agy => 2,
        };
        probed[idx] = status.map(|st| AvatarImageAgentStatus { agent, status: st });
    }
    AvatarGenerationCapability {
        agents: probed.into_iter().flatten().collect(),
    }
}

fn quick_output(binary: &str, args: &[&str], path: Option<&OsStr>) -> Option<String> {
    let mut cmd = crate::platform::background_command(binary);
    cmd.args(args);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    let out = crate::settings::child::output(&mut cmd, PROBE_TIMEOUT, 256 * 1024).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())
        .flatten()
}

/// Checks `codex features list` output for an enabled `image_generation` flag.
#[must_use]
pub fn codex_image_generation_enabled(feature_list: &str) -> bool {
    static RE: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"(?m)^image_generation\s+.*\btrue\s*$").ok());
    RE.as_ref().is_some_and(|re| re.is_match(feature_list))
}

fn probe_codex(path: Option<&OsStr>) -> Option<AvatarAgentStatusKind> {
    let features = quick_output("codex", &["features", "list"], path)?;
    if !codex_image_generation_enabled(&features) {
        return None;
    }
    let status = if quick_output("codex", &["login", "status"], path).is_some() {
        AvatarAgentStatusKind::Ready
    } else {
        AvatarAgentStatusKind::SignedOut
    };
    Some(status)
}

fn resolve_grok_home_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("GROK_HOME").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(v));
    }
    krill_proto::os::home_dir().map(|h| h.join(".grok"))
}

fn resolve_grok_sessions_dir() -> Option<PathBuf> {
    resolve_grok_home_dir().map(|h| h.join("sessions"))
}

fn probe_grok(path: Option<&OsStr>) -> Option<AvatarAgentStatusKind> {
    quick_output("grok", &["--version"], path)?;
    let has_key = std::env::var_os("XAI_API_KEY").is_some_and(|v| !v.is_empty());
    let has_auth = resolve_grok_home_dir().is_some_and(|h| h.join("auth.json").is_file());
    Some(if has_key || has_auth {
        AvatarAgentStatusKind::Ready
    } else {
        AvatarAgentStatusKind::SignedOut
    })
}

fn probe_agy(path: Option<&OsStr>) -> Option<AvatarAgentStatusKind> {
    quick_output("agy", &["--version"], path).map(|_| AvatarAgentStatusKind::Ready)
}

fn json_lines(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Extracts Codex's emitted thread id so we can look up `$CODEX_HOME/generated_images/<id>`.
#[must_use]
pub fn codex_thread_id(stdout: &str) -> Option<String> {
    for record in json_lines(stdout) {
        if record.get("type").and_then(Value::as_str) == Some("thread.started")
            && let Some(id) = record.get("thread_id").and_then(Value::as_str)
        {
            return is_safe_id(id).then(|| id.to_owned());
        }
    }
    None
}

fn collect_conversation_ids(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_conversation_ids(item, into);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                if k == "conversation_id"
                    && let Some(id) = v.as_str()
                    && is_safe_id(id)
                    && !into.iter().any(|existing| existing == id)
                {
                    into.push(id.to_owned());
                } else {
                    collect_conversation_ids(v, into);
                }
            }
        }
        _ => {}
    }
}

/// Extracts `conversation_id` entries emitted by `agy --output-format stream-json`.
#[must_use]
pub fn antigravity_conversation_ids(stdout: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for record in json_lines(stdout) {
        collect_conversation_ids(&record, &mut ids);
    }
    ids
}

fn grok_tool_path(text: &str) -> Option<String> {
    let val: Value = serde_json::from_str(text).ok()?;
    val.get("path").and_then(Value::as_str).map(str::to_owned)
}

/// Extracts saved image paths reported by Grok's completed `image_gen` tool calls.
#[must_use]
pub fn grok_image_paths(stdout: &str) -> Vec<String> {
    let mut calls = std::collections::BTreeSet::new();
    let mut paths = Vec::new();
    for record in json_lines(stdout) {
        let typ = record.get("type").and_then(Value::as_str);
        if typ == Some("tool_call")
            && record.get("toolName").and_then(Value::as_str) == Some("image_gen")
        {
            if let Some(id) = record.get("toolCallId").and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| Some(v.to_string()))
            }) {
                calls.insert(id);
            }
            continue;
        }
        if typ != Some("tool_call_update")
            || record.get("status").and_then(Value::as_str) != Some("completed")
        {
            continue;
        }
        let call_id = record.get("toolCallId").and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| Some(v.to_string()))
        });
        if !call_id.as_deref().is_some_and(|id| calls.contains(id)) {
            continue;
        }
        if let Some(entries) = record.get("content").and_then(Value::as_array) {
            for entry in entries {
                if let Some(inner) = entry.get("content")
                    && inner.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = inner.get("text").and_then(Value::as_str)
                    && let Some(path) = grok_tool_path(text)
                {
                    paths.push(path);
                }
            }
        }
    }
    paths
}

fn string_field<'a>(record: &'a Value, key: &str) -> &'a str {
    record
        .get(key)
        .and_then(|v| {
            v.as_str()
                .or_else(|| v.get("message").and_then(Value::as_str))
        })
        .unwrap_or("")
}

#[must_use]
pub fn agent_error_text(stdout: &str) -> String {
    let mut parts = Vec::new();
    for record in json_lines(stdout) {
        let typ = record.get("type").and_then(Value::as_str);
        let ev = record.get("event").and_then(Value::as_str);
        if matches!(typ, Some("error" | "turn.failed")) || ev == Some("error") {
            parts.push(format!(
                "{} {}",
                string_field(&record, "message"),
                string_field(&record, "error")
            ));
        } else if ev == Some("result")
            && let Some(result) = record.get("result")
            && result.get("status").and_then(Value::as_str) != Some("SUCCESS")
        {
            parts.push(format!(
                "{} {}",
                string_field(result, "error"),
                string_field(result, "response")
            ));
        }
    }
    parts.join("\n")
}

#[must_use]
pub fn agent_reply_text(stdout: &str) -> String {
    let mut out = String::new();
    for record in json_lines(stdout) {
        if record.get("type").and_then(Value::as_str) == Some("text")
            && let Some(data) = record.get("data").and_then(Value::as_str)
        {
            out.push_str(data);
        } else if record.get("event").and_then(Value::as_str) == Some("result")
            && let Some(result) = record.get("result")
        {
            out.push_str(string_field(result, "response"));
            out.push('\n');
        } else if record.get("type").and_then(Value::as_str) == Some("item.completed")
            && let Some(item) = record.get("item")
            && item.get("type").and_then(Value::as_str) == Some("agent_message")
            && let Some(text) = item.get("text").and_then(Value::as_str)
        {
            out.push_str(text);
            out.push('\n');
        }
    }
    out
}

/// Classifies an unsuccessful agent run output into an [`AvatarGenerationFailure`].
#[must_use]
pub fn classify_avatar_agent_failure(output: &str) -> AvatarGenerationFailure {
    static USAGE_LIMIT: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r"(?i)usage limit|rate limit|quota|too many requests|\b429\b|credits? (exhausted|remaining)|out of credits").ok()
    });
    static SIGNED_OUT: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r"(?i)not logged in|not signed in|log ?in again|sign ?in again|please (log|sign) ?in|unauthori[sz]ed|\b401\b|authentication (failed|required)|invalid api key|token (expired|revoked)").ok()
    });
    if USAGE_LIMIT.as_ref().is_some_and(|re| re.is_match(output)) {
        return AvatarGenerationFailure::UsageLimit;
    }
    if SIGNED_OUT.as_ref().is_some_and(|re| re.is_match(output)) {
        return AvatarGenerationFailure::SignedOut;
    }
    AvatarGenerationFailure::Failed
}

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn create() -> std::io::Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let base = std::env::temp_dir();
        for _ in 0..16 {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = base.join(format!("krill-avatar-{}-{n}", std::process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "failed to create temp dir for avatar generation",
        ))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn antigravity_home() -> Option<PathBuf> {
    krill_proto::os::home_dir().map(|h| h.join(".gemini").join("antigravity-cli"))
}

fn codex_images_dir(stdout: &str) -> Option<PathBuf> {
    let thread_id = codex_thread_id(stdout)?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| krill_proto::os::home_dir().map(|h| h.join(".codex")))?;
    Some(codex_home.join("generated_images").join(thread_id))
}

fn newest_image_in(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if !matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg" | "webp")) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(b, _)| mtime > *b) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

fn read_bounded_stream(mut reader: impl std::io::Read) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(n) = reader.read(&mut chunk) {
        if n == 0 {
            break;
        }
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(buf.len());
        if remaining == 0 {
            break;
        }
        let take = n.min(remaining);
        if let Some(slice) = chunk.get(..take) {
            buf.extend_from_slice(slice);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn grok_encoded_cwd_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Runs a single local avatar generation using `agent`, cancelling promptly via `cancel`.
pub fn generate_avatar_sync(
    agent: AvatarImageAgent,
    description: &str,
    cancel: &AvatarCancelHandle,
) -> AvatarRunResult {
    if cancel.is_canceled() {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::Canceled,
            agent: Some(agent),
        };
    }
    if describes_real_person(description) {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::RealPerson,
            agent: Some(agent),
        };
    }

    let path_env = resolved_cli_path();
    let cap = probe_capability_with_path(path_env.as_deref());
    CAPABILITY_CACHE.set(cap.clone());
    let Some(entry) = cap.agents.iter().find(|e| e.agent == agent) else {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::Unavailable,
            agent: Some(agent),
        };
    };
    if entry.status == AvatarAgentStatusKind::SignedOut {
        invalidate_capability_cache();
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::SignedOut,
            agent: Some(agent),
        };
    }

    let Ok(temp_dir) = TempDirGuard::create() else {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::Failed,
            agent: Some(agent),
        };
    };
    let cwd = temp_dir.path();
    let prompt = avatar_generation_prompt(description);

    if agent == AvatarImageAgent::Grok && fs::write(cwd.join(PROMPT_FILE), &prompt).is_err() {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::Failed,
            agent: Some(agent),
        };
    }
    if cancel.is_canceled() {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::Canceled,
            agent: Some(agent),
        };
    }

    let mut cmd = crate::platform::background_command(agent.binary());
    cmd.current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if agent == AvatarImageAgent::Codex {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }

    let agy_prompt;
    let prompt_file_str;
    let cwd_str = cwd.to_string_lossy().into_owned();
    match agent {
        AvatarImageAgent::Codex => {
            cmd.args([
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--ephemeral",
                "-s",
                "read-only",
                "-c",
                "model_reasoning_effort=\"low\"",
                "-C",
                &cwd_str,
                "-",
            ]);
        }
        AvatarImageAgent::Grok => {
            prompt_file_str = cwd.join(PROMPT_FILE).to_string_lossy().into_owned();
            cmd.args([
                "--prompt-file",
                &prompt_file_str,
                "--permission-mode",
                "dontAsk",
                "--tools",
                "image_gen",
                "--allow",
                "image_gen",
                "--disable-web-search",
                "--reasoning-effort",
                "low",
                "--output-format",
                "streaming-json",
                "--cwd",
                &cwd_str,
            ]);
        }
        AvatarImageAgent::Agy => {
            agy_prompt = format!("{ANTIGRAVITY_IMAGE_STEER} {prompt}");
            cmd.args([
                "-p",
                &agy_prompt,
                "--output-format",
                "stream-json",
                "--print-timeout",
                "230s",
            ]);
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group so cancelling tears down helper subagents too.
        cmd.process_group(0);
    }

    let Ok(mut child) = cmd.spawn() else {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::Failed,
            agent: Some(agent),
        };
    };
    cancel.set_pid(Some(child.id()));

    if agent == AvatarImageAgent::Codex
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin.write_all(prompt.as_bytes()).ok();
    }

    let out_reader = child
        .stdout
        .take()
        .map(|s| std::thread::spawn(move || read_bounded_stream(s)));
    let err_reader = child
        .stderr
        .take()
        .map(|s| std::thread::spawn(move || read_bounded_stream(s)));

    let start = Instant::now();
    let mut timed_out = false;
    let exit_status = loop {
        if cancel.is_canceled() {
            kill_pid(child.id());
            child.wait().ok();
            break None;
        }
        if start.elapsed() >= RUN_TIMEOUT {
            timed_out = true;
            kill_pid(child.id());
            child.wait().ok();
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(80)),
            Err(_) => break None,
        }
    };
    cancel.set_pid(None);

    let stdout = out_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_reader.and_then(|h| h.join().ok()).unwrap_or_default();

    let res = evaluate_run_output(
        agent,
        cwd,
        exit_status.map(|s| s.success()),
        timed_out,
        cancel.is_canceled(),
        &stdout,
        &stderr,
    );
    cleanup_agent_artifacts(agent, cwd, &stdout);
    res
}

fn cleanup_agent_artifacts(agent: AvatarImageAgent, cwd: &Path, stdout: &str) {
    match agent {
        AvatarImageAgent::Codex => {
            if let Some(dir) = codex_images_dir(stdout) {
                fs::remove_dir_all(dir).ok();
            }
        }
        AvatarImageAgent::Agy => {
            if let Some(home) = antigravity_home() {
                for id in antigravity_conversation_ids(stdout) {
                    fs::remove_dir_all(home.join("brain").join(&id)).ok();
                    for suffix in [".db", ".db-wal", ".db-shm"] {
                        fs::remove_file(home.join("conversations").join(format!("{id}{suffix}")))
                            .ok();
                    }
                }
            }
        }
        AvatarImageAgent::Grok => {
            if let Some(sessions_root) = resolve_grok_sessions_dir() {
                for candidate in [Some(cwd.to_path_buf()), fs::canonicalize(cwd).ok()]
                    .into_iter()
                    .flatten()
                {
                    let name = grok_encoded_cwd_dir_name(&candidate);
                    fs::remove_dir_all(sessions_root.join(name)).ok();
                }
            }
        }
    }
}

fn find_generated_image_path(agent: AvatarImageAgent, stdout: &str) -> Option<PathBuf> {
    match agent {
        AvatarImageAgent::Codex => codex_images_dir(stdout).and_then(|d| newest_image_in(&d)),
        AvatarImageAgent::Agy => {
            let home = antigravity_home()?;
            antigravity_conversation_ids(stdout)
                .into_iter()
                .find_map(|id| newest_image_in(&home.join("brain").join(id)))
        }
        AvatarImageAgent::Grok => {
            let last = grok_image_paths(stdout).pop()?;
            let path = PathBuf::from(last);
            if !path.is_absolute() {
                return None;
            }
            let real = fs::canonicalize(&path).ok()?;
            let sessions = resolve_grok_sessions_dir().and_then(|d| fs::canonicalize(d).ok())?;
            real.starts_with(&sessions).then_some(real)
        }
    }
}

static REFUSED_REPLY: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\bREFUSED\b").ok());

fn evaluate_run_output(
    agent: AvatarImageAgent,
    _cwd: &Path,
    succeeded: Option<bool>,
    timed_out: bool,
    canceled: bool,
    stdout: &str,
    stderr: &str,
) -> AvatarRunResult {
    if canceled {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::Canceled,
            agent: Some(agent),
        };
    }
    if timed_out {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::TimedOut,
            agent: Some(agent),
        };
    }
    if let Some(img_path) = find_generated_image_path(agent, stdout)
        && let Ok(meta) = fs::metadata(&img_path)
        && meta.len() > 0
        && usize::try_from(meta.len()).unwrap_or(usize::MAX) <= PICTURE_MAX_BYTES
        && let Ok(raw) = fs::read(&img_path)
        && let Ok(png_bytes) = prepare_picture(&raw)
    {
        return AvatarRunResult::Ok {
            agent,
            image: GeneratedAvatarImage { png_bytes },
        };
    }

    let combined = format!("{}\n{stderr}", agent_error_text(stdout));
    if succeeded != Some(true) {
        let err = classify_avatar_agent_failure(&combined);
        if err == AvatarGenerationFailure::SignedOut {
            invalidate_capability_cache();
        }
        return AvatarRunResult::Err {
            error: err,
            agent: Some(agent),
        };
    }

    let reply = agent_reply_text(stdout);
    if REFUSED_REPLY.as_ref().is_some_and(|re| re.is_match(&reply)) {
        return AvatarRunResult::Err {
            error: AvatarGenerationFailure::RealPerson,
            agent: Some(agent),
        };
    }

    let err = classify_avatar_agent_failure(&combined);
    let failure = if err == AvatarGenerationFailure::Failed {
        AvatarGenerationFailure::NoImage
    } else {
        if err == AvatarGenerationFailure::SignedOut {
            invalidate_capability_cache();
        }
        err
    };
    AvatarRunResult::Err {
        error: failure,
        agent: Some(agent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_person_detection_matches_orca_rules() {
        assert!(describes_real_person("a portrait of me in space"));
        assert!(describes_real_person("draw myself as a wizard"));
        assert!(describes_real_person("my selfie as pixel art"));
        assert!(describes_real_person("a celebrity astronaut"));
        assert!(!describes_real_person(
            "a cute orange crab holding a wrench"
        ));
        assert!(!describes_real_person(
            "friendly robot librarian in watercolor"
        ));
    }

    #[test]
    fn codex_feature_detection_and_json_output_parsers() {
        let features = "analytics  experimental  false\nimage_generation  experimental  true\n";
        assert!(codex_image_generation_enabled(features));
        assert!(!codex_image_generation_enabled(
            "image_generation  experimental  false\n"
        ));

        let stdout = r#"
not json
{"type":"thread.started","thread_id":"thr-123-abc"}
{"type":"item.completed","item":{"type":"agent_message","text":"REFUSED"}}
"#;
        assert_eq!(codex_thread_id(stdout).as_deref(), Some("thr-123-abc"));
        assert!(agent_reply_text(stdout).contains("REFUSED"));

        let agy_stdout = r#"{"event":"progress","conversation_id":"conv-1"}
{"event":"subagent","nested":{"conversation_id":"conv-2"}}
{"event":"result","result":{"status":"FAILED","error":"401 unauthorized"}}"#;
        assert_eq!(
            antigravity_conversation_ids(agy_stdout),
            vec!["conv-1", "conv-2"]
        );
        assert_eq!(
            classify_avatar_agent_failure(&agent_error_text(agy_stdout)),
            AvatarGenerationFailure::SignedOut
        );

        let grok_stdout = r#"
{"type":"tool_call","toolName":"image_gen","toolCallId":"tc-1"}
{"type":"tool_call_update","toolCallId":"tc-1","status":"completed","content":[{"content":{"type":"text","text":"{\"path\":\"/tmp/out.png\"}"}}]}
"#;
        assert_eq!(grok_image_paths(grok_stdout), vec!["/tmp/out.png"]);
    }

    #[test]
    fn pick_avatar_agent_prefers_remembered_then_ready() {
        let agents = vec![
            AvatarImageAgentStatus {
                agent: AvatarImageAgent::Codex,
                status: AvatarAgentStatusKind::SignedOut,
            },
            AvatarImageAgentStatus {
                agent: AvatarImageAgent::Grok,
                status: AvatarAgentStatusKind::Ready,
            },
        ];
        assert_eq!(
            pick_avatar_agent(&agents, None),
            Some(AvatarImageAgent::Grok)
        );
        assert_eq!(
            pick_avatar_agent(&agents, Some(AvatarImageAgent::Codex)),
            Some(AvatarImageAgent::Codex)
        );
        assert_eq!(
            pick_avatar_agent(&agents, Some(AvatarImageAgent::Agy)),
            Some(AvatarImageAgent::Grok)
        );
    }

    #[test]
    fn failure_classification_covers_usage_and_sign_in() {
        assert_eq!(
            classify_avatar_agent_failure("HTTP 429 too many requests"),
            AvatarGenerationFailure::UsageLimit
        );
        assert_eq!(
            classify_avatar_agent_failure("Please sign in again"),
            AvatarGenerationFailure::SignedOut
        );
        assert_eq!(
            classify_avatar_agent_failure("random network error"),
            AvatarGenerationFailure::Failed
        );
    }
}
