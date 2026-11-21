use super::*;
use krill_proto::screen::{Cell, Cursor};
fn screen(lines: &[&str]) -> Screen {
    Screen {
        cols: 150,
        lines: 55,
        rows: lines
            .iter()
            .map(|line| {
                line.chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect()
            })
            .collect(),
        cursor: Cursor {
            line: 3,
            column: 2,
            visible: false,
        },
        ..Screen::default()
    }
}

const RULE: &str = "────────────────────────────────────────";

/// Rows `above` the composer, then the 2.1.316 composer and footer.
fn composer_below(above: &[&str]) -> Screen {
    let mut lines = above.to_vec();
    lines.extend([
        RULE,
        "❯\u{a0}",
        RULE,
        "  Opus 5.5 · high | 미니멀리즘* | 0/1.0M | 5h 1% (49m) | 7d 6% (5d20h)",
        "  ⏵⏵ auto mode on (shift+tab to cycle)",
    ]);
    let mut screen = screen(&lines);
    screen.cursor.line = u16::try_from(above.len() + 1).unwrap();
    screen
}

#[test]
fn native_aliases_are_bounded() {
    assert_eq!(ALIASES.len(), 5);
    assert!(ALIASES.iter().any(|(id, _)| *id == "sonnet"));
}

#[test]
fn a_banner_on_screen_must_meet_the_floor_and_absence_passes() {
    assert!(verified(&screen(&[" ▐▛███▛█   Claude Code v2.1.316"])));
    assert!(verified(&screen(&[" ▐▛███▛█   Claude Code v2.1.311"])));
    assert!(verified(&screen(&[" ▐▛███▛█   Claude Code v3.0.0"])));
    assert!(!verified(&screen(&[" ▐▛███▛█   Claude Code v2.1.310"])));
    assert!(
        !verified(&screen(&[" ▐▛███▛█   Claude Code v2.2.0-beta"])),
        "an unparsable banner fails closed"
    );
    assert!(verified(&screen(&[
        "  ⎿  SessionStart:startup says: recent"
    ])));
    assert!(
        verified(&screen(&[
            "Hook summary: compared with Claude Code v2.1.200"
        ])),
        "prose is not a banner"
    );
}

#[test]
fn effort_has_its_own_floor_and_needs_no_banner() {
    assert!(effort_version(&screen(&["Claude Code v2.1.314"])));
    assert!(effort_version(&screen(&["Claude Code v2.1.316"])));
    assert!(!effort_version(&screen(&["Claude Code v2.1.313"])));
    assert!(effort_available(&composer_below(&["  ⎿  SessionStart"])));
}

#[test]
fn the_idle_placeholder_is_an_empty_composer_but_a_draft_is_not() {
    let mut idle = composer_below(&[]);
    idle.cursor.line = 1;
    let placeholder = "Try \"create a util logging.py that...\"";
    idle.rows[1].truncate(2);
    idle.rows[1].extend(placeholder.chars().map(|ch| Cell {
        ch,
        ..Cell::default()
    }));
    assert!(!composer(&idle, None), "undimmed text is a draft");
    for cell in idle.rows[1].iter_mut().skip(2) {
        cell.flags = Flags::DIM;
    }
    assert!(composer(&idle, None));
}
#[test]
fn empty_draft_and_exact_command_require_native_borders_cursor_and_nbsp() {
    let mut s = screen(&[
        "Claude Code v2.1.311",
        "",
        "────────────────────────────────────────",
        "❯\u{a0} ",
        "────────────────────────────────────────",
    ]);
    assert!(composer(&s, None));
    s.rows[3].extend("draft".chars().map(|ch| Cell {
        ch,
        ..Cell::default()
    }));
    assert!(!composer(&s, None));
    s.rows[3] = "❯\u{a0}/model "
        .chars()
        .map(|ch| Cell {
            ch,
            ..Cell::default()
        })
        .collect();
    s.cursor.column = 8;
    assert!(composer(&s, Some("/model")));
    assert!(!composer(&s, Some("/model forged")));
    s.rows[4].clear();
    assert!(!composer(&s, Some("/model")));
}
#[test]
fn only_the_chosen_model_is_confirmed_as_a_row_or_a_floating_note() {
    let row = "⎿ Set model to Sonnet 4.6 and saved as your default for new sessions";
    assert!(confirmed(row, "Sonnet"));
    assert!(!confirmed(row, "Opus"));
    let note = "   1562\n        Set model to Opus 5.5 and saved as your default";
    assert!(confirmed(note, "Opus"));
    assert!(confirmed(
        "Set model to Default (Opus 5.5)",
        "Default (recommended)"
    ));
    assert!(!confirmed("/model opus", "Opus"));
}

#[cfg(unix)]
#[test]
fn excluded_known_alias_is_rejected_before_any_pty_write() {
    use crate::{appearance::Appearance, session::Session};
    use krill_proto::{SessionId, holder::WinSize};
    use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};
    let root = std::env::temp_dir().join(format!("krill-claude-allowlist-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("settings.json"),
        r#"{"availableModels":["opus"]}"#,
    )
    .unwrap();
    let context = Context {
        home: root.clone(),
        cwd: root.clone(),
        agent: "claude".into(),
        config: Some(root.clone()),
        env: BTreeMap::new(),
    };
    assert_eq!(choices(&context), vec![("opus", "Opus")]);
    let pty = rustix_openpty::openpty(None, None).unwrap();
    rustix::fs::fcntl_setfl(
        &pty.user,
        rustix::fs::fcntl_getfl(&pty.user).unwrap() | rustix::fs::OFlags::NONBLOCK,
    )
    .unwrap();
    let session = Session::start(
        SessionId(941),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    assert_eq!(
        set_model(&context, &session.tap(), false, "sonnet")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let mut bytes = [0; 64];
    assert_eq!(
        rustix::io::read(&pty.user, &mut bytes).unwrap_err(),
        rustix::io::Errno::AGAIN
    );
    drop(pty.user);
    session.wait().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn observe_screen_reads_idle_statusline_and_welcome_banner() {
    let footer = composer_below(&["  Claude Code v2.1.316"]);
    assert_eq!(
        observe_screen(&footer),
        Some(("opus".into(), "Opus 5.5".into(), Some("high".into())))
    );

    let banner_only = screen(&[
        " * ▐▛███▜▌ *   Claude Code v2.1.316",
        "* ▝▜█████▛▘ *  Opus 5.5 (1M context) · Claude Max",
        " *  ▘▘ ▝▝  *   ~/project",
    ]);
    assert_eq!(
        observe_screen(&banner_only),
        Some(("opus".into(), "Opus 5.5".into(), None))
    );

    for (line, expected_id, expected_label, expected_effort) in [
        (
            "Opus 5.5 (1M context) · max",
            "opus",
            "Opus 5.5",
            Some("max"),
        ),
        (
            "Sonnet 5.5 · medium · Claude Pro",
            "sonnet",
            "Sonnet 5.5",
            Some("medium"),
        ),
        ("Haiku 5.5 · low", "haiku", "Haiku 5.5", Some("low")),
        ("Fable 5.1", "fable", "Fable 5.1", None),
    ] {
        let s = screen(&["Claude Code v2.1.316", RULE, "❯\u{a0}", RULE, line]);
        assert_eq!(
            observe_screen(&s),
            Some((
                expected_id.into(),
                expected_label.into(),
                expected_effort.map(str::to_owned),
            ))
        );
    }

    let narrow_banner = screen(&[
        "Claude Code v2.1.316",
        "▝▜█████▛▘  Opus 5.5 (1M",
        "  ▘▘ ▝▝    context) · Claude Max",
    ]);
    assert_eq!(
        observe_screen(&narrow_banner),
        Some(("opus".into(), "Opus 5.5".into(), None))
    );

    let narrow_footer = screen(&[
        RULE,
        "❯\u{a0}",
        RULE,
        "  Sonnet 5.5 (200k context)",
        "  · xhigh",
    ]);
    assert_eq!(
        observe_screen(&narrow_footer),
        Some(("sonnet".into(), "Sonnet 5.5".into(), Some("xhigh".into())))
    );
}

#[test]
fn observe_screen_and_parser_reject_prose() {
    assert!(parse_model_spec("I prefer Opus 5.5 for coding").is_none());
    assert!(parse_model_spec("Opus 5.5 is faster than Sonnet 5.5").is_none());
    assert!(parse_model_spec("Please use Sonnet 5.5 · medium.").is_none());

    let prose = screen(&[
        "Claude Code v2.1.316",
        "~/project",
        "",
        "Try Opus 5.5 (1M context) · high on difficult tasks.",
        "",
        "Opus 5.5 works best with explicit plans.",
        "────────────────────────",
        "Opus 5.5 · high",
    ]);
    assert!(observe_screen(&prose).is_none());
}

#[test]
fn stale_startup_banner_never_overwrites_existing_running_model() {
    let banner = screen(&[
        " * ▐▛███▜▌ *   Claude Code v2.1.316",
        "* ▝▜█████▛▘ *  Opus 5.5 · high",
    ]);
    let mut status = json!({
        "agent": "claude",
        "model": "claude-sonnet-5-5",
        "model_label": null,
        "effort": null
    });
    merge_screen_observation(&mut status, &banner);
    assert_eq!(status["model"], "claude-sonnet-5-5");
    assert!(status["model_label"].is_null());
    assert!(status["effort"].is_null());
}
