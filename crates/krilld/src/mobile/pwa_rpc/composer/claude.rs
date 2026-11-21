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
        match models(context, &tap) {
            Ok(native) => *catalog = native,
            Err(error) => set(
                catalog,
                "reason",
                super::super::sanitize_tap_reason(&error).into(),
            ),
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
        set_model(context, &live.tap()?, live.working(), model)
    }

    fn set_effort(&self, _context: &Context, live: &Live<'_>, effort: &str) -> io::Result<Value> {
        set_effort(&live.tap()?, live.working(), effort)
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
        let screen = live.screen();
        observe_screen_into(screen.as_ref(), status);
    }
}

fn observe_screen_into(screen: Option<&Screen>, status: &mut Value) {
    if let Some(screen) = screen {
        merge_screen_observation(status, screen);
    }
    let effort_supported = status.get("supported").and_then(Value::as_bool) == Some(true)
        && screen.is_some_and(effort_available);
    set(status, "effort_supported", effort_supported.into());
    if effort_supported {
        set(
            status,
            "efforts",
            json!(["low", "medium", "high", "xhigh", "max"]),
        );
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

// Claude Code names an alias's version only once that model runs, and the phone shows the running
// model's observed id. ponytail: static list; update it when a Claude model family moves.
const VERSIONS: &[(&str, &str)] = &[
    ("opus", "Opus 5.5"),
    ("fable", "Fable 5.1"),
    ("sonnet", "Sonnet 5.5"),
    ("haiku", "Haiku 5.5"),
];

const ALIASES: &[(&str, &str)] = &[
    ("default", "Default (recommended)"),
    ("opus", "Opus"),
    ("fable", "Fable"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];

pub(super) fn merge_screen_observation(status: &mut Value, screen: &Screen) {
    let Some((id, label, effort)) = observe_screen(screen) else {
        return;
    };
    let existing = status.get("model").and_then(Value::as_str);
    let matches_existing =
        existing.is_none_or(|cur| cur == id || cur.starts_with(&format!("claude-{id}-")));
    if existing.is_none() {
        super::observe_live_model(status, &id);
    }
    if matches_existing {
        if status.get("model_label").and_then(Value::as_str).is_none() {
            set(status, "model_label", label.into());
        }
        if let Some(effort) =
            effort.filter(|_| status.get("effort").and_then(Value::as_str).is_none())
        {
            set(status, "effort", effort.into());
        }
    }
}

pub(super) fn observe_screen(screen: &Screen) -> Option<(String, String, Option<String>)> {
    if let Some(found) = observe_footer(screen) {
        return Some(found);
    }
    observe_banner(screen)
}

pub(super) fn parse_model_spec(segment: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = segment.trim();
    let (id, label, rest) = parse_claude_model(trimmed)?;
    let rest = skip_model_context(rest)?;
    let TrailingStatus::Valid(effort) = parse_trailing_status(rest) else {
        return None;
    };
    Some((id, label, effort))
}

fn parse_claude_model(text: &str) -> Option<(String, String, &str)> {
    let lower = text.to_ascii_lowercase();
    if let Some(after_claude) = lower.strip_prefix("claude-") {
        for (alias, _) in VERSIONS {
            if let Some(after_alias) = after_claude.strip_prefix(alias)
                && (after_alias.is_empty()
                    || !after_alias
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric()))
            {
                let consumed = "claude-".len() + alias.len();
                let mut rest = text.get(consumed..)?;
                let mut version = String::new();
                while let Some(part) = rest.strip_prefix('-') {
                    let digits_len = part.chars().take_while(char::is_ascii_digit).count();
                    if digits_len == 0 {
                        break;
                    }
                    let (token, after_digits) = part.split_at_checked(digits_len)?;
                    if digits_len == 8 && !version.is_empty() {
                        rest = after_digits;
                        break;
                    }
                    if !version.is_empty() {
                        version.push('.');
                    }
                    version.push_str(token);
                    rest = after_digits;
                }
                if !rest.is_empty()
                    && !rest
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '·' | '|'))
                {
                    return None;
                }
                let title = title_case_family(alias);
                let label = if version.is_empty() {
                    title
                } else {
                    format!("{title} {version}")
                };
                return Some(((*alias).to_owned(), label, rest));
            }
        }
        return None;
    }

    for (alias, _) in VERSIONS {
        if lower.starts_with(alias) {
            let Some(after_alias) = text.get(alias.len()..) else {
                continue;
            };
            if after_alias
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace() && !matches!(c, '(' | '·' | '|'))
            {
                continue;
            }
            let title = title_case_family(alias);
            let after_ws = after_alias.trim_start();
            let ver_len = after_ws
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .count();
            if ver_len > 0
                && let Some((candidate, after_ver)) = after_ws.split_at_checked(ver_len)
                && candidate.chars().any(|c| c.is_ascii_digit())
                && !candidate.starts_with('.')
                && !candidate.ends_with('.')
                && !candidate.contains("..")
                && after_ver
                    .chars()
                    .next()
                    .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '·' | '|'))
            {
                return Some((
                    (*alias).to_owned(),
                    format!("{title} {candidate}"),
                    after_ver,
                ));
            }
            return Some(((*alias).to_owned(), title, after_alias));
        }
    }
    None
}

fn title_case_family(alias: &str) -> String {
    let mut chars = alias.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn skip_model_context(mut rest: &str) -> Option<&str> {
    loop {
        rest = rest.trim_start();
        let Some(inside) = rest.strip_prefix('(') else {
            return Some(rest);
        };
        if let Some((body, after_paren)) = inside.split_once(')') {
            if body.len() > 36 || body.contains(['?', '!']) || body.contains(". ") {
                return None;
            }
            rest = after_paren;
        } else {
            if inside.len() <= 24
                && !inside.contains(['?', '!', ';'])
                && inside
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '/'))
            {
                return Some("");
            }
            return None;
        }
    }
}

enum TrailingStatus {
    Valid(Option<String>),
    Invalid,
}

fn parse_trailing_status(rest: &str) -> TrailingStatus {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return TrailingStatus::Valid(None);
    }
    if let Some(after_sep) = trimmed
        .strip_prefix('·')
        .or_else(|| trimmed.strip_prefix('|'))
    {
        let tail = after_sep.trim();
        if tail.contains(". ") || tail.contains(['?', '!']) || tail.ends_with('.') {
            return TrailingStatus::Invalid;
        }
        let first_seg = tail.split(['·', '|']).next().unwrap_or("").trim();
        return TrailingStatus::Valid(extract_effort_token(first_seg));
    }
    let first_word = trimmed.split([' ', '\t', '·', '|']).next().unwrap_or("");
    let lower = first_word.to_ascii_lowercase();
    if super::valid_effort(&lower) {
        let rem = trimmed.get(first_word.len()..).unwrap_or("").trim_start();
        if rem.is_empty() || rem.starts_with('·') || rem.starts_with('|') {
            return TrailingStatus::Valid(Some(lower));
        }
    }
    TrailingStatus::Invalid
}

fn extract_effort_token(segment: &str) -> Option<String> {
    let mut words = segment.split_whitespace();
    let first = words.next()?;
    let lower = first.to_ascii_lowercase();
    if !super::valid_effort(&lower) {
        return None;
    }
    match words.next() {
        None => Some(lower),
        Some(second) if second.eq_ignore_ascii_case("effort") && words.next().is_none() => {
            Some(lower)
        }
        Some(_) => None,
    }
}

fn observe_footer(screen: &Screen) -> Option<(String, String, Option<String>)> {
    let lines = screen.rows.len();
    let cursor_y = usize::from(screen.cursor.line);
    let border_y = if composer(screen, None) {
        ((cursor_y + 1)..=(cursor_y + 5).min(lines.saturating_sub(1)))
            .find(|&y| native::border(screen, y))
    } else {
        (0..lines).rev().find(|&y| {
            native::border(screen, y)
                && (y.saturating_sub(5)..y)
                    .any(|py| native::row(screen, py).trim_start().starts_with('❯'))
                && ((y + 1)..lines)
                    .filter(|&ry| !native::row(screen, ry).trim().is_empty())
                    .count()
                    <= 3
        })
    }?;

    let mut rows = Vec::new();
    for y in (border_y + 1)..=(border_y + 3).min(lines.saturating_sub(1)) {
        if native::border(screen, y) {
            break;
        }
        let text = native::row(screen, y);
        if !text.trim().is_empty() {
            rows.push(text);
        }
    }

    let mut fallback = None;
    for (idx, line) in rows.iter().enumerate() {
        if let Some(next) = rows.get(idx + 1) {
            let joined = format!("{} {}", line.trim_end(), next.trim_start());
            if let Some(spec) = scan_statusline_segments(&joined)
                && spec.2.is_some()
            {
                return Some(spec);
            }
            if fallback.is_none() {
                fallback = scan_statusline_segments(&joined);
            }
        }
        if let Some(spec) = scan_statusline_segments(line) {
            if spec.2.is_some() {
                return Some(spec);
            }
            if fallback.is_none() {
                fallback = Some(spec);
            }
        }
    }
    fallback
}

fn scan_statusline_segments(line: &str) -> Option<(String, String, Option<String>)> {
    let mut pipe_parts = line.split('|').peekable();
    while let Some(part) = pipe_parts.next() {
        let trimmed = part.trim();
        let mut candidates = vec![trimmed];
        for (idx, ch) in trimmed.char_indices() {
            if ch == '·'
                && let Some(after) = trimmed.get(idx + ch.len_utf8()..)
            {
                candidates.push(after.trim_start());
            }
        }
        for candidate in candidates {
            if let Some((id, label, mut effort)) = parse_model_spec(candidate) {
                if effort.is_none()
                    && let Some(next_part) = pipe_parts.peek()
                {
                    effort = extract_effort_token(next_part.trim());
                }
                return Some((id, label, effort));
            }
        }
    }
    None
}

fn strip_banner_frame(row: &str) -> &str {
    row.trim_start_matches(|c: char| {
        c.is_whitespace()
            || matches!(c, '*' | '│' | '┃' | '╭' | '╰' | '├' | '✻' | '✳' | '✶')
            || ('\u{2580}'..='\u{259F}').contains(&c)
    })
    .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '┃'))
}

fn observe_banner(screen: &Screen) -> Option<(String, String, Option<String>)> {
    if !verified(screen) {
        return None;
    }
    let lines = screen.rows.len();
    let banner_y = (0..lines).find(|&y| native::row(screen, y).contains(BANNER))?;
    let rows: Vec<String> = ((banner_y + 1)..=(banner_y + 4).min(lines.saturating_sub(1)))
        .map(|y| strip_banner_frame(&native::row(screen, y)).to_owned())
        .collect();
    for (idx, row_text) in rows.iter().enumerate() {
        if let Some(next) = rows.get(idx + 1)
            && !next.is_empty()
            && (row_text.ends_with('(')
                || row_text
                    .rsplit_once('(')
                    .is_some_and(|(_, tail)| !tail.contains(')'))
                || next.starts_with('·'))
        {
            let joined = format!("{row_text} {next}");
            if let Some(found) = parse_model_spec(&joined) {
                return Some(found);
            }
        }
        if let Some(found) = parse_model_spec(row_text) {
            return Some(found);
        }
    }
    None
}

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
    let current = observe_screen(&screen).map(|(id, _, _)| id);
    Ok(
        json!({"agent":"claude","supported":true,"current":current,"items":choices(context).iter().map(|(id,label)|json!({"id":id,"label":VERSIONS.iter().find(|(alias,_)|alias==id).map_or(*label,|(_,name)|*name)})).collect::<Vec<_>>(),"truncated":false,"reason":"Changes the model in this running Claude chat. Claude also saves its default for new sessions.","note":"Claude also saves this as the default for new sessions."}),
    )
}

fn set_model(context: &Context, tap: &Tap, working: bool, id: &str) -> io::Result<Value> {
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
    super::command(
        tap,
        working,
        revision.0,
        command.as_bytes(),
        |screen, pasted| verified(screen) && composer(screen, pasted.then_some(command.as_str())),
    )?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(25));
        let (screen, after, _) = tap.capture();
        if after.0 > revision.0
            && composer(&screen, None)
            && confirmed(&native::fresh_text(tap, seq.0), label)
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

fn set_effort(tap: &Tap, working: bool, effort: &str) -> io::Result<Value> {
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
    super::command(
        tap,
        working,
        revision.0,
        command.as_bytes(),
        |screen, pasted| {
            effort_version(screen) && composer(screen, pasted.then_some(command.as_str()))
        },
    )?;
    Ok(
        json!({"applied":false,"requested":true,"supported":true,"effort":effort,"reason":"Claude received the effort command. Its current effort has not been observed; the command will not be resent."}),
    )
}

/// Only output since the command counts, so an older confirmation cannot match. Claude draws
/// this as a transcript row when idle and as a floating note mid-turn.
fn confirmed(fresh: &str, label: &str) -> bool {
    let label = label.strip_suffix(" (recommended)").unwrap_or(label);
    native::contains_phrase(fresh, &format!("Set model to {label}"))
}

#[cfg(test)]
mod tests;
