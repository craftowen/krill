//! Codex direct /model argument semantics, from 0.164.0. Unknown layouts fail closed.
use super::{
    Adapter, ChatPollSnapshot, Context, Discovered, Duration, Instant, Live, Value, io, json,
    no_current_model, observe_live_model, read, set, validate_effort, validate_model,
};
use krill_proto::screen::{Flags, Screen};

pub(super) struct Codex;

const BANNER: &str = ">_ OpenAI Codex (v";
const FLOOR: &[u64] = &[0, 164, 0];

/// ponytail: a floor rather than an allowlist, because these CLIs auto-update. A later CLI that
/// changes /model is caught by the composer, echo and confirmation checks, but one that stops
/// recognising /model could receive the text as a prompt. Upgrade path: restore an allowlist.
pub(super) fn at_least(version: &str, floor: &[u64]) -> bool {
    version
        .split('.')
        .map(|part| part.parse().ok())
        .collect::<Option<Vec<u64>>>()
        .is_some_and(|version| version.as_slice() >= floor)
}

fn banner<'a>(row: &'a str, name: &str) -> Option<&'a str> {
    let (art, rest) = row.rsplit_once(name)?;
    let word = rest.split_whitespace().next()?;
    // `)` closes Codex's `(v…)`, so the word claims to be a version even when it does not parse.
    let closed = word.strip_suffix(')');
    (!art.chars().any(char::is_alphanumeric)
        && (closed.is_some() || word.starts_with(|c: char| c.is_ascii_digit())))
    .then_some(closed.unwrap_or(word))
}

/// A banner still on screen must meet `floor`. Absence proves nothing, because startup hook output
/// scrolls the banner away within a second.
pub(super) fn banner_meets(screen: &Screen, name: &str, floor: &[u64]) -> bool {
    (0..screen.rows.len())
        .rev()
        .find_map(|y| banner(&row(screen, y), name).map(str::to_owned))
        .is_none_or(|version| at_least(&version, floor))
}

/// Whitespace-normalized text of wrapped rows, including the NBSP after a prompt.
pub(super) fn words(rows: &[String]) -> String {
    rows.join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Adapter for Codex {
    fn catalog(&self, context: &Context) -> Value {
        let mut models = Discovered::default();
        let root = context
            .config
            .clone()
            .unwrap_or_else(|| context.home.join(".codex"));
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
                    models.add(
                        id,
                        model
                            .get("display_name")
                            .and_then(Value::as_str)
                            .unwrap_or(id),
                        Some(model),
                    );
                }
            }
        }
        if let Some(config) =
            read(&root.join("config.toml")).and_then(|s| toml::from_str::<toml::Table>(&s).ok())
        {
            if let Some(id) = config.get("model").and_then(toml::Value::as_str) {
                models.add(id, id, None);
            }
            for profile in config
                .get("profiles")
                .and_then(toml::Value::as_table)
                .into_iter()
                .flat_map(|p| p.values())
                .take(128)
            {
                if let Some(id) = profile.get("model").and_then(toml::Value::as_str) {
                    models.add(id, id, None);
                }
            }
        }
        models.into_catalog("codex", "This running Codex session does not expose a verified model control connection. Discovered models are from local configuration/cache; no model was changed.")
    }

    fn control(&self, _context: &Context, live: &Live<'_>, models: &mut Value) {
        match live.tap() {
            Ok(tap) => {
                let (screen, _, _) = tap.capture();
                if idle(&screen) {
                    set(models, "supported", true.into());
                    set(
                        models,
                        "reason",
                        "Changes the model in this running Codex chat session only.".into(),
                    );
                    if let Some(current) = observed_model(models, &screen) {
                        set(models, "current", current.into());
                    }
                } else {
                    set(models, "reason", "Wait for an idle Codex chat with an empty native composer. Direct model control needs Codex 0.164.0 or later.".into());
                }
            }
            Err(error) => set(models, "reason", error.to_string().into()),
        }
    }

    fn set_model(
        &self,
        context: &Context,
        live: &Live<'_>,
        model: &str,
        effort: Option<&str>,
    ) -> io::Result<Value> {
        let catalog = context.models();
        validate_model(&catalog, model)?;
        validate_effort(&catalog, model, effort)?;
        let tap = live.tap()?;
        let (before, revision, seq) = tap.capture();
        if !idle(&before) {
            return Err(io::Error::other(
                "Wait for an idle Codex 0.164.0 or later chat with an empty native composer",
            ));
        }
        if observed_model(&catalog, &before).as_deref() == Some(model)
            && effort.is_none_or(|effort| self::effort(&before).as_deref() == Some(effort))
        {
            return Ok(
                json!({"applied":true,"supported":true,"model":model,"effort":effort,"already_selected":true}),
            );
        }
        let command = effort.map_or_else(
            || format!("/model {model}"),
            |effort| format!("/model {model} --reasoning={effort}"),
        );
        tap.guarded_command(revision.0, command.as_bytes(), |screen, pasted| {
            verified(screen) && composer(screen, pasted.then_some(command.as_str()))
        })?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(25));
            let (screen, observed, _) = tap.capture();
            if observed.0 > revision.0
                && composer(&screen, None)
                && observed_model(&catalog, &screen).as_deref() == Some(model)
                && latest_confirmation(&screen, model)
                && fresh_confirmation(&tap, seq.0, model)
                && effort.is_none_or(|effort| self::effort(&screen).as_deref() == Some(effort))
            {
                return Ok(json!({"applied":true,"supported":true,"model":model,"effort":effort}));
            }
        }
        Ok(
            json!({"applied":false,"supported":true,"reason":"Codex received the model command, but its session-only change was not confirmed. Check the chat; the command will not be resent."}),
        )
    }

    fn set_effort(&self, context: &Context, live: &Live<'_>, effort: &str) -> io::Result<Value> {
        let mut catalog = context.models();
        self.control(context, live, &mut catalog);
        let model = catalog
            .get("current")
            .and_then(Value::as_str)
            .ok_or_else(no_current_model)?;
        self.set_model(context, live, model, Some(effort))
    }

    fn observe(
        &self,
        _context: &Context,
        live: &Live<'_>,
        catalog: &Value,
        _snapshot: &ChatPollSnapshot,
        status: &mut Value,
    ) {
        let supported = status.get("supported").cloned().unwrap_or(false.into());
        set(status, "effort_supported", supported);
        let Ok(tap) = live.tap() else {
            return;
        };
        let (screen, _, _) = tap.capture();
        if idle(&screen) {
            if let Some(model) = observed_model(catalog, &screen) {
                observe_live_model(status, &model);
            }
            if let Some(effort) = effort(&screen) {
                set(status, "effort", effort.into());
            }
        }
    }
}

fn idle(screen: &Screen) -> bool {
    verified(screen) && composer(screen, None)
}

fn observed_model(models: &Value, screen: &Screen) -> Option<String> {
    let footer = row(screen, usize::from(screen.cursor.line) + 2);
    models
        .get("items")?
        .as_array()?
        .iter()
        .filter_map(|item| Some((item.get("label")?.as_str()?, item.get("id")?.as_str()?)))
        .filter(|(label, _)| footer.trim_start().starts_with(&format!("{label} ")))
        .max_by_key(|(label, _)| label.len())
        .map(|(_, id)| id.to_owned())
}

pub(super) fn border(screen: &Screen, y: usize) -> bool {
    let row = row(screen, y);
    row.chars().count() >= 20 && row.chars().all(|c| c == '─')
}

pub(super) fn row(screen: &Screen, y: usize) -> String {
    screen
        .rows
        .get(y)
        .into_iter()
        .flatten()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn verified(screen: &Screen) -> bool {
    banner_meets(screen, BANNER, FLOOR)
}

fn effort(screen: &Screen) -> Option<String> {
    let footer = row(screen, usize::from(screen.cursor.line) + 2);
    let (model, _) = footer.split_once(" · ")?;
    model
        .split_whitespace()
        .last()
        .filter(|effort| super::valid_effort(effort))
        .map(str::to_owned)
}

fn composer(screen: &Screen, command: Option<&str>) -> bool {
    if !screen.cursor.visible {
        return false;
    }
    let y = usize::from(screen.cursor.line);
    let Some(cells) = screen.rows.get(y) else {
        return false;
    };
    if cells
        .first()
        .is_none_or(|c| c.ch != '›' || !c.flags.contains(Flags::BOLD))
        || cells.get(1).is_none_or(|c| c.ch != ' ')
        || !cells.iter().all(|c| c.zerowidth.is_empty())
        || y == 0
        || !row(screen, y - 1).trim().is_empty()
        || !row(screen, y + 1).trim().is_empty()
        || !row(screen, y + 2).contains(" · ")
    {
        return false;
    }
    match command {
        Some(command) => {
            row(screen, y) == format!("› {command}")
                && usize::from(screen.cursor.column) == command.len() + 2
                && cells.iter().skip(2).all(|c| !c.flags.contains(Flags::DIM))
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

#[cfg(test)]
fn confirmations(screen: &Screen, id: &str) -> usize {
    let prefix = format!("• Model changed to {id} ");
    let mut count = 0;
    let mut line = String::new();
    for cells in screen.rows.iter().take(usize::from(screen.cursor.line)) {
        let wrapped = cells
            .last()
            .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
        let text: String = cells.iter().map(|c| c.ch).collect();
        line.push_str(if wrapped { &text } else { text.trim_end() });
        if !wrapped {
            count +=
                usize::from(line.starts_with(&prefix) && line.ends_with(" for this session only"));
            line.clear();
        }
    }
    count
}

pub(super) fn fresh_text(tap: &crate::session::Tap, from: u64) -> String {
    let crate::session::Read::Bytes(bytes) = tap.read(from, 64 * 1024) else {
        return String::new();
    };
    plain(&bytes)
}

fn plain(bytes: &[u8]) -> String {
    crate::term::printed_text(bytes)
}

pub(super) fn contains_phrase(output: &str, phrase: &str) -> bool {
    // Native renderers may position each word with CSI column moves instead of printing spaces.
    // The current screen still supplies exact target/layout authority; this only checks fresh text.
    output
        .split_whitespace()
        .collect::<String>()
        .contains(&phrase.split_whitespace().collect::<String>())
}

fn fresh_confirmation(tap: &crate::session::Tap, from: u64, id: &str) -> bool {
    let output = fresh_text(tap, from);
    contains_phrase(&output, &format!("Model changed to {id} "))
        && contains_phrase(&output, "for this session only")
}

/// Reads from the newest bullet down. Below it can sit the acknowledgment's own wrapped tail and
/// the right-aligned rate-limit warning Codex pins above the composer.
fn latest_confirmation(screen: &Screen, id: &str) -> bool {
    let rows: Vec<_> = (0..usize::from(screen.cursor.line))
        .map(|y| row(screen, y))
        .collect();
    let Some(text) = rows
        .iter()
        .rposition(|row| row.starts_with('•'))
        .and_then(|newest| rows.get(newest..))
        .map(words)
    else {
        return false;
    };
    text.starts_with(&format!("• Model changed to {id} "))
        && text.contains(" for this session only")
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::screen::{Cell, Cursor};
    #[test]
    fn cursor_positioned_words_are_fresh_confirmation_text() {
        let printed = plain(b"\x1b[2GSet\x1b[6Gmodel\x1b[12Gto\x1b[15GOpus\x1b[20G5.5");
        assert!(contains_phrase(&printed, "Set model to"));
        assert!(!contains_phrase(&printed, "Model changed to"));
        assert!(!contains_phrase("unrelated output", "Set model to"));
    }
    fn screen(draft: &str) -> Screen {
        let lines = [
            ">_ OpenAI Codex (v0.164.0)",
            "",
            "› ",
            "",
            "  Actual model default · /workspace",
        ];
        let mut screen = Screen {
            cols: 80,
            lines: 5,
            rows: lines
                .iter()
                .map(|s| {
                    s.chars()
                        .map(|ch| Cell {
                            ch,
                            ..Cell::default()
                        })
                        .collect()
                })
                .collect(),
            cursor: Cursor {
                line: 2,
                column: 2,
                visible: true,
            },
            ..Screen::default()
        };
        screen.rows[2][0].flags = Flags::BOLD;
        screen.rows[2].extend(draft.chars().map(|ch| Cell {
            ch,
            ..Cell::default()
        }));
        screen
    }
    #[test]
    fn draft_cursor_and_styles_are_required() {
        let mut s = screen("Ask Codex to do anything");
        assert!(!composer(&s, None));
        for c in s.rows[2].iter_mut().skip(2) {
            c.flags = Flags::DIM;
        }
        assert!(composer(&s, None));
        s.cursor.column = 3;
        assert!(!composer(&s, None));
        let mut draft = screen("/model actual-model");
        draft.cursor.column = 21;
        assert!(!composer(&draft, None));
        assert!(composer(&draft, Some("/model actual-model")));
        assert!(!composer(&draft, Some("/model forged")));
    }
    #[test]
    fn acknowledgment_must_match_exact_id_and_session_only_semantics() {
        let mut s = screen("");
        s.rows.insert(
            0,
            "• Model changed to actual-model medium for this session only"
                .chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect(),
        );
        assert_eq!(confirmations(&s, "actual-model"), 1);
        assert_eq!(confirmations(&s, "actual"), 0);
        assert_eq!(confirmations(&s, "forged"), 0);
    }

    #[test]
    fn repeated_model_confirmation_does_not_depend_on_viewport_counts() {
        let output =
            plain(b"\x1b[32mModel changed to actual-model medium for this session only\x1b[0m");
        assert!(output.contains("Model changed to actual-model "));
        // An old identical acknowledgment may leave the viewport as this new one enters.
        // The backend uses this command's raw seq plus the actual live model footer instead of count growth.
        let stale = plain(b"\x1b[2Jmodel selector closed");
        assert!(!stale.contains("Model changed to actual-model "));
        let ack_a = "• Model changed to model-a medium for this session only";
        let ack_b = "• Model changed to model-b medium for this session only";
        let mut before = screen("");
        before.rows[0] = ack_a
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        before.rows[1] = ack_b
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        let mut after = before.clone();
        after.rows.swap(0, 1);
        assert_eq!(
            confirmations(&before, "model-a"),
            confirmations(&after, "model-a")
        );
        assert!(!latest_confirmation(&before, "model-a"));
        assert!(latest_confirmation(&after, "model-a"));
    }
    /// Rows `above` the 0.166.0 composer and footer.
    fn composer_below(above: &[&str]) -> Screen {
        let mut screen = screen("");
        let at = above.len();
        screen.rows.splice(
            0..2,
            above.iter().map(|line| {
                line.chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect::<Vec<_>>()
            }),
        );
        screen.cursor.line = u16::try_from(at).unwrap();
        screen
    }
    #[test]
    fn versions_compare_numerically_and_unparsable_fails_closed() {
        for version in ["0.164.0", "0.166.0", "0.1000.0", "0.164.0.1", "1.0.0"] {
            assert!(at_least(version, &[0, 164, 0]), "{version}");
        }
        for version in [
            "0.163.9",
            "0.99.0",
            "0.164",
            "",
            "v0.166.0",
            "0.166.0-alpha.1",
            "0..1",
        ] {
            assert!(!at_least(version, &[0, 164, 0]), "{version}");
        }
    }
    #[test]
    fn a_banner_on_screen_must_meet_the_floor_and_absence_passes() {
        let idle_below = |line: &str| {
            let mut s = screen("");
            s.rows[0] = line
                .chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect();
            idle(&s)
        };
        assert!(idle_below(""), "a banner scrolled off the screen passes");
        assert!(idle_below("  >_ OpenAI Codex (v0.164.0)"));
        assert!(idle_below("  >_ OpenAI Codex (v0.166.0)"));
        assert!(idle_below("  >_ OpenAI Codex (v1.0.0)"));
        assert!(!idle_below("  >_ OpenAI Codex (v0.163.9)"));
        for unparsable in ["0.167.0-alpha.1", "next", ""] {
            let line = format!("  >_ OpenAI Codex (v{unparsable})");
            assert!(!idle_below(&line), "{line}");
        }
        assert!(
            idle_below("Compared with >_ OpenAI Codex (v0.1.0)"),
            "prose is not a banner"
        );
    }
    #[test]
    fn the_pinned_rate_limit_warning_does_not_hide_the_acknowledgment() {
        let warning = "                                                                     ⚠ 5h limit: 22% left · /status";
        let s = composer_below(&[
            "  >_ OpenAI Codex (v0.166.0)",
            "⚠ Heads up, you have less than 25% of your 5h limit left. Run /status for a breakdown.",
            "",
            "• Model changed to gpt-6.1-sol low for this session only",
            warning,
            "",
        ]);
        assert!(composer(&s, None));
        assert!(latest_confirmation(&s, "gpt-6.1-sol"));
        assert!(!latest_confirmation(&s, "gpt-6"));
    }
    #[test]
    fn an_acknowledgment_wrapped_by_a_narrow_terminal_is_recognized() {
        let s = composer_below(&[
            "• Model changed to gpt-6.1-sol low for this",
            "session only",
            "                         ⚠ 5h limit: 22% left",
            "",
        ]);
        assert!(latest_confirmation(&s, "gpt-6.1-sol"));
    }
    #[test]
    fn newer_output_outranks_an_older_acknowledgment() {
        let s = composer_below(&[
            "• Model changed to gpt-6.1-sol low for this session only",
            "• Done",
            "",
        ]);
        assert!(!latest_confirmation(&s, "gpt-6.1-sol"));
    }
    #[test]
    fn effort_is_read_from_the_verified_native_footer_only() {
        let mut current = screen("");
        current.rows[4] = "  Actual model xhigh · /workspace"
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        assert_eq!(effort(&current).as_deref(), Some("xhigh"));
        current.rows[4] = "arbitrary assistant response high"
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        assert!(effort(&current).is_none());
    }
}
