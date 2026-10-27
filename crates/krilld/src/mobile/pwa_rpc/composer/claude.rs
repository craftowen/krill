//! Verified Claude 2.1.311 native aliases. Discovery is read-only; native /model may save its default.
use super::{Context, Duration, Instant, Value, io, json, native, read};
use crate::session::Tap;
use krill_proto::screen::{Flags, Screen};

const ALIASES: &[(&str, &str)] = &[
    ("default", "Default (recommended)"),
    ("opus", "Opus"),
    ("fable", "Fable"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];

fn verified(screen: &Screen) -> bool {
    screen
        .rows
        .iter()
        .enumerate()
        .any(|(y, _)| native::row(screen, y).ends_with("Claude Code v2.1.311"))
}

fn border(screen: &Screen, y: usize) -> bool {
    let row = native::row(screen, y);
    row.chars().count() >= 20 && row.chars().all(|c| c == '─')
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

pub(super) fn models(context: &Context, tap: &Tap) -> io::Result<Value> {
    let (screen, _, _) = tap.capture();
    if !verified(&screen) || !composer(&screen, None) {
        return Err(io::Error::other(
            "Wait for an idle Claude Code 2.1.311 chat with an empty native composer",
        ));
    }
    Ok(
        json!({"agent":"claude","supported":true,"current":null,"items":choices(context).iter().map(|(id,label)|json!({"id":id,"label":label})).collect::<Vec<_>>(),"truncated":false,"reason":"Changes the model in this running Claude chat. Claude also saves its default for new sessions.","note":"Claude also saves this as the default for new sessions."}),
    )
}

pub(super) fn set_model(context: &Context, tap: &Tap, id: &str) -> io::Result<Value> {
    let choices = choices(context);
    let Some((_, label)) = choices.iter().find(|(alias, _)| *alias == id) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Model is not a verified Claude native alias",
        ));
    };
    let (before, revision, seq) = tap.capture();
    if !verified(&before) || !composer(&before, None) {
        return Err(io::Error::other(
            "Wait for an idle Claude Code 2.1.311 chat with an empty native composer",
        ));
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
            && confirmed(&screen, label)
            && native::contains_phrase(&native::fresh_text(tap, seq.0), "Set model to")
        {
            return Ok(json!({"applied":true,"supported":true,"model":id}));
        }
    }
    Ok(
        json!({"applied":false,"supported":true,"reason":"Claude received the model command, but its native confirmation was not recognized. It will not be resent."}),
    )
}

fn confirmed(screen: &Screen, label: &str) -> bool {
    let Some(row) = screen
        .rows
        .iter()
        .take(usize::from(screen.cursor.line).saturating_sub(1))
        .rev()
        .map(|cells| cells.iter().map(|c| c.ch).collect::<String>())
        .find(|row| !row.trim().is_empty())
    else {
        return false;
    };
    let row = row.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = label.strip_suffix(" (recommended)").unwrap_or(label);
    let Some(rest) = row
        .strip_prefix("⎿ Set model to ")
        .and_then(|value| value.strip_prefix(label))
    else {
        return false;
    };
    (rest.starts_with(' ') || rest.starts_with('('))
        && (row.contains("default for new sessions") || row.contains("for this session only"))
}

#[cfg(test)]
mod tests;
