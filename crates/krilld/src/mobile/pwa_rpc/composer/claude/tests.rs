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
#[test]
fn native_verified_aliases_are_bounded_and_version_is_required() {
    assert_eq!(ALIASES.len(), 5);
    assert!(ALIASES.iter().any(|(id, _)| *id == "sonnet"));
    let mut s = screen(&["Claude Code v2.1.311"]);
    assert!(verified(&s));
    s.rows[0].clear();
    assert!(!verified(&s));
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
    let s = screen(&[
        "⎿  Set model to Sonnet actual-version for this session only",
        "",
        "",
        "",
    ]);
    assert!(confirmed(&s, "Sonnet"));
    assert!(!confirmed(&s, "Opus"));
    let persistent = screen(&[
        "⎿  Set model to Sonnet actual-version and saved as your default for new sessions",
        "",
        "",
        "",
    ]);
    assert!(confirmed(&persistent, "Sonnet"));
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
        verified_codex: false,
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
