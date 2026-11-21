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
    assert!(confirmed(row, "sonnet", "Sonnet"));
    assert!(!confirmed(row, "opus", "Opus"));
    let note = "   1562\n        Set model to Opus 5.5 and saved as your default";
    assert!(confirmed(note, "opus", "Opus"));
    assert!(confirmed(
        "Set model to Default (Opus 5.5)",
        "default",
        "Default (recommended)"
    ));
    assert!(confirmed(
        "Set model to Opus 5.5 (default) and saved as your default for new sessions",
        "default",
        "Default (recommended)"
    ));
    assert!(confirmed(
        "Model set to Sonnet 5.5 for this session only",
        "sonnet",
        "Sonnet"
    ));
    assert!(confirmed("Kept model as Opus 5.5", "opus", "Opus"));
    assert!(!confirmed("/model opus", "opus", "Opus"));
}

#[test]
fn effort_confirmation_handles_normal_capped_and_rejected_outputs() {
    assert_eq!(
        effort_confirmed(
            "Set effort level to high (saved as your default for new sessions): Comprehensive",
            "high"
        ),
        Some("high".into())
    );
    assert_eq!(
        effort_confirmed(
            "Set effort level to medium (this session only): Balanced",
            "medium"
        ),
        Some("medium".into())
    );
    assert_eq!(
        effort_confirmed(
            "Effort 'max' exceeds the cap for opus set by your settings or organization; set to 'high' instead (this session only): Comprehensive",
            "max"
        ),
        Some("high".into())
    );
    assert_eq!(effort_confirmed("/effort high", "high"), None);
    assert_eq!(effort_confirmed("Set effort level to low", "high"), None);
    assert_eq!(
        effort_confirmed(
            "CLAUDE_CODE_EFFORT_LEVEL=low overrides this session — clear it and high takes over",
            "high"
        ),
        None
    );
    assert!(effort_rejected(
        "CLAUDE_CODE_EFFORT_LEVEL=low overrides this session"
    ));
    assert!(effort_rejected("Couldn't set effort level: invalid"));
}

#[test]
fn cache_confirm_dialog_matches_only_safe_focused_cache_dialogs() {
    let model_dialog = screen(&[
        "Switch model?",
        "Your next response will be slower and use more tokens",
        "This conversation is cached for the current model. Switching to Sonnet 5.5 means the full history gets re-read.",
        "❯ 1. Yes, switch to Sonnet 5.5",
        "  2. No, go back",
    ]);
    assert!(cache_confirm_dialog(
        &model_dialog,
        CacheDialog::Model {
            id: "sonnet",
            label: "Sonnet"
        }
    ));
    assert!(!cache_confirm_dialog(
        &model_dialog,
        CacheDialog::Model {
            id: "opus",
            label: "Opus"
        }
    ));

    let default_dialog = screen(&[
        "Switch model?",
        "Your next response will be slower and use more tokens",
        "This conversation is cached for the current model. Switching to Opus 5.5 (default) means the full history gets re-read.",
        "❯ 1. Yes, switch to Opus 5.5 (default)",
        "  2. No, go back",
    ]);
    assert!(cache_confirm_dialog(
        &default_dialog,
        CacheDialog::Model {
            id: "default",
            label: "Default (recommended)"
        }
    ));

    let effort_dialog = screen(&[
        "Change effort level?",
        "Your next response will be slower and use more tokens",
        "This conversation is cached for the current effort level. Switching to high means the full history gets re-read.",
        "❯ 1. Yes, switch to high",
        "  2. No, go back",
    ]);
    assert!(cache_confirm_dialog(
        &effort_dialog,
        CacheDialog::Effort("high")
    ));
    assert!(!cache_confirm_dialog(
        &effort_dialog,
        CacheDialog::Effort("low")
    ));

    let cancel_focused = screen(&[
        "Change effort level?",
        "This conversation is cached for the current effort level. Switching to high means the full history gets re-read.",
        "  1. Yes, switch to high",
        "❯ 2. No, go back",
    ]);
    assert!(!cache_confirm_dialog(
        &cancel_focused,
        CacheDialog::Effort("high")
    ));

    let hook_dialog = screen(&[
        "Switch model?",
        "A PreModelSwitch hook asked you to confirm",
        "This conversation is cached for the current model.",
        "❯ 1. Yes, switch to Sonnet 5.5",
        "  2. No, go back",
    ]);
    assert!(!cache_confirm_dialog(
        &hook_dialog,
        CacheDialog::Model {
            id: "sonnet",
            label: "Sonnet"
        }
    ));
    assert!(needs_manual_confirm(&hook_dialog));
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
        ("Fable 5.2 · xhigh", "fable", "Fable 5.2", Some("xhigh")),
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

#[test]
fn live_footer_overrides_stale_transcript_model_and_effort() {
    let footer = screen(&[
        "Claude Code v2.1.330",
        RULE,
        "❯\u{a0}",
        RULE,
        "  Sonnet 5.5 · medium | main*",
    ]);
    let mut status = json!({
        "agent": "claude",
        "model": "claude-opus-5-5",
        "model_label": "Opus 5.5",
        "effort": "high"
    });
    merge_screen_observation(&mut status, &footer);
    assert_eq!(status["model"], "sonnet");
    assert_eq!(status["model_label"], "Sonnet 5.5");
    assert_eq!(status["effort"], "medium");

    let effort_only = screen(&[
        "Claude Code v2.1.330",
        RULE,
        "❯\u{a0}",
        RULE,
        "  Opus 5.5 · low | main*",
    ]);
    let mut same_family = json!({
        "agent": "claude",
        "model": "claude-opus-5-5",
        "model_label": null,
        "effort": "xhigh"
    });
    merge_screen_observation(&mut same_family, &effort_only);
    assert_eq!(same_family["model"], "claude-opus-5-5");
    assert_eq!(same_family["model_label"], "Opus 5.5");
    assert_eq!(same_family["effort"], "low");
}

#[test]
fn waiting_screen_observation_populates_model_without_idle_composer() {
    let prompt_screen = screen(&[
        " * ▐▛███▜▌ *   Claude Code v2.1.316",
        "* ▝▜█████▛▘ *  Opus 5.5 · high",
        "Allow Read on Cargo.toml? 1. Yes / 2. No",
    ]);
    let mut status = json!({
        "agent": "claude",
        "supported": true,
        "model": null,
        "model_label": null,
        "effort": null
    });
    observe_screen_into(Some(&prompt_screen), &mut status);
    assert_eq!(status["model"], "opus");
    assert_eq!(status["model_label"], "Opus 5.5");
    assert_eq!(status["effort"], "high");
    assert_eq!(status["effort_supported"], false);
}

#[cfg(unix)]
mod pty_tests {
    use super::*;
    use crate::{appearance::Appearance, session::Session};
    use krill_proto::{SessionId, holder::WinSize};
    use std::{
        collections::BTreeMap,
        io::{Read as _, Write as _},
        path::PathBuf,
        sync::Arc,
    };

    fn pty_fixture(id: u64) -> (Session, std::fs::File) {
        let pty = rustix_openpty::openpty(None, None).unwrap();
        let mut termios = rustix::termios::tcgetattr(&pty.user).unwrap();
        termios.make_raw();
        rustix::termios::tcsetattr(&pty.user, rustix::termios::OptionalActions::Now, &termios)
            .unwrap();
        rustix::fs::fcntl_setfl(
            &pty.user,
            rustix::fs::fcntl_getfl(&pty.user).unwrap() | rustix::fs::OFlags::NONBLOCK,
        )
        .unwrap();
        let session = Session::start(
            SessionId(id),
            pty.controller,
            WinSize {
                cols: 120,
                rows: 30,
            },
            Arc::new(Appearance::load(PathBuf::new())),
            None,
        )
        .unwrap();
        let mut user = std::fs::File::from(pty.user);
        let initial = format!(
            "\x1b[2J\x1b[HClaude Code v2.1.330\r\n{RULE}\r\n❯\u{a0}\r\n{RULE}\r\n  Opus 5.5 · medium\x1b[3;3H"
        );
        user.write_all(initial.as_bytes()).unwrap();
        let started = Instant::now();
        while !effort_available(&session.tap().capture().0) {
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(110));
        (session, user)
    }

    fn read_bytes(user: &mut std::fs::File, expected: &[u8]) {
        let mut buf = Vec::new();
        let started = Instant::now();
        while !buf.windows(expected.len()).any(|w| w == expected) {
            let mut chunk = [0u8; 256];
            match user.read(&mut chunk) {
                Ok(0) => panic!("PTY closed while waiting for {expected:?}"),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("PTY read error: {e}"),
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "timed out waiting for {expected:?}, got {buf:?}"
            );
        }
    }

    fn test_context() -> Context {
        let missing = std::env::temp_dir().join("krill-claude-test-nonexistent");
        Context {
            home: missing.clone(),
            cwd: missing,
            agent: "claude".into(),
            config: None,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn set_effort_confirms_directly_from_pty_output() {
        let (session, mut user) = pty_fixture(942);
        let responder = std::thread::spawn(move || {
            read_bytes(&mut user, b"/effort high");
            user.write_all(b"\x1b[3;1H\x1b[2K\xe2\x9d\xaf\xc2\xa0/effort high")
                .unwrap();
            read_bytes(&mut user, b"\r");
            let done = format!(
                "\x1b[2J\x1b[HClaude Code v2.1.330\r\n  Set effort level to high (saved as your default for new sessions): Comprehensive\r\n{RULE}\r\n❯\u{a0}\r\n{RULE}\r\n  Opus 5.5 · high\x1b[4;3H"
            );
            user.write_all(done.as_bytes()).unwrap();
            user
        });
        let result = set_effort(&session.tap(), false, "high").unwrap();
        assert_eq!(result["applied"], true);
        assert_eq!(result["effort"], "high");
        let user = responder.join().unwrap();
        drop(user);
        session.wait().unwrap();
    }

    #[test]
    fn set_effort_auto_confirms_cache_miss_dialog() {
        let (session, mut user) = pty_fixture(943);
        let responder = std::thread::spawn(move || {
            read_bytes(&mut user, b"/effort high");
            user.write_all(b"\x1b[3;1H\x1b[2K\xe2\x9d\xaf\xc2\xa0/effort high")
                .unwrap();
            read_bytes(&mut user, b"\r");
            user.write_all(
                b"\x1b[2J\x1b[HClaude Code v2.1.330\r\nChange effort level?\r\nYour next response will be slower and use more tokens\r\nThis conversation is cached for the current effort level. Switching to high means the full history gets re-read.\r\n\xe2\x9d\xaf 1. Yes, switch to high\r\n  2. No, go back\r\n",
            )
            .unwrap();
            read_bytes(&mut user, b"\r");
            let done = format!(
                "\x1b[2J\x1b[HClaude Code v2.1.330\r\n  Set effort level to high (this session only): Comprehensive\r\n{RULE}\r\n❯\u{a0}\r\n{RULE}\r\n  Opus 5.5 · high\x1b[4;3H"
            );
            user.write_all(done.as_bytes()).unwrap();
            user
        });
        let result = set_effort(&session.tap(), false, "high").unwrap();
        assert_eq!(result["applied"], true);
        assert_eq!(result["effort"], "high");
        let user = responder.join().unwrap();
        drop(user);
        session.wait().unwrap();
    }

    #[test]
    fn set_model_auto_confirms_cache_miss_dialog() {
        let (session, mut user) = pty_fixture(944);
        let responder = std::thread::spawn(move || {
            read_bytes(&mut user, b"/model sonnet");
            user.write_all(b"\x1b[3;1H\x1b[2K\xe2\x9d\xaf\xc2\xa0/model sonnet")
                .unwrap();
            read_bytes(&mut user, b"\r");
            user.write_all(
                b"\x1b[2J\x1b[HClaude Code v2.1.330\r\nSwitch model?\r\nYour next response will be slower and use more tokens\r\nThis conversation is cached for the current model. Switching to Sonnet 5.5 means the full history gets re-read.\r\n\xe2\x9d\xaf 1. Yes, switch to Sonnet 5.5\r\n  2. No, go back\r\n",
            )
            .unwrap();
            read_bytes(&mut user, b"\r");
            let done = format!(
                "\x1b[2J\x1b[HClaude Code v2.1.330\r\n  Set model to Sonnet 5.5 and saved as your default for new sessions\r\n{RULE}\r\n❯\u{a0}\r\n{RULE}\r\n  Sonnet 5.5 · medium\x1b[4;3H"
            );
            user.write_all(done.as_bytes()).unwrap();
            user
        });
        let result = set_model(&test_context(), &session.tap(), false, "sonnet").unwrap();
        assert_eq!(result["applied"], true);
        assert_eq!(result["model"], "sonnet");
        let user = responder.join().unwrap();
        drop(user);
        session.wait().unwrap();
    }

    #[test]
    fn set_model_never_auto_confirms_pre_model_switch_hook_dialog() {
        let (session, mut user) = pty_fixture(945);
        let responder = std::thread::spawn(move || {
            read_bytes(&mut user, b"/model sonnet");
            user.write_all(b"\x1b[3;1H\x1b[2K\xe2\x9d\xaf\xc2\xa0/model sonnet")
                .unwrap();
            read_bytes(&mut user, b"\r");
            user.write_all(
                b"\x1b[2J\x1b[HClaude Code v2.1.330\r\nSwitch model?\r\nA PreModelSwitch hook asked you to confirm\r\n\xe2\x9d\xaf 1. Yes, switch to Sonnet 5.5\r\n  2. No, go back\r\n",
            )
            .unwrap();
            user
        });
        let result = set_model(&test_context(), &session.tap(), false, "sonnet").unwrap();
        assert_eq!(result["applied"], false);
        assert_eq!(result["reason"], MANUAL_CONFIRM);
        let mut user = responder.join().unwrap();
        let mut extra = [0u8; 64];
        assert_eq!(
            user.read(&mut extra).unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "hook dialog must not receive an automatic Enter"
        );
        drop(user);
        session.wait().unwrap();
    }
}
