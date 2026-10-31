//! Antigravity CLI /model control, from 1.4.12. `agy models` runs off the polled status path.
use super::{
    Adapter, ChatPollSnapshot, Context, Duration, Instant, Live, Value, io, json, native,
    no_current_model, observe_live_model, set, valid_effort, valid_id,
};
use crate::session::lock;
use crate::usage::{accounts, direct};
use krill_proto::screen::Screen;
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
const SETTLE: Duration = Duration::from_secs(if cfg!(test) { 60 } else { 5 });
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

/// One row of `agy models`. The effort is part of the id, so `/model <id>` sets both at once.
struct Level {
    effort: String,
    id: String,
    label: String,
}

/// What the picker shows: one model, with its efforts as a second choice.
struct Family {
    id: String,
    label: String,
    levels: Vec<Level>,
}

impl Family {
    fn level(&self, effort: &str) -> Option<&Level> {
        self.levels.iter().find(|level| level.effort == effort)
    }

    fn item(&self) -> Value {
        let efforts = self.levels.iter().map(|level| level.effort.as_str());
        json!({"id":self.id,"label":self.label,"efforts":efforts.collect::<Vec<_>>()})
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

/// Returns what is known now and refreshes on its own thread, so a poll never waits on `agy`.
fn cached(context: &Context) -> (Option<Families>, Option<&'static str>) {
    let key = context.config.clone();
    let mut cache = lock(&CACHE);
    if cache.len() >= 16 && !cache.contains_key(&key) {
        cache.pop_first();
    }
    let slot = cache.entry(key.clone()).or_default();
    if slot.loaded.is_none_or(|at| at.elapsed() >= TTL)
        && slot.attempted.is_none_or(|at| at.elapsed() >= RETRY)
    {
        slot.attempted = Some(Instant::now());
        let home = context.home.clone();
        std::thread::Builder::new()
            .name("agy-models".into())
            .spawn(move || refresh(key, &home))
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

fn fetch(config: Option<&Path>, home: &Path) -> Result<Vec<Family>, &'static str> {
    let exe = direct::agy_exe()
        .or_else(|| krill_proto::agent::which("agy", home.join(".local/bin").as_os_str()))
        .ok_or(NOT_INSTALLED)?;
    let file_token = accounts::host_default_dir(home, ProviderKind::Antigravity)
        .join("antigravity-oauth-token")
        .is_file();
    let ssh = config.is_some() || file_token;
    let version = direct::agy::run_agy(
        &exe,
        &["--version".to_owned()],
        ssh,
        VERSION_TIMEOUT,
        MAX_BYTES,
    )
    .ok_or(LIST_FAILED)?;
    if !String::from_utf8_lossy(&version)
        .split_whitespace()
        .find(|word| word.contains('.'))
        .is_some_and(|word| native::at_least(word, FLOOR))
    {
        return Err(UNVERIFIED);
    }
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

/// Groups `id\tlabel` rows by model. A row that does not encode its effort is left out.
fn parse(output: &str) -> Vec<Family> {
    let mut families: Vec<Family> = Vec::new();
    for (id, label) in output
        .lines()
        .take(256)
        .filter_map(|line| line.split_once('\t'))
    {
        let label = label.trim();
        let Some((family_label, effort)) =
            label.strip_suffix(')').and_then(|l| l.rsplit_once(" ("))
        else {
            continue;
        };
        let effort = effort.to_ascii_lowercase();
        let Some(family_id) = id.strip_suffix(&format!("-{effort}")) else {
            continue;
        };
        if !valid_id(id) || !valid_effort(&effort) || !plain_label(family_label) {
            continue;
        }
        let level = Level {
            effort,
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
    }
    for family in &mut families {
        family
            .levels
            .sort_by_key(|level| EFFORT_ORDER.iter().position(|e| *e == level.effort));
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
    let expected = format!("> {}", draft.unwrap_or_default());
    screen.cursor.visible
        && y > 0
        && native::border(screen, y - 1)
        && native::border(screen, y + 1)
        && screen
            .rows
            .get(y)
            .is_some_and(|cells| cells.iter().all(|c| c.zerowidth.is_empty()))
        && native::row(screen, y) == expected.trim_end()
        && usize::from(screen.cursor.column) == 2 + draft.map_or(0, str::len)
}

/// The echoed command and the completion row Enter would select. A narrow terminal cuts the id.
fn echoed(screen: &Screen, command: &str, level: &Level) -> bool {
    let completion = native::row(screen, usize::from(screen.cursor.line) + 2);
    composer(screen, Some(command))
        && completion
            .strip_prefix(&format!("> {}  ", level.label))
            .is_some_and(|id| {
                id == level.id
                    || id
                        .strip_suffix("...")
                        .is_some_and(|cut| level.id.starts_with(cut))
            })
}

/// `(label, effort)` from the footer, the screen's last row in every observed layout. The status
/// line is configurable, so only its first segment is read.
fn footer(screen: &Screen) -> Option<(String, String)> {
    let row = (0..screen.rows.len())
        .rev()
        .map(|y| native::row(screen, y))
        .find(|row| !row.trim().is_empty())?;
    let status = row.split('│').next()?.trim();
    let (label, effort) = status.rsplit_once(" · ").or_else(|| {
        let (label, rest) = status.split_once(" (")?;
        Some((label, rest.split_once(')')?.0))
    })?;
    let effort = effort.to_ascii_lowercase();
    (plain_label(label) && valid_effort(&effort)).then(|| (label.to_owned(), effort))
}

fn confirmed(screen: &Screen, label: &str) -> bool {
    (0..usize::from(screen.cursor.line).saturating_sub(1))
        .rev()
        .map(|y| native::row(screen, y))
        .find(|row| !row.trim().is_empty())
        .is_some_and(|row| {
            let row = row.split_whitespace().collect::<Vec<_>>().join(" ");
            row == format!("⎿ Model set to {label}")
                || row == format!("⎿ Model already set to {label}")
        })
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
            Err(error) => return set(catalog, "reason", error.to_string().into()),
        };
        let (screen, _, _) = tap.capture();
        if !verified(&screen) || !composer(&screen, None) {
            return set(catalog, "reason", WAIT.into());
        }
        set(catalog, "supported", true.into());
        set(catalog, "reason", CONTROL.into());
        set(catalog, "note", NOTE.into());
        if let Some(family) = footer(&screen)
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
        let current = footer(&before);
        let level = match effort {
            Some(effort) => family.level(effort),
            None => current
                .as_ref()
                .and_then(|(_, effort)| family.level(effort))
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
        if current.is_some_and(|(label, effort)| label == family.label && effort == level.effort) {
            return Ok(done(true));
        }
        let command = format!("/model {}", level.id);
        tap.guarded_command(revision.0, command.as_bytes(), |screen, pasted| {
            verified(screen)
                && if pasted {
                    echoed(screen, &command, level)
                } else {
                    composer(screen, None)
                }
        })?;
        let started = Instant::now();
        while started.elapsed() < CONFIRM {
            std::thread::sleep(Duration::from_millis(25));
            let (screen, after, _) = tap.capture();
            if after.0 > revision.0
                && composer(&screen, None)
                && confirmed(&screen, &level.label)
                && native::contains_phrase(
                    &native::fresh_text(&tap, seq.0),
                    &format!("set to {}", level.label),
                )
            {
                return Ok(done(false));
            }
        }
        Ok(json!({"applied":false,"supported":true,"reason":NOT_CONFIRMED}))
    }

    fn set_effort(&self, context: &Context, live: &Live<'_>, effort: &str) -> io::Result<Value> {
        let (families, _) = cached(context);
        let (label, _) = footer(&live.tap()?.capture().0).ok_or_else(no_current_model)?;
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
        let supported = status.get("supported").cloned().unwrap_or(false.into());
        set(status, "effort_supported", supported);
        let Some((label, effort)) = live.screen().as_ref().and_then(footer) else {
            return;
        };
        let (families, _) = cached(context);
        if let Some(family) = families
            .iter()
            .flat_map(|families| families.iter())
            .find(|family| family.label == label)
        {
            observe_live_model(status, &family.id);
        }
        set(status, "model_label", label.into());
        set(status, "effort", effort.into());
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
