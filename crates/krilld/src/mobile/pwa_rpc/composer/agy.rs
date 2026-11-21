//! Antigravity CLI /model control, from 1.4.12. `agy models` runs off the polled status path.
use super::{
    Adapter, ChatPollSnapshot, Context, Duration, Instant, Live, Value, io, json, native,
    no_current_model, observe_live_model, set, valid_effort, valid_id,
};
use crate::session::lock;
use crate::usage::{accounts, direct};
use krill_proto::screen::{Color, Flags, Screen};
use krill_proto::usage::ProviderKind;
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
struct Level {
    effort: Option<String>,
    id: String,
    label: String,
}

/// What the picker shows: one model, with its efforts as a second choice when available.
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

static CACHE: Mutex<BTreeMap<Option<PathBuf>, Slot>> = Mutex::new(BTreeMap::new());
type ExeStamp = (PathBuf, Option<std::time::SystemTime>, u64, bool);
static VERIFIED_EXE: Mutex<BTreeMap<ExeStamp, Result<(), &'static str>>> =
    Mutex::new(BTreeMap::new());

pub(super) fn prewarm(context: &Context) {
    prewarm_path(&context.home, context.config.clone());
}

pub(crate) fn prewarm_path(home: &Path, config: Option<PathBuf>) {
    let _ = ensure_cached(home, config);
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
    let result = fetch(config.as_deref(), home);
    let mut cache = lock(&CACHE);
    let slot = cache.entry(config).or_default();
    match result {
        Ok(families) => {
            slot.families = Some(Arc::new(families));
            slot.failure = None;
            slot.loaded = Some(Instant::now());
        }
        Err(reason) => {
            slot.families = None;
            slot.failure = Some(reason);
        }
    }
}

fn verify_exe(exe: &Path, ssh: bool) -> Result<(), &'static str> {
    let stamp = std::fs::metadata(exe)
        .ok()
        .map(|m| (exe.to_path_buf(), m.modified().ok(), m.len(), ssh));
    if let Some(ref key) = stamp
        && let Some(&cached) = lock(&VERIFIED_EXE).get(key)
    {
        return cached;
    }
    let version = direct::agy::run_agy(
        exe,
        &["--version".to_owned()],
        ssh,
        VERSION_TIMEOUT,
        MAX_BYTES,
    )
    .ok_or(LIST_FAILED)?;
    let result = if String::from_utf8_lossy(&version)
        .split_whitespace()
        .find(|word| word.contains('.'))
        .is_some_and(|word| native::at_least(word, FLOOR))
    {
        Ok(())
    } else {
        Err(UNVERIFIED)
    };
    if let Some(key) = stamp {
        let mut verified = lock(&VERIFIED_EXE);
        if verified.len() >= 16 && !verified.contains_key(&key) {
            verified.pop_first();
        }
        verified.insert(key, result);
    }
    result
}

fn fetch(config: Option<&Path>, home: &Path) -> Result<Vec<Family>, &'static str> {
    let exe = direct::agy_exe()
        .or_else(|| krill_proto::agent::which("agy", home.join(".local/bin").as_os_str()))
        .ok_or(NOT_INSTALLED)?;
    let file_token = accounts::host_default_dir(home, ProviderKind::Antigravity)
        .join("antigravity-oauth-token")
        .is_file();
    let ssh = config.is_some() || file_token;
    verify_exe(&exe, ssh)?;
    let mut args = Vec::new();
    if let Some(dir) = config {
        let gemini = dir.join(".gemini");
        args.push(format!(
            "--gemini_dir={}",
            gemini.to_str().ok_or(LIST_FAILED)?
        ));
    }
    args.push("models".to_owned());
    let output =
        direct::agy::run_agy(&exe, &args, ssh, LIST_TIMEOUT, MAX_BYTES).ok_or(LIST_FAILED)?;
    let families = parse(&String::from_utf8_lossy(&output));
    if families.is_empty() {
        return Err(LIST_FAILED);
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

pub(super) struct Agy;

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
        let family = families
            .iter()
            .find(|family| family.id == model)
            .ok_or_else(|| invalid("Model is not in this session's discovered catalog"))?;
        let tap = live.tap()?;
        let (before, revision, seq) = tap.capture();
        if !verified(&before) || !composer(&before, None) {
            return Err(io::Error::other(WAIT));
        }
        let current = footer_with(&before, Some(&families));
        let level = match effort {
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
        let effort_supported = supported
            && family.is_none_or(|f| f.levels.iter().any(|l| l.effort.is_some()))
            || effort.is_some();
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
