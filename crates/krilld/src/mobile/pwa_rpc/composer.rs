//! Session-scoped discovery. Configuration is evidence of available choices, never of a live model.
use super::{Backend, Reply, Request, SessionId, Value, invalid, json};
use crate::agent::control::{LaunchAccount, StoredLaunch};
use krill_proto::agent::{Agents, Profile, canonical_id};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod claude;
mod native;

pub(super) struct Context {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub agent: String,
    pub config: Option<PathBuf>,
    env: BTreeMap<String, String>,
    verified_codex: bool,
}

impl Backend {
    pub(super) fn composer_models(&self, session: u64) -> io::Result<Value> {
        let context = self.composer_context(session)?;
        let mut models = context.models();
        if cfg!(windows) {
            set(&mut models, "reason", "Verified model changes are unavailable on Windows. Keep chatting with the current model.".into());
        }
        if cfg!(unix) && context.agent == "codex" {
            match self
                .registry
                .upgrade()
                .ok_or_else(invalid)?
                .mobile_composer_tap(SessionId(session))
            {
                Ok(tap) => {
                    let (screen, _, _) = tap.capture();
                    if (context.verified_codex || native::verified(&screen))
                        && native::composer(&screen, None)
                    {
                        set(&mut models, "supported", true.into());
                        set(
                            &mut models,
                            "reason",
                            "Changes the model in this running Codex chat session only.".into(),
                        );
                        if let Some(current) = observed_model(&models, &screen) {
                            set(&mut models, "current", current.into());
                        }
                    } else {
                        set(&mut models, "reason", "Wait for an idle Codex chat with an empty native composer. Direct model control is verified for Codex 0.164.0.".into());
                    }
                }
                Err(error) => set(&mut models, "reason", error.to_string().into()),
            }
        }
        if cfg!(unix) && context.agent == "claude" {
            match self
                .registry
                .upgrade()
                .ok_or_else(invalid)?
                .mobile_composer_tap(SessionId(session))
                .and_then(|tap| claude::models(&context, &tap))
            {
                Ok(native) => return Ok(native),
                Err(error) => set(&mut models, "reason", error.to_string().into()),
            }
        }
        Ok(models)
    }

    pub(super) fn set_composer_model(&self, session: u64, model: &str) -> io::Result<Value> {
        let context = self.composer_context(session)?;
        if context.agent == "claude" {
            let tap = self
                .registry
                .upgrade()
                .ok_or_else(invalid)?
                .mobile_composer_tap(SessionId(session))?;
            return claude::set_model(&context, &tap, model);
        }
        let catalog = context.models();
        let fallback = context.validate_model(&catalog, model)?;
        if context.agent != "codex" {
            return Ok(fallback);
        }
        let tap = self
            .registry
            .upgrade()
            .ok_or_else(invalid)?
            .mobile_composer_tap(SessionId(session))?;
        let (before, revision, seq) = tap.capture();
        if !(context.verified_codex || native::verified(&before))
            || !native::composer(&before, None)
        {
            return Err(io::Error::other(
                "Wait for an idle Codex 0.164.0 chat with an empty native composer",
            ));
        }
        if observed_model(&catalog, &before).as_deref() == Some(model) {
            return Ok(
                json!({"applied":true,"supported":true,"model":model,"already_selected":true}),
            );
        }
        let command = format!("/model {model}");
        tap.guarded_command(revision.0, command.as_bytes(), |screen, pasted| {
            (context.verified_codex || native::verified(screen))
                && native::composer(screen, pasted.then_some(command.as_str()))
        })?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(25));
            let (screen, observed, _) = tap.capture();
            if observed.0 > revision.0
                && native::composer(&screen, None)
                && observed_model(&catalog, &screen).as_deref() == Some(model)
                && native::latest_confirmation(&screen, model)
                && native::fresh_confirmation(&tap, seq.0, model)
            {
                return Ok(json!({"applied":true,"supported":true,"model":model}));
            }
        }
        Ok(
            json!({"applied":false,"supported":true,"reason":"Codex received the model command, but its session-only change was not confirmed. Check the chat; the command will not be resent."}),
        )
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

fn observed_model(models: &Value, screen: &krill_proto::screen::Screen) -> Option<String> {
    let footer = native::row(screen, usize::from(screen.cursor.line) + 2);
    models
        .get("items")?
        .as_array()?
        .iter()
        .filter_map(|item| Some((item.get("label")?.as_str()?, item.get("id")?.as_str()?)))
        .filter(|(label, _)| footer.trim_start().starts_with(&format!("{label} ")))
        .max_by_key(|(label, _)| label.len())
        .map(|(_, id)| id.to_owned())
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
        verified_codex: stored.is_some_and(verified_codex_transcript),
    }
}

fn verified_codex_transcript(stored: &StoredLaunch) -> bool {
    let Some(path) = stored
        .transcript_path
        .as_deref()
        .filter(|p| crate::agent::control::valid_transcript_path(p))
    else {
        return false;
    };
    let Some(id) = stored.session_id.as_deref() else {
        return false;
    };
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(8192).read_to_end(&mut bytes).is_err() {
        return false;
    }
    let Some(line) = bytes.split(|b| *b == b'\n').next() else {
        return false;
    };
    let Ok(meta) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    meta.get("type").and_then(Value::as_str) == Some("session_meta")
        && meta.pointer("/payload/id").and_then(Value::as_str) == Some(id)
        && meta.pointer("/payload/cli_version").and_then(Value::as_str) == Some("0.164.0")
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

impl Context {
    pub(super) fn models(&self) -> Value {
        let mut items = BTreeMap::new();
        let mut add = |id: &str, label: &str| {
            if valid_id(id) && items.len() < 128 {
                items.entry(id.to_owned()).or_insert_with(|| json!({"id":id,"label":label.chars().filter(|c| !c.is_control()).take(160).collect::<String>()}));
            }
        };
        match self.agent.as_str() {
            "codex" => {
                let root = self
                    .config
                    .clone()
                    .unwrap_or_else(|| self.home.join(".codex"));
                if let Some(cache) = read(&root.join("models_cache.json"))
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                {
                    for model in cache
                        .get("models")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .take(512)
                    {
                        if model
                            .get("visibility")
                            .and_then(Value::as_str)
                            .is_some_and(|v| v != "list")
                        {
                            continue;
                        }
                        if let Some(id) = model.get("slug").and_then(Value::as_str) {
                            add(
                                id,
                                model
                                    .get("display_name")
                                    .and_then(Value::as_str)
                                    .unwrap_or(id),
                            );
                        }
                    }
                }
                if let Some(config) = read(&root.join("config.toml"))
                    .and_then(|s| toml::from_str::<toml::Table>(&s).ok())
                {
                    if let Some(id) = config.get("model").and_then(toml::Value::as_str) {
                        add(id, id);
                    }
                    for profile in config
                        .get("profiles")
                        .and_then(toml::Value::as_table)
                        .into_iter()
                        .flat_map(|p| p.values())
                        .take(128)
                    {
                        if let Some(id) = profile.get("model").and_then(toml::Value::as_str) {
                            add(id, id);
                        }
                    }
                }
            }
            "claude" => {
                let root = self
                    .config
                    .clone()
                    .unwrap_or_else(|| self.home.join(".claude"));
                for path in [
                    root.join("settings.json"),
                    self.cwd.join(".claude/settings.json"),
                    self.cwd.join(".claude/settings.local.json"),
                ] {
                    if let Some(settings) =
                        read(&path).and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    {
                        if let Some(id) = settings.get("model").and_then(Value::as_str) {
                            add(id, id);
                        }
                        for id in settings
                            .get("availableModels")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .take(128)
                        {
                            add(id, id);
                        }
                    }
                }
                for (key, id) in &self.env {
                    if key == "ANTHROPIC_MODEL"
                        || key.starts_with("ANTHROPIC_DEFAULT_") && key.ends_with("_MODEL")
                    {
                        add(id, id);
                    }
                }
            }
            _ => {}
        }
        let truncated = items.len() == 128;
        json!({"agent":self.agent,"supported":false,"current":null,"items":items.into_values().collect::<Vec<_>>(),"truncated":truncated,"reason":self.reason()})
    }

    fn reason(&self) -> &'static str {
        match self.agent.as_str() {
            "codex" => {
                "This running Codex session does not expose a verified model control connection. Discovered models are from local configuration/cache; no model was changed."
            }
            "claude" => {
                "This running Claude Code session does not expose verified model control. Configured models are shown for reference; no model was changed."
            }
            _ => {
                "This provider does not expose verified model control for the running chat session. No model was changed."
            }
        }
    }

    #[cfg(test)]
    pub(super) fn set_model(&self, id: &str) -> io::Result<Value> {
        self.validate_model(&self.models(), id)
    }

    fn validate_model(&self, catalog: &Value, id: &str) -> io::Result<Value> {
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
        Ok(json!({"applied":false,"supported":false,"reason":self.reason()}))
    }
}

#[cfg(test)]
mod tests;
