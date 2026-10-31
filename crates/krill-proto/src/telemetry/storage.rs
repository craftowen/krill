//! Atomic bounded queue. A separate upload lock permits opt-out during network IO.
//! Generation checks fence replies from uploads started before disable/re-enable.

use super::usage::valid_uuid;
use super::{
    Ack, Batch, DISABLED_FILE, Env, MAX_BATCH, MAX_PENDING, MAX_REQUEST_BYTES, UsageEvent,
    UsageName, consent, random_uuid,
};
use crate::os::fs::DirBuilderExt as _;
use serde::{Deserialize, Serialize};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, DirBuilder, File, TryLockError};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DIR: &str = "telemetry";
const FILE: &str = "state.json";
const MAX_STATE_BYTES: u64 = 128 * 1024;
const DAY: u64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    install_id: String,
    generation: String,
    first_open: bool,
    active_day: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_active_day: Option<u64>,
    #[serde(deserialize_with = "decode_events")]
    events: Vec<UsageEvent>,
    failures: u32,
    next_attempt_unix: Option<u64>,
    last_success_unix: Option<u64>,
    last_outcome: Outcome,
}
fn decode_events<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<UsageEvent>, D::Error> {
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|value| serde_json::from_value::<UsageEvent>(value).ok())
        .filter(UsageEvent::valid)
        .take(MAX_PENDING)
        .collect())
}

fn prune(queue: &mut Queue, now: u64) {
    queue.events.retain(|event| {
        event.valid()
            && event.install_id == queue.install_id
            && now.saturating_sub(event.ts_unix) <= 30 * DAY
            && event.ts_unix <= now.saturating_add(300)
    });
}

impl Queue {
    fn new() -> io::Result<Self> {
        Ok(Self {
            install_id: random_uuid()?,
            generation: random_uuid()?,
            first_open: false,
            active_day: None,
            agent_active_day: None,
            events: Vec::new(),
            failures: 0,
            next_attempt_unix: None,
            last_success_unix: None,
            last_outcome: Outcome::Empty,
        })
    }
    fn valid(&self) -> bool {
        valid_uuid(&self.install_id)
            && valid_uuid(&self.generation)
            && self.events.len() <= MAX_PENDING
            && self
                .events
                .iter()
                .all(|event| event.valid() && event.install_id == self.install_id)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    #[default]
    Empty,
    Sent,
    Deferred,
    Backoff,
    Disabled,
    Busy,
    Withdrawn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub enabled: bool,
    pub disabled_by_user: bool,
    pub veto: Option<String>,
    pub pending: usize,
    pub install_id: Option<String>,
    pub next_attempt_unix: Option<u64>,
    pub last_success_unix: Option<u64>,
    pub last_outcome: Outcome,
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FlushReport {
    pub outcome: Outcome,
    pub accepted: usize,
    pub pending: usize,
    pub next_attempt_unix: Option<u64>,
}

fn dir(state: &Path) -> io::Result<PathBuf> {
    let dir = state.join(DIR);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    if !fs::symlink_metadata(&dir)?.is_dir() {
        return Err(io::Error::other("invalid telemetry directory"));
    }
    Ok(dir)
}
fn open(path: &Path, create: bool) -> io::Result<File> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags};
        File::from(rustix::fs::open(
            path,
            (if create {
                OFlags::RDWR | OFlags::CREATE
            } else {
                OFlags::RDONLY
            }) | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?)
    };
    #[cfg(not(unix))]
    let file = {
        if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
            return Err(io::Error::other("invalid telemetry file"));
        }
        OpenOptions::new()
            .read(true)
            .write(create)
            .create(create)
            .truncate(false)
            .open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("invalid telemetry file"));
    }
    Ok(file)
}
fn lock(dir: &Path, name: &str, wait: Duration) -> io::Result<File> {
    let file = open(&dir.join(name), true)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::Error(e)) => return Err(e),
            Err(TryLockError::WouldBlock) => {}
        }
        if start.elapsed() >= wait {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn read(dir: &Path) -> io::Result<Option<Queue>> {
    let file = match open(&dir.join(FILE), false) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
        return Err(io::Error::other("telemetry state exceeds limit"));
    }
    let mut queue: Queue = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    queue
        .events
        .retain(|event| event.install_id == queue.install_id);
    queue.next_attempt_unix = queue
        .next_attempt_unix
        .map(|at| at.min(super::now_unix().saturating_add(DAY)));
    if !queue.valid() {
        return Err(io::Error::other("invalid telemetry state"));
    }
    Ok(Some(queue))
}
fn write(dir: &Path, queue: &Queue) -> io::Result<()> {
    let bytes = serde_json::to_vec(queue).map_err(io::Error::other)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
        return Err(io::Error::other("telemetry state exceeds limit"));
    }
    let tmp = dir.join("state.tmp");
    let result = (|| {
        let mut file = open(&tmp, true)?;
        file.set_len(0)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, dir.join(FILE))?;
        #[cfg(unix)]
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        fs::remove_file(tmp).ok();
    }
    result
}

pub(super) fn set_enabled(state: &Path, enabled: bool) -> io::Result<()> {
    if crate::error_reporting::shielded(state) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test process must not change the default telemetry consent",
        ));
    }
    let dir = dir(state)?;
    let _lock = lock(&dir, "queue.lock", Duration::from_secs(3))?;
    let marker = state.join(DISABLED_FILE);
    if enabled && !marker.exists() {
        return Ok(());
    }
    if !enabled {
        // Publish the veto before any fallible queue work. A failed fence still
        // prevents recording/upload; a future enable must fence again first.
        let mut file = open(&marker, true)?;
        file.set_len(0)?;
        file.write_all(b"disabled\n")?;
        file.sync_all()?;
    }
    let mut queue = match read(&dir)? {
        Some(queue) => queue,
        None if enabled => Queue::new()?,
        None => return Ok(()),
    };
    queue.events.clear();
    queue.generation = random_uuid()?;
    queue.failures = 0;
    queue.next_attempt_unix = None;
    queue.last_outcome = if enabled {
        Outcome::Empty
    } else {
        Outcome::Disabled
    };
    write(&dir, &queue)?;
    if enabled {
        // Last commit step: if preparing/writing the new generation fails, the
        // marker remains and old cached inputs/events cannot become enabled.
        fs::remove_file(marker)?;
    }
    Ok(())
}

/// Opaque local policy token. Never serialized into a telemetry event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityGeneration(String);

/// Background-only policy refresh. Disabled/unknown policies yield no token.
/// An enabled policy establishes its durable queue generation before the UI may
/// retain any input. In particular, missing identity is never a wildcard token.
pub fn activity_generation(state: &Path, env: &Env) -> io::Result<Option<ActivityGeneration>> {
    if !consent(Some(state), env).enabled() {
        return Ok(None);
    }
    let dir = dir(state)?;
    let _lock = lock(&dir, "queue.lock", Duration::from_secs(1))?;
    if !consent(Some(state), env).enabled() {
        return Ok(None);
    }
    let queue = if let Some(queue) = read(&dir)? {
        queue
    } else {
        let queue = Queue::new()?;
        write(&dir, &queue)?;
        queue
    };
    Ok(Some(ActivityGeneration(queue.generation)))
}

/// Record only if the input's original policy generation is still enabled.
/// Both the permission check and generation comparison occur under queue.lock,
/// so disable/enable between a policy refresh and this lock rejects the input.
pub fn record_activity_for_generation(
    state: &Path,
    env: &Env,
    now: u64,
    expected: &ActivityGeneration,
) -> io::Result<bool> {
    record(state, env, now, false, Some(expected))
}

pub fn status_with(state: &Path, env: &Env) -> io::Result<Status> {
    let queue = read(&state.join(DIR))?;
    let permission = consent(Some(state), env);
    Ok(Status {
        enabled: permission.enabled(),
        disabled_by_user: state.join(DISABLED_FILE).exists(),
        veto: permission.veto_var().map(str::to_owned),
        pending: queue.as_ref().map_or(0, |q| q.events.len()),
        install_id: queue.as_ref().map(|q| q.install_id.clone()),
        next_attempt_unix: queue.as_ref().and_then(|q| q.next_attempt_unix),
        last_success_unix: queue.as_ref().and_then(|q| q.last_success_unix),
        last_outcome: queue.map_or(Outcome::Empty, |q| q.last_outcome),
    })
}

/// Startup records `first_open` once per installation and `app_opened` per app process.
/// Call only from the app startup background task, never daemon restore/CLI attach.
pub fn record_open(state: &Path, env: &Env, now: u64) -> io::Result<()> {
    record(state, env, now, true, None).map(|_| ())
}
/// Synchronous foreground input only. Deferred app input must use
/// [`record_activity_for_generation`] to retain its original policy boundary.
pub fn record_activity(state: &Path, env: &Env, now: u64) -> io::Result<()> {
    record(state, env, now, false, None).map(|_| ())
}

/// Records `agent_active_day` (and `active_day` if not yet recorded for the UTC day).
pub fn record_agent_active(state: &Path, env: &Env, now: u64) -> io::Result<bool> {
    if !consent(Some(state), env).enabled() {
        return Ok(false);
    }
    let dir = dir(state)?;
    let _lock = lock(&dir, "queue.lock", Duration::from_secs(1))?;
    if !consent(Some(state), env).enabled() {
        return Ok(false);
    }
    let mut queue = match read(&dir)? {
        Some(queue) => queue,
        None => Queue::new()?,
    };
    prune(&mut queue, now);
    let day = now.checked_div(DAY).unwrap_or(0);
    let mut names = Vec::new();
    if queue.active_day.is_none_or(|previous| day > previous) {
        names.push(UsageName::ActiveDay);
        queue.active_day = Some(day);
    }
    if queue.agent_active_day.is_none_or(|previous| day > previous) {
        names.push(UsageName::AgentActiveDay);
        queue.agent_active_day = Some(day);
    }
    if names.is_empty() {
        return Ok(false);
    }
    for name in names {
        push_event(&mut queue, name, now)?;
    }
    write(&dir, &queue)?;
    Ok(true)
}

/// Records an explicit funnel event (e.g. `star_prompt_*`) under the current consent.
pub fn record_event(state: &Path, env: &Env, name: UsageName, now: u64) -> io::Result<bool> {
    if !consent(Some(state), env).enabled() {
        return Ok(false);
    }
    let dir = dir(state)?;
    let _lock = lock(&dir, "queue.lock", Duration::from_secs(1))?;
    if !consent(Some(state), env).enabled() {
        return Ok(false);
    }
    let mut queue = match read(&dir)? {
        Some(queue) => queue,
        None => Queue::new()?,
    };
    prune(&mut queue, now);
    push_event(&mut queue, name, now)?;
    write(&dir, &queue)?;
    Ok(true)
}

fn push_event(queue: &mut Queue, name: UsageName, now: u64) -> io::Result<()> {
    let event = UsageEvent::new(&queue.install_id, name, now)?;
    if queue.events.len() >= MAX_PENDING {
        let oldest = queue
            .events
            .iter()
            .position(|event| event.name != UsageName::FirstOpen)
            .unwrap_or(0);
        queue.events.remove(oldest);
    }
    queue.events.push(event);
    Ok(())
}

fn record(
    state: &Path,
    env: &Env,
    now: u64,
    startup: bool,
    expected: Option<&ActivityGeneration>,
) -> io::Result<bool> {
    if !consent(Some(state), env).enabled() {
        return Ok(false);
    }
    let dir = dir(state)?;
    let _lock = lock(&dir, "queue.lock", Duration::from_secs(1))?;
    if !consent(Some(state), env).enabled() {
        return Ok(false);
    }
    let mut queue = match (read(&dir)?, expected) {
        (Some(queue), Some(token)) if queue.generation == token.0 => queue,
        (Some(queue), None) => queue,
        (None, None) => Queue::new()?,
        _ => return Ok(false),
    };
    prune(&mut queue, now);
    let day = now.checked_div(DAY).unwrap_or(0);
    let mut names = Vec::new();
    if startup {
        if !queue.first_open {
            names.push(UsageName::FirstOpen);
            queue.first_open = true;
        }
        names.push(UsageName::AppOpened);
    } else if queue.active_day.is_none_or(|previous| day > previous) {
        names.push(UsageName::ActiveDay);
        queue.active_day = Some(day);
    } else {
        return Ok(false);
    }
    for name in names {
        push_event(&mut queue, name, now)?;
    }
    write(&dir, &queue)?;
    Ok(true)
}

/// One bounded HTTP batch. Transport returns HTTP status and response body; only
/// 200 with exact whole-batch accepted count removes events. Retries retain IDs.
pub fn flush_with(
    state: &Path,
    env: &Env,
    now: u64,
    mut transport: impl FnMut(&[u8]) -> io::Result<(u16, Vec<u8>)>,
) -> io::Result<FlushReport> {
    let report = |outcome, accepted, queue: Option<&Queue>| FlushReport {
        outcome,
        accepted,
        pending: queue.map_or(0, |q| q.events.len()),
        next_attempt_unix: queue.and_then(|q| q.next_attempt_unix),
    };
    if !consent(Some(state), env).enabled() {
        return Ok(report(Outcome::Disabled, 0, None));
    }
    let dir = dir(state)?;
    let _flight = match lock(&dir, "upload.lock", Duration::ZERO) {
        Ok(lock) => lock,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            return Ok(report(Outcome::Busy, 0, None));
        }
        Err(e) => return Err(e),
    };
    let queue = {
        let _lock = lock(&dir, "queue.lock", Duration::from_secs(1))?;
        if !consent(Some(state), env).enabled() {
            return Ok(report(Outcome::Disabled, 0, None));
        }
        let Some(mut queue) = read(&dir)? else {
            return Ok(report(Outcome::Empty, 0, None));
        };
        prune(&mut queue, now);
        write(&dir, &queue)?;
        if queue.events.is_empty() {
            return Ok(report(Outcome::Empty, 0, Some(&queue)));
        }
        if queue.next_attempt_unix.is_some_and(|at| at > now) {
            return Ok(report(Outcome::Backoff, 0, Some(&queue)));
        }
        // Persist retry deadline before IO so killing the helper cannot bypass backoff.
        queue.failures = queue.failures.saturating_add(1);
        let delay = 30_u64
            .saturating_mul(
                1_u64
                    .checked_shl(queue.failures.saturating_sub(1).min(12))
                    .unwrap_or(1024),
            )
            .min(DAY);
        queue.next_attempt_unix = Some(now.saturating_add(delay));
        queue.last_outcome = Outcome::Deferred;
        write(&dir, &queue)?;
        queue
    };
    let batch = Batch {
        v: 1,
        events: queue.events.iter().take(MAX_BATCH).cloned().collect(),
    };
    let body = serde_json::to_vec(&batch).map_err(io::Error::other)?;
    if !batch.valid() || body.len() > MAX_REQUEST_BYTES {
        return Err(io::Error::other("invalid telemetry batch"));
    }
    if !consent(Some(state), env).enabled() {
        return Ok(report(Outcome::Withdrawn, 0, None));
    }
    {
        let _lock = lock(&dir, "queue.lock", Duration::from_secs(1))?;
        if !consent(Some(state), env).enabled()
            || read(&dir)?.is_none_or(|current| current.generation != queue.generation)
        {
            return Ok(report(Outcome::Withdrawn, 0, None));
        }
    }
    let accepted = transport(&body).is_ok_and(|(status, body)| {
        status == 200
            && body.len() <= 1024
            && serde_json::from_slice::<Ack>(&body)
                .is_ok_and(|ack| ack.accepted == batch.events.len())
    });
    let _lock = lock(&dir, "queue.lock", Duration::from_secs(1))?;
    let Some(mut current) = read(&dir)? else {
        return Ok(report(Outcome::Withdrawn, 0, None));
    };
    if !consent(Some(state), env).enabled() || current.generation != queue.generation {
        return Ok(report(Outcome::Withdrawn, 0, Some(&current)));
    }
    if accepted {
        current
            .events
            .retain(|event| !batch.events.iter().any(|sent| sent.id == event.id));
        current.failures = 0;
        current.next_attempt_unix = None;
        current.last_success_unix = Some(now);
        current.last_outcome = Outcome::Sent;
        write(&dir, &current)?;
        Ok(report(Outcome::Sent, batch.events.len(), Some(&current)))
    } else {
        Ok(report(Outcome::Deferred, 0, Some(&current)))
    }
}
