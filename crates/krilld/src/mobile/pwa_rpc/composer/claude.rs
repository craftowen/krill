//! Claude native aliases, from 2.1.311. Discovery is read-only; native /model may save its default.
use super::{
    Adapter, ChatPollSnapshot, Context, Discovered, Duration, Instant, Live, Tap, Value, io, json,
    native, read, set,
};
use krill_proto::screen::{Flags, Screen};
use native::border;

const BANNER: &str = "Claude Code v";
const FLOOR: &[u64] = &[2, 1, 311];
const EFFORT_FLOOR: &[u64] = &[2, 1, 314];
const WAIT: &str =
    "Wait for an idle Claude Code 2.1.311 or later chat with an empty native composer";

pub(super) struct Claude;

impl Adapter for Claude {
    fn catalog(&self, context: &Context) -> Value {
        let mut models = Discovered::default();
        let root = context
            .config
            .clone()
            .unwrap_or_else(|| context.home.join(".claude"));
        for path in [
            root.join("settings.json"),
            context.cwd.join(".claude/settings.json"),
            context.cwd.join(".claude/settings.local.json"),
        ] {
            if let Some(settings) = read(&path).and_then(|s| serde_json::from_str::<Value>(&s).ok())
            {
                if let Some(id) = settings.get("model").and_then(Value::as_str) {
                    models.add(id, id, None);
                }
                for id in settings
                    .get("availableModels")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .take(128)
                {
                    models.add(id, id, None);
                }
            }
        }
        for (key, id) in &context.env {
            if key == "ANTHROPIC_MODEL"
                || key.starts_with("ANTHROPIC_DEFAULT_") && key.ends_with("_MODEL")
            {
                models.add(id, id, None);
            }
        }
        models.into_catalog("claude", "This running Claude Code session does not expose verified model control. Configured models are shown for reference; no model was changed.")
    }

    fn control(&self, context: &Context, live: &Live<'_>, catalog: &mut Value) {
        match live.tap().and_then(|tap| models(context, &tap)) {
            Ok(native) => *catalog = native,
            Err(error) => set(catalog, "reason", error.to_string().into()),
        }
    }

    fn set_model(
        &self,
        context: &Context,
        live: &Live<'_>,
        model: &str,
        effort: Option<&str>,
    ) -> io::Result<Value> {
        if effort.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Choose Claude effort separately from the model",
            ));
        }
        set_model(context, &live.tap()?, model)
    }

    fn set_effort(&self, _context: &Context, live: &Live<'_>, effort: &str) -> io::Result<Value> {
        set_effort(&live.tap()?, effort)
    }

    fn observe(
        &self,
        context: &Context,
        live: &Live<'_>,
        _catalog: &Value,
        snapshot: &ChatPollSnapshot,
        status: &mut Value,
    ) {
        observe_statusline(context, live, snapshot, status);
        let effort_supported = status.get("supported").and_then(Value::as_bool) == Some(true)
            && live
                .tap()
                .is_ok_and(|tap| effort_available(&tap.capture().0));
        set(status, "effort_supported", effort_supported.into());
        if effort_supported {
            set(
                status,
                "efforts",
                json!(["low", "medium", "high", "xhigh", "max"]),
            );
        }
    }
}

fn observe_statusline(
    context: &Context,
    live: &Live<'_>,
    snapshot: &ChatPollSnapshot,
    status: &mut Value,
) {
    let profile = context
        .config
        .clone()
        .unwrap_or_else(|| context.home.join(".claude"));
    if krill_proto::agent::canonical_id(&snapshot.agent) == "claude"
        && snapshot.bound
        && let Some(observed) = live.backend.dir.as_deref().and_then(|dir| {
            krill_proto::statusline::context::read(dir, &profile, &snapshot.session_id)
        })
        && (status.get("model").is_none_or(Value::is_null)
            || status.get("model").and_then(Value::as_str) == observed.model.as_deref())
    {
        if let Some(model) = observed.model {
            set(status, "model", model.into());
        }
        if let Some(effort) = observed.effort {
            set(status, "effort", effort.into());
        }
        if let Some(percent) = observed
            .percent
            .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
        {
            set(status, "context", json!({"percent":percent}));
        }
    }
}

const ALIASES: &[(&str, &str)] = &[
    ("default", "Default (recommended)"),
    ("opus", "Opus"),
    ("fable", "Fable"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];

fn verified(screen: &Screen) -> bool {
    native::banner_meets(screen, BANNER, FLOOR)
}

fn composer(screen: &Screen, command: Option<&str>) -> bool {
    let y = usize::from(screen.cursor.line);
    let Some(cells) = screen.rows.get(y) else {
        return false;
    };
    if y == 0
        || !border(screen, y - 1)
        || !border(screen, y + 1)
        || cells.first().is_none_or(|c| c.ch != '❯')
        || cells.get(1).is_none_or(|c| c.ch != '\u{a0}')
        || cells.iter().any(|c| !c.zerowidth.is_empty())
    {
        return false;
    }
    match command {
        Some(command) => {
            native::row(screen, y) == format!("❯\u{a0}{command}")
                && usize::from(screen.cursor.column) == command.len() + 2
        }
        None => {
            screen.cursor.column == 2
                && cells
                    .iter()
                    .skip(2)
                    .all(|c| c.ch.is_whitespace() || c.flags.contains(Flags::DIM))
        }
    }
}

fn allowed(context: &Context) -> Option<Vec<String>> {
    let root = context
        .config
        .clone()
        .unwrap_or_else(|| context.home.join(".claude"));
    let mut allowed = None;
    for path in [
        root.join("settings.json"),
        context.cwd.join(".claude/settings.json"),
        context.cwd.join(".claude/settings.local.json"),
    ] {
        if let Some(settings) = read(&path).and_then(|s| serde_json::from_str::<Value>(&s).ok())
            && let Some(values) = settings.get("availableModels").and_then(Value::as_array)
        {
            allowed = Some(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .take(128)
                    .map(str::to_owned)
                    .collect(),
            );
        }
    }
    allowed
}

fn choices(context: &Context) -> Vec<(&'static str, &'static str)> {
    let allowed = allowed(context);
    ALIASES
        .iter()
        .copied()
        .filter(|(id, _)| {
            allowed
                .as_ref()
                .is_none_or(|items| items.iter().any(|item| item == id))
        })
        .collect()
}

fn models(context: &Context, tap: &Tap) -> io::Result<Value> {
    let (screen, _, _) = tap.capture();
    if !verified(&screen) || !composer(&screen, None) {
        return Err(io::Error::other(WAIT));
    }
    Ok(
        json!({"agent":"claude","supported":true,"current":null,"items":choices(context).iter().map(|(id,label)|json!({"id":id,"label":label})).collect::<Vec<_>>(),"truncated":false,"reason":"Changes the model in this running Claude chat. Claude also saves its default for new sessions.","note":"Claude also saves this as the default for new sessions."}),
    )
}

fn set_model(context: &Context, tap: &Tap, id: &str) -> io::Result<Value> {
    let choices = choices(context);
    let Some((_, label)) = choices.iter().find(|(alias, _)| *alias == id) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Model is not a verified Claude native alias",
        ));
    };
    let (before, revision, seq) = tap.capture();
    if !verified(&before) || !composer(&before, None) {
        return Err(io::Error::other(WAIT));
    }
    let command = format!("/model {id}");
    tap.guarded_command(revision.0, command.as_bytes(), |screen, pasted| {
        verified(screen) && composer(screen, pasted.then_some(command.as_str()))
    })?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(25));
        let (screen, after, _) = tap.capture();
        if after.0 > revision.0
            && composer(&screen, None)
            && confirmed(&screen, &command, label)
            && native::contains_phrase(&native::fresh_text(tap, seq.0), "Set model to")
        {
            return Ok(json!({"applied":true,"supported":true,"model":id}));
        }
    }
    Ok(
        json!({"applied":false,"supported":true,"reason":"Claude received the model command, but its native confirmation was not recognized. It will not be resent."}),
    )
}

fn effort_version(screen: &Screen) -> bool {
    native::banner_meets(screen, BANNER, EFFORT_FLOOR)
}

fn effort_available(screen: &Screen) -> bool {
    effort_version(screen) && composer(screen, None)
}

fn set_effort(tap: &Tap, effort: &str) -> io::Result<Value> {
    if !matches!(effort, "low" | "medium" | "high" | "xhigh" | "max") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Effort is not a verified Claude effort level",
        ));
    }
    let (before, revision, _) = tap.capture();
    if !effort_available(&before) {
        return Err(io::Error::other(
            "Wait for an idle Claude Code chat with an empty native composer",
        ));
    }
    let command = format!("/effort {effort}");
    tap.guarded_command(revision.0, command.as_bytes(), |screen, pasted| {
        effort_version(screen) && composer(screen, pasted.then_some(command.as_str()))
    })?;
    Ok(
        json!({"applied":false,"requested":true,"supported":true,"effort":effort,"reason":"Claude received the effort command. Its current effort has not been observed; the command will not be resent."}),
    )
}

/// The newest prompt row must be this command's echo, which rules out an older confirmation. The
/// text below it runs on through a wrapped confirmation and the effort indicator.
fn confirmed(screen: &Screen, command: &str, label: &str) -> bool {
    let rows: Vec<_> = (0..usize::from(screen.cursor.line).saturating_sub(1))
        .map(|y| native::row(screen, y))
        .collect();
    let Some((echo, output)) = rows
        .iter()
        .rposition(|row| row.starts_with('❯'))
        .and_then(|echo| rows.get(echo..))
        .and_then(<[String]>::split_first)
    else {
        return false;
    };
    let output = native::words(output);
    let label = label.strip_suffix(" (recommended)").unwrap_or(label);
    let Some(rest) = output
        .strip_prefix("⎿ Set model to ")
        .and_then(|value| value.strip_prefix(label))
    else {
        return false;
    };
    echo.split_whitespace()
        .eq(format!("❯ {command}").split_whitespace())
        && (rest.starts_with(' ') || rest.starts_with('('))
        && (output.contains("default for new sessions") || output.contains("for this session only"))
}

#[cfg(test)]
mod tests;
