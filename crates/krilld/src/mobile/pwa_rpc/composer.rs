//! Session-scoped discovery. Configuration is evidence of available choices, never of a live model.
use super::{Backend, Reply, Request, SessionId, Value, invalid, json};
use crate::agent::control::{LaunchAccount, StoredLaunch};
use crate::session::Tap;
use krill_proto::agent::{Agents, Profile, canonical_id};
use krill_proto::agent_history::ChatPollSnapshot;
use krill_proto::screen::Screen;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod agy;
mod claude;
mod native;

/// One provider's composer control. Everything that differs by provider lives behind this.
trait Adapter: Sync {
    /// Read-only choices. Configuration is reference data, never proof of the live model.
    fn catalog(&self, context: &Context) -> Value;
    /// Upgrades `catalog` to live control, or records why this session cannot have it.
    fn control(&self, context: &Context, live: &Live<'_>, catalog: &mut Value);
    fn set_model(
        &self,
        context: &Context,
        live: &Live<'_>,
        model: &str,
        effort: Option<&str>,
    ) -> io::Result<Value>;
    fn set_effort(&self, context: &Context, live: &Live<'_>, effort: &str) -> io::Result<Value>;
    /// Adds live model, effort and context to the polled `status`.
    fn observe(
        &self,
        context: &Context,
        live: &Live<'_>,
        catalog: &Value,
        snapshot: &ChatPollSnapshot,
        status: &mut Value,
    );
    /// Runs before an explicit catalog read, never on the polled status path.
    fn settle(&self, _context: &Context) {}
}

fn adapter(agent: &str) -> Option<&'static dyn Adapter> {
    match agent {
        "codex" => Some(&native::Codex),
        "claude" => Some(&claude::Claude),
        "agy" => Some(&agy::Agy),
        _ => None,
    }
}

struct Live<'a> {
    backend: &'a Backend,
    session: SessionId,
}

impl Live<'_> {
    /// Gated on an idle agent, so holding one is the authority to write.
    fn tap(&self) -> io::Result<Tap> {
        self.backend
            .registry
            .upgrade()
            .ok_or_else(invalid)?
            .mobile_composer_tap(self.session)
    }

    /// Read-only, so it also works while the agent is busy.
    fn screen(&self) -> Option<Screen> {
        self.backend
            .registry
            .upgrade()?
            .session_capture(self.session)
    }
}

pub(super) struct Context {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub agent: String,
    pub config: Option<PathBuf>,
    env: BTreeMap<String, String>,
}

impl Backend {
    pub(super) fn composer_models(&self, session: u64) -> io::Result<Value> {
        let context = self.composer_context(session)?;
        if let Some(adapter) = adapter(&context.agent) {
            adapter.settle(&context);
        }
        Ok(self.composer_catalog(&context, session))
    }

    fn composer_catalog(&self, context: &Context, session: u64) -> Value {
        let mut models = context.models();
        if cfg!(windows) {
            set(&mut models, "reason", "Verified model changes are unavailable on Windows. Keep chatting with the current model.".into());
        } else if let Some(adapter) = adapter(&context.agent) {
            adapter.control(context, &self.live(session), &mut models);
        }
        models
    }

    pub(super) fn set_composer_model(
        &self,
        session: u64,
        model: &str,
        effort: Option<&str>,
    ) -> io::Result<Value> {
        let context = self.composer_context(session)?;
        match adapter(&context.agent) {
            Some(adapter) => adapter.set_model(&context, &self.live(session), model, effort),
            None => validate_model(&context.models(), model),
        }
    }

    pub(super) fn composer_status(&self, session: u64) -> io::Result<Value> {
        let context = self.composer_context(session)?;
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        let snapshot = registry.chat_poll(SessionId(session), u64::MAX);
        let mut value = super::composer_status::read(&snapshot, &context)?;
        let catalog = self.composer_catalog(&context, session);
        set(
            &mut value,
            "models",
            catalog.get("items").cloned().unwrap_or_else(|| json!([])),
        );
        let supported = catalog
            .get("supported")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        set(&mut value, "supported", supported.into());
        set(&mut value, "effort_supported", false.into());
        if let Some(adapter) = adapter(&context.agent) {
            let live = self.live(session);
            adapter.observe(&context, &live, &catalog, &snapshot, &mut value);
        }
        let identity = crate::agent_history::chat::generation(&snapshot);
        let generation =
            super::hex(ring::digest::digest(&ring::digest::SHA256, identity.as_bytes()).as_ref());
        set(&mut value, "generation", generation.into());
        Ok(value)
    }

    pub(super) fn set_composer_effort(&self, session: u64, effort: &str) -> io::Result<Value> {
        let context = self.composer_context(session)?;
        adapter(&context.agent).map_or_else(
            || Err(no_current_model()),
            |adapter| adapter.set_effort(&context, &self.live(session), effort),
        )
    }

    fn live(&self, session: u64) -> Live<'_> {
        Live {
            backend: self,
            session: SessionId(session),
        }
    }

    pub(super) fn composer_context(&self, session: u64) -> io::Result<Context> {
        // Resolve a live session before touching any account/project metadata. Client paths are never accepted.
        let cwd = self.cwd(session)?;
        let control = crate::agent::control::Control::new(self.dir.as_deref());
        let stored = control.stored(SessionId(session))?;
        let observed = match self.control(&Request::States)? {
            Reply::States(states) => states.into_iter().find(|s| s.session.0 == session),
            _ => None,
        };
        let status = observed.as_ref().map(|s| &s.status);
        let agent = status
            .map(|s| s.agent.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| status.and_then(|s| s.foreground.as_deref()))
            .unwrap_or_default();
        let home = krill_proto::os::home_dir().ok_or_else(invalid)?;
        Ok(resolve(
            &home,
            &cwd,
            agent,
            stored.as_ref(),
            &control.agents()?,
            |key| std::env::var(key).ok(),
        ))
    }
}

fn no_current_model() -> io::Error {
    io::Error::other("Wait for an idle chat with an observed current model")
}

pub(super) fn command_context(
    stored: Option<&StoredLaunch>,
    agents: &Agents,
    observed: &str,
) -> String {
    let profile = stored.map_or(observed, |s| s.profile.as_str());
    let configured = agents.resolve(profile).map(|(_, profile)| profile);
    let bytes = serde_json::to_vec(&(profile, stored.and_then(|s| s.account.as_ref()), configured))
        .unwrap_or_default();
    super::hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref())
}

fn set(value: &mut Value, key: &str, item: Value) {
    if let Some(fields) = value.as_object_mut() {
        fields.insert(key.into(), item);
    }
}

fn observe_live_model(value: &mut Value, model: &str) {
    if value.get("model").and_then(Value::as_str) != Some(model) {
        set(value, "context", Value::Null);
        set(value, "effort", Value::Null);
    }
    set(value, "model", model.into());
}

fn valid_effort(effort: &str) -> bool {
    matches!(
        effort,
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    )
}

fn validate_effort(catalog: &Value, model: &str, effort: Option<&str>) -> io::Result<()> {
    let Some(effort) = effort else {
        return Ok(());
    };
    let supported = valid_effort(effort)
        && catalog
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(model))
            .is_some_and(|item| {
                item.get("efforts")
                    .and_then(Value::as_array)
                    .is_some_and(|levels| levels.iter().any(|value| value.as_str() == Some(effort)))
                    || item.get("default_effort").and_then(Value::as_str) == Some(effort)
            });
    if !supported {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Effort is not in this session model's discovered catalog",
        ));
    }
    Ok(())
}

fn provider(agent: &str, profile: Option<&Profile>) -> String {
    if matches!(
        canonical_id(agent),
        "codex" | "claude" | "agy" | "cursor" | "opencode" | "pi"
    ) {
        return canonical_id(agent).to_owned();
    }
    let program = profile.and_then(Profile::program).unwrap_or(agent);
    canonical_id(
        Path::new(program)
            .file_stem()
            .and_then(|p| p.to_str())
            .unwrap_or(program),
    )
    .to_owned()
}

fn resolve(
    home: &Path,
    cwd: &Path,
    observed: &str,
    stored: Option<&StoredLaunch>,
    agents: &Agents,
    inherited: impl Fn(&str) -> Option<String>,
) -> Context {
    let stored_profile = stored
        .and_then(|s| agents.resolve(&s.profile))
        .map(|(_, p)| p);
    let observed_profile = agents.resolve(observed).map(|(_, p)| p);
    let observed_provider = provider(observed, observed_profile);
    // Hooks report canonical provider names; retain the actual custom launch profile's env.
    let profile = if stored.is_some_and(|s| {
        observed.is_empty() || provider(&s.profile, stored_profile) == observed_provider
    }) {
        stored_profile
    } else {
        observed_profile
    };
    let agent = if observed.is_empty() {
        provider(stored.map_or("", |s| s.profile.as_str()), profile)
    } else {
        provider(observed, profile)
    };
    let key = match agent.as_str() {
        "claude" => "CLAUDE_CONFIG_DIR",
        "codex" => "CODEX_HOME",
        "agy" => "ANTIGRAVITY_CLI_HOME",
        "opencode" => "OPENCODE_CONFIG_DIR",
        "pi" => "PI_CODING_AGENT_DIR",
        _ => "",
    };
    let account = stored
        .and_then(|s| s.account.as_ref())
        .filter(|a| a.env_key == key);
    let configured = if let Some(LaunchAccount { dir, .. }) = account {
        // An explicitly recorded host account must not inherit a different profile selected later.
        (!dir.is_empty()).then(|| dir.clone())
    } else {
        profile
            .and_then(|p| p.env.get(key).cloned())
            .or_else(|| inherited(key))
    };
    Context {
        home: home.to_owned(),
        cwd: cwd.to_owned(),
        agent,
        config: configured.filter(|p| !p.is_empty()).map(|p| {
            let path = PathBuf::from(p);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }),
        env: profile.map(|p| p.env.clone()).unwrap_or_default(),
    }
}

fn read(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() || file.metadata().ok()?.len() > 2 * 1024 * 1024 {
        return None;
    }
    let mut bytes = String::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut bytes)
        .ok()?;
    (bytes.len() <= 2 * 1024 * 1024).then_some(bytes)
}

fn valid_id(id: &str) -> bool {
    id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !id.split('/').any(|part| part == "..")
        && id.len() <= 160
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c))
}

/// Read-only discovery results, deduplicated and bounded.
#[derive(Default)]
struct Discovered(BTreeMap<String, Value>);

impl Discovered {
    fn add(&mut self, id: &str, label: &str, metadata: Option<&Value>) {
        if valid_id(id) && self.0.len() < 128 {
            self.0.entry(id.to_owned()).or_insert_with(|| json!({"id":id,"label":label.chars().filter(|c| !c.is_control()).take(160).collect::<String>(),"efforts":metadata.and_then(|m|m.get("supported_reasoning_levels")).and_then(Value::as_array).into_iter().flatten().filter_map(|e|e.get("effort").and_then(Value::as_str)).filter(|e|valid_effort(e)).take(16).collect::<Vec<_>>(),"default_effort":metadata.and_then(|m|m.get("default_reasoning_level")).and_then(Value::as_str).filter(|e|valid_effort(e))}));
        }
    }

    fn into_catalog(self, agent: &str, reason: &str) -> Value {
        let truncated = self.0.len() == 128;
        json!({"agent":agent,"supported":false,"current":null,"items":self.0.into_values().collect::<Vec<_>>(),"truncated":truncated,"reason":reason})
    }
}

impl Context {
    pub(super) fn models(&self) -> Value {
        adapter(&self.agent).map_or_else(
            || Discovered::default().into_catalog(&self.agent, "This provider does not expose verified model control for the running chat session. No model was changed."),
            |adapter| adapter.catalog(self),
        )
    }

    #[cfg(test)]
    pub(super) fn set_model(&self, id: &str) -> io::Result<Value> {
        validate_model(&self.models(), id)
    }
}

fn validate_model(catalog: &Value, id: &str) -> io::Result<Value> {
    if !valid_id(id)
        || !catalog
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("id").and_then(Value::as_str) == Some(id))
            })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Model is not in this session's discovered catalog",
        ));
    }
    // Never send a blind TUI /model command, resume a replacement, or mutate account config.
    Ok(json!({"applied":false,"supported":false,"reason":catalog.get("reason")}))
}

#[cfg(test)]
mod tests;
