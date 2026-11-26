//! Antigravity CLI /model control, from 1.4.12. `agy models` runs off the polled status path.
use super::{
    Adapter, ChatPollSnapshot, Context, Duration, Instant, Live, Value, io, json, native,
    no_current_model, observe_live_model, set, valid_effort, valid_id,
};
use crate::session::lock;
use crate::usage::direct;
use krill_proto::bots::{BotAgentModel, BotAgentModels};
use krill_proto::screen::{Color, Flags, Screen};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const FLOOR: &[u64] = &[1, 4, 12];
const EFFORT_ORDER: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
const TTL: Duration = Duration::from_mins(10);
/// macOS can take over 5s to run a just-written fake script under test load.
const VERSION_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 30 } else { 5 });
const LIST_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 30 } else { 15 });
/// Longer than a whole [`fetch`], so one catalog never has two refreshes running.
const RETRY: Duration = VERSION_TIMEOUT
    .saturating_add(LIST_TIMEOUT)
    .saturating_add(Duration::from_secs(10));
const MAX_BYTES: u64 = 64 * 1024;
const SETTLE: Duration = Duration::from_secs(if cfg!(test) { 60 } else { 10 });
const CONFIRM: Duration = Duration::from_secs(3);

const NOT_INSTALLED: &str = "Antigravity CLI was not found on this computer.";
const UNVERIFIED: &str =
    "Model control needs Antigravity CLI 1.4.12 or later. Keep chatting with the current model.";
const LIST_FAILED: &str = "Antigravity did not return its model list. Try again in a moment.";
const LOADING: &str = "Antigravity models are still loading. Try again in a moment.";
const WAIT: &str = "Wait for an idle Antigravity chat with an empty composer.";
const CONTROL: &str = "Changes the model in this running Antigravity chat. Antigravity also saves it as the default for new sessions.";
const NOTE: &str = "Antigravity also saves this as the default for new sessions.";
const NOT_CONFIRMED: &str = "Antigravity received the model command, but its confirmation was not recognized. It will not be resent.";

/// One row of `agy models`. When present, the effort is part of the id, so `/model <id>` sets both at once.
#[derive(Debug, PartialEq, Eq)]
struct Level {
    effort: Option<String>,
    id: String,
    label: String,
}

/// What the picker shows: one model, with its efforts as a second choice when available.
#[derive(Debug, PartialEq, Eq)]
struct Family {
    id: String,
    label: String,
    levels: Vec<Level>,
}

impl Family {
    fn level(&self, effort: Option<&str>) -> Option<&Level> {
        self.levels
            .iter()
            .find(|level| level.effort.as_deref() == effort)
    }

    fn item(&self) -> Value {
        let efforts: Vec<&str> = self
            .levels
            .iter()
            .filter_map(|level| level.effort.as_deref())
            .collect();
        json!({"id":self.id,"label":self.label,"efforts":efforts})
    }
}

type Families = Arc<Vec<Family>>;

#[derive(Default)]
struct Slot {
    families: Option<Families>,
    failure: Option<&'static str>,
    loaded: Option<Instant>,
    attempted: Option<Instant>,
}

/// Outcome of a single `agy` model-catalog fetch (Orca `fcc3027d` / `#46647`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchError {
    /// Stopped or refused by the Antigravity launch gate; must never be cached as a probe failure.
    Stopped,
    /// Genuine probe error (`NOT_INSTALLED`, `UNVERIFIED`, `LIST_FAILED`).
    Failed(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VerifiedExe {
    Ok,
    TooOld(String),
    Failed(&'static str),
}

static CACHE: Mutex<BTreeMap<Option<PathBuf>, Slot>> = Mutex::new(BTreeMap::new());
type ExeStamp = (PathBuf, Option<std::time::SystemTime>, u64, bool);
static VERIFIED_EXE: Mutex<BTreeMap<ExeStamp, VerifiedExe>> = Mutex::new(BTreeMap::new());

/// Evicts cached Antigravity model listings (e.g. after sign-in or account switch).
pub(crate) fn evict_cache() {
    lock(&CACHE).clear();
}

pub(super) fn prewarm(context: &Context) {
    prewarm_path(&context.home, context.config.clone());
}

pub(crate) fn prewarm_path(home: &Path, config: Option<PathBuf>) {
    let _ = ensure_cached(home, config);
}

fn to_listed_models(families: &[Family]) -> Vec<krill_proto::agent::AgyListedModel> {
    families
        .iter()
        .map(|family| {
            let efforts: Vec<String> = family
                .levels
                .iter()
                .filter_map(|level| level.effort.clone())
                .collect();
            if efforts.len() > 1 {
                krill_proto::agent::AgyListedModel {
                    id: family.id.clone(),
                    efforts,
                }
            } else {
                // Single-level or standalone models stay named by their own ID so callers keep it.
                let id = family
                    .levels
                    .first()
                    .map_or_else(|| family.id.clone(), |l| l.id.clone());
                krill_proto::agent::AgyListedModel {
                    id,
                    efforts: Vec::new(),
                }
            }
        })
        .collect()
}

/// Peeks the cached Antigravity model listing and folds a level model ID (e.g.
/// `gemini-4-argon-medium`) onto its listed family + effort (`("gemini-4-argon", "medium")`) so
/// effort stays adjustable after launch (Orca `c5de2672` / `#46941`).
/// Cold listings, standalone models, and unlisted efforts return `None` so callers keep the ID.
pub(crate) fn fold_listed_family(
    config: Option<&Path>,
    model: &str,
    effort: Option<&str>,
) -> Option<(String, String)> {
    let families = {
        let cache = lock(&CACHE);
        cache
            .get(&config.map(Path::to_path_buf))
            .or_else(|| cache.get(&None))
            .and_then(|slot| slot.families.clone())
    }?;
    let listed = to_listed_models(&families);
    krill_proto::agent::antigravity_listed_family_selection(model, effort, &listed)
}

/// Checks if the Antigravity CLI at `exe` is older than [`FLOOR`] (`1.4.12`), returning
/// `(installed_version, minimum_version)` so a refused bot chat can name both versions.
pub(crate) fn installed_version_problem(exe: &Path, ssh: bool) -> Option<(String, String)> {
    let never = std::sync::atomic::AtomicBool::new(false);
    match verify_exe_status(exe, ssh, &never) {
        Ok(VerifiedExe::TooOld(installed)) => Some((installed, "1.4.12".to_owned())),
        _ => None,
    }
}

/// Returns what is known now and refreshes on its own thread, so a poll never waits on `agy`.
fn cached(context: &Context) -> (Option<Families>, Option<&'static str>) {
    ensure_cached(&context.home, context.config.clone())
}

fn ensure_cached(home: &Path, config: Option<PathBuf>) -> (Option<Families>, Option<&'static str>) {
    let mut cache = lock(&CACHE);
    if cache.len() >= 16 && !cache.contains_key(&config) {
        cache.pop_first();
    }
    let slot = cache.entry(config.clone()).or_default();
    if slot.loaded.is_none_or(|at| at.elapsed() >= TTL)
        && slot.attempted.is_none_or(|at| at.elapsed() >= RETRY)
    {
        slot.attempted = Some(Instant::now());
        let home = home.to_path_buf();
        std::thread::Builder::new()
            .name("agy-models".into())
            .spawn(move || refresh(config, &home))
            .ok();
    }
    (slot.families.clone(), slot.failure)
}

fn refresh(config: Option<PathBuf>, home: &Path) {
    let gen_before = direct::agy::current_listing_generation();
    let result = fetch_gated(config.as_deref(), home);
    let mut cache = lock(&CACHE);
    let slot = cache.entry(config).or_default();
    match result {
        Ok(families) => {
            // Discard stale listing if an account operation ran while we were fetching.
            if direct::agy::current_listing_generation() != gen_before {
                slot.attempted = None;
                return;
            }
            slot.families = Some(Arc::new(families));
            slot.failure = None;
            slot.loaded = Some(Instant::now());
        }
        Err(FetchError::Stopped) => {
            // Gate refusal or gate-driven stop says nothing about the account: never cache as a
            // probe failure, and allow an immediate retry once the gate opens.
            slot.attempted = None;
        }
        Err(FetchError::Failed(reason)) => {
            if direct::agy::current_listing_generation() != gen_before {
                slot.attempted = None;
                return;
            }
            slot.families = None;
            slot.failure = Some(reason);
        }
    }
}

fn verify_exe_status(
    exe: &Path,
    ssh: bool,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<VerifiedExe, FetchError> {
    let stamp = std::fs::metadata(exe)
        .ok()
        .map(|m| (exe.to_path_buf(), m.modified().ok(), m.len(), ssh));
    if let Some(ref key) = stamp
        && let Some(cached) = lock(&VERIFIED_EXE).get(key).cloned()
    {
        return Ok(cached);
    }
    let version = match direct::agy::run_agy_cancellable(
        exe,
        &["--version".to_owned()],
        ssh,
        VERSION_TIMEOUT,
        MAX_BYTES,
        cancel,
    ) {
        Ok(bytes) => bytes,
        Err(direct::agy::RunAgyError::Stopped) => return Err(FetchError::Stopped),
        Err(direct::agy::RunAgyError::Failed) => return Ok(VerifiedExe::Failed(LIST_FAILED)),
    };
    let text = String::from_utf8_lossy(&version);
    let result = classify_version_output(&text);
    if let Some(key) = stamp {
        let mut verified = lock(&VERIFIED_EXE);
        if verified.len() >= 16 && !verified.contains_key(&key) {
            verified.pop_first();
        }
        verified.insert(key, result.clone());
    }
    Ok(result)
}

fn extract_version_token(text: &str) -> Option<&str> {
    text.split_whitespace().find_map(|raw| {
        let trimmed = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
        let stripped = trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed);
        let mut parts = stripped.split('.');
        let major = parts.next()?;
        let minor = parts.next()?;
        if !major.is_empty()
            && major.bytes().all(|b| b.is_ascii_digit())
            && minor.bytes().next().is_some_and(|b| b.is_ascii_digit())
        {
            Some(stripped)
        } else {
            None
        }
    })
}

fn classify_version_output(text: &str) -> VerifiedExe {
    match extract_version_token(text) {
        Some(ver) if native::at_least(ver, FLOOR) => VerifiedExe::Ok,
        Some(ver) => VerifiedExe::TooOld(ver.to_owned()),
        None => VerifiedExe::Failed(UNVERIFIED),
    }
}

#[cfg(all(test, unix))]
fn fetch(config: Option<&Path>, home: &Path) -> Result<Vec<Family>, &'static str> {
    match fetch_gated(config, home) {
        Ok(families) => Ok(families),
        Err(FetchError::Failed(reason)) => Err(reason),
        Err(FetchError::Stopped) => Err(LIST_FAILED),
    }
}

fn fetch_gated(config: Option<&Path>, home: &Path) -> Result<Vec<Family>, FetchError> {
    let exe = direct::agy_exe()
        .or_else(|| krill_proto::agent::which("agy", home.join(".local/bin").as_os_str()))
        .ok_or(FetchError::Failed(NOT_INSTALLED))?;
    let gate = direct::agy::enter_listing().map_err(|_| FetchError::Stopped)?;
    // Native host `agy models` uses the host credential store directly (Orca parity);
    // only explicit isolated `--gemini_dir` configs enable forced file storage.
    let ssh = config.is_some();
    match verify_exe_status(&exe, ssh, gate.cancel_flag())? {
        VerifiedExe::Ok => {}
        VerifiedExe::TooOld(_) => return Err(FetchError::Failed(UNVERIFIED)),
        VerifiedExe::Failed(reason) => return Err(FetchError::Failed(reason)),
    }
    let mut args = Vec::new();
    if let Some(dir) = config {
        let gemini = dir.join(".gemini");
        args.push(format!(
            "--gemini_dir={}",
            gemini.to_str().ok_or(FetchError::Failed(LIST_FAILED))?
        ));
    } else if home.is_absolute()
        && let Some(gemini) = home.join(".gemini").to_str()
    {
        args.push(format!("--gemini_dir={gemini}"));
    }
    args.push("models".to_owned());
    let output = match direct::agy::run_agy_cancellable(
        &exe,
        &args,
        ssh,
        LIST_TIMEOUT,
        MAX_BYTES,
        gate.cancel_flag(),
    ) {
        Ok(bytes) => bytes,
        Err(direct::agy::RunAgyError::Stopped) => return Err(FetchError::Stopped),
        Err(direct::agy::RunAgyError::Failed) => {
            if gate.is_stopped() || direct::agy::current_listing_generation() != gate.generation() {
                return Err(FetchError::Stopped);
            }
            return Err(FetchError::Failed(LIST_FAILED));
        }
    };
    if gate.is_stopped() || direct::agy::current_listing_generation() != gate.generation() {
        return Err(FetchError::Stopped);
    }
    let families = parse(&String::from_utf8_lossy(&output));
    if families.is_empty() {
        return Err(FetchError::Failed(LIST_FAILED));
    }
    Ok(families)
}

fn plain_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && label
            .chars()
            .all(|c| c.is_alphanumeric() || " .-_".contains(c))
}

fn plain_model_label(label: &str) -> bool {
    let mut depth = 0usize;
    !label.is_empty()
        && label.len() <= 64
        && label.chars().all(|c| match c {
            '(' => {
                depth += 1;
                depth == 1
            }
            ')' => {
                let ok = depth == 1;
                depth = depth.saturating_sub(1);
                ok
            }
            _ => c.is_alphanumeric() || " .-_".contains(c),
        })
        && depth == 0
}

/// Groups `id\tlabel` rows by model, plus standalone models without an effort suffix.
fn parse(output: &str) -> Vec<Family> {
    let mut families: Vec<Family> = Vec::new();
    for (id, label) in output
        .lines()
        .take(256)
        .filter_map(|line| line.split_once('\t'))
    {
        let id = id.trim();
        let label = label.trim();
        if !valid_id(id) {
            continue;
        }
        if let Some((family_label, effort_tag)) =
            label.strip_suffix(')').and_then(|l| l.rsplit_once(" ("))
        {
            let effort = effort_tag.to_ascii_lowercase();
            if valid_effort(&effort) {
                let Some(family_id) = id.strip_suffix(&format!("-{effort}")) else {
                    continue;
                };
                if !plain_label(family_label) {
                    continue;
                }
                let level = Level {
                    effort: Some(effort),
                    id: id.to_owned(),
                    label: label.to_owned(),
                };
                if let Some(family) = families.iter_mut().find(|family| family.id == family_id) {
                    family.levels.push(level);
                } else if families.len() < 64 {
                    families.push(Family {
                        id: family_id.to_owned(),
                        label: family_label.to_owned(),
                        levels: vec![level],
                    });
                }
                continue;
            }
        }
        if !plain_model_label(label) || families.iter().any(|family| family.id == id) {
            continue;
        }
        if families.len() < 64 {
            families.push(Family {
                id: id.to_owned(),
                label: label.to_owned(),
                levels: vec![Level {
                    effort: None,
                    id: id.to_owned(),
                    label: label.to_owned(),
                }],
            });
        }
    }
    for family in &mut families {
        family.levels.sort_by_key(|level| {
            EFFORT_ORDER
                .iter()
                .position(|e| Some(*e) == level.effort.as_deref())
        });
    }
    families
}

/// A cached catalog proves the installed version. A banner still on screen outranks it,
/// because the running session may predate an update.
fn verified(screen: &Screen) -> bool {
    native::banner_meets(screen, "Antigravity CLI ", FLOOR)
}

/// Returns true when the Antigravity TUI has rendered an empty interactive composer (`> `).
pub(super) fn composer_ready(screen: &Screen) -> bool {
    composer(screen, None)
}

fn composer(screen: &Screen, draft: Option<&str>) -> bool {
    let y = usize::from(screen.cursor.line);
    let Some(cells) = screen.rows.get(y) else {
        return false;
    };
    if !screen.cursor.visible
        || y == 0
        || !native::border(screen, y - 1)
        || !native::border(screen, y + 1)
        || !cells.iter().all(|c| c.zerowidth.is_empty())
        || usize::from(screen.cursor.column) != 2 + draft.map_or(0, str::len)
    {
        return false;
    }
    if let Some(draft) = draft {
        let expected = format!("> {draft}");
        return native::row(screen, y) == expected.trim_end();
    }
    if cells.first().is_none_or(|c| c.ch != '>') || cells.get(1).is_some_and(|c| c.ch != ' ') {
        return false;
    }
    let tail = cells.get(2..).unwrap_or_default();
    if tail.iter().all(|c| c.ch.is_whitespace()) {
        return true;
    }
    let prompt_fg = cells.first().map_or(Color::Default, |c| c.fg);
    let first_non_ws = tail.iter().find(|c| !c.ch.is_whitespace()).map(|c| c.ch);
    !matches!(first_non_ws, Some('/' | '!' | '@'))
        && tail.iter().all(|c| {
            c.ch.is_whitespace()
                || c.flags.contains(Flags::DIM)
                || (c.fg != Color::Default && c.fg != prompt_fg)
        })
}

/// The echoed command and the completion row Enter would select. Narrow or multi-row menus
/// may place the highlighted row below `y + 2` or omit the id column.
fn echoed(screen: &Screen, command: &str, level: &Level) -> bool {
    if !composer(screen, Some(command)) {
        return false;
    }
    let start = usize::from(screen.cursor.line) + 2;
    let end = start.saturating_add(12).min(screen.rows.len());
    for y in start..end {
        let line = native::row(screen, y);
        if line.trim().is_empty() || line.contains("Navigate") || line.contains('│') {
            break;
        }
        if line.starts_with("> ") {
            let Some(after_label) = line.strip_prefix(&format!("> {}", level.label)) else {
                return false;
            };
            if after_label.is_empty() {
                return true;
            }
            let Some(padded) = after_label.strip_prefix("  ") else {
                return false;
            };
            let id = padded.trim_start();
            return id == level.id
                || id
                    .strip_suffix("...")
                    .is_some_and(|cut| !cut.is_empty() && level.id.starts_with(cut));
        }
    }
    false
}

fn strip_context_window(status: &str) -> &str {
    let Some(without_paren) = status.strip_suffix(')') else {
        return status;
    };
    let Some((prefix, size)) = without_paren.rsplit_once(" (") else {
        return status;
    };
    let Some(num) = size.strip_suffix(['k', 'K', 'm', 'M']) else {
        return status;
    };
    let mut parts = num.split('.');
    let valid_num = match (parts.next(), parts.next(), parts.next()) {
        (Some(whole), None, None) => !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()),
        (Some(whole), Some(frac), None) => {
            !whole.is_empty()
                && !frac.is_empty()
                && whole.bytes().all(|b| b.is_ascii_digit())
                && frac.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    };
    if valid_num { prefix } else { status }
}

/// `(label, effort)` from the footer, the screen's last row in every observed layout. The status
/// line is configurable, so only its first segment is read.
#[cfg(all(test, unix))]
fn footer(screen: &Screen) -> Option<(String, Option<String>)> {
    footer_with(screen, None)
}

fn ends_with_agy_mode_tag(segment: &str) -> bool {
    let last = segment.split_whitespace().last().unwrap_or("");
    matches!(
        last.to_ascii_lowercase().as_str(),
        "auto" | "manual" | "yolo" | "plan" | "safe" | "ask"
    )
}

fn footer_with(screen: &Screen, families: Option<&[Family]>) -> Option<(String, Option<String>)> {
    let row = (0..screen.rows.len())
        .rev()
        .map(|y| native::row(screen, y))
        .find(|row| !row.trim().is_empty())?;
    let status = row.split('│').next()?.trim();
    if status.starts_with('>') {
        return None;
    }
    if let Some((label, effort)) = status.rsplit_once(" · ").or_else(|| {
        let (label, rest) = status.split_once(" (")?;
        Some((label, rest.split_once(')')?.0))
    }) {
        let effort = effort.to_ascii_lowercase();
        if plain_label(label) && valid_effort(&effort) {
            return Some((label.to_owned(), Some(effort)));
        }
    }
    // Default Antigravity CLI footer without a custom `statusLine`:
    //   "? for shortcuts    auto  · Gemini 3.8 Flash · low"
    //   "esc to cancel    auto  · Gemini 3.8 Flash · low"
    //   "? for shortcuts    auto  · Orchestrator (Neon)"
    if !row.contains('│') && status.contains('·') {
        let parts: Vec<&str> = status
            .split('·')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if let [.., mode, model, effort_raw] = parts.as_slice()
            && ends_with_agy_mode_tag(mode)
            && plain_label(model)
        {
            let effort = effort_raw.to_ascii_lowercase();
            if valid_effort(&effort) {
                return Some(((*model).to_owned(), Some(effort)));
            }
        }
        if let [.., mode, tail] = parts.as_slice()
            && ends_with_agy_mode_tag(mode)
        {
            let candidate = strip_context_window(tail).trim();
            let known_standalone = families.is_some_and(|fams| {
                fams.iter()
                    .any(|f| f.label == candidate && f.levels.iter().all(|l| l.effort.is_none()))
            });
            if known_standalone || plain_model_label(candidate) {
                return Some((candidate.to_owned(), None));
            }
        }
    }
    let candidate = strip_context_window(status).trim();
    let known_standalone = families.is_some_and(|fams| {
        fams.iter()
            .any(|f| f.label == candidate && f.levels.iter().all(|l| l.effort.is_none()))
    });
    if known_standalone || (row.contains('│') && plain_model_label(candidate)) {
        Some((candidate.to_owned(), None))
    } else {
        None
    }
}

/// Reads from the newest `⎿` down, ignoring blank rows and sticky `┃` callout banners pinned
/// above the composer border, and joining wrapped continuation lines on narrow mobile screens.
fn confirmed(screen: &Screen, label: &str) -> bool {
    let mut rows: Vec<_> = (0..usize::from(screen.cursor.line).saturating_sub(1))
        .map(|y| native::row(screen, y))
        .collect();
    while rows
        .last()
        .is_some_and(|row| row.trim().is_empty() || row.trim_start().starts_with('┃'))
    {
        rows.pop();
    }
    let Some(newest) = rows
        .iter()
        .rposition(|row| row.trim_start().starts_with('⎿'))
    else {
        return false;
    };
    if rows
        .get(newest.saturating_add(1)..)
        .unwrap_or_default()
        .iter()
        .any(|row| row.trim().is_empty() || !row.starts_with("  "))
    {
        return false;
    }
    let text = native::words(rows.get(newest..).unwrap_or_default());
    text == format!("⎿ Model set to {label}") || text == format!("⎿ Model already set to {label}")
}

/// Every `agy models` row, effort kept in the id: what `agy --model` accepts at launch.
pub(super) fn launch_models(context: &Context) -> BotAgentModels {
    let (families, failure) = cached(context);
    let models: Vec<_> = families
        .iter()
        .flat_map(|families| families.iter())
        .flat_map(|family| &family.levels)
        .map(|level| BotAgentModel {
            id: level.id.clone(),
            label: level.label.clone(),
        })
        .collect();
    let reason = models
        .is_empty()
        .then(|| failure.unwrap_or(LOADING).to_owned());
    BotAgentModels { models, reason }
}

pub(super) struct Agy;

fn agy_effort_supported(supported: bool, family: Option<&Family>, effort: Option<&str>) -> bool {
    supported
        && (family.is_none_or(|f| f.levels.iter().any(|l| l.effort.is_some())) || effort.is_some())
}

impl Adapter for Agy {
    fn catalog(&self, context: &Context) -> Value {
        let (families, failure) = cached(context);
        let reason = failure.unwrap_or(if families.is_some() { WAIT } else { LOADING });
        let items = families
            .iter()
            .flat_map(|families| families.iter().map(Family::item))
            .collect::<Vec<_>>();
        json!({"agent":"agy","supported":false,"current":null,"items":items,"truncated":false,"reason":reason})
    }

    fn control(&self, context: &Context, live: &Live<'_>, catalog: &mut Value) {
        let (Some(families), _) = cached(context) else {
            return;
        };
        let tap = match live.tap() {
            Ok(tap) => tap,
            Err(error) => {
                return set(
                    catalog,
                    "reason",
                    super::super::sanitize_tap_reason(&error).into(),
                );
            }
        };
        let (screen, _, _) = tap.capture();
        if !verified(&screen) || !composer(&screen, None) {
            return set(catalog, "reason", WAIT.into());
        }
        set(catalog, "supported", true.into());
        set(catalog, "reason", CONTROL.into());
        set(catalog, "note", NOTE.into());
        if let Some(family) = footer_with(&screen, Some(&families))
            .and_then(|(label, _)| families.iter().find(|family| family.label == label))
        {
            set(catalog, "current", family.id.as_str().into());
        }
    }

    fn set_model(
        &self,
        context: &Context,
        live: &Live<'_>,
        model: &str,
        effort: Option<&str>,
    ) -> io::Result<Value> {
        let (families, failure) = cached(context);
        let families = families.ok_or_else(|| io::Error::other(failure.unwrap_or(LOADING)))?;
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        let folded = krill_proto::agent::antigravity_listed_family_selection(
            model,
            effort,
            &to_listed_models(&families),
        );
        let (target_model, target_effort) = match &folded {
            Some((fam, eff)) => (fam.as_str(), Some(eff.as_str())),
            None => (model, effort),
        };
        let family = families
            .iter()
            .find(|family| family.id == target_model)
            .ok_or_else(|| invalid("Model is not in this session's discovered catalog"))?;
        let tap = live.tap()?;
        let (before, revision, seq) = tap.capture();
        if !verified(&before) || !composer(&before, None) {
            return Err(io::Error::other(WAIT));
        }
        let current = footer_with(&before, Some(&families));
        let level = match target_effort {
            Some(effort) => family.level(Some(effort)),
            None => current
                .as_ref()
                .and_then(|(_, effort)| effort.as_deref())
                .and_then(|effort| family.level(Some(effort)))
                .or_else(|| family.levels.first()),
        }
        .ok_or_else(|| invalid("Effort is not in this session model's discovered catalog"))?;
        let done = |already_selected: bool| {
            let mut reply = json!({"applied":true,"supported":true,"model":family.id,"label":family.label,"effort":level.effort});
            if already_selected {
                set(&mut reply, "already_selected", true.into());
            }
            reply
        };
        if current.is_some_and(|(label, eff)| label == family.label && eff == level.effort) {
            return Ok(done(true));
        }
        let command = format!("/model {}", level.id);
        let working = live.working();
        super::command(
            &tap,
            working,
            revision.0,
            command.as_bytes(),
            |screen, pasted| {
                verified(screen)
                    && if pasted {
                        echoed(screen, &command, level)
                    } else {
                        composer(screen, None)
                    }
            },
        )?;
        let started = Instant::now();
        while started.elapsed() < CONFIRM {
            std::thread::sleep(Duration::from_millis(25));
            let (screen, after, _) = tap.capture();
            if after.0 > revision.0
                && composer(&screen, None)
                // Mid-turn output pushes the note away, and the footer is Antigravity's own report.
                && (working
                    && footer_with(&screen, Some(&families))
                        .is_some_and(|(label, eff)| label == family.label && eff == level.effort)
                    || confirmed(&screen, &level.label)
                        && native::contains_phrase(
                            &native::fresh_text(&tap, seq.0),
                            &format!("set to {}", level.label),
                        ))
            {
                return Ok(done(false));
            }
        }
        Ok(json!({"applied":false,"supported":true,"reason":NOT_CONFIRMED}))
    }

    fn set_effort(&self, context: &Context, live: &Live<'_>, effort: &str) -> io::Result<Value> {
        let (families, _) = cached(context);
        let (label, _) = footer_with(
            &live.tap()?.capture().0,
            families.as_deref().map(Vec::as_slice),
        )
        .ok_or_else(no_current_model)?;
        let family = families
            .iter()
            .flat_map(|families| families.iter())
            .find(|family| family.label == label)
            .ok_or_else(no_current_model)?;
        self.set_model(context, live, &family.id, Some(effort))
    }

    fn observe(
        &self,
        context: &Context,
        live: &Live<'_>,
        _catalog: &Value,
        _snapshot: &ChatPollSnapshot,
        status: &mut Value,
    ) {
        let supported = status
            .get("supported")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (families, _) = cached(context);
        let Some((label, effort)) = live
            .screen()
            .as_ref()
            .and_then(|screen| footer_with(screen, families.as_deref().map(Vec::as_slice)))
        else {
            set(status, "effort_supported", supported.into());
            return;
        };
        let family = families
            .iter()
            .flat_map(|families| families.iter())
            .find(|family| family.label == label);
        let effort_supported = agy_effort_supported(supported, family, effort.as_deref());
        if let Some(family) = family {
            observe_live_model(status, &family.id);
        }
        set(status, "effort_supported", effort_supported.into());
        set(status, "model_label", label.into());
        set(status, "effort", effort.map_or(Value::Null, Into::into));
    }

    fn settle(&self, context: &Context) {
        let started = Instant::now();
        while started.elapsed() < SETTLE {
            let (families, failure) = cached(context);
            if families.is_some() || failure.is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
