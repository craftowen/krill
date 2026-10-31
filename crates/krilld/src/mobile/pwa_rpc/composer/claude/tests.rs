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
const EFFORT_ROW: &str = "                                                                                  ● high · /effort";
const SAVED: &str = "  ⎿  Set model to Opus 5.5 and saved as your default for new sessions";

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
fn native_persistence_is_disclosed_but_unrelated_models_are_not_confirmed() {
    let s = composer_below(&[
        "❯ /model sonnet",
        "  ⎿  Set model to Sonnet actual-version for this session only",
    ]);
    assert!(confirmed(&s, "/model sonnet", "Sonnet"));
    assert!(!confirmed(&s, "/model sonnet", "Opus"));
    let persistent = composer_below(&[
        "❯ /model sonnet",
        "  ⎿  Set model to Sonnet actual-version and saved as your default for new sessions",
    ]);
    assert!(confirmed(&persistent, "/model sonnet", "Sonnet"));
    let default = composer_below(&[
        "❯ /model default",
        "  ⎿  Set model to Default (Opus 5.5) for this session only",
    ]);
    assert!(confirmed(
        &default,
        "/model default",
        "Default (recommended)"
    ));
}

#[test]
fn the_effort_indicator_row_does_not_hide_the_confirmation() {
    let s = composer_below(&["❯ /model opus", SAVED, EFFORT_ROW]);
    assert!(confirmed(&s, "/model opus", "Opus"));
    assert!(!confirmed(&s, "/model sonnet", "Sonnet"));
}

#[test]
fn a_confirmation_wrapped_by_a_narrow_terminal_is_recognized() {
    let s = composer_below(&[
        "❯ /model opus",
        "  ⎿  Set model to Opus 5.5 and saved as your default for",
        "     new sessions",
        "                                      ● high · /effort",
    ]);
    assert!(confirmed(&s, "/model opus", "Opus"));
}

#[test]
fn an_older_confirmation_is_not_this_commands_confirmation() {
    for above in [
        &["❯ /model opus", SAVED, "❯ /model opus", EFFORT_ROW][..],
        &["❯ /model opus", SAVED, "❯ hello", "● Hi", EFFORT_ROW],
        &[
            "❯ /model opus",
            SAVED,
            "❯ /model opus",
            "  ⎿  Model unavailable",
        ],
        &["❯ /model sonnet", SAVED, EFFORT_ROW],
        &[SAVED, EFFORT_ROW],
    ] {
        assert!(!confirmed(&composer_below(above), "/model opus", "Opus"));
    }
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
        set_model(&context, &session.tap(), "sonnet")
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
